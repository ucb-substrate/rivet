use std::fmt::Debug;
use std::fmt::Write as FmtWrite;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::{fs, io};

use crate::{Checkpoint, MmmcConfig, SubmoduleInfo, Substep, mmmc, sdc};
use fs::File;
use indoc::formatdoc;
use rivet::Step;
use std::sync::Arc;

/// Defines the working directory of the tool and which module to synthesize
#[derive(Debug)]
pub struct GenusStep {
    pub work_dir: PathBuf,
    pub module: String,
    pub substeps: Vec<Substep>,
    pub pinned: bool,
    pub start_checkpoint: Option<Checkpoint>,
    pub dependencies: Vec<Arc<dyn Step>>,
    pub netlist: Option<PathBuf>,
}

impl GenusStep {
    pub fn new(
        work_dir: impl Into<PathBuf>,
        module: impl Into<String>,
        steps: Vec<Substep>,
        pinned: bool,
        deps: Vec<Arc<dyn Step>>,
    ) -> Self {
        let dir = work_dir.into();
        let modul = module.into();
        GenusStep {
            work_dir: dir,
            module: modul,
            substeps: steps,
            pinned,
            start_checkpoint: None,
            dependencies: deps,
            netlist: None,
        }
    }

    /// Generates the tcl file for synthesis
    fn make_tcl_file(&self, path: &PathBuf, steps: Vec<Substep>) -> io::Result<()> {
        let mut tcl_file = File::create(path).expect("failed to create syn.tcl file");

        writeln!(
            tcl_file,
            "set_db super_thread_debug_directory super_thread_debug"
        )?;

        if let Some(checkpoint) = self.start_checkpoint.as_ref() {
            writeln!(tcl_file, "read_db {}", checkpoint.path.display()).expect("Failed to write");
        }

        for step in steps.into_iter() {
            println!("\n--> Parsing step: {}\n", step.name);
            //generate tcl for checkpointing

            if step.checkpoint {
                let checkpoint_file = self.work_dir.join(format!("pre_{}", step.name.clone()));

                writeln!(tcl_file, "write_db -to_file {}", checkpoint_file.display())?;
            }
            writeln!(tcl_file, "{}", step.command)?;
        }
        writeln!(tcl_file, "quit")?;

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

    pub fn netlist(&self) -> PathBuf {
        self.netlist.as_ref().unwrap().clone()
    }

    pub fn add_checkpoint(&mut self, name: String, checkpoint_path: PathBuf) {
        self.start_checkpoint = Some(Checkpoint {
            name: name,
            path: checkpoint_path,
        });
    }
}

impl Step for GenusStep {
    fn execute(&self) {
        let tcl_path = self.work_dir.clone().join("syn.tcl");

        let mut substeps = self.substeps.clone();

        if let Some(checkpoint) = self.start_checkpoint.as_ref() {
            let slice_index = substeps
                .iter()
                .position(|s| s.name == checkpoint.name)
                .expect("Failed to find checkpoint name");
            substeps = substeps[slice_index..].to_vec();
        }

        self.make_tcl_file(&tcl_path, substeps)
            .expect("Failed to create syn.tcl");

        let status = Command::new("genus")
            .args(["-f", tcl_path.to_str().unwrap()])
            .current_dir(self.work_dir.clone())
            .status()
            .expect("Failed to execute syn.tcl");

        if !status.success() {
            eprintln!("Failed to execute syn.tcl");
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

pub fn set_default_options() -> Substep {
    Substep {
        checkpoint: false,
        name: "set_default_options".into(),
        command: r#"
            set_db hdl_error_on_blackbox true
            set_db max_cpus_per_server 12
            set_multi_cpu_usage -local_cpu 12
            set_db super_thread_debug_jobs true
            set_db super_thread_debug_directory super_thread_debug
            set_db lp_clock_gating_infer_enable  true
            set_db lp_clock_gating_prefix  {CLKGATE}
            set_db lp_insert_clock_gating  true
            set_db lp_clock_gating_register_aware true
            set_db root: .auto_ungroup none
"#
        .into(),
    }
}

pub fn dont_avoid_lib_cells(base_name: &str) -> Substep {
    Substep {
        checkpoint: false,
        name: format!("dont_avoid_lib_cells_{base_name}"),
        command: formatdoc!(
            r#"set_db [get_db lib_cells -if {{.base_name == {base_name}}}] .avoid false"#
        ),
    }
}

/// Reads the module verilog, mmmc.tcl, pdk lefs, ilms paths, and sdc constraints
pub fn syn_read_design_files(
    work_dir: &Path,
    module_path: &Path,
    mmmc_conf: MmmcConfig,
    tlef: &Path,
    pdk_lef: &Path,
    submodules: Option<Vec<SubmoduleInfo>>,
) -> Substep {
    let mut sdc_file =
        File::create(work_dir.join("clock_pin_constraints.sdc")).expect("failed to create file");
    writeln!(sdc_file, "{}", sdc()).expect("Failed to write");
    let mmmc_tcl = mmmc(mmmc_conf);
    let mmmc_tcl_path = work_dir.to_path_buf().join("mmmc.tcl");
    let _ = fs::write(&mmmc_tcl_path, mmmc_tcl);
    let module_file_path = module_path.to_path_buf();
    let module_string = module_file_path.display();

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
            read_mmmc {}
            read_physical -lef {{ {} }}
            read_hdl -sv {}
            "#,
        mmmc_tcl_path.display(),
        lefs,
        module_string
    );

    if let Some(submodule_vec) = submodules {
        for submodule in submodule_vec {
            writeln!(
                command,
                "read_ilm -cell {} -directory {}",
                submodule.name,
                submodule.ilm.display(),
            )
            .unwrap();
        }
    }

    Substep {
        checkpoint: false,
        command: command,
        name: "read_design_files".into(),
    }
}

pub fn elaborate(module: &String) -> Substep {
    Substep {
        checkpoint: false,
        command: format!("elaborate {}", module),
        name: "elaborate".to_string(),
    }
}

pub fn syn_init_design(module: &String) -> Substep {
    Substep {
        checkpoint: false,
        command: format!("init_design -top {}", module),
        name: "init_design".to_string(),
    }
}

/// Write power_spec.cpf and run power_intent TCL commands.
pub fn power_intent(work_dir: &Path, module: &str) -> Substep {
    let power_spec_file_path = work_dir.join("power_spec.cpf");
    let mut power_spec_file = File::create(&power_spec_file_path).expect("failed to create file");
    writeln!(
        power_spec_file,
        "{}",
        formatdoc! {
        r#"
            set_cpf_version 1.0e
            set_hierarchy_separator /
            set_design {}
            create_power_nets -nets VDD -voltage 1.8
            create_power_nets -nets VPWR -voltage 1.8
            create_power_nets -nets VPB -voltage 1.8
            create_power_nets -nets vdd -voltage 1.8
            create_ground_nets -nets {{ VSS VGND VNB vss }}
            create_power_domain -name AO -default
            update_power_domain -name AO -primary_power_net VDD -primary_ground_net VSS
            create_global_connection -domain AO -net VDD -pins [list VDD]
            create_global_connection -domain AO -net VPWR -pins [list VPWR]
            create_global_connection -domain AO -net VPB -pins [list VPB]
            create_global_connection -domain AO -net vdd -pins [list vdd]
            create_global_connection -domain AO -net VSS -pins [list VSS]
            create_global_connection -domain AO -net VGND -pins [list VGND]
            create_global_connection -domain AO -net VNB -pins [list VNB]
            create_nominal_condition -name nominal -voltage 1.8
            create_power_mode -name aon -default -domain_conditions {{AO@nominal}}
            end_design
        "#, module.to_string()
        }
    )
    .expect("Failed to write");
    let power_spec_file_string = power_spec_file_path.display();
    Substep {
        checkpoint: true,
        command: formatdoc!(
            r#"
        read_power_intent -cpf {power_spec_file_string}
        apply_power_intent -summary
        commit_power_intent
        "#
        ),
        name: "power_intent".into(),
    }
}

pub fn syn_generic() -> Substep {
    Substep {
        checkpoint: true,
        command: "syn_generic".to_string(),
        name: "syn_generic".to_string(),
    }
}

pub fn syn_map() -> Substep {
    Substep {
        checkpoint: true,
        command: "syn_map".to_string(),
        name: "syn_map".to_string(),
    }
}

pub fn add_tieoffs() -> Substep {
    Substep {
        checkpoint: true,
        command: formatdoc!(
            r#"set_db message:WSDF-201 .max_print 20
        set_db use_tiehilo_for_const duplicate
        set ACTIVE_SET [string map {{ .setup_view .setup_set .hold_view .hold_set .extra_view .extra_set }} [get_db [get_analysis_views] .name]]
        set HI_TIEOFF [get_db base_cell:TIEHI .lib_cells -if {{ .library.library_set.name == $ACTIVE_SET }}]
        set LO_TIEOFF [get_db base_cell:TIELO .lib_cells -if {{ .library.library_set.name == $ACTIVE_SET }}]
        add_tieoffs -high $HI_TIEOFF -low $LO_TIEOFF -max_fanout 1 -verbose
"#
        ),
        name: "add_tieoffs".into(),
    }
}

pub fn syn_write_design(module: &str) -> Substep {
    let module = module.to_owned();
    Substep {
        checkpoint: true,
        command: formatdoc!(
            r#"
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
        write_reports -directory reports -tag final
        report_timing -unconstrained -max_paths 50 > reports/final_unconstrained.rpt

        write_hdl > {module}.mapped.v
        write_template -full -outfile {module}.mapped.scr
        write_sdc -view ss_100C_1v60.setup_view > {module}.mapped.sdc
        write_sdf > {module}.mapped.sdf
        write_design -gzip_files {module}
            "#
        ),
        //the paths for write hdl, write sdc, and write sdf need to be fixed
        name: "write_design".into(),
    }
}
