use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Clone)]
pub struct Step {
    pub name: String,
    pub command: String,
    pub checkpoint: bool,
}

pub trait Tool {
    /// The tool's work directory.
    fn work_dir(&self) -> PathBuf;

    /// Checkpoint paths for each step.
    fn checkpoints(&self, steps: Vec<Step>) -> Vec<PathBuf>;

    /// Runs the tool for the given steps.
    fn invoke(&self, steps: Vec<Step>);

    /// Writes a checkpoint to the given path.
    fn write_checkpoint(&self, path: &PathBuf) -> Step;

    /// Reads a checkpoint from the given path.
    fn read_checkpoint(&self, path: &PathBuf) -> Step;
}

pub struct FlowNode {
    pub name: String,
    pub tool: Arc<dyn Tool>,
    pub steps: Vec<Step>,
    pub deps: Vec<Arc<FlowNode>>,
}

pub struct Flow {
    pub workflow: Vec<Arc<FlowNode>>,
}

impl Flow {
    pub fn new(workflow: Vec<Arc<FlowNode>>) -> Self {
        Flow { workflow }
    }

    pub fn execute(&self, node: &FlowNode) {
        node.tool.invoke(node.steps.clone());
        for dep in &node.deps {
            self.execute(dep);
        }
    }
}
