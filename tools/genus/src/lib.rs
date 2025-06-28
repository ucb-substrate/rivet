use rivet::flow::{Step, Tool};
use std::fmt::Debug;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

#[derive(Debug)]
pub struct Genus {
    pub work_dir: PathBuf,
}

impl Genus {
    pub fn new(work_dir: impl Into<PathBuf>) -> Self {
        let dir = work_dir.into();
        Genus { work_dir: dir }
    }

    //concatenate steps to a tcl file, syn.tcl file, genus.tcl

    fn make_tcl_file(&self, path: &PathBuf, steps: Vec<Step>) -> () {
        use crate::fs::File;
        use std::io::prelude::*;
        let mut tcl_file = File::create(path).expect("failed to create tcl file");

        for step in steps.into_iter() {
            writeln!(tcl_file, "{}", step.command)?;
        }
    }
}

impl Tool for Genus {
    fn work_dir(&self) -> PathBuf {
        self.work_dir.clone()
    }
    // genus -files syn.tcl -no_gui
    fn invoke(&self, steps: Vec<Step>) {
        let mut tcl_path = PathBuf::new();
        tcl_path.push(self.work_dir);
        tcl_path.push("syn.tcl");

        self.make_tcl_file(tcl_path, steps);

        let status = Command::new("genus")
            .args(["-files", tcl_path.into_os_string().into_string(), "-no_gui"])
            .current_dir(&self.work_dir)
            .status()
            .expect("Failed to execute syn.tcl");

        if !status.success() {
            eprintln!("Failed to execute syn.tcl");
            panic!("Stopped flow");
        }
    }

    fn write_checkpoint(&self, path: &PathBuf) -> Step {
        let checkpoint_command = format!("write_db -to_file {}", path.to_str().unwrap());
        println!("  - Writing checkpoint w command: {}", checkpoint_command);

        Step {
            name: format!("write_checkpoint_to_{}", path.to_str().unwrap()).to_string(),
            command: checkpoint_command,
            checkpoint: true,
        }
    }

    fn read_checkpoint(&self, path: &PathBuf) -> Step {
        let command = format!("read_db", path.to_str().unwrap());
        println!("  - Reading checkpoint with command: {}", command);
        Step {
            name: "read_checkpoint".to_string(),
            command,
            checkpoint: false,
        }
    }

    //todo: change checkpoints to checkpoint

    // fn checkpoints(&self, steps: Vec<Step>) -> Vec<PathBuf> {
    //     let mut ret: Vec<PathBuf> = vec![];
    //     for step in steps.into_iter() {
    //         if step.checkpoint {
    //             ret.push(self.work_dir.join(format!("{}.checkpoint", step.name)))
    //         }
    //     }
    //     return ret;
    // }
}

#[cfg(test)]

mod tests {
    use std::env;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::Arc;

    use shammer::flow::{FlowNode, Step, Tool};

    use crate::Genus::Genus;

    #[test]
    fn test_basic_flow() {
        let s1 = Step {
            name: "step_1".to_string(),
            //command: "dummyDB=\"1\"".to_string(),
            command: "echo \"1\" >> dummydb.txt".to_string(),
            checkpoint: true,
        };

        let s15 = Step {
            name: "step_15".to_string(),
            command: "echo \"step_1.5\"".to_string(),
            checkpoint: false,
        };

        let s2 = Step {
            name: "step_2".to_string(),
            //command : "dummyDB=\"2\"".to_string(),
            command: "echo \"2\" >> dummydb.txt".to_string(),
            checkpoint: true,
        };

        let x = Genus::new(PathBuf::from("."));

        let flno = FlowNode {
            name: "test".to_string(),
            steps: vec![s1, s15, s2],
            deps: vec![],
            tool: Arc::new(x),
        };

        flno.tool.invoke(flno.steps);

        assert_eq!(2 + 2, 4)
    }
}
