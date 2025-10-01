use std::fmt::Debug;
use std::fmt::Write as FmtWrite;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::{fs, io};

use crate::cadence::{MmmcConfig, MmmcCorner, Substep, mmmc, sdc};
use fs::File;
use indoc::formatdoc;
use rivet::Step;
use rust_decimal::Decimal;

#[derive(Debug)]
pub struct InnovusStep {
    pub work_dir: PathBuf,
    pub module: String,
    pub steps: Vec<Substep>,
}

impl InnovusStep {
    pub fn new(
        work_dir: impl Into<PathBuf>,
        module: impl Into<String>,
        steps: Vec<Substep>,
    ) -> Self {
        let dir = work_dir.into();
        let modul = module.into();
        InnovusStep {
            work_dir: dir,
            module: modul,
            steps: steps,
        }
    }

    fn make_tcl_file(
        &self,
        path: &PathBuf,
        steps: Vec<Substep>,
        checkpoint_dir: Option<PathBuf>,
    ) -> io::Result<()> {
        let mut tcl_file = File::create(&path).expect("failed to create par.tcl file");

        if let Some(actual_checkpt_dir) = checkpoint_dir {
            //there is actually a checkpoint to read from
            use colored::Colorize;
            println!("{}", "\nCheckpoint specified, reading from it...\n".blue());
            let complete_checkpoint_path = self.work_dir.join(actual_checkpt_dir);
            writeln!(
                tcl_file,
                "{}",
                format!(
                    "read_db {}",
                    complete_checkpoint_path
                        .into_os_string()
                        .into_string()
                        .expect("Failed to read from checkpoint path")
                )
            )
            .expect("Failed to write");
        }

        for astep in steps.into_iter() {
            use colored::Colorize;
            println!("\n--> Parsing step: {}\n", astep.step.name.green());
            if astep.step.checkpoint {
                //generate tcl for checkpointing
                let mut checkpoint_command = String::new();

                let mut checkpoint_file = astep
                    .checkpoint_path
                    .into_os_string()
                    .into_string()
                    .expect("Failed to create checkpoint file");
                writeln!(
                    checkpoint_command,
                    "write_db {cdir}.cpf",
                    cdir = checkpoint_file
                )
                .expect("Failed to write");

                writeln!(tcl_file, "{}", checkpoint_command)?;
            }
            writeln!(tcl_file, "{}", astep.step.command)?;
        }
        writeln!(tcl_file, "exit")?;
        use colored::Colorize;

        let temp_str = format!("{}", "\nFinished creating tcl file\n".green());
        println!("{}", temp_str);
        Ok(())
    }

    pub fn read_design_files(
        &self,
        netlist_path: &PathBuf,
        mmmc_conf: MmmcConfig,
        tlef: &PathBuf,
        pdk_lef: &PathBuf,
    ) -> Substep {
        let sdc_file_path = self.work_dir.join("clock_pin_constraints.sdc");
        let mut sdc_file = File::create(&sdc_file_path).expect("failed to create file");
        writeln!(sdc_file, "{}", sdc()).expect("Failed to write");
        let mmmc_tcl = mmmc(mmmc_conf);
        let mmmc_tcl_path = self.work_dir.clone().join("mmmc.tcl");
        fs::write(&mmmc_tcl_path, mmmc_tcl);
        let netlist_file_path = netlist_path.clone();
        let netlist_string = netlist_file_path.display();
        let cache_tlef = tlef.display();
        let pdk = pdk_lef.display();

        Substep {
            command: formatdoc!(
                r#"
                    read_physical -lef {{ {} {} }}
                    read_mmmc {}
                    read_netlist {} -top {}
                    "#,
                cache_tlef,
                pdk,
                mmmc_tcl_path.display(),
                netlist_string,
                self.module
            ),
            name: "read_design_files".into(),
        }
    }

    pub fn init_design() -> Substep {
        Substep {
            command: format!("init_design"),
            name: "init_design".to_string(),
        }
    }

    pub fn innovus_settings() -> Substep {
        Substep {
            command: formatdoc!(
                r#"
                set_db design_bottom_routing_layer 2
                set_db design_top_routing_layer 6
                set_db design_flow_effort standard
                set_db design_power_effort low
               "#
            ),
            name: "innovus_settings".into(),
        }
    }

    pub fn floorplan_design(&self) -> Substep {
        // TODO: Parametrize the floowplan.tcl command
        let floorplan_tcl_path = self.work_dir.join("floorplan.tcl");
        let mut floorplan_tcl_file =
            File::create(&floorplan_tcl_path).expect("failed to create file");
        writeln!(floorplan_tcl_file, "{}", "create_floorplan -core_margins_by die -flip f -die_size_by_io_height max -site CoreSite -die_size { 30 30 0 0 0 0 }").expect("Failed to write");
        let floorplan_path_string = floorplan_tcl_path.display();

        let power_spec_file_path = self.work_dir.join("power_spec.cpf");
        let mut power_spec_file =
            File::create(&power_spec_file_path).expect("failed to create file");
        writeln!(
            power_spec_file,
            "{}",
            formatdoc! {
            r#"
            set_cpf_version 1.0e
            set_hierarchy_separator /
            set_design decoder
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
            "#
            }
        )
        .expect("Failed to write");
        let power_spec_file_string = power_spec_file_path.display();
        Substep {
            command: formatdoc!(
                r#"
                source -echo -verbose {floorplan_path_string} 
                read_power_intent -cpf {power_spec_file_string}
                commit_power_intent

                "#
            ),
            name: "floorplan_design".into(),
        }
    }

    //TODO: for non cadence standard cells which do not come pretapped
    pub fn place_tap_cells() -> Substep {
        Substep {
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
                strap.spacing.to_string()
            )
            .expect("Failed to write");
            writeln!(&mut definitions, "{}", strap.add_stripes_command).expect("Failed to write");
        }

        Substep {
            command: definitions.into(),
            name: "power_straps".into(),
        }
    }

    pub fn place_pins(
        top_layer: &str,
        bot_layer: &str,
        assignments: Vec<PinAssignment>,
    ) -> Substep {
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

        //for pin in pin assignments
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
            command: place_pins_commands,
            name: "place_pins".into(),
        }
    }

    pub fn place_opt_design() -> Substep {
        Substep {
            command: formatdoc!(
                r#"
                set unplaced_pins [get_db ports -if {{.place_status == unplaced}}]
                if {{$unplaced_pins ne ""}} {{
                    print_message -error "Some pins remain unplaced, which will cause invalid placement and routing. These are the unplaced pins: $unplaced_pins"
                    exit 2
                }}
                set_db opt_enable_podv2_clock_opt_flow true
                place_opt_design
            "#
            ),
            name: "place_opt_design".into(),
        }
    }

    pub fn add_fillers(filler_cells: Vec<String>) -> Substep {
        let cells = format!("\"{}\"", filler_cells.join(" "));
        Substep {
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
            command: formatdoc!(
                r#"
            puts "set_db design_express_route true" 
            set_db design_express_route true
            puts "route_design" 
            route_design
            "#
            ),
            name: "route_design".into(),
        }
    }

    pub fn opt_design() -> Substep {
        Substep {
            command: formatdoc!(
                r#"
                    set_db opt_post_route_hold_recovery auto
                    set_db opt_post_route_fix_si_transitions true
                    set_db opt_verbose true
                    set_db opt_detail_drv_failure_reason true
                    set_db opt_sequential_genus_restructure_report_failure_reason true
                    opt_design -post_route -setup -hold -expanded_views -timing_debug_report
                "#
            ),
            name: "opt_design".into(),
        }
    }

    //TODO:needs to be updated to be hierarchal
    pub fn write_regs() -> Substep {
        Substep {
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
            "#
            ),
            name: "write_regs".into(),
        }
    }

    //TODO: add a parameter of a list of excluded cells
    pub fn write_design(&self) -> Substep {
        let par_rundir = self.work_dir.display();
        let module = self.module.clone();
        Substep {
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
                write_stream -mode ALL -format stream -map_file /scratch/cs199-cbc/labs/sp25-chipyard/vlsi/hammer/hammer/technology/sky130/sky130_lefpin.map -uniquify_cell_names -merge {{ /home/ff/eecs251b/sky130/sky130_cds/sky130_scl_9T_0.0.5/gds/sky130_scl_9T.gds }}  {par_rundir}/{module}.gds
                write_sdf -max_view ss_100C_1v60.setup_view -min_view ff_n40C_1v95.hold_view -typical_view tt_025C_1v80.extra_view {par_rundir}/{module}.par.sdf
                set_db extract_rc_coupled true
                extract_rc
                write_parasitics -spef_file {par_rundir}/{module}.ss_100C_1v60.par.spef -rc_corner ss_100C_1v60.setup_rc
                write_parasitics -spef_file {par_rundir}/{module}.ff_n40C_1v95.par.spef -rc_corner ff_n40C_1v95.hold_rc
                write_parasitics -spef_file {par_rundir}/{module}.tt_025C_1v80.par.spef -rc_corner tt_025C_1v80.extra_rc
                write_db post_write_design
                ln -sfn post_write_design latest
                "#
            ),
            name: "write_design".into(),
        }
    }
}

impl Step for InnovusStep {
    fn execute(&self, work_dir: PathBuf, start_checkpoint: Option<PathBuf>, steps: Vec<Substep>) {
        let tcl_path = work_dir.clone().join("par.tcl");

        self.make_tcl_file(&tcl_path, steps, start_checkpoint)
            .expect("Failed to create par.tcl");

        let status = Command::new("innovus")
            .args(["-file", tcl_path.to_str().unwrap(), "-stylus"])
            .current_dir(work_dir)
            .status()
            .expect("Failed to execute par.tcl");

        if !status.success() {
            eprintln!("Failed to execute par.tcl");
            panic!("Stopped flow");
        }
    }
}

pub struct Layer {
    pub top: String,
    pub bot: String,
    pub spacing: Decimal,
    pub trim_antenna: bool,
    pub add_stripes_command: String,
}

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
        name: "set_default_options".into(),
        command: formatdoc!(
            r#"
        set_db design_process_node {} 
        set_multi_cpu_usage -local_cpu 12
        set_db timing_analysis_cppr both
        set_db timing_analysis_type ocv
        "#,
            node_size
        )
        .into(),
    }
}
