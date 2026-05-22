use by_address::ByAddress;
use std::any::Any;
use std::collections::HashSet;
use std::fmt::Debug;
use std::ops::Deref;
use std::sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard};

pub mod bash;
pub mod rust;

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

pub trait Step: Debug + Any + Send + Sync {
    fn deps(&self) -> Vec<StepRef<dyn Step>>;
    fn pinned(&self) -> bool;
    fn execute(&self);
}

#[derive(Default)]
pub struct Executor {
    executed: HashSet<ByAddress<StepRef<dyn Step>>>,
}

impl Executor {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn execute<T: Step>(mut self, step: StepRef<T>) -> Self {
        self.execute_step(step.into_dyn());
        self
    }

    fn execute_step(&mut self, step: StepRef<dyn Step>) {
        let step_addr = ByAddress(step.clone());
        if self.executed.contains(&step_addr) {
            return;
        }
        if step.read().pinned() {
            self.executed.insert(step_addr);
            return;
        }
        let deps = step.read().deps();
        for dep in deps {
            self.execute_step(dep);
        }
        step.read().execute();
        self.executed.insert(ByAddress(step));
    }
}

pub fn execute(step: StepRef<impl Step + 'static>) {
    Executor::new().execute(step);
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

#[derive(Debug)]
pub struct StepRef<T: ?Sized> {
    inner: Arc<RwLock<T>>,
}

impl<T: ?Sized> Clone for StepRef<T> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<T: ?Sized> Deref for StepRef<T> {
    type Target = RwLock<T>;

    fn deref(&self) -> &Self::Target {
        &*self.inner
    }
}

impl<T> StepRef<T> {
    pub fn new(data: T) -> Self {
        Self {
            inner: Arc::new(RwLock::new(data)),
        }
    }
}

impl<T: ?Sized> StepRef<T> {
    pub fn read(&self) -> RwLockReadGuard<'_, T> {
        self.inner.read().unwrap()
    }

    pub fn write(&self) -> RwLockWriteGuard<'_, T> {
        self.inner.write().unwrap()
    }

    pub fn get<R>(&self, get_fn: impl FnOnce(&T) -> R) -> R {
        let inner = self.inner.read().unwrap();
        get_fn(&inner)
    }

    pub fn update<R>(&self, update_fn: impl FnOnce(&mut T) -> R) -> R {
        let mut inner = self.inner.write().unwrap();
        update_fn(&mut inner)
    }
}

impl<T: Step + 'static> StepRef<T> {
    pub fn into_dyn(self) -> StepRef<dyn Step> {
        StepRef {
            inner: self.inner as Arc<RwLock<dyn Step>>,
        }
    }
}
