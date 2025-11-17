use std::fmt::Debug;
use std::fmt::Write as FmtWrite;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::{fs, io};

use crate::MmmcCorner;
use crate::{Checkpoint, MmmcConfig, SubmoduleInfo, Substep, mmmc, sdc};
use fs::File;
use indoc::formatdoc;
use rivet::Step;
use rust_decimal::Decimal;
use std::sync::Arc;

#[derive(Debug)]
pub struct InnovusStep {
    pub work_dir: PathBuf,
    pub module: String,
    pub substeps: Vec<Substep>,
    pub pinned: bool,
    pub start_checkpoint: Option<Checkpoint>,
    pub dependencies: Vec<Arc<dyn Step>>,
}

impl InnovusStep {
    pub fn new(
        work_dir: impl Into<PathBuf>,
        module: impl Into<String>,
        substeps: Vec<Substep>,
        pinned: bool,
        deps: Vec<Arc<dyn Step>>,
    ) -> Self {
        let dir = work_dir.into();
        let modul = module.into();
        InnovusStep {
            work_dir: dir,
            module: modul,
            substeps,
            pinned,
            start_checkpoint: None,
            dependencies: deps,
        }
    }

    fn make_tcl_file(&self, path: &PathBuf, steps: Vec<Substep>) -> io::Result<()> {
        let mut tcl_file = File::create(path).expect("failed to create par.tcl file");

        if let Some(checkpoint) = self.start_checkpoint.as_ref() {
            writeln!(tcl_file, "read_db {}", checkpoint.path.display()).expect("Failed to write");
        }

        for step in steps.into_iter() {
            println!("\n--> Parsing step: {}\n", step.name);
            if step.checkpoint {
                let checkpoint_file = self.work_dir.join(format!("pre_{}", step.name.clone()));

                writeln!(tcl_file, "write_db {}", checkpoint_file.display())?;
            }
            writeln!(tcl_file, "{}", step.command)?;
        }
        writeln!(tcl_file, "exit")?;

        println!("\nFinished creating tcl file\n");
        Ok(())
    }

    pub fn add_hook(&mut self, name: &str, tcl: &str, index: usize, checkpointed: bool) {
        self.substeps.insert(
            index,
            Substep {
                name: name.to_string(),
                command: tcl.to_string(),
                checkpoint: checkpointed,
            },
        );
    }

    pub fn ilm_path(&self) -> PathBuf {
        self.work_dir.join(format!("{}ILMDir", self.module))
    }

    pub fn lef_path(&self) -> PathBuf {
        self.work_dir.join(format!("{}ILM.lef", self.module))
    }

    pub fn sdc_path(&self) -> PathBuf {
        self.work_dir.join(format!("{}.mapped.sdc", self.module))
    }

    pub fn add_checkpoint(&mut self, name: String, checkpoint_path: PathBuf) {
        self.start_checkpoint = Some(Checkpoint {
            name: name,
            path: checkpoint_path,
        });
    }
}

impl Step for InnovusStep {
    fn execute(&self) {
        let tcl_path = self.work_dir.clone().join("par.tcl");

        let mut substeps = self.substeps.clone();

        if let Some(checkpoint) = self.start_checkpoint.as_ref() {
            let slice_index = substeps
                .iter()
                .position(|s| s.name == checkpoint.name)
                .expect("Failed to find checkpoint name");
            substeps = substeps[slice_index..].to_vec();
        }

        self.make_tcl_file(&tcl_path, substeps)
            .expect("Failed to create par.tcl");

        let status = Command::new("innovus")
            .args(["-file", tcl_path.to_str().unwrap(), "-stylus"])
            .current_dir(self.work_dir.clone())
            .status()
            .expect("Failed to execute par.tcl");

        if !status.success() {
            eprintln!("Failed to execute par.tcl");
            panic!("Stopped flow");
        }
    }

    fn deps(&self) -> Vec<Arc<dyn Step>> {
        self.dependencies.clone()
    }

    fn pinned(&self) -> bool {
        self.pinned
    }
}

#[derive(Debug, Clone)]
pub struct Layer {
    pub top: String,
    pub bot: String,
    pub spacing: Decimal,
    pub trim_antenna: bool,
    pub add_stripes_command: String,
}

#[derive(Debug, Clone)]
pub struct PinAssignment {
    pub pins: String,
    pub module: String,
    pub patterns: String,
    pub layer: String,
    pub side: String,
    pub start: String,
    pub end: String,
    pub assign: String,
    pub width: String,
    pub depth: String,
}

pub fn set_default_process(node_size: i64) -> Substep {
    Substep {
        checkpoint: false,
        name: "set_default_options".into(),
        command: formatdoc!(
            r#"
        set_db design_process_node {} 
        set_multi_cpu_usage -local_cpu 12
        set_db timing_analysis_cppr both
        set_db timing_analysis_type ocv
        "#,
            node_size
        ),
    }
}

pub fn par_read_design_files(
    work_dir: &Path,
    module: &str,
    netlist_path: &Path,
    mmmc_conf: MmmcConfig,
    tlef: &Path,
    pdk_lef: &Path,
    submodules: Option<Vec<SubmoduleInfo>>,
) -> Substep {
    let mut sdc_file =
        File::create(&work_dir.join("clock_pin_constraints.sdc")).expect("failed to create file");
    writeln!(sdc_file, "{}", sdc()).expect("Failed to write");
    let mmmc_tcl = mmmc(mmmc_conf);
    let mmmc_tcl_path = work_dir.to_path_buf().join("mmmc.tcl");
    let _ = fs::write(&mmmc_tcl_path, mmmc_tcl);
    let netlist = netlist_path.to_path_buf();
    let mut lefs_vec = vec![tlef.display().to_string(), pdk_lef.display().to_string()];

    if let Some(submodule_lefs) = &submodules {
        lefs_vec.extend(
            submodule_lefs
                .iter()
                .map(|p| p.lef.to_string_lossy().to_string()),
        );
    }

    let lefs: String = lefs_vec.join(" ");
    let mut command = formatdoc!(
        r#"
            read_physical -lef {{ {} }}
            read_mmmc {}
            read_netlist {} -top {}
            "#,
        lefs,
        mmmc_tcl_path.display(),
        netlist.display(),
        module.to_owned(),
    );

    if let Some(submodule_vec) = submodules {
        for submodule in submodule_vec {
            // genus 231
            writeln!(
                command,
                "read_ilm -cell {} -directory {}",
                submodule.name,
                submodule.ilm.display(),
            )
            .unwrap();

            // genus 221 actually need to include the left in addition to the ilm when reading ilms
            // writeln!(
            //     command,
            //     "read_ilm -module_name {} -basename {}",
            //     submodule.name,
            //     submodule.ilm.display(),
            // )
            // .unwrap();
        }
    }

    Substep {
        checkpoint: false,
        command: command,
        name: "read_design_files".into(),
    }
}

pub fn par_init_design() -> Substep {
    Substep {
        checkpoint: false,
        command: "init_design".to_string(),
        name: "init_design".to_string(),
    }
}

pub fn innovus_settings(bottom_routing: i64, top_routing: i64) -> Substep {
    Substep {
        checkpoint: false,
        command: formatdoc!(
            r#"
            set_db design_bottom_routing_layer {}
            set_db design_top_routing_layer {}
            set_db design_flow_effort standard
            set_db design_power_effort low
            "#,
            bottom_routing,
            top_routing
        ),
        name: "innovus_settings".into(),
    }
}

pub fn floorplan_design(
    work_dir: &Path,
    power_spec: &String,
    die_constraints: DieConstraints,
) -> Substep {
    let floorplan_tcl_path = work_dir.join("floorplan.tcl");
    let constraints = format!(
        "{} {} {} {} {} {}",
        die_constraints.w,
        die_constraints.h,
        die_constraints.left,
        die_constraints.bottom,
        die_constraints.right,
        die_constraints.top
    );
    let mut floorplan_tcl_file = File::create(&floorplan_tcl_path).expect("failed to create file");
    writeln!(floorplan_tcl_file,"create_floorplan -core_margins_by die -flip f -die_size_by_io_height max -site CoreSite -die_size {{ {} }}", constraints ).expect("Failed to write");
    let floorplan_path_string = floorplan_tcl_path.display();

    let power_spec_file_path = work_dir.join("power_spec.cpf");
    let mut power_spec_file = File::create(&power_spec_file_path).expect("failed to create file");
    writeln!(power_spec_file, "{}", power_spec).expect("Failed to write");
    let power_spec_file_string = power_spec_file_path.display();
    let command = formatdoc!(
        r#"
            source -echo -verbose {floorplan_path_string} 
            flatten_ilm 
            read_power_intent -cpf {power_spec_file_string}
            commit_power_intent
            unflatten_ilm
            "#
    );
    Substep {
        checkpoint: true,
        command: command,
        name: "floorplan_design".into(),
    }
}

pub fn place_tap_cells() -> Substep {
    Substep {
        checkpoint: true,
        command: "".into(),
        name: "place_tap_cells".into(),
    }
}

pub fn power_straps(straps: Vec<Layer>) -> Substep {
    let mut definitions = String::new();
    for strap in straps.into_iter() {
        writeln!(
            &mut definitions,
            "#Power strap definition for layer {}:",
            strap.top
        )
        .expect("Failed to write");
        writeln!(
            &mut definitions,
            "set_db add_stripes_stacked_via_top_layer {}",
            strap.top
        )
        .expect("Failed to write");
        writeln!(
            &mut definitions,
            "set_db add_stripes_stacked_via_bottom_layer {}",
            strap.bot
        )
        .expect("Failed to write");

        if strap.trim_antenna {
            writeln!(
                &mut definitions,
                "set_db add_stripes_trim_antenna_back_to_shape {{stripe}}"
            )
            .expect("Failed to write");
        }
        writeln!(
            &mut definitions,
            "set_db add_stripes_spacing_from_block {}",
            strap.spacing
        )
        .expect("Failed to write");
        writeln!(&mut definitions, "{}", strap.add_stripes_command).expect("Failed to write");
    }

    Substep {
        checkpoint: true,
        command: definitions,
        name: "power_straps".into(),
    }
}

pub fn place_pins(top_layer: &str, bot_layer: &str, assignments: Vec<PinAssignment>) -> Substep {
    let mut place_pins_commands = String::new();
    writeln!(place_pins_commands, "set_db assign_pins_edit_in_batch true")
        .expect("Failed to write");
    writeln!(
        place_pins_commands,
        "set_db assign_pins_promoted_macro_bottom_layer {bot_layer}"
    )
    .expect("Failed to write");
    writeln!(
        place_pins_commands,
        "set_db assign_pins_promoted_macro_top_layer {top_layer}"
    )
    .expect("Failed to write");

    writeln!(place_pins_commands, "set all_ppins \"\" ").expect("Failed to write");

    for assignment in assignments.into_iter() {
        writeln!(
            place_pins_commands,
            "edit_pin -fixed_pin -pin {} -hinst {} {} {} {} {} {} {} {} {}",
            assignment.pins,
            assignment.module,
            assignment.patterns,
            assignment.layer,
            assignment.side,
            assignment.start,
            assignment.end,
            assignment.assign,
            assignment.width,
            assignment.depth
        )
        .expect("Failed to write");
    }

    writeln!(place_pins_commands, "if {{[llength $all_ppins] ne 0}} {{assign_io_pins -move_fixed_pin -pins [get_db $all_ppins .net.name]}}").expect("Failed to write");
    writeln!(
        place_pins_commands,
        "set_db assign_pins_edit_in_batch false"
    )
    .expect("Failed to write");

    Substep {
        checkpoint: true,
        command: place_pins_commands,
        name: "place_pins".into(),
    }
}

pub fn place_opt_design(sdc_files: Option<PathBuf>) -> Substep {
    let sdc_command = if let Some(sdc_files) = sdc_files {
        sdc_files.display().to_string()
    } else {
        "".to_string()
    };

    let command = formatdoc!(
        r#"
            set unplaced_pins [get_db ports -if {{.place_status == unplaced}}]
            if {{$unplaced_pins ne ""}} {{
                print_message -error "Some pins remain unplaced, which will cause invalid placement and routing. These are the unplaced pins: $unplaced_pins"
                exit 2
            }}
            {sdc_command}
            set_db opt_enable_podv2_clock_opt_flow true
            place_opt_design
        "#
    );
    Substep {
        checkpoint: true,
        command: command,
        name: "place_opt_design".into(),
    }
}

pub fn add_fillers(filler_cells: Vec<String>) -> Substep {
    let cells = format!("\"{}\"", filler_cells.join(" "));
    Substep {
        checkpoint: true,
        command: formatdoc!(
            r#"
            set_db add_fillers_cells {cells}
            add_fillers
            "#
        ),
        name: "add_fillers".into(),
    }
}

pub fn route_design() -> Substep {
    Substep {
        checkpoint: true,
        command: formatdoc!(
            r#"
                flatten_ilm
                set_db design_express_route true
                route_design
            "#
        ),
        name: "route_design".into(),
    }
}

pub fn opt_design() -> Substep {
    Substep {
        checkpoint: true,
        command: formatdoc!(
            r#"
                set_db opt_post_route_hold_recovery auto
                set_db opt_post_route_fix_si_transitions true
                set_db opt_verbose true
                set_db opt_detail_drv_failure_reason true
                set_db opt_sequential_genus_restructure_report_failure_reason true
                opt_design -post_route -setup -hold -expanded_views -timing_debug_report
                unflatten_ilm
            "#
        ),
        name: "opt_design".into(),
    }
}

pub fn write_regs() -> Substep {
    // TODO: add childmodule.tcl
    let childmodule_tcl = "";
    Substep {
        checkpoint: true,
        command: formatdoc!(
            r#"
            flatten_ilm
            {childmodule_tcl}
        set write_cells_ir "./find_regs_cells.json"
        set write_cells_ir [open $write_cells_ir "w"]
        puts $write_cells_ir "\["

        set refs [get_db [get_db lib_cells -if .is_sequential==true] .base_name]

        set len [llength $refs]

        for {{set i 0}} {{$i < [llength $refs]}} {{incr i}} {{
            if {{$i == $len - 1}} {{
                puts $write_cells_ir "    \"[lindex $refs $i]\""
            }} else {{
                puts $write_cells_ir "    \"[lindex $refs $i]\","
            }}
        }}

        puts $write_cells_ir "\]"
        close $write_cells_ir
        set write_regs_ir "./find_regs_paths.json"
        set write_regs_ir [open $write_regs_ir "w"]
        puts $write_regs_ir "\["

        set regs [get_db [get_db [all_registers -edge_triggered -output_pins] -if .direction==out] .name]

        set len [llength $regs]

        for {{set i 0}} {{$i < [llength $regs]}} {{incr i}} {{
            #regsub -all {{/}} [lindex $regs $i] . myreg
            set myreg [lindex $regs $i]
            if {{$i == $len - 1}} {{
                puts $write_regs_ir "    \"$myreg\""
            }} else {{
                puts $write_regs_ir "    \"$myreg\","
            }}
        }}

        puts $write_regs_ir "\]"

        close $write_regs_ir
        unflatten_ilm
        "#
        ),
        name: "write_regs".into(),
    }
}

pub fn par_write_design(
    pdk_root: &Path,
    work_dir: &Path,
    module: &str,
    corners: Vec<MmmcCorner>,
) -> Substep {
    let root = pdk_root.display();
    let par_rundir = work_dir.display();
    let module = module.to_owned();
    let setup = corners
        .iter()
        .find(|p| p.corner_type == "setup")
        .unwrap()
        .name
        .clone();
    let hold = corners
        .iter()
        .find(|p| p.corner_type == "hold")
        .unwrap()
        .name
        .clone();
    let typical = corners
        .iter()
        .find(|p| p.corner_type == "extra")
        .unwrap()
        .name
        .clone();

    Substep {
        checkpoint: true,
        command: formatdoc!(
            r#"
            set_db timing_enable_simultaneous_setup_hold_mode true
            write_db {module}_FINAL -def -verilog
            set_db write_stream_virtual_connection false
            connect_global_net VDD -type net -net_base_name VPWR
            connect_global_net VDD -type net -net_base_name VPB
            connect_global_net VDD -type net -net_base_name vdd
            connect_global_net VSS -type net -net_base_name VGND
            connect_global_net VSS -type net -net_base_name VNB
            connect_global_net VSS -type net -net_base_name vss
            write_netlist {par_rundir}/{module}.lvs.v -top_module_first -top_module {module} -exclude_leaf_cells -phys -flat -exclude_insts_of_cells {{}}
            write_netlist {par_rundir}/{module}.sim.v -top_module_first -top_module {module} -exclude_leaf_cells -exclude_insts_of_cells {{}}
            write_stream -mode ALL -format stream -map_file /scratch/cs199-cbc/rivet/pdks/sky130/src/sky130_lefpin.map -uniquify_cell_names -merge {{ {root}/sky130/sky130_cds/sky130_scl_9T_0.0.5/gds/sky130_scl_9T.gds }}  {par_rundir}/{module}.gds
            write_sdf -max_view {setup}.setup_view -min_view {hold}.hold_view -typical_view {typical}.extra_view {par_rundir}/{module}.par.sdf
            set_db extract_rc_coupled true
            extract_rc
            write_parasitics -spef_file {par_rundir}/{module}.{setup}.par.spef -rc_corner {setup}.setup_rc
            write_parasitics -spef_file {par_rundir}/{module}.{hold}.par.spef -rc_corner {hold}.hold_rc
            write_parasitics -spef_file {par_rundir}/{module}.{typical}.par.spef -rc_corner {typical}.extra_rc
            write_db post_write_design
            ln -sfn post_write_design latest
            "#
        ),
        name: "write_design".into(),
    }
}

pub fn write_ilm(work_dir: &Path, module: &str, layer: &Layer) -> Substep {
    // def output_ilm_sdcs(self) -> List[str]:
    // corners = self.get_mmmc_corners()
    // if corners:
    //     filtered = list(filter(lambda c: c.type in [MMMCCornerType.Setup, MMMCCornerType.Hold], corners))
    //     ctype_map = {MMMCCornerType.Setup: "setup", MMMCCornerType.Hold: "hold"}
    //     return list(map(lambda c: os.path.join(self.run_dir, "{top}_postRoute_{corner_name}.{corner_type}_view.core.sdc".format(
    //         top=self.top_module, corner_name=c.name, corner_type=ctype_map[c.type])), filtered))
    // else:
    //     return [os.path.join(self.run_dir, "{top}_postRoute.core.sdc".format(top=self.top_module))]
    let ilm_dir = work_dir
        .join(format!("{}ILMDir", module))
        .display()
        .to_string();
    let top_layer = layer.top.clone();

    let command = formatdoc!(
        r#"

            set_db timing_enable_simultaneous_setup_hold_mode false
            time_design -post_route
            time_design -post_route -hold
            check_process_antenna
            write_lef_abstract -5.8 -top_layer {top_layer} -stripe_pins -pg_pin_layers {{{top_layer}}} {module}ILM.lef
            write_ilm -model_type all -to_dir {ilm_dir} -type_flex_ilm ilm
            "#
    );

    // for sdc_out in self.output_ilm_sdcs:
    //     self.append('gzip -d -c {ilm_dir_name}/mmmc/ilm_data/{top}/{sdc_in}.gz | sed "s/get_pins/get_pins -hierarchical/g" > {sdc_out}'.format(
    //         ilm_dir_name=self.ilm_dir_name, top=self.top_module, sdc_in=os.path.basename(sdc_out), sdc_out=sdc_out))

    Substep {
        checkpoint: false,
        command: command,
        name: "write_ilm".into(),
    }
}
/// w: width, h: height, left: x-coordinate of left edge, bottom: y-coordinate of bottom edge, right: x-coordinate of right edge, top: y-coordinate of top edge
#[derive(Debug, Clone)]
pub struct DieConstraints {
    pub w: i64,
    pub h: i64,
    pub left: i64,
    pub bottom: i64,
    pub right: i64,
    pub top: i64,
}
