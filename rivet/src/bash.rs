use crate::Step;
use std::fmt::Debug;
use std::fs::File;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;

#[derive(Debug)]
pub struct BashStep {
    pub work_dir: PathBuf,
    pub name: String,
    pub block: String,
    pub dependencies: Vec<Arc<dyn Step>>,
}

impl BashStep {
    pub fn new(
        work_dir: impl Into<PathBuf>,
        name: impl Into<String>,
        block: impl Into<String>,
        deps: Vec<Arc<dyn Step>>,
    ) -> Self {
        let dir = work_dir.into();
        let file = name.into();
        let module = block.into();
        BashStep {
            work_dir: dir,
            name: file,
            block: module,
            dependencies: deps,
        }
    }
}

impl Step for BashStep {
    fn execute(&self) {
        let out_path = self
            .work_dir
            .join(format!("{}.{}.out", self.block, self.name));
        let err_path = self
            .work_dir
            .join(format!("{}.{}.err", self.block, self.name));

        let out_file = File::create(out_path).expect("Failed to create stdout file");
        let err_file = File::create(err_path).expect("Failed to create stderr file");

        let status = Command::new("/bin/bash")
            .args([format!("run_{}.sh", self.name)])
            .current_dir(&self.work_dir)
            .stdout(out_file)
            .stderr(err_file)
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
