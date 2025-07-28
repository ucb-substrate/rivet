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
pub struct Innovus {
    pub work_dir: PathBuf,
}

impl Innovus {
    pub fn new(work_dir: impl Into<PathBuf>) -> Self {
        let dir = work_dir.into();
        Innovus { work_dir: dir }
    }
    //concatenate steps to a tcl file, par.tcl file, Innovus.tcl

    fn make_tcl_file(&self, path: &PathBuf, steps: Vec<AnnotatedStep>) -> io::Result<()> {
        let mut tcl_file = File::create(&path).expect("failed to create par.tcl file");

        writeln!(
            tcl_file,
            "set_db super_thread_debug_directory super_thread_debug"
        )?;

        for astep in steps.into_iter() {
            if astep.step.checkpoint {
                //generate tcl for checkpointing
                let mut checkpoint_command = String::new();

                writeln!(
                    checkpoint_command,
                    "write_db -to_file pre_{}",
                    astep.step.name
                );
                writeln!(tcl_file, "{}", checkpoint_command)?;
            }
            writeln!(tcl_file, "{}", astep.step.command)?;
        }
        writeln!(tcl_file, "quit")?;

        Ok(())
    }

    fn read_design_files() -> Step {
        Step {
            checkpoint: false,
            //the sky130 cache filepath is hardcoded
            command: formatdoc!(
                r#"
                read_physical -lef {{ /scratch/cs199-cbc/labs/sp25-chipyard/vlsi/build/lab4/tech-sky130-cache/sky130_scl_9T.tlef /home/ff/eecs251b/sky130/sky130_cds/sky130_scl_9T_0.0.5/lef/sky130_scl_9T.lef }}
                {mmmc_tcl}
                read_netlist {decoder_string} -top {module}
                "#
            ),
            name: "read_design_files".into(),
        }
    }

    pub fn init_design() -> Step {
        Step {
            checkpoint: false,
            command: format!("init_design"),
            name: "init_design".to_string(),
        }
    }

    pub fn innovus_settings() -> Step {
        Step {
            checkpoint: false,
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

    pub fn floorplan_design(&self) -> Step {
        //create a pathbuf that is the {work_dir}/floorplan.tcl
        //write this command "create_floorplan -core_margins_by die -flip f -die_size_by_io_height max -site CoreSite -die_size { 30 30 0 0 0 0 }" to this file
        //source it in the floorplan_design step
        let floorplan_tcl_path = self.work_dir.join("floorplan.tcl");
        let mut floorplan_tcl_file =
            File::create(&floorplan_tcl_path).expect("failed to create file");
        writeln!(floorplan_tcl_file, "{}", "create_floorplan -core_margins_by die -flip f -die_size_by_io_height max -site CoreSite -die_size { 30 30 0 0 0 0 }");
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
        );
        let power_spec_file_string = power_spec_file_path.display();
        Step {
            checkpoint: true,
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
}

impl Tool for Innovus {
    //fn work_dir(&self) -> PathBuf {
    //    self.work_dir.clone()
    //}
    // Innovus -files par.tcl -no_gui
    fn invoke(
        &self,
        work_dir: PathBuf,
        start_checkpoint: Option<PathBuf>,
        steps: Vec<AnnotatedStep>,
    ) {
        let mut tcl_path = work_dir.clone().join("par.tcl");

        self.make_tcl_file(&tcl_path, steps);

        //this Innovus cli command is also hardcoded since I think there are some issues with the
        //work_dir input and also the current_dir attribute of the command
        let status = Command::new("innovus")
            .args(["-f", tcl_path.to_str().unwrap()])
            .current_dir(work_dir)
            .status()
            .expect("Failed to execute par.tcl");

        if !status.success() {
            eprintln!("Failed to execute par.tcl");
            panic!("Stopped flow");
        }
    }
}

//needs a parameter for the node process size
pub fn set_default_options() -> Step {
    Step {
        name: "set_default_options".into(),
        command: r#"
        set_db design_process_node 130
        set_multi_cpu_usage -local_cpu 12
        set_db timing_analysis_cppr both
        set_db timing_analysis_type ocv
        "#
        .into(),
        checkpoint: false,
    }
}
