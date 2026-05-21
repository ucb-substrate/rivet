use crate::Step;
use std::fmt::Debug;
use std::fs::File;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct BashStep {
    pub work_dir: PathBuf,
    pub name: String,
    pub block: String,
    pub deps: Vec<Arc<dyn Step>>,
    pub pinned: bool,
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
            deps,
            pinned: false,
        }
    }

    pub fn pin(&mut self) {
        self.pinned = true;
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

        // TODO: Make this similar to a TCL tool where a bash script is composed of several
        // substeps and templated here, rather than running a hardcoded script path.
        let status = Command::new("/bin/bash")
            .args([format!("run_{}.sh", self.name)])
            .current_dir(&self.work_dir)
            .stdout(out_file)
            .stderr(err_file)
            .status()
            .expect("Failed to execute BashStep");

        if !status.success() {
            panic!(
                "BashStep '{}.{}' failed in directory: {}",
                self.block,
                self.name,
                self.work_dir.display()
            );
        }
    }

    fn deps(&self) -> Vec<Arc<dyn Step>> {
        self.deps.clone()
    }

    fn pinned(&self) -> bool {
        self.pinned
    }
}
