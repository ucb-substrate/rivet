use crate::fs::File;
use rivet::flow::{Step, Tool};
use std::collections::HashSet;
use std::fmt::Debug;
use std::io::prelude::*;
use std::path::PathBuf;
use std::process::Command;
use std::{fs, io};

#[derive(Debug)]
pub struct Genus {
    pub work_dir: PathBuf,
    pub func_list: HashSet<String>,
}

impl Genus {
    pub fn new(work_dir: impl Into<PathBuf>) -> Self {
        let dir = work_dir.into();
        let mut helpers = HashSet::new();
        helpers.insert("init_environment".to_string());
        helpers.insert("predict_floorplan".to_string());
        helpers.insert("syn_generic".to_string());
        helpers.insert("syn_map".to_string());
        helpers.insert("add_tieoffs".to_string());
        helpers.insert("write_regs".to_string());
        helpers.insert("generate_reports".to_string());
        helpers.insert("write_outputs".to_string());

        Genus {
            work_dir: dir,
            func_list: helpers,
        }
    }

    //concatenate steps to a tcl file, syn.tcl file, genus.tcl

    fn make_tcl_file(&self, path: &PathBuf, steps: Vec<Step>) -> io::Result<()> {
        let file_path = path.join("syn.tcl");
        let mut tcl_file = File::create(&file_path).expect("failed to create syn.tcl file");

        writeln!(tcl_file, "puts \"{}\"", "set_db hdl_error_on_blackbox true")?;
        writeln!(tcl_file, "set_db hdl_error_on_blackbox true")?;
        writeln!(tcl_file, "puts \"{}\"", "set_db max_cpus_per_server 12")?;
        writeln!(tcl_file, "set_db max_cpus_per_server 12")?;
        writeln!(tcl_file, "puts \"{}\"", "set_multi_cpu_usage -local_cpu 12")?;
        writeln!(tcl_file, "set_multi_cpu_usage -local_cpu 12")?;
        writeln!(
            tcl_file,
            "puts \"{}\"",
            "set_db super_thread_debug_jobs true"
        )?;
        writeln!(tcl_file, "set_db super_thread_debug_jobs true")?;
        writeln!(
            tcl_file,
            "puts \"{}\"",
            "set_db super_thread_debug_directory super_thread_debug"
        )?;
        writeln!(
            tcl_file,
            "set_db super_thread_debug_directory super_thread_debug"
        )?;

        for step in steps.into_iter() {
            //if step name is in a hashset of helper functions run the helper function instead
            if self.func_list.contains(&step.name) {
                match step.name.as_str() {
                    "init_environment" => self.init_environment(&mut tcl_file)?,
                    "predict_floorplan" => self.predict_floorplan(&mut tcl_file)?,
                    "syn_generic" => self.syn_generic(&mut tcl_file)?,
                    "syn_map" => self.syn_map(&mut tcl_file)?,
                    "add_tieoffs" => self.add_tieoffs(&mut tcl_file)?,
                    "write_regs" => self.write_regs(&mut tcl_file)?,
                    "generate_reports" => self.generate_reports(&mut tcl_file)?,
                    "write_outputs" => self.write_outputs(&mut tcl_file)?,
                    "run_genus" => self.run_genus(&mut tcl_file)?,
                    // This case should not be reached if func_list is correct
                    _ => panic!("Helper function '{}' is in func_list but not implemented in match statement.", step.name),
                }
            } else {
                writeln!(tcl_file, "puts\"{}\"", step.command.to_string())?;
                writeln!(tcl_file, "{}", step.command)?;
            }
        }
        writeln!(tcl_file, "puts \"{}\"", "quit")?;
        writeln!(tcl_file, "quit")?;

        Ok(())
    }

    fn init_environment(tcl_file: &mut file) -> bool {}
    fn predict_floorplan(tcl_file: &mut file) -> bool {}
    fn syn_generic(tcl_file: &mut file) -> bool {}
    fn syn_map(tcl_file: &mut file) -> bool {}
    fn add_tieoffs(tcl_file: &mut file) -> bool {}
    fn write_regs(tcl_file: &mut file) -> bool {}
    fn generate_reports(tcl_file: &mut file) -> bool {}
    fn write_outputs(tcl_file: &mut file) -> bool {}
    fn run_genus(tcl_file: &mut file) -> bool {}
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

        self.make_tcl_file(&tcl_path, steps);

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

    fn checkpoint(&self, step: Step) -> PathBuf {
        let mut ret = PathBuf::new();
        ret.push(self.work_dir);
        ret.push(format!("{}.checkpoint", step.name));

        return ret;
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
}

#[cfg(test)]

mod tests {
    use std::env;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::Arc;

    use rivet::flow::{FlowNode, Step, Tool};

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
