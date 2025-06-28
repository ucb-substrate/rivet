use crate::cli::Config;
use serde::Deserialize;
use std::collections::HashMap;
use std::collections::HashSet;
use std::fmt::Debug;
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Deserialize, Debug, Clone)]
struct Config {
    #[serde(flatten)]
    tools: HashMap<String, ToolConfig>,
}

#[derive(Deserialize, Debug, Clone)]
struct ToolStart {
    step: String,
    checkpoint: Option<PathBuf>,
}

#[derive(Deserialize, Debug, Clone)]
#[serde(rename_all = "kebab-case")]
struct ToolConfig {
    start: Option<ToolStart>, //start from a specified step or start from beginning
    stop: Option<String>,     //end at a specific step or end at the end
    pin: Option<bool>,
    //output_dir: Option<PathBuf>,
}

#[derive(Clone, Debug)]
pub struct Step {
    pub name: String,
    pub command: String,
    pub checkpoint: bool,
}

pub trait Tool: Send + Sync + Debug {
    /// The tool's work directory.
    fn work_dir(&self) -> PathBuf;

    /// Checkpoint paths for each step.
    ///
    /// e.g. step "syn_generic" gives "post_syn_generic".
    fn checkpoint(&self, step: Step) -> PathBuf;

    /// Runs the tool for the given steps.
    fn invoke(&self, steps: Vec<Step>);

    /// Returns a TCL string that instructs a tool to write a checkpoint to the given path.
    ///
    /// e.g. "write_db {path}".
    fn write_checkpoint(&self, path: &PathBuf) -> Step;

    /// Returns a TCL string that instructs a tool to read a checkpoint to the given path.
    ///
    /// e.g. "read_db {path}".
    fn read_checkpoint(&self, path: &PathBuf) -> Step;
}

#[derive(Debug)]
pub struct FlowNode {
    pub name: String,
    pub tool: Arc<dyn Tool>,
    pub steps: Vec<Step>,
    pub deps: Vec<String>,
}

#[derive(Debug)]
pub struct Flow {
    pub workflow: HashMap<String, FlowNode>,
}

impl Flow {
    pub fn new(workflow: HashMap<String, FlowNode>) -> Self {
        Flow { workflow }
    }
    /// Recursively executes a node and its dependencies, respecting pins and checkpoints.
    pub fn execute(&self, node: &str, config: &Arc<Config>) {
        let mut executed = HashSet::new();
        self.execute_inner(node, config, &mut executed);
    }

    /// Recursively executes a node and its dependencies, respecting pins and checkpoints.
    fn execute_inner(&self, node: &str, config: &Arc<Config>, executed: &mut HashSet<String>) {
        let target_node = self.workflow.get(node).unwrap_or_else(|| {
            panic!("Error: Node '{}' not found in the defined flow.", node,);
        });

        // If this node has already been executed in this run, skip it.
        if executed.contains(node) {
            return;
        }

        // --- 1. Execute Dependencies First ---
        println!("Evaluating node '{}'...", node);
        for dep in &target_node.deps {
            self.execute_inner(dep, config, executed);
        }

        // --- 2. Execute the Current Node ---
        let tool_config = config.tools.get(node);

        // Check if the node is pinned in the config.
        if let Some(true) = tool_config.and_then(|c| c.pin) {
            println!("---> Node '{}' is pinned. Skipping execution.", node);
            executed.insert(node.to_string());
            return;
        }

        println!("---> Executing node '{}'", node);

        let steps_to_run = get_steps_for_tool(target_node, tool_config);
        let checkpoint = tool_config.and_then(|c| c.checkpoint.as_ref());

        if steps_to_run.is_empty() {
            println!(
                "---> No steps to run for node '{}' based on configuration.",
                node,
            );
        } else {
            if let Some(cp) = checkpoint {
                println!("---> Starting from checkpoint: {:?}", cp);
                let read_checkpoint_step = target_node.tool.read_checkpoint(cp);

                // Execute the command from the returned step to restore the state
                let status = std::process::Command::new("zsh")
                    .arg("-c")
                    .arg(&read_checkpoint_step.command)
                    // .current_dir(&node.tool.work_dir())
                    .status()
                    .expect("Failed to execute read_checkpoint command");

                if !status.success() {
                    panic!("Failed to read checkpoint from file");
                }
            }
            target_node.tool.invoke(steps_to_run);
        }

        println!("---> Finished node '{}'", node);
        executed.insert(node.to_string());
    }
}

/// Filters the steps for a tool based on the `start` and `stop` keys in the config.
fn get_steps_for_tool(node: &FlowNode, config: Option<&ToolConfig>) -> Vec<Step> {
    let all_steps = &node.steps;
    let tool_config = match config {
        Some(tc) => tc,
        None => return all_steps.to_vec(), // No config, run all steps.
    };

    let start_index = tool_config
        .start
        .as_ref()
        .and_then(|start_name| all_steps.iter().position(|s| &s.name == start_name))
        .unwrap_or(0);

    let stop_index = tool_config
        .stop
        .as_ref()
        .and_then(|stop_name| all_steps.iter().position(|s| &s.name == stop_name))
        .map(|x| x + 1)
        .unwrap_or(all_steps.len());

    if start_index >= stop_index {
        return vec![];
    }

    all_steps[start_index..stop_index].to_vec()
}
