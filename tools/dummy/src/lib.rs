use rivet::flow::{Step, Tool};
use std::fmt::Debug;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

#[derive(Debug)]
pub struct DummyTool;

impl DummyTool {
    pub fn new() -> Self {
        DummyTool {}
    }
}

impl Tool for DummyTool {
    fn invoke(&self, work_dir: PathBuf, start_checkpoint: Option<PathBuf>, steps: Vec<Step>) {
        use colored::Colorize;
        println!("Beginning invoking...");
        if let Some(start_check_point) = start_checkpoint {
            //there is a start_checkpoint
            let absolute_path = work_dir.join(&start_check_point);

            //now need to read from checkpoint
            match fs::read_to_string(&absolute_path) {
                Ok(content) => {
                    println!("\nContent of {:?}:\n{}".green(), dummy_file_path, content);

                    //get dummy_db_int from this
                    let dummy_db_int = content.parse::<i32>().unwrap();
                }
                Err(e) => {
                    panic!(
                        "\n --> Error reading checkpoint at {:?}: {}\n ".red(),
                        absolute_path, e
                    );
                }
            }
        } else {
            //let the dummy_db_int be 0 if no checkpoint is specified
            println!("\nNo checkpoint specified.\n");
            let dummy_db_int: i32 = 0;
        }
        for step in steps {
            println!("  - Running step: '{}'", step.name);
            println!("    Command: {}", step.command);

            //we parse the command; first character is {+ - *} while the second is an integer

            let operation = step.command.chars().next().unwrap();
            let command_int = step
                .command
                .chars()
                .skip(1)
                .collect()
                .parse::<i32>()
                .unwrap();

            if operation == "+" {
                dummy_db_int = dummy_db_int + command_int;
            } else if operation == "-" {
                dummy_db_int = dummy_db_int - command_int;
            } else if operation == "*" {
                dummy_db_int = dummy_db_int * command_int;
            } else {
                panic!(
                    "\nError: unknown operation {} at {} in step {}".red(),
                    operation, step.command, step.name
                );
            }

            if let Some(step_checkpoint) = step.checkpoint {
                //get the complete path to the checkpoint file
                let absolute_checkpoint_path_str =
                    work_dir.join(&step_checkpoint).display().to_string();
                //get checkpointing command
                let checkpoint_command = format!(
                    "echo \"{}\" >> {loc}",
                    dummy_db_int,
                    loc = absolute_checkpoint_path_str
                );

                let status = Command::new("zsh")
                    .arg("-c")
                    .arg(&absolute_checkpoint_path_str)
                    .current_dir(&work_dir)
                    .status()
                    .expect("\nFailed to make checkpoint...\n");

                if !status.success() {
                    eprintln!(
                        "Error: Checkpointing at step '{}' failed with exit code {}".red(),
                        step.name, status
                    );
                    panic!("Stopping flow.".red());
                }
            }
        }
    }
}

// impl Tool for DummyTool {
//     fn work_dir(&self) -> PathBuf {
//         self.work_dir.clone()
//     }

//     fn invoke(&self, steps: Vec<Step>) {
//         // let dummyDB_stat = Command::new("zsh")
//         //     .arg("-c")
//         //     .arg("echo \"start\" >> dummydb.txt")
//         //     .current_dir(&self.work_dir)
//         //     .status()
//         //     .expect("Failed");
//         // if !dummyDB_stat.success() {
//         //             eprintln!("Failed to write checkpoint to file");
//         // }

//         for step in steps {
//             println!("  - Running step: '{}'", step.name);
//             println!("    Command: {}", step.command);

//             let status = Command::new("zsh")
//                 .arg("-c")
//                 .arg(&step.command)
//                 .current_dir(&self.work_dir)
//                 .status()
//                 .expect("Failed");

//             if !status.success() {
//                 eprintln!(
//                     "Error: Step '{}' failed with exit code {}",
//                     step.name, status
//                 );
//                 panic!("Stopping flow.");
//             }

//             if step.checkpoint {
//                 let temp =
//                     self.write_checkpoint(&self.work_dir.join(format!("{}.checkpoint", step.name)));
//                 //let mut file = File::create_new("temp.sh").expect("failed to make temp file");
//                 //file.write_all(format!("!/bin/bash\n{}",&temp.command));

//                 let check_status = Command::new("zsh")
//                     .arg("-c")
//                     .arg(&temp.command)
//                     .current_dir(&self.work_dir)
//                     .status()
//                     .expect("Failed");
//                 if !check_status.success() {
//                     eprintln!("Failed to write checkpoint to file");
//                 }
//             }

//             // if step.checkpoint {
//             //     let check_status = Command::new("bash")
//             //         .arg("-c")
//             //         .arg(format!("echo $dummydummyDB > {}.txt", step.name))
//             //         .current_dir(&self.work_dir)
//             //         .status()
//             //         .expect("failed");

//             //     if !check_status.success() {
//             //         eprintln!("Error: Checkpoint Step '{}' failed with exit code {}", step.name, check_status);
//             //         panic!("Stopping flow.");
//             //     }

//             // }
//         }
//     }

//     fn write_checkpoint(&self, path: &PathBuf) -> Step {
//         let checkpoint_command = format!("cat dummydb.txt > {}", path.to_str().unwrap());
//         println!("  - Writing checkpoint w command: {}", checkpoint_command);

//         Step {
//             name: "write_checkpoint".to_string(),
//             command: checkpoint_command,
//             checkpoint: true,
//         }
//     }

//     fn read_checkpoint(&self, path: &PathBuf) -> Step {
//         let command = format!("cat {} > dummydb.txt", path.to_str().unwrap());
//         println!("  - Reading checkpoint with command: {}", command);
//         Step {
//             name: "read_checkpoint".to_string(),
//             command,
//             checkpoint: false,
//         }
//     }

//     fn checkpoints(&self, steps: Vec<Step>) -> Vec<PathBuf> {
//         let mut ret: Vec<PathBuf> = vec![];
//         for step in steps.into_iter() {
//             if step.checkpoint {
//                 ret.push(self.work_dir.join(format!("{}.checkpoint", step.name)))
//             }
//         }
//         return ret;
//     }
// }

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
