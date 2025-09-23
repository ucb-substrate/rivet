use std::fmt::Debug;
use std::fmt::Write as FmtWrite;
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;
use std::{fs, io};

use indoc::formatdoc;
use rivet::flow::{AnnotatedStep, Step, Tool};

use crate::fs::File;

#[derive(Debug)]
pub struct Pegasus {
    pub work_dir: PathBuf,
    pub func: String,
    pub module: String,
}

impl Pegasus {
    pub fn new(work_dir: impl Into<PathBuf>, func: String, module: String) -> Self {
        let dir = work_dir.into();
        Pegasus {
            work_dir: dir,
            func: func,
            module: module,
        }
    }

    fn make_ctl_file(
        &self,
        path: &PathBuf,
        steps: Vec<AnnotatedStep>,
        checkpoint_dir: Option<PathBuf>,
        work_dir: PathBuf,
    ) -> io::Result<()> {
        let mut ctl_file =
            File::create(&path).expect("failed to create pegasus{self.func}ctl file");

        if let Some(actual_checkpt_dir) = checkpoint_dir {
            //there is actually a checkpoint to read from
            use colored::Colorize;
            println!("{}", "\nCheckpoint specified, reading from it...\n".blue());
            let complete_checkpoint_path = work_dir.join(actual_checkpt_dir);
            writeln!(
                ctl_file,
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
                //generate ctl for checkpointing
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

                writeln!(ctl_file, "{}", checkpoint_command)?;
            }
            writeln!(ctl_file, "{}", astep.step.command)?;
        }
        // writeln!(ctl_file, "puts \"{}\"", "quit")?;
        writeln!(ctl_file, "quit")?;
        use colored::Colorize;

        let temp_str = format!("{}", "\nFinished creating ctl file\n".green());
        println!("{}", temp_str);
        Ok(())
    }
}

impl Tool for Pegasus {
    fn invoke(
        &self,
        work_dir: PathBuf,
        start_checkpoint: Option<PathBuf>,
        steps: Vec<AnnotatedStep>,
    ) {
        let ctl_path = work_dir.clone().join("{}.ctl");
        let schematic = format!("./{}.spice", self.module);
        let layout = format!("./{}.gds", self.module);

        if self.func == "lvs" {
            let status = Command::new("pegasus")
                .args(["-f", ctl_path.to_str().unwrap()])
                .current_dir(work_dir.clone())
                .status()
                .expect("Failed to execute pegasus");

            let lvs_status = Command::new("pegasus")
                .args([
                    "-lvs",
                    "-dp",
                    "12",
                    "-license_dp_continue",
                    "-automatch",
                    "-check_schematic",
                    "-rc_data",
                    "-ui_data",
                    "-source_cdl",
                    &schematic,
                    "-gds",
                    &layout,
                    "-source_top_cell",
                    &self.module,
                    "-layout_top_cell",
                    &self.module,
                    "/home/ff/eecs251b/sky130/sky130_cds/sky130_release_0.0.4/Sky130_LVS/sky130.lvs.pvl",
                ])
                .current_dir(work_dir.clone())
                .status()
                .expect("Failed to execute pegasus for LVS");

            if !lvs_status.success() {
                eprintln!("Pegasus LVS command failed with status: {}", lvs_status);
                panic!("Stopped flow due to LVS failure");
            } else {
                println!("Pegasus LVS completed successfully.");
            }
        }

        if self.func == "drc" {
            let drc_status = Command::new("pegasus")
                .args([
                    "-drc",
                    "-dp",
                    "12",
                    "-license_dp_continue",
                    "-gds",
                    &layout,
                    "-top_cell",
                    &self.module,
                    "-ui_data",
                    "/home/ff/eecs251b/sky130/sky130_cds/sky130_release_0.0.4/Sky130_DRC/sky130_rev_0.0_1.0.drc.pvl",
                ])
                .current_dir(work_dir.clone())
                .status()
                .expect("Failed to execute pegasus for DRC");

            if !drc_status.success() {
                eprintln!("Pegasus DRC command failed with status: {}", drc_status);
                panic!("Stopped flow due to DRC failure");
            } else {
                println!("Pegasus DRC completed successfully.");
            }
        }
    }
}
