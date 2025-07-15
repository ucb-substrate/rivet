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
        //use colored::Colorize;
        println!("Beginning invoking...");
        let mut dummy_db_int: i32 = 0;

        if let Some(start_check_point) = start_checkpoint {
            //there is a start_checkpoint
            let absolute_path = work_dir.join(&start_check_point);

            //now need to read from checkpoint
            match fs::read_to_string(&absolute_path) {
                Ok(content) => {
                    println!(
                        "\nContent of {:?}:\n{cont}",
                        absolute_path.display().to_string(),
                        cont = content
                    );

                    //get dummy_db_int from this
                    dummy_db_int = content.parse::<i32>().unwrap();
                }
                Err(e) => {
                    panic!(
                        "\n --> Error reading checkpoint at {abs_path}: {e_str}\n ",
                        abs_path = absolute_path.display().to_string(),
                        e_str = e,
                    );
                }
            }
        } else {
            //let the dummy_db_int be 0 if no checkpoint is specified
            println!("\nNo checkpoint specified.\n");
            dummy_db_int = 0;
        }
        for step in steps {
            println!("  - Running step: '{}'", step.name);
            println!("    Command: {}", step.command);

            //we parse the command; first character is {+ - *} while the second is an integer

            let operation = step.command.chars().next().unwrap();

            let command_str: String = step.command.chars().skip(1).collect();
            let command_int: i32 = command_str.parse::<i32>().unwrap();

            //execute the step's command

            if operation == '+' {
                dummy_db_int = dummy_db_int + command_int;
            } else if operation == '-' {
                dummy_db_int = dummy_db_int - command_int;
            } else if operation == '*' {
                dummy_db_int = dummy_db_int * command_int;
            } else {
                panic!(
                    "\nError: unknown operation {op} at {com} in step {name}",
                    op = operation,
                    com = step.command,
                    name = step.name
                );
            }

            //if checkpoint then have to save
            if let Some(step_checkpoint) = step.checkpoint {
                //get the complete path to the checkpoint file
                let absolute_checkpoint_path_str =
                    work_dir.join(&step_checkpoint).display().to_string();
                //get checkpointing command
                let checkpoint_command = format!(
                    "echo \"{db}\" >> {loc}",
                    db = dummy_db_int,
                    loc = absolute_checkpoint_path_str
                );

                let status = Command::new("zsh")
                    .arg("-c")
                    .arg(&checkpoint_command)
                    .current_dir(&work_dir)
                    .status()
                    .expect("\nFailed to make checkpoint...\n");

                if !status.success() {
                    eprintln!(
                        "Error: Checkpointing at step '{}' failed with exit code {}",
                        step.name, status
                    );
                    panic!("Stopping flow.");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::Arc;

    use rivet::flow::Config;
    use rivet::flow::Flow;
    use rivet::flow::ToolConfig;
    use rivet::flow::{FlowNode, Step, Tool};

    use super::*;
    //use crate::dummytool::DummyTool;

    #[test]
    fn test_basic_flow() {
        let s1 = Step {
            name: "step_1".to_string(),
            command: "-1".to_string(),
            checkpoint: Some(PathBuf::from("./step_1_checkpt.txt")),
        };

        let s15 = Step {
            name: "step_15".to_string(),
            command: "+3".to_string(),
            checkpoint: None,
        };

        let s2 = Step {
            name: "step_2".to_string(),
            //command : "dummyDB=\"2\"".to_string(),
            command: "*2".to_string(),
            checkpoint: Some(PathBuf::from("./step_2_checkpt.txt")),
        };

        let x = DummyTool::new();

        let flno = FlowNode {
            name: "test_flow_node".to_string(),
            tool: Arc::new(x),
            work_dir: PathBuf::from("./dummy_work_dir"),
            checkpoint_dir: PathBuf::from("./dummy_work_dir/checkpoint_dir"),
            steps: vec![s1, s15, s2],
            deps: vec![],
        };

        let flow = Flow {
            workflow: HashMap::from([("test_flow_node".to_string(), flno)]),
        };

        let x_config = ToolConfig {
            start: None,
            stop: None,
            pin: None,
        };

        let config = Config {
            tools: HashMap::from([("test_flow_node".to_string(), x_config)]),
        };

        flow.execute("test_flow_node", &Arc::new(config).clone());

        assert_eq!(2 + 2, 4)
    }
}
