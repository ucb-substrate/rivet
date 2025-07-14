use serde::Deserialize;
use std::collections::HashMap;
use std::collections::HashSet;
use std::fmt::Debug;
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Deserialize, Debug, Clone)]
pub struct Config {
    #[serde(flatten)]
    tools: HashMap<String, ToolConfig>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct ToolStart {
    step: String,
    checkpoint: Option<PathBuf>,
}

#[derive(Deserialize, Debug, Clone)]
#[serde(rename_all = "kebab-case")]
pub struct ToolConfig {
    start: Option<ToolStart>, //start from a specified step or start from beginning
    stop: Option<String>,     //end at a specific step or end at the end
    pin: Option<bool>,
    //output_dir: Option<PathBuf>,
}

#[derive(Clone, Debug)]
pub struct Step {
    pub name: String,
    pub command: String,
    pub checkpoint: Option<PathBuf>,
}

pub trait Tool: Debug {
    /// Runs the tool for the given steps.
    fn invoke(&self, work_dir: PathBuf, start_checkpoint: Option<PathBuf>, steps: Vec<Step>);
}

#[derive(Debug)]
pub struct FlowNode {
    pub name: String,
    pub tool: Arc<dyn Tool>,
    pub work_dir: PathBuf,
    pub checkpoint_dir: PathBuf,
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

    //So have a bunch of nodes; each of these has a bunch of steps. Want to execute flow, so execute
    //node but also have to deal w dependencies that may or may not be pinned

    /// Recursively executes a node and its dependencies, respecting pins and checkpoints.
    pub fn execute(&self, node: &str, config: &Arc<Config>) {
        let mut executed = HashSet::new();
        self.execute_inner(node, config, &mut executed);
    }

    //recusrively execute each node and dependencies, respecting the pins and checkpoints
    //each node has steps that may or may not be checkpoints
    fn execute_inner(&self, node: &str, config: &Arc<Config>, executed: &mut HashSet<String>) {
        let target_node = self
            .workflow
            .get(node)
            .expect(&format!("Error: Node {} not found in flow", node));

        //need to check if this node has already been executed
        if executed.contains(node) {
            return;
        }

        //evaluate all the dependency nodes first
        println!("Evaluating node '{}'...", node);

        for dependency in &target_node.deps {
            self.execute_inner(dependency, config, executed);
        }

        //now we execute the current node

        //need to check if this node is pinned

        let tool_config = config.tools.get(node);

        if let Some(true) = tool_config.and_then(|c| c.pin) {
            println!("---> Node '{}' is pinned. Skipping execution.", node);
            executed.insert(node.to_string());
            return;
        }

        //now we execute the steps inside the node

        let steps_to_run = get_steps_for_tool(target_node, tool_config);

        if steps_to_run.is_empty() {
            println!(
                "---> No steps to run for node '{}' based on configuration.",
                node
            );
        } else {
            let start_checkpoint = tool_config
                .and_then(|config_ref| config_ref.start.as_ref())
                .and_then(|tool_start_ref| tool_start_ref.checkpoint.as_ref())
                .cloned();

            // tool invokes the steps to run, but we need to specify checkpoint that we are running from
            target_node.tool.as_ref().invoke(
                target_node.work_dir.clone(),
                start_checkpoint,
                steps_to_run,
            );
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
        .and_then(|start_name| all_steps.iter().position(|s| &s.name == start_name)) //fix
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
