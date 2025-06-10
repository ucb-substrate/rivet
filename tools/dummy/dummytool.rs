use crate::{Step, Tool};
use std::fs;
use std::path::{Path, PathBuf};

pub struct DummyTool {
    word_dir: PathBuf;
}

impl DummyTool {
    pub fn new (work_dir: impl Into<PathBuf>) -> Self {
        let dir = work_dir.into();
        DummyTool {work_dir: dir}
    }
}

impl Tool for DummyTool {
    fn work_dir(&self) -> PathBuf {
        self.work_dir.clone()
    }

    fn invoke(&self, steps:Vec<Step>) {
        for step in steps {
            
        }
    }
}
