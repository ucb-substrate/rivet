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
           println!("  - Running step: '{}'", step.name);
            println!("    Command: {}", step.command);

            // Execute the command using a bash shell
            let status = Command::new("./dummyscript.sh")
                .arg(&step.command) // Pass the step's command as an argument to the script
                .current_dir(&self.work_dir) // Run command in the tool's work directory
                .status() // Wait for the command to finish
                .expect("Failed to execute dummyscript.sh script. Make sure it's in the work_dir and executable.");
 

            if !status.success() {
                // If the command failed, print an error and stop.
                // In a real scenario, you'd want more robust error handling.
                eprintln!("Error: Step '{}' failed with exit code {}", step.name, status);
                panic!("Stopping flow due to failed step.");
            }

            if step.checkpoint {
                // The checkpoint command is now just another step,
                // but we can still have a dedicated function for creating it.
                self.write_checkpoint(&self.work_dir.join(format!("{}.checkpoint", step.name)));
            }       
        }
    }

    fn write_checkpoint(&self, path: &Path) -> Step {
       let checkpoint_command = format!("echo 'checkpoint data' > {}", path.to_str().unwrap());
        println!("  - Writing checkpoint with command: {}", checkpoint_command);

        // We can return a Step struct representing this action, although
        // for this dummy tool, we execute it directly.
        Step {
            name: "write_checkpoint".to_string(),
            command: checkpoint_command,
            checkpoint: true, // This is meta-data about the step itself
        } 
    }

    fn read_checkpoint(&self, path: &Path) -> Step {
        println!("  - Reading checkpoint from: {:?}", path);
        let content = fs::read_to_string(path).expect("Failed to read checkpoint.");
        println!("    Checkpoint content: {}", content.trim());
        Step {
            name: "read_checkpoint".to_string(),
            command: format!("cat {}", path.to_str().unwrap()),
            checkpoint: false,
        }
    }

    
}
