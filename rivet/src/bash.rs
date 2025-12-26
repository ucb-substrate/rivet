use crate::Step;
use fs::File;
use indoc::formatdoc;
use std::fmt::Debug;
use std::fmt::Write as FmtWrite;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::{fs, io};

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

    fn deps(&self) {
        self.dependencies.clone();
    }

    fn pinned(&self) {
        self.pinned;
    }
}
