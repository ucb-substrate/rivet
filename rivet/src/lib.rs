use by_address::ByAddress;
use std::collections::HashSet;
use std::fmt::Debug;
use std::sync::{Arc, Mutex, MutexGuard};

pub mod bash;
pub mod exec;
pub mod executor;
pub mod progress;
pub mod rust;

pub use executor::{execute, ExecuteConfig, ExecuteError, StepFailure, Summary};
pub use progress::OutputMode;

#[derive(Debug)]
pub struct Dag<F> {
    pub node: F,
    pub directed_edges: Vec<Arc<Dag<F>>>,
}
pub trait NamedNode {
    fn name(&self) -> String;
}

pub struct DagIter<'a, F> {
    stack: Vec<&'a Dag<F>>,
    visited: HashSet<ByAddress<&'a Dag<F>>>,
}

impl<'a, F> Iterator for DagIter<'a, F> {
    type Item = &'a F;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let dag = self.stack.pop()?;
            if !self.visited.insert(ByAddress(dag)) {
                continue;
            }
            self.stack
                .extend(dag.directed_edges.iter().rev().map(|e| e.as_ref()));
            return Some(&dag.node);
        }
    }
}

impl<F> Dag<F> {
    pub fn iter(&self) -> DagIter<'_, F> {
        DagIter {
            stack: vec![self],
            visited: HashSet::new(),
        }
    }
}

impl<'a, F> IntoIterator for &'a Dag<F> {
    type Item = &'a F;
    type IntoIter = DagIter<'a, F>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl<F: NamedNode> Dag<F> {
    pub fn get(&self, target: &str) -> Option<&F> {
        self.iter().find(|n| n.name() == target)
    }
}

pub trait Step: Debug + Send + Sync {
    fn deps(&self) -> Vec<Arc<dyn Step>>;
    fn pinned(&self) -> bool;
    fn execute(&self);

    /// Short name for this step in progress output.
    ///
    /// Defaults to the step's type name; implementations that know what they
    /// are working on should say so instead (`"decoder par"` beats
    /// `"InnovusStep"`).
    fn label(&self) -> String {
        let name = std::any::type_name::<Self>();
        name.rsplit("::").next().unwrap_or(name).to_string()
    }
}

pub fn hierarchical<M, F>(dag: &Dag<M>, flat_flow_gen: &impl Fn(&M, Vec<(&M, &F)>) -> F) -> Dag<F> {
    let new_edges: Vec<Arc<Dag<F>>> = dag
        .directed_edges
        .iter()
        .map(|edge_dag| Arc::new(hierarchical(edge_dag, flat_flow_gen))) // Added Arc::new() here
        .collect();

    let sub_blocks: Vec<(&M, &F)> = dag
        .directed_edges
        .iter()
        .zip(new_edges.iter())
        .map(|(original_dag, new_dag)| (&original_dag.node, &new_dag.node))
        .collect();

    let new_node = flat_flow_gen(&dag.node, sub_blocks);

    Dag {
        node: new_node,
        directed_edges: new_edges,
    }
}

#[derive(Debug, Clone)]
pub struct StepRef<T: Step> {
    inner: Arc<Mutex<T>>,
}

impl<T: Step> StepRef<T> {
    pub fn new(data: T) -> Self {
        Self {
            inner: Arc::new(Mutex::new(data)),
        }
    }

    pub fn lock(&self) -> MutexGuard<'_, T> {
        self.inner.lock().unwrap()
    }

    pub fn get<R>(&self, get_fn: impl FnOnce(&T) -> R) -> R {
        let inner = self.inner.lock().unwrap();
        get_fn(&inner)
    }

    pub fn update<R>(&self, update_fn: impl FnOnce(&mut T) -> R) -> R {
        let mut inner = self.inner.lock().unwrap();
        update_fn(&mut inner)
    }
}
impl<T: Step> Step for StepRef<T> {
    fn execute(&self) {
        self.lock().execute();
    }

    fn deps(&self) -> Vec<Arc<dyn Step>> {
        self.lock().deps()
    }

    fn pinned(&self) -> bool {
        self.lock().pinned()
    }

    fn label(&self) -> String {
        self.lock().label()
    }
}
