use rivet::flow::{Step, Tool};
use std::fmt::Debug;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

#[derive(Debug)]
pub struct DummyTool;

impl DummyTool {
    pub fn new(work_dir: impl Into<PathBuf>) -> Self {
        let dir = work_dir.into();
        DummyTool { work_dir: dir }
    }
}

impl Tool for DummyTool {
    fn work_dir(&self) -> PathBuf {
        self.work_dir.clone()
    }

    fn invoke(&self, steps: Vec<Step>) {
        // let dummyDB_stat = Command::new("zsh")
        //     .arg("-c")
        //     .arg("echo \"start\" >> dummydb.txt")
        //     .current_dir(&self.work_dir)
        //     .status()
        //     .expect("Failed");
        // if !dummyDB_stat.success() {
        //             eprintln!("Failed to write checkpoint to file");
        // }

        for step in steps {
            println!("  - Running step: '{}'", step.name);
            println!("    Command: {}", step.command);

            let status = Command::new("zsh")
                .arg("-c")
                .arg(&step.command)
                .current_dir(&self.work_dir)
                .status()
                .expect("Failed");

            if !status.success() {
                eprintln!(
                    "Error: Step '{}' failed with exit code {}",
                    step.name, status
                );
                panic!("Stopping flow.");
            }

            if step.checkpoint {
                let temp =
                    self.write_checkpoint(&self.work_dir.join(format!("{}.checkpoint", step.name)));
                //let mut file = File::create_new("temp.sh").expect("failed to make temp file");
                //file.write_all(format!("!/bin/bash\n{}",&temp.command));

                let check_status = Command::new("zsh")
                    .arg("-c")
                    .arg(&temp.command)
                    .current_dir(&self.work_dir)
                    .status()
                    .expect("Failed");
                if !check_status.success() {
                    eprintln!("Failed to write checkpoint to file");
                }
            }

            // if step.checkpoint {
            //     let check_status = Command::new("bash")
            //         .arg("-c")
            //         .arg(format!("echo $dummydummyDB > {}.txt", step.name))
            //         .current_dir(&self.work_dir)
            //         .status()
            //         .expect("failed");

            //     if !check_status.success() {
            //         eprintln!("Error: Checkpoint Step '{}' failed with exit code {}", step.name, check_status);
            //         panic!("Stopping flow.");
            //     }

            // }
        }
    }

    fn write_checkpoint(&self, path: &PathBuf) -> Step {
        let checkpoint_command = format!("cat dummydb.txt > {}", path.to_str().unwrap());
        println!("  - Writing checkpoint w command: {}", checkpoint_command);

        Step {
            name: "write_checkpoint".to_string(),
            command: checkpoint_command,
            checkpoint: true,
        }
    }

    fn read_checkpoint(&self, path: &PathBuf) -> Step {
        let command = format!("cat {} > dummydb.txt", path.to_str().unwrap());
        println!("  - Reading checkpoint with command: {}", command);
        Step {
            name: "read_checkpoint".to_string(),
            command,
            checkpoint: false,
        }
    }

    fn checkpoints(&self, steps: Vec<Step>) -> Vec<PathBuf> {
        let mut ret: Vec<PathBuf> = vec![];
        for step in steps.into_iter() {
            if step.checkpoint {
                ret.push(self.work_dir.join(format!("{}.checkpoint", step.name)))
            }
        }
        return ret;
    }
}

#[cfg(test)]

mod tests {
    use std::env;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::Arc;

    use shammer::flow::{FlowNode, Step, Tool};

    use crate::dummytool::DummyTool;

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

        let x = DummyTool::new(PathBuf::from("."));

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
