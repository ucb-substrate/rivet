use crate::Step;
use std::fmt::Debug;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;

#[derive(Debug)]
pub struct BashStep {
    pub work_dir: PathBuf,
    pub file_name: String,
    pub dependencies: Vec<Arc<dyn Step>>,
}

impl BashStep {
    pub fn new(
        work_dir: impl Into<PathBuf>,
        file_name: impl Into<String>,
        deps: Vec<Arc<dyn Step>>,
    ) -> Self {
        let dir = work_dir.into();
        let file = file_name.into();
        BashStep {
            work_dir: dir,
            file_name: file,
            dependencies: deps,
        }
    }
}

impl Step for BashStep {
    fn execute(&self) {
        let status = Command::new("bash")
            .args([format!("{}.sh", self.file_name)])
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
        false
    }
}
