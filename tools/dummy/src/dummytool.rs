use shammer::flow::{FlowNode, Tool, Step};
use std::fs;
use std::sync::Arc;
use std::process::Command;
use std::path::{Path, PathBuf};


pub struct DummyTool {
    pub work_dir: PathBuf,
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
           println!("  - Running step: '{}'", step.name);
            println!("    Command: {}", step.command);

            let status = Command::new(step.command.to_string())
                .arg(&step.command) 
                .current_dir(&self.work_dir) 
                .status() 
                .expect("Failed");
 

            if !status.success() {
                eprintln!("Error: Step '{}' failed with exit code {}", step.name, status);
                panic!("Stopping flow.");
            }

            if step.checkpoint {
                self.write_checkpoint(&self.work_dir.join(format!("{}.checkpoint", step.name)));
            }       
        }
    }

    fn write_checkpoint(&self, path: &PathBuf) -> Step {
       let checkpoint_command = format!("echo 'checkpoint data' > {}", path.to_str().unwrap());
        println!("  - Writing checkpoint with command: {}", checkpoint_command);

        Step {
            name: "write_checkpoint".to_string(),
            command: checkpoint_command,
            checkpoint: true, 
        } 
    }

    fn read_checkpoint(&self, path: &PathBuf) -> Step {
        println!("  - Reading checkpoint from: {:?}", path);
        let content = fs::read_to_string(path).expect("Failed to read checkpoint.");
        println!("    Checkpoint content: {}", content.trim());
        Step {
            name: "read_checkpoint".to_string(),
            command: format!("cat {}", path.to_str().unwrap()),
            checkpoint: false,
        }
    }

    /// Checkpoint paths for each step.
    fn checkpoints(&self, steps: Vec<Step>) -> Vec<PathBuf> {
        unimplemented!();
    }

    
}

use std::env;

#[cfg(test)]

mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use shammer::flow::{Step, Tool, FlowNode};

    use crate::dummytool::DummyTool;

    #[test]
    fn test_basic_flow() {

        let s1 = Step{
            name: "step 1".to_string(),
            command: "./dummyscript.sh \" 1  \" > nohup1.out".to_string(),
            checkpoint: true,
        };

        let s2 = Step {
            name : "step 2".to_string(),
            command : "./dummyscript.sh \" 2  \" > nohup1.out".to_string(),
            checkpoint: true,
        };

        let x = DummyTool::new(PathBuf::new());

        let flno = FlowNode {
            name : "test".to_string(),
            steps : vec![s1, s2],
            deps : vec![],
            tool : Arc::new(x)
            
        };

        assert_eq!(2+2, 5)

    }
}
