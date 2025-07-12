use rivet::flow::{Step, Tool};
use std::fmt::Debug;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

#[derive(Debug)]
pub struct DummyTool {
    pub work_dir: PathBuf,
}

impl DummyTool {
    pub fn new(work_dir: impl Into<PathBuf>) -> Self {
        let dir = work_dir.into();
        DummyTool { work_dir: dir }
    }
}

impl Tool for DummyTool {
    fn invoke(&self, work_dir: PathBuf, start_checkpoint: Option<PathBuf>, steps: Vec<Step>) {
        // let dummyDB_stat = Command::new("zsh")
        //     .arg("-c")
        //     .arg("echo \"start\" >> dummydb.txt")
        //     .current_dir(&self.work_dir)
        //     .status()
        //     .expect("Failed");
        // if !dummyDB_stat.success() {
        //             eprintln!("Failed to write checkpoint to file");
        // }

        if let Some(start_checkpoint) = start_checkpoint {
            let db = std::fs::read_to_string(start_checkpoint).unwrap();
        }

        for step in steps {
            println!("  - Running step: '{}'", step.name);
            println!("    Command: {}", step.command);

            let status = Command::new("zsh")
                .arg("-c")
                .arg(&step.command)
                .current_dir(&work_dir)
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
}

#[cfg(test)]
mod tests {
    use std::env;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::Arc;

    use rivet::flow::{FlowNode, Step, Tool};

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
