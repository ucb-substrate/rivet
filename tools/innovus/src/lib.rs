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
                write_db pre_sky130_innovus_settings
                ln -sfn pre_sky130_innovus_settings latest
               "#
            ),
            name: "innovus_settings".into(),
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
