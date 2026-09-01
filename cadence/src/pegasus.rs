use std::fmt::Debug;
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;
use std::{fs, io};

use crate::Substep;
use fs::File;
use rivet::exec;
use rivet::progress;
use rivet::{Step, StepRef, StepResult};

#[derive(Debug)]
pub struct PegasusStep {
    pub work_dir: PathBuf,
    pub func: String,
    pub module: String,
    pub pinned: bool,
    pub dependencies: Vec<StepRef<dyn Step>>,
}

impl PegasusStep {
    pub fn new(
        work_dir: impl Into<PathBuf>,
        func: String,
        module: String,
        pinned: bool,
        deps: Vec<StepRef<dyn Step>>,
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

    #[allow(dead_code)]
    fn make_ctl_file(
        &self,
        path: &PathBuf,
        steps: Vec<Substep>,
        checkpoint_dir: Option<PathBuf>,
        work_dir: PathBuf,
    ) -> io::Result<()> {
        let mut ctl_file = File::create(path)?;

        if let Some(actual_checkpt_dir) = checkpoint_dir {
            progress::status("reading checkpoint");
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

        let total = steps.len();
        for (index, step) in steps.into_iter().enumerate() {
            // Braces rather than quotes: TCL performs no substitution inside them.
            writeln!(
                ctl_file,
                "puts {{{}}}",
                progress::banner(index + 1, total, &step.name)
            )?;

            if step.checkpoint {
                let checkpoint_file = self.work_dir.join(format!("pre_{}", step.name.clone()));

                writeln!(ctl_file, "write_db -to_file {}", checkpoint_file.display())?;
            }

            writeln!(ctl_file, "{}", step.command)?;
        }
        writeln!(ctl_file, "quit")?;

        Ok(())
    }
}

impl Step for PegasusStep {
    fn execute(&self) -> StepResult {
        let ctl_path = self.work_dir.clone().join("{}.ctl");
        let schematic = format!("./{}.spice", self.module);
        let layout = format!("./{}.gds", self.module);

        if self.func == "lvs" {
            let mut ctl = Command::new("pegasus");
            ctl.args(["-f", ctl_path.to_str().unwrap()])
                .current_dir(self.work_dir.clone());
            let status = exec::run_logged_in(
                &mut ctl,
                &self.work_dir,
                &format!("{}.lvs.ctl", self.module),
            )?;
            if !status.success() {
                return Err(format!("pegasus ctl run exited with {status}").into());
            }

            let mut lvs = Command::new("pegasus");
            lvs.args([
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
            .current_dir(self.work_dir.clone());

            let lvs_status =
                exec::run_logged_in(&mut lvs, &self.work_dir, &format!("{}.lvs", self.module))?;

            if !lvs_status.success() {
                // An LVS mismatch is an ordinary result, not a bug.
                return Err(format!(
                    "LVS did not match for {} (pegasus exited with {lvs_status}); see {}",
                    self.module,
                    self.work_dir
                        .join(format!("{}.lvs.out", self.module))
                        .display()
                )
                .into());
            }
            progress::status("LVS clean");
        }

        if self.func == "drc" {
            let mut drc = Command::new("pegasus");
            drc.args([
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
            .current_dir(self.work_dir.clone());

            let drc_status =
                exec::run_logged_in(&mut drc, &self.work_dir, &format!("{}.drc", self.module))?;

            if !drc_status.success() {
                return Err(format!(
                    "DRC violations in {} (pegasus exited with {drc_status}); see {}",
                    self.module,
                    self.work_dir
                        .join(format!("{}.drc.out", self.module))
                        .display()
                )
                .into());
            }
            progress::status("DRC clean");
        }

        Ok(())
    }

    fn label(&self) -> String {
        format!("{} {}", self.module, self.func)
    }

    fn deps(&self) -> Vec<StepRef<dyn Step>> {
        self.dependencies.clone()
    }

    fn pinned(&self) -> bool {
        self.pinned
    }

    fn log_dir(&self) -> Option<PathBuf> {
        Some(self.work_dir.clone())
    }
}
