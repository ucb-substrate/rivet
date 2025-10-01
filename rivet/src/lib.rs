use serde::Deserialize;
use std::collections::HashMap;
use std::collections::HashSet;
use std::fmt::Debug;
use std::path::PathBuf;
use std::sync::Arc;

pub struct Dag<F> {
    node: F,
    directed_edges: Vec<F>,
}

pub trait Step {
    fn deps(&self) -> Vec<Arc<dyn Step>>;
    fn pinned(&self) -> bool;
    fn execute(&self);
}

pub fn execute(target: impl Step) {
    //traverse dag, execute deps unless they are pinned

    let mut executed = HashSet::new();
    execute_inner(target, &mut executed); // this is assuming that a target is a Step not a tree of
                                          // flat flows for now
}

fn execute_inner(step: &impl Step, executed: &mut HashSet<dyn Step>) {
    if executed.contains(step) {
        return;
    }

    // use colored::Colorize;
    // println!(
    //     "{}",
    //     format!(
    //         "\n{main_str} '{node_str}'...\n",
    //         main_str = "Evaluating node".green(),
    //         node_str = node.green()
    //     )
    // );
    //
    for dependency in &step.deps() {
        execute_inner(dependency, executed);
    }

    if step.pinned() {
        // println!("---> Node '{}' is pinned. Skipping execution.", node);
        executed.insert(step.clone());
        return;
    }

    step.execute();

    // let temp_str = format!(
    //     "{main_str} '{node_str}'",
    //     main_str = "--> Finished node".green(),
    //     node_str = node.green()
    // );
    // println!("\n{}\n", temp_str);
    executed.insert(step.clone());
}

pub fn hierarchical(dag: Dag<M>, flat_flow_gen: impl Fn(&M) -> F) -> Dag<F> {
    //TODO
    // This is supposed to convert a dag of `ModuleInfo` and `FlatFlow` into a dag of flat flows
}

// /// Contains all the configs for tools used in the flow
// #[derive(Deserialize, Debug, Clone)]
// pub struct Config {
//     #[serde(flatten)]
//     pub tools: HashMap<String, ToolConfig>,
// }
//
// /// Indicates the starting step for a tool and an optional checkpoint path
// #[derive(Deserialize, Debug, Clone)]
// pub struct ToolStart {
//     pub step: String,
//     pub checkpoint: Option<PathBuf>,
// }
//
// /// Configures a tool for the following properties:
// ///     - start from a specified step or start from beginning
// ///     - end at a specific step or end at the last step
// ///     - be pinned and not be rebuilt in the flow
// #[derive(Deserialize, Debug, Clone)]
// #[serde(rename_all = "kebab-case")]
// pub struct ToolConfig {
//     pub start: Option<ToolStart>,
//     pub stop: Option<String>,
//     pub pin: Option<bool>,
// }

// /// Contains all Steps and maps them to a user-defined name
// #[derive(Debug)]
// pub struct Flow {
//     pub steps: HashMap<String, Step>,
// }

// / Represents a node in a design flow and describes the following properties:
// /     - the tool being used for the node
// /     - the work directory of the node for building
// /     - the directory where the node will output checkpoints
// /     - the steps that run in that node
// /     - the dependencies of the current FlowNode given by the labels of the other FlowNodes in
// /     the Flow

// #[derive(Debug)]
// pub struct FlowNode {
//     pub tool: Arc<dyn Tool>,
//     pub work_dir: PathBuf,
//     pub checkpoint_dir: PathBuf,
//     pub steps: Vec<Step>,
//     pub deps: Vec<String>,
// }

//
// /// Tool plugins adapt the api invoke to run the tool for the configured steps.
// pub trait Tool: Debug {
//     fn invoke(
//         &self,
//         work_dir: PathBuf,
//         start_checkpoint: Option<PathBuf>,
//         steps: Vec<AnnotatedStep>,
//     );
// }

// // Steps with a checkpoint for directly reading checkpoints
// pub struct AnnotatedStep {
//     pub step: Step,
//     pub checkpoint_path: PathBuf,
// }
//
// /// Steps have the following properties:
// ///     - Labelled
// ///     - Contain a TCL command
// ///     - Can be checkpointed
// #[derive(Clone, Debug)]
// pub struct Substep {
//     pub name: String,
//     pub command: String,
//     pub checkpoint: bool,
// }

// impl Flow {
//     pub fn new(nodes: HashMap<String, FlowNode>) -> Self {
//         Flow { nodes }
//     }
//
//
//     /// Recursively executes a node and its dependencies, respecting pins and checkpoints.
//     pub fn execute(&self, node: &str, config: &Config) {
//         let mut executed = HashSet::new();
//         self.execute_inner(node, config, &mut executed);
//     }
//
//     fn execute_inner(&self, node: &str, config: &Config, executed: &mut HashSet<String>) {
//         let target_node = self
//             .nodes
//             .get(node)
//             .expect(&format!("Error: Node {} not found in flow", node));
//
//         if executed.contains(node) {
//             return;
//         }
//
//         use colored::Colorize;
//         println!(
//             "{}",
//             format!(
//                 "\n{main_str} '{node_str}'...\n",
//                 main_str = "Evaluating node".green(),
//                 node_str = node.green()
//             )
//         );
//
//         for dependency in &target_node.deps {
//             self.execute_inner(dependency, config, executed);
//         }
//
//         let tool_config = config.tools.get(node);
//
//         if let Some(true) = tool_config.and_then(|c| c.pin) {
//             println!("---> Node '{}' is pinned. Skipping execution.", node);
//             executed.insert(node.to_string());
//             return;
//         }
//
//         let steps_to_run = get_steps_for_tool(target_node, tool_config);
//
//         if steps_to_run.is_empty() {
//             println!(
//                 "---> No steps to run for node '{}' based on configuration.",
//                 node
//             );
//         } else {
//             let start_checkpoint = tool_config
//                 .and_then(|config_ref| config_ref.start.as_ref())
//                 .and_then(|tool_start_ref| tool_start_ref.checkpoint.as_ref())
//                 .cloned();
//
//             target_node.tool.as_ref().invoke(
//                 target_node.work_dir.clone(),
//                 start_checkpoint,
//                 steps_to_run
//                     .into_iter()
//                     .map(|step| {
//                         let checkpoint_path = target_node.checkpoint_dir.join(&step.name);
//                         AnnotatedStep {
//                             step,
//                             checkpoint_path,
//                         }
//                     })
//                     .collect(),
//             );
//         }
//
//         let temp_str = format!(
//             "{main_str} '{node_str}'",
//             main_str = "--> Finished node".green(),
//             node_str = node.green()
//         );
//         println!("\n{}\n", temp_str);
//         executed.insert(node.to_string());
//     }
// }

// /// Filters the steps for a tool based on the `start` and `stop` keys in the config.
// fn get_steps_for_tool(node: &FlowNode, config: Option<&ToolConfig>) -> Vec<Step> {
//     use colored::Colorize;
//     println!("{}", "\nGetting steps for tool...".blue());
//
//     let all_steps = &node.steps;
//     let tool_config = match config {
//         Some(tc) => tc,
//         None => return all_steps.to_vec(), // No config, run all steps.
//     };
//
//     let start_index = tool_config
//         .start
//         .as_ref()
//         .and_then(|start_name| all_steps.iter().position(|s| s.name == start_name.step)) //fix(ed)?
//         .unwrap_or(0);
//
//     let stop_index = tool_config
//         .stop
//         .as_ref()
//         .and_then(|stop_name| all_steps.iter().position(|s| &s.name == stop_name))
//         .map(|x| x + 1)
//         .unwrap_or(all_steps.len());
//
//     if start_index >= stop_index {
//         return vec![];
//     }
//
//     all_steps[start_index..stop_index].to_vec()
// }
