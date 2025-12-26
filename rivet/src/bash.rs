use crate::Step;
use std::fmt::Debug;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;

#[derive(Debug)]
pub struct BashStep {
    pub work_dir: PathBuf,
    pub command: String,
    pub pinned: bool,
    pub dependencies: Vec<Arc<dyn Step>>,
}

impl Step for BashStep {
    fn execute(&self) {
        let status = Command::new(self.command.clone())
            .current_dir(self.work_dir.clone())
            .status()
            .expect("Failed to execute BashStep");

        if !status.success() {
            eprintln!("Failed to execute bash command");
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
