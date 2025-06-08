pub struct FlowNode {
    name: String,
    tool: Arc<dyn Tool>,
    steps: Vec<Step>,
    deps: Vec<Arc<FlowNode>>,
}

pub trait Tool {
    /// The tool's work directory.
    fn work_dir(&self) -> PathBuf;

    /// Checkpoint paths for each step.
    fn checkpoints(&self, steps: Vec<Step>) -> Vec<PathBuf>;

    /// Runs the tool for the given steps.
    fn invoke(&self, steps: Vec<Step>);

    /// Writes a checkpoint to the given path.
    fn write_checkpoint(&self, path: impl AsRef<Path>) -> Step;

    /// Reads a checkpoint from the given path.
    fn read_checkpoint(&self, path: impl AsRef<Path>) -> Step;
}

pub struct Step {
    name: String,
    command: String,
    checkpoint: bool,
}
