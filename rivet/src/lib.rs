use by_address::ByAddress;
use std::any::Any;
use std::collections::HashSet;
use std::fmt::Debug;
use std::ops::Deref;
use std::path::PathBuf;
use std::sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard};

pub mod bash;
mod clipboard;
pub mod exec;
pub mod executor;
mod keys;
pub mod log;
pub mod progress;
pub mod rust;

pub use executor::{
    execute, BlockedStep, ExecuteConfig, ExecuteError, Executor, StepFailure, Summary,
};

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

/// Why a step failed.
///
/// Any error type converts with `?`, and a message can be turned into one with
/// `.into()`:
///
/// ```
/// # use rivet::StepResult;
/// fn check(clean: bool) -> StepResult {
///     if !clean {
///         return Err("LVS mismatch: 3 unmatched nets".into());
///     }
///     Ok(())
/// }
/// # assert!(check(false).is_err());
/// ```
pub type StepError = Box<dyn std::error::Error + Send + Sync>;

/// What [`Step::execute`] returns.
pub type StepResult = Result<(), StepError>;

pub trait Step: Debug + Any + Send + Sync {
    fn deps(&self) -> Vec<StepRef<dyn Step>>;
    fn pinned(&self) -> bool;

    /// Do the step's work.
    ///
    /// Return `Err` for an expected failure — a tool exiting non-zero, LVS not
    /// matching, a missing input. Panicking is for bugs; the executor catches
    /// panics so they do not take down the run, but reports them as such.
    fn execute(&self) -> StepResult;

    /// Short name for this step in progress output.
    ///
    /// Defaults to the step's type name; implementations that know what they
    /// are working on should say so instead (`"decoder par"` beats
    /// `"InnovusStep"`).
    fn label(&self) -> String {
        let name = std::any::type_name::<Self>();
        name.rsplit("::").next().unwrap_or(name).to_string()
    }

    /// Directory this step's own log file goes in.
    ///
    /// Whatever the step logs with `tracing` while it runs is written to
    /// `{label}.rivet.log` here, next to the `.out` and `.err` files of the
    /// tools it drove. A step with a working directory should say so:
    ///
    /// ```
    /// # use std::path::PathBuf;
    /// # struct ParStep { work_dir: PathBuf }
    /// # impl ParStep {
    /// fn log_dir(&self) -> Option<PathBuf> {
    ///     Some(self.work_dir.clone())
    /// }
    /// # }
    /// ```
    ///
    /// Returning `None`, the default, is not a way to turn logging off: the
    /// step's events still reach the run-wide
    /// [`rivet.log`](crate::log::RUN_LOG), they just have nowhere of their own
    /// to go. Steps that do no work on disk have no reason to leave a file
    /// somewhere.
    fn log_dir(&self) -> Option<PathBuf> {
        None
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
        &self.inner
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
