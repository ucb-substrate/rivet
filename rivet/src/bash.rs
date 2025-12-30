use crate::Step;
use fs::File;
use std::fmt::Debug;
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::{fs, io};

#[derive(Debug)]
pub struct BashStep {
    pub work_dir: PathBuf,
    pub file_name: String,
    pub command: String,
    pub dependencies: Vec<Arc<dyn Step>>,
}

impl BashStep {
    pub fn new(
        work_dir: impl Into<PathBuf>,
        file_name: impl Into<String>,
        command: impl Into<String>,
        deps: Vec<Arc<dyn Step>>,
    ) -> Self {
        let dir = work_dir.into();
        let file = file_name.into();
        let com = command.into();
        BashStep {
            work_dir: dir,
            file_name: file,
            command: com,
            dependencies: deps,
        }
    }
    fn make_bash_file(&self) -> io::Result<()> {
        let bash_file = File::create(self.work_dir.join(format!("{}.sh", self.file_name)));

        writeln!(bash_file?, "{}", self.command)?;

        println!("\nFinished creating bash file\n");
        Ok(())
    }
}

impl Step for BashStep {
    fn execute(&self) {
        self.make_bash_file().expect("Failed to create bash script");
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
