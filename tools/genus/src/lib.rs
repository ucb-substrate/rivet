use std::fmt::Debug;
use std::fmt::Write as FmtWrite;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::{fs, io};

use indoc::formatdoc;
use rivet::cadence::*;
use rivet::flow::{AnnotatedStep, Step, Tool};

use crate::fs::File;

#[derive(Debug)]
pub struct Genus {
    pub work_dir: PathBuf,
}

impl Genus {
    pub fn new(work_dir: impl Into<PathBuf>) -> Self {
        let dir = work_dir.into();
        Genus { work_dir: dir }
    }
    //concatenate steps to a tcl file, syn.tcl file, genus.tcl

    fn make_tcl_file(
        &self,
        path: &PathBuf,
        steps: Vec<AnnotatedStep>,
        checkpoint_dir: Option<PathBuf>,
        work_dir: PathBuf,
    ) -> io::Result<()> {
        let mut tcl_file = File::create(&path).expect("failed to create syn.tcl file");

        writeln!(
            tcl_file,
            "set_db super_thread_debug_directory super_thread_debug"
        )?;

        if let Some(actual_checkpt_dir) = checkpoint_dir {
            //there is actually a checkpoint to read from
            use colored::Colorize;
            println!("{}", "\nCheckpoint specified, reading from it...\n".blue());
            let complete_checkpoint_path = work_dir.join(actual_checkpt_dir);
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
            );
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
                //before had write_db -to_file pre_{astep.step.name} -> no checkpt dir
                writeln!(
                    checkpoint_command,
                    "write_db -to_file {cdir}.cpf",
                    cdir = checkpoint_file
                );
                //                 writeln!(
                //                     checkpoint_command,
                //                     "write_db -to_file pre_{}",
                //                     astep.step.name
                //                 );
                //writeln!(tcl_file, "puts \"{}\"", checkpoint_command)?;
                writeln!(tcl_file, "{}", checkpoint_command)?;
            }
            // writeln!(tcl_file, "puts\"{}\"", astep.step.command.to_string())?;
            writeln!(tcl_file, "{}", astep.step.command)?;
        }
        // writeln!(tcl_file, "puts \"{}\"", "quit")?;
        writeln!(tcl_file, "quit")?;
        use colored::Colorize;

        let temp_str = format!("{}", "\nFinished creating tcl file\n".green());
        println!("{}", temp_str);
        Ok(())
    }

    pub fn read_design_files(syn_work_dir: &PathBuf, work_dir: &PathBuf, module: &str) -> Step {
        // Write SDC and mmmc.tcl, run commands up to read_hdl.
        //read mmmc.tcl
        //read physical -lef
        //read_hdl -sv {}

        let sdc_file_path = syn_work_dir.join("clock_pin_constraints.sdc");
        let mut sdc_file = File::create(&sdc_file_path).expect("failed to create file");
        writeln!(sdc_file, "{}", sdc());
        let mmmc_tcl = sky130_cds_mmmc(sdc_file_path);
        let module_file_path = work_dir.join("{module}.v");
        let module_string = module_file_path.display();

        //fix the path fo the sky130 lef in my scratch folder
        Step {
            checkpoint: false,
            //the sky130 cache filepath is hardcoded
            command: formatdoc!(
                r#"
                {mmmc_tcl}
                read_physical -lef {{/scratch/cs199-cbc/labs/sp25-chipyard/vlsi/build/lab4/tech-sky130-cache/sky130_scl_9T.tlef  /home/ff/eecs251b/sky130/sky130_cds/sky130_scl_9T_0.0.5/lef/sky130_scl_9T.lef }}
                read_hdl -sv {module_string}

                "#
            ),
            name: "read_design_files".into(),
        }
    }
    //

    fn predict_floorplan(innovus_path: &PathBuf) -> Step {
        let mut command = String::new();
        // In a real implementation, this would be based on a setting like
        // `synthesis.genus.phys_flow_effort`. This example assumes "high" effort.

        writeln!(&mut command, "set_db invs_temp_dir temp_invs");
        // The innovus binary path would be a configurable parameter.
        writeln!(
            &mut command,
            "set_db innovus_executable {}",
            innovus_path.display()
        );
        writeln!(
            &mut command,
            "set_db predict_floorplan_enable_during_generic true"
        );
        writeln!(&mut command, "set_db physical_force_predict_floorplan true");
        writeln!(&mut command, "set_db predict_floorplan_use_innovus true");

        writeln!(&mut command, "predict_floorplan");

        Step {
            name: "predict_floorplan".to_string(),
            command,
            checkpoint: true,
        }
    }

    pub fn elaborate(module: &str) -> Step {
        Step {
            checkpoint: false,
            command: format!("elaborate {module}"),
            name: "elaborate".to_string(),
        }
    }

    pub fn init_design(module: &str) -> Step {
        Step {
            checkpoint: false,
            command: format!("init_design -top {module}"),
            name: "init_design".to_string(),
        }
    }

    pub fn power_intent(work_dir: &PathBuf) -> Step {
        // Write power_spec.cpf and run power_intent TCL commands.
        let power_spec_file_path = work_dir.join("power_spec.cpf");
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
        );
        //create the power_spec cpf file with the contents hard coded
        let power_spec_file_string = power_spec_file_path.display();
        Step {
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

    pub fn syn_generic() -> Step {
        Step {
            checkpoint: true,
            command: "syn_generic".to_string(),
            name: "syn_generic".to_string(),
        }
    }

    pub fn syn_map() -> Step {
        Step {
            checkpoint: true,
            command: "syn_map".to_string(),
            name: "syn_map".to_string(),
        }
    }

    pub fn add_tieoffs() -> Step {
        Step {
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

    pub fn write_design(module: &str) -> Step {
        // All write TCL commands
        // this includes write regs, write reports, write outputs
        Step {
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
            puts "write_reports -directory reports -tag final"
            write_reports -directory reports -tag final
            puts "report_timing -unconstrained -max_paths 50 > reports/final_unconstrained.rpt"
            report_timing -unconstrained -max_paths 50 > reports/final_unconstrained.rpt

            puts "write_hdl > {module}.mapped.v"
            write_hdl > {module}.mapped.v
            puts "write_template -full -outfile {module}.mapped.scr"
            write_template -full -outfile {module}.mapped.scr
            puts "write_sdc -view ss_100C_1v60.setup_view > {module}.mapped.sdc"
            write_sdc -view ss_100C_1v60.setup_view > {module}.mapped.sdc
            puts "write_sdf > {module}.mapped.sdf"
            write_sdf > {module}.mapped.sdf
            puts "write_design -gzip_files {module}"
            write_design -gzip_files {module}
                "#
            ),
            //the paths for write hdl, write sdc, and write sdf need to be fixed
            name: "write_design".into(),
        }
    }
}

impl Tool for Genus {
    //fn work_dir(&self) -> PathBuf {
    //    self.work_dir.clone()
    //}
    // genus -files syn.tcl -no_gui
    fn invoke(
        &self,
        work_dir: PathBuf,
        start_checkpoint: Option<PathBuf>,
        steps: Vec<AnnotatedStep>,
    ) {
        let mut tcl_path = work_dir.clone().join("syn.tcl");

        self.make_tcl_file(&tcl_path, steps, start_checkpoint, work_dir.clone());

        //this genus cli command is also hardcoded since I think there are some issues with the
        //work_dir input and also the current_dir attribute of the command
        let status = Command::new("genus")
            .args(["-f", tcl_path.to_str().unwrap()])
            .current_dir(work_dir)
            .status()
            .expect("Failed to execute syn.tcl");

        if !status.success() {
            eprintln!("Failed to execute syn.tcl");
            panic!("Stopped flow");
        }
    }
}

pub fn set_default_options() -> Step {
    Step {
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
        checkpoint: false,
    }
}
pub fn sdc() -> String {
    // Combine contents of clock_constraints_fragment.sdc and pin_constraints_fragment.sdc

    formatdoc!(
        r#"create_clock clk -name clk -period 2.0
            set_clock_uncertainty 0.01 [get_clocks clk]
            set_clock_groups -asynchronous  -group {{ clk }}
            set_load 1.0 [all_outputs]
            set_input_delay -clock clk 0 [all_inputs]
            set_output_delay -clock clk 0 [all_outputs]"#
    )
}

fn sky130_cds_mmmc(sdc_file: impl AsRef<Path>) -> String {
    let sdc_file = sdc_file.as_ref();
    //the sdc files need their paths not hardcoded to the chipyard directory
    formatdoc!(
        r#"puts "create_constraint_mode -name my_constraint_mode -sdc_files [list /home/ff/eecs251b/sp25-chipyard/vlsi/build/lab4/syn-rundir/clock_constraints_fragment.sdc /home/ff/eecs251b/sp25-chipyard/vlsi/build/lab4/syn-rundir/pin_constraints_fragment.sdc] "
        create_constraint_mode -name my_constraint_mode -sdc_files {sdc_file:?}
        puts "create_library_set -name ss_100C_1v60.setup_set -timing [list /home/ff/eecs251b/sky130/sky130_cds/sky130_scl_9T_0.0.5/lib/sky130_ss_1.62_125_nldm.lib]"
        create_library_set -name ss_100C_1v60.setup_set -timing [list /home/ff/eecs251b/sky130/sky130_cds/sky130_scl_9T_0.0.5/lib/sky130_ss_1.62_125_nldm.lib]
        puts "create_timing_condition -name ss_100C_1v60.setup_cond -library_sets [list ss_100C_1v60.setup_set]"
        create_timing_condition -name ss_100C_1v60.setup_cond -library_sets [list ss_100C_1v60.setup_set]
        puts "create_rc_corner -name ss_100C_1v60.setup_rc -temperature 100.0 "
        create_rc_corner -name ss_100C_1v60.setup_rc -temperature 100.0
        puts "create_delay_corner -name ss_100C_1v60.setup_delay -timing_condition ss_100C_1v60.setup_cond -rc_corner ss_100C_1v60.setup_rc"
        create_delay_corner -name ss_100C_1v60.setup_delay -timing_condition ss_100C_1v60.setup_cond -rc_corner ss_100C_1v60.setup_rc
        puts "create_analysis_view -name ss_100C_1v60.setup_view -delay_corner ss_100C_1v60.setup_delay -constraint_mode my_constraint_mode"
        create_analysis_view -name ss_100C_1v60.setup_view -delay_corner ss_100C_1v60.setup_delay -constraint_mode my_constraint_mode
        puts "create_library_set -name ff_n40C_1v95.hold_set -timing [list /home/ff/eecs251b/sky130/sky130_cds/sky130_scl_9T_0.0.5/lib/sky130_ff_1.98_0_nldm.lib]"
        create_library_set -name ff_n40C_1v95.hold_set -timing [list /home/ff/eecs251b/sky130/sky130_cds/sky130_scl_9T_0.0.5/lib/sky130_ff_1.98_0_nldm.lib]
        puts "create_timing_condition -name ff_n40C_1v95.hold_cond -library_sets [list ff_n40C_1v95.hold_set]"
        create_timing_condition -name ff_n40C_1v95.hold_cond -library_sets [list ff_n40C_1v95.hold_set]
        puts "create_rc_corner -name ff_n40C_1v95.hold_rc -temperature -40.0 "
        create_rc_corner -name ff_n40C_1v95.hold_rc -temperature -40.0
        puts "create_delay_corner -name ff_n40C_1v95.hold_delay -timing_condition ff_n40C_1v95.hold_cond -rc_corner ff_n40C_1v95.hold_rc"
        create_delay_corner -name ff_n40C_1v95.hold_delay -timing_condition ff_n40C_1v95.hold_cond -rc_corner ff_n40C_1v95.hold_rc
        puts "create_analysis_view -name ff_n40C_1v95.hold_view -delay_corner ff_n40C_1v95.hold_delay -constraint_mode my_constraint_mode"
        create_analysis_view -name ff_n40C_1v95.hold_view -delay_corner ff_n40C_1v95.hold_delay -constraint_mode my_constraint_mode
        puts "create_library_set -name tt_025C_1v80.extra_set -timing [list /home/ff/eecs251b/sky130/sky130_cds/sky130_scl_9T_0.0.5/lib/sky130_tt_1.8_25_nldm.lib]"
        create_library_set -name tt_025C_1v80.extra_set -timing [list /home/ff/eecs251b/sky130/sky130_cds/sky130_scl_9T_0.0.5/lib/sky130_tt_1.8_25_nldm.lib]
        puts "create_timing_condition -name tt_025C_1v80.extra_cond -library_sets [list tt_025C_1v80.extra_set]"
        create_timing_condition -name tt_025C_1v80.extra_cond -library_sets [list tt_025C_1v80.extra_set]
        puts "create_rc_corner -name tt_025C_1v80.extra_rc -temperature 25.0 "
        create_rc_corner -name tt_025C_1v80.extra_rc -temperature 25.0
        puts "create_delay_corner -name tt_025C_1v80.extra_delay -timing_condition tt_025C_1v80.extra_cond -rc_corner tt_025C_1v80.extra_rc"
        create_delay_corner -name tt_025C_1v80.extra_delay -timing_condition tt_025C_1v80.extra_cond -rc_corner tt_025C_1v80.extra_rc
        puts "create_analysis_view -name tt_025C_1v80.extra_view -delay_corner tt_025C_1v80.extra_delay -constraint_mode my_constraint_mode"
        create_analysis_view -name tt_025C_1v80.extra_view -delay_corner tt_025C_1v80.extra_delay -constraint_mode my_constraint_mode
        puts "set_analysis_view -setup {{ ss_100C_1v60.setup_view }} -hold {{ ff_n40C_1v95.hold_view tt_025C_1v80.extra_view }} -dynamic tt_025C_1v80.extra_view -leakage tt_025C_1v80.extra_view"
        set_analysis_view -setup {{ ss_100C_1v60.setup_view }} -hold {{ ff_n40C_1v95.hold_view tt_025C_1v80.extra_view }} -dynamic tt_025C_1v80.extra_view -leakage tt_025C_1v80.extra_view"#
    )
}

pub fn dont_avoid_lib_cells(base_name: &str) -> Step {
    Step {
        name: format!("dont_avoid_lib_cells_{base_name}"),
        command: formatdoc!(
            r#"set_db [get_db lib_cells -if {{.base_name == {base_name}}}] .avoid false"#
        ),
        checkpoint: false,
    }
}

pub struct MmmcCorner {
    name: String,
    libs: Vec<PathBuf>,
    temperature: f64,
}

pub fn mmmc(sdc_file: impl AsRef<Path>, corners: Vec<MmmcCorner>) -> String {
    //the sdc files need their paths not hardcoded to the chipyard directory
    let sdc_file = sdc_file.as_ref();
    writeln!(
        &mut mmmc,
        "create_constraint_mode -name my_constraint_mode -sdc_files [list {sdc_file:?}]"
    )
    .unwrap();
    let mut mmmc = String::new();
    for corner in corners {
        let library_set_name = format!("{}.set", corner.name);
        write!(&mut mmmc, "create_library_set -name TODO").unwrap();
        for lib in corner.libs {
            write!(&mut mmmc, " {lib:?}").unwrap();
        }
        writeln!(&mut mmmc, "]").unwrap();
        writeln!(&mut mmmc, "TODO").unwrap();
    }
    formatdoc!(
        r#"
            create_library_set -name ss_100C_1v60.setup_set -timing [list /home/ff/eecs251b/sky130/sky130_cds/sky130_scl_9T_0.0.5/lib/sky130_ss_1.62_125_nldm.lib]
            create_rc_corner -name ss_100C_1v60.setup_rc -temperature 100.0
            create_delay_corner -name ss_100C_1v60.setup_delay -timing_condition ss_100C_1v60.setup_cond -rc_corner ss_100C_1v60.setup_rc
            create_analysis_view -name ss_100C_1v60.setup_view -delay_corner ss_100C_1v60.setup_delay -constraint_mode my_constraint_mode
        "#
    )
}

//#[cfg(test)]
//
//mod tests {
//    use std::env;
//    use std::fs;
//    use std::path::PathBuf;
//    use std::sync::Arc;
//
//    use rivet::flow::{FlowNode, Step, Tool};
//
//    use crate::Genus;
//
//    #[test]
//    fn test_basic_flow() {
//        let genus: Arc<dyn Tool> = Arc::new(Genus::new(PathBuf::from(".")));
//        let genus_steps = vec![
//            Genus::init_environment(),
//            Genus::syn_generic(),
//            Genus::syn_map(),
//            Genus::add_tieoffs(),
//            Genus::generate_reports(),
//            Genus::write_outputs(),
//        ];
//
//        let basic = FlowNode {
//            name: "Genus".to_string(),
//            tool: Arc::clone(&genus),
//            steps: genus_steps,
//            deps: vec![],
//        };
//
//        genus.invoke(basic.steps);
//    }
//}
