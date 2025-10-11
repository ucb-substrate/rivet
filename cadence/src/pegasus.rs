use std::fmt::Debug;
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;
use std::{fs, io};

use crate::Substep;
use fs::File;
use rivet::Step;
use std::sync::Arc;

#[derive(Debug)]
pub struct PegasusStep {
    pub work_dir: PathBuf,
    pub func: String,
    pub module: String,
    pub pinned: bool,
    pub dependencies: Vec<Arc<dyn Step>>,
}

impl PegasusStep {
    pub fn new(
        work_dir: impl Into<PathBuf>,
        func: String,
        module: String,
        pinned: bool,
        deps: Vec<Arc<dyn Step>>,
    ) -> Self {
        let dir = work_dir.into();
        PegasusStep {
            work_dir: dir,
            func,
            module,
            pinned,
            dependencies: deps,
        }
    }

    fn make_ctl_file(
        &self,
        path: &PathBuf,
        steps: Vec<Substep>,
        checkpoint_dir: Option<PathBuf>,
        work_dir: PathBuf,
    ) -> io::Result<()> {
        let mut ctl_file = File::create(path).expect("failed to create pegasus{self.func}ctl file");

        if let Some(actual_checkpt_dir) = checkpoint_dir {
            println!("\nCheckpoint specified, reading from it...\n");
            let complete_checkpoint_path = work_dir.join(actual_checkpt_dir);
            let _ = writeln!(
                ctl_file,
                "read_db {}",
                complete_checkpoint_path
                    .into_os_string()
                    .into_string()
                    .expect("Failed to read from checkpoint path")
            );
        }

        for step in steps.into_iter() {
            println!("\n--> Parsing step: {}\n", step.name);

            if step.checkpoint {
                let checkpoint_file = self.work_dir.join(format!("pre_{}", step.name.clone()));

                writeln!(ctl_file, "write_db -to_file {}", checkpoint_file.display())?;
            }

            writeln!(ctl_file, "{}", step.command)?;
        }
        writeln!(ctl_file, "quit")?;

        println!("\nFinished creating ctl file\n");
        Ok(())
    }
}

impl Step for PegasusStep {
    fn execute(&self) {
        let ctl_path = self.work_dir.clone().join("{}.ctl");
        let schematic = format!("./{}.spice", self.module);
        let layout = format!("./{}.gds", self.module);

        if self.func == "lvs" {
            let status = Command::new("pegasus")
                .args(["-f", ctl_path.to_str().unwrap()])
                .current_dir(self.work_dir.clone())
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
                .current_dir(self.work_dir.clone())
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
                .current_dir(self.work_dir.clone())
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
    fn deps(&self) -> Vec<Arc<dyn Step>> {
        self.dependencies.clone()
    }

    fn pinned(&self) -> bool {
        self.pinned
    }
}
