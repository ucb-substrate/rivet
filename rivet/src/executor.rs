//! Parallel execution of a step graph.
//!
//! The graph reachable from a target step is flattened once up front, then run
//! by a pool of worker threads: a step becomes eligible as soon as every one of
//! its dependencies has finished, so independent branches (say DRC and LVS,
//! which both depend on P&R) run at the same time rather than one after the
//! other.
//!
//! Steps are identified by the address of their `Arc`, so a step shared by
//! several dependents is run exactly once no matter how many paths reach it.

use std::any::Any;
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::panic::{self, AssertUnwindSafe, PanicHookInfo};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use by_address::ByAddress;

use crate::progress::{self, Outcome, OutputMode, Reporter};
use crate::Step;

/// Settings for a run.
///
/// ```no_run
/// # use rivet::{ExecuteConfig, Step};
/// # fn demo(target: impl Step + 'static) -> Result<(), rivet::ExecuteError> {
/// ExecuteConfig::new().concurrency(2).run(target)?;
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct ExecuteConfig {
    concurrency: usize,
    output: OutputMode,
    progress: bool,
}

impl Default for ExecuteConfig {
    fn default() -> Self {
        Self {
            concurrency: default_concurrency(),
            output: OutputMode::default(),
            progress: true,
        }
    }
}

impl ExecuteConfig {
    pub fn new() -> Self {
        Self::default()
    }

    /// Maximum number of steps to run at once. Values below 1 are treated as 1.
    ///
    /// Defaults to `RIVET_JOBS` if it is set, otherwise to the number of
    /// available cores. Tools that hold licences or saturate a machine on their
    /// own are usually worth capping explicitly.
    pub fn concurrency(mut self, concurrency: usize) -> Self {
        self.concurrency = concurrency.max(1);
        self
    }

    /// How output from running steps is displayed.
    pub fn output(mut self, output: OutputMode) -> Self {
        self.output = output;
        self
    }

    /// Whether to draw the live progress display. When disabled (or when stderr
    /// is not a terminal) the run falls back to plain line logging.
    pub fn progress(mut self, progress: bool) -> Self {
        self.progress = progress;
        self
    }

    /// Execute `target` and everything it depends on.
    pub fn run(&self, target: impl Step + 'static) -> Result<Summary, ExecuteError> {
        self.run_arc(Arc::new(target) as Arc<dyn Step>)
    }

    /// Execute an already-shared step.
    pub fn run_arc(&self, target: Arc<dyn Step>) -> Result<Summary, ExecuteError> {
        run(self, target)
    }
}

/// What a completed run did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Summary {
    /// Steps in the graph.
    pub total: usize,
    /// Steps that ran.
    pub executed: usize,
    /// Steps that were skipped because they were pinned.
    pub skipped: usize,
    /// Wall-clock time for the whole run.
    pub elapsed: Duration,
}

/// A step that panicked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepFailure {
    pub label: String,
    pub message: String,
}

/// Why a run did not finish.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecuteError {
    /// One or more steps panicked. Steps already in flight were allowed to
    /// finish, so this can hold more than one failure.
    Failed(Vec<StepFailure>),
    /// The graph contains a dependency cycle; the listed steps could never
    /// become runnable.
    Cycle(Vec<String>),
}

impl fmt::Display for ExecuteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Failed(failures) => {
                write!(f, "{} step(s) failed:", failures.len())?;
                for failure in failures {
                    write!(f, "\n  {}: {}", failure.label, failure.message)?;
                }
                Ok(())
            }
            Self::Cycle(labels) => {
                write!(f, "dependency cycle among {} step(s):", labels.len())?;
                for label in labels {
                    write!(f, "\n  {label}")?;
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for ExecuteError {}

/// Execute `target` and everything it depends on, in parallel.
///
/// Panics if any step fails. Use [`ExecuteConfig::run`] to handle failures
/// yourself.
pub fn execute(target: impl Step + 'static) {
    if let Err(error) = ExecuteConfig::new().run(target) {
        panic!("{error}");
    }
}

fn default_concurrency() -> usize {
    if let Some(jobs) = std::env::var("RIVET_JOBS")
        .ok()
        .and_then(|j| j.parse::<usize>().ok())
    {
        return jobs.max(1);
    }
    thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
}

// ---------------------------------------------------------------------------
// Graph
// ---------------------------------------------------------------------------

struct Node {
    step: Arc<dyn Step>,
    label: String,
    pinned: bool,
    deps: Vec<usize>,
    dependents: Vec<usize>,
}

struct Graph {
    nodes: Vec<Node>,
}

impl Graph {
    /// Walk the graph reachable from `root`, interning each step by address.
    ///
    /// A pinned step is treated as a leaf: it is assumed to be up to date, so
    /// its dependencies are neither collected nor run. That matches the
    /// semantics of pinning elsewhere in rivet.
    fn flatten(root: Arc<dyn Step>) -> Self {
        struct Builder {
            nodes: Vec<Node>,
            index: HashMap<ByAddress<Arc<dyn Step>>, usize>,
        }

        impl Builder {
            fn intern(&mut self, step: Arc<dyn Step>) -> usize {
                let key = ByAddress(Arc::clone(&step));
                if let Some(&existing) = self.index.get(&key) {
                    return existing;
                }

                let pinned = step.pinned();
                let label = step.label();
                let index = self.nodes.len();
                self.index.insert(key, index);
                self.nodes.push(Node {
                    step: Arc::clone(&step),
                    label,
                    pinned,
                    deps: Vec::new(),
                    dependents: Vec::new(),
                });

                let deps = if pinned { Vec::new() } else { step.deps() };
                let mut resolved: Vec<usize> = Vec::with_capacity(deps.len());
                for dep in deps {
                    let dep_index = self.intern(dep);
                    // The same dependency listed twice must only be counted
                    // once, or the dependent would wait for a decrement that
                    // never comes.
                    if !resolved.contains(&dep_index) {
                        resolved.push(dep_index);
                    }
                }
                for &dep_index in &resolved {
                    self.nodes[dep_index].dependents.push(index);
                }
                self.nodes[index].deps = resolved;
                index
            }
        }

        let mut builder = Builder {
            nodes: Vec::new(),
            index: HashMap::new(),
        };
        builder.intern(root);
        Graph {
            nodes: builder.nodes,
        }
    }
}

// ---------------------------------------------------------------------------
// Scheduling
// ---------------------------------------------------------------------------

struct Shared {
    ready: VecDeque<usize>,
    unfinished_deps: Vec<usize>,
    in_flight: usize,
    remaining: usize,
    aborted: bool,
    stuck: bool,
    failures: Vec<StepFailure>,
}

enum Work {
    Run(usize),
    Wait,
    Done,
    /// Nothing is running and nothing is runnable: the rest of the graph is
    /// unreachable, which means there is a cycle.
    Stuck,
}

impl Shared {
    fn take_work(&mut self) -> Work {
        if self.remaining == 0 {
            return Work::Done;
        }
        if self.aborted {
            // Let in-flight steps finish, but start nothing new.
            return if self.in_flight == 0 {
                Work::Done
            } else {
                Work::Wait
            };
        }
        if let Some(index) = self.ready.pop_front() {
            self.in_flight += 1;
            return Work::Run(index);
        }
        if self.in_flight == 0 {
            return Work::Stuck;
        }
        Work::Wait
    }
}

fn run(config: &ExecuteConfig, root: Arc<dyn Step>) -> Result<Summary, ExecuteError> {
    let graph = Graph::flatten(root);
    let total = graph.nodes.len();

    let label_width = graph
        .nodes
        .iter()
        .map(|node| node.label.chars().count())
        .max()
        .unwrap_or(0);
    let reporter = Reporter::new(total, label_width, config.output, config.progress);
    progress::set_active_reporter(Some(Arc::clone(&reporter)));

    let unfinished_deps: Vec<usize> = graph.nodes.iter().map(|node| node.deps.len()).collect();
    let ready: VecDeque<usize> = (0..total).filter(|&i| unfinished_deps[i] == 0).collect();

    let shared = Mutex::new(Shared {
        ready,
        unfinished_deps,
        in_flight: 0,
        remaining: total,
        aborted: false,
        stuck: false,
        failures: Vec::new(),
    });
    let condvar = Condvar::new();

    let workers = config.concurrency.max(1).min(total.max(1));
    let started = Instant::now();

    let hook_guard = PanicHookGuard::install();
    thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| worker(&graph, &shared, &condvar, &reporter));
        }
    });
    drop(hook_guard);

    let elapsed = started.elapsed();
    reporter.finish_all(elapsed);
    progress::set_active_reporter(None);

    let shared = shared.into_inner().unwrap();
    if !shared.failures.is_empty() {
        return Err(ExecuteError::Failed(shared.failures));
    }
    if shared.stuck {
        let cycle = (0..total)
            .filter(|&i| shared.unfinished_deps[i] > 0)
            .map(|i| graph.nodes[i].label.clone())
            .collect();
        return Err(ExecuteError::Cycle(cycle));
    }

    let (finished, skipped, _) = reporter.counts();
    Ok(Summary {
        total,
        executed: finished.saturating_sub(skipped),
        skipped,
        elapsed,
    })
}

fn worker(graph: &Graph, shared: &Mutex<Shared>, condvar: &Condvar, reporter: &Arc<Reporter>) {
    IS_WORKER.with(|is_worker| is_worker.set(true));

    loop {
        let mut guard = shared.lock().unwrap();
        let index = loop {
            match guard.take_work() {
                Work::Run(index) => break index,
                Work::Wait => guard = condvar.wait(guard).unwrap(),
                Work::Done => {
                    // Wake the others so they observe the same state.
                    condvar.notify_all();
                    return;
                }
                Work::Stuck => {
                    guard.stuck = true;
                    guard.aborted = true;
                    condvar.notify_all();
                    return;
                }
            }
        };
        drop(guard);

        let node = &graph.nodes[index];
        let outcome = run_node(node, reporter);

        let mut guard = shared.lock().unwrap();
        guard.in_flight -= 1;
        guard.remaining -= 1;
        match outcome {
            Ok(()) => {
                for &dependent in &node.dependents {
                    guard.unfinished_deps[dependent] -= 1;
                    if guard.unfinished_deps[dependent] == 0 {
                        guard.ready.push_back(dependent);
                    }
                }
            }
            Err(failure) => {
                guard.failures.push(failure);
                guard.aborted = true;
            }
        }
        condvar.notify_all();
    }
}

fn run_node(node: &Node, reporter: &Arc<Reporter>) -> Result<(), StepFailure> {
    if node.pinned {
        reporter.skip(&node.label, "pinned");
        return Ok(());
    }

    let handle = reporter.start(&node.label);
    progress::set_current_step(Some(handle.clone()));
    take_panic_message();

    let result = panic::catch_unwind(AssertUnwindSafe(|| node.step.execute()));

    progress::set_current_step(None);

    match result {
        Ok(()) => {
            reporter.finish(&handle, Outcome::Completed, None);
            Ok(())
        }
        Err(payload) => {
            let message = take_panic_message().unwrap_or_else(|| payload_message(&*payload));
            reporter.finish(&handle, Outcome::Failed, Some(&message));
            Err(StepFailure {
                label: node.label.clone(),
                message,
            })
        }
    }
}

// ---------------------------------------------------------------------------
// Panic capture
// ---------------------------------------------------------------------------

thread_local! {
    static IS_WORKER: Cell<bool> = const { Cell::new(false) };
    static LAST_PANIC: RefCell<Option<String>> = const { RefCell::new(None) };
}

type Hook = Box<dyn Fn(&PanicHookInfo<'_>) + Sync + Send + 'static>;

/// Diverts panic messages from worker threads into [`LAST_PANIC`] so they can
/// be attributed to a step instead of being splattered across the display.
struct PanicHookGuard {
    previous: Arc<Hook>,
}

impl PanicHookGuard {
    fn install() -> Self {
        let previous: Arc<Hook> = Arc::new(panic::take_hook());
        let inner = Arc::clone(&previous);
        panic::set_hook(Box::new(move |info| {
            if IS_WORKER.with(|is_worker| is_worker.get()) {
                let mut message = payload_message(info.payload());
                if let Some(location) = info.location() {
                    message.push_str(&format!(" (at {}:{})", location.file(), location.line()));
                }
                LAST_PANIC.with(|last| *last.borrow_mut() = Some(message));
            } else {
                inner(info);
            }
        }));
        Self { previous }
    }
}

impl Drop for PanicHookGuard {
    fn drop(&mut self) {
        let previous = Arc::clone(&self.previous);
        panic::set_hook(Box::new(move |info| previous(info)));
    }
}

fn take_panic_message() -> Option<String> {
    LAST_PANIC.with(|last| last.borrow_mut().take())
}

fn payload_message(payload: &(dyn Any + Send)) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_string()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "step panicked".to_string()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

    struct TestStep {
        name: String,
        deps: Mutex<Vec<Arc<dyn Step>>>,
        pinned: bool,
        action: Box<dyn Fn() + Send + Sync>,
    }

    impl fmt::Debug for TestStep {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.debug_struct("TestStep")
                .field("name", &self.name)
                .finish()
        }
    }

    impl Step for TestStep {
        fn deps(&self) -> Vec<Arc<dyn Step>> {
            self.deps.lock().unwrap().clone()
        }

        fn pinned(&self) -> bool {
            self.pinned
        }

        fn execute(&self) {
            (self.action)();
        }

        fn label(&self) -> String {
            self.name.clone()
        }
    }

    fn step(name: &str, deps: Vec<Arc<dyn Step>>) -> Arc<dyn Step> {
        acting(name, deps, || {})
    }

    fn acting(
        name: &str,
        deps: Vec<Arc<dyn Step>>,
        action: impl Fn() + Send + Sync + 'static,
    ) -> Arc<dyn Step> {
        Arc::new(TestStep {
            name: name.to_string(),
            deps: Mutex::new(deps),
            pinned: false,
            action: Box::new(action),
        })
    }

    fn pinned(name: &str, deps: Vec<Arc<dyn Step>>) -> Arc<dyn Step> {
        Arc::new(TestStep {
            name: name.to_string(),
            deps: Mutex::new(deps),
            pinned: true,
            action: Box::new(|| panic!("a pinned step must not run")),
        })
    }

    fn config() -> ExecuteConfig {
        ExecuteConfig::new()
            .progress(false)
            .output(OutputMode::Quiet)
    }

    #[test]
    fn shared_dependency_runs_exactly_once() {
        let runs = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&runs);
        let par = acting("par", vec![], move || {
            counter.fetch_add(1, AtomicOrdering::SeqCst);
        });
        let drc = step("drc", vec![Arc::clone(&par)]);
        let lvs = step("lvs", vec![Arc::clone(&par)]);
        let signoff = step("signoff", vec![drc, lvs]);

        let summary = config().run_arc(signoff).unwrap();

        assert_eq!(runs.load(AtomicOrdering::SeqCst), 1);
        assert_eq!(summary.total, 4);
        assert_eq!(summary.executed, 4);
    }

    #[test]
    fn independent_branches_run_concurrently() {
        // Both checks block until the other has started. If the executor were
        // serial the first would time out and never record an overlap.
        let gate = Arc::new((Mutex::new(0usize), Condvar::new()));
        let overlaps = Arc::new(AtomicUsize::new(0));

        let par = step("par", vec![]);
        let mut checks: Vec<Arc<dyn Step>> = Vec::new();
        for name in ["drc", "lvs"] {
            let gate = Arc::clone(&gate);
            let overlaps = Arc::clone(&overlaps);
            checks.push(acting(name, vec![Arc::clone(&par)], move || {
                let (lock, condvar) = &*gate;
                let mut started = lock.lock().unwrap();
                *started += 1;
                condvar.notify_all();
                let (_started, timeout) = condvar
                    .wait_timeout_while(started, Duration::from_secs(10), |started| *started < 2)
                    .unwrap();
                if !timeout.timed_out() {
                    overlaps.fetch_add(1, AtomicOrdering::SeqCst);
                }
            }));
        }
        let signoff = step("signoff", checks);

        config().concurrency(4).run_arc(signoff).unwrap();

        assert_eq!(
            overlaps.load(AtomicOrdering::SeqCst),
            2,
            "drc and lvs did not overlap"
        );
    }

    #[test]
    fn dependencies_finish_before_dependents_start() {
        let order = Arc::new(Mutex::new(Vec::new()));
        let record = |order: &Arc<Mutex<Vec<&'static str>>>, name: &'static str| {
            let order = Arc::clone(order);
            move || order.lock().unwrap().push(name)
        };

        let syn = acting("syn", vec![], record(&order, "syn"));
        let par = acting("par", vec![Arc::clone(&syn)], record(&order, "par"));
        let drc = acting("drc", vec![Arc::clone(&par)], record(&order, "drc"));
        let lvs = acting("lvs", vec![Arc::clone(&par)], record(&order, "lvs"));
        let signoff = acting("signoff", vec![drc, lvs], record(&order, "signoff"));

        config().concurrency(4).run_arc(signoff).unwrap();

        let order = order.lock().unwrap().clone();
        let position = |name| order.iter().position(|n| *n == name).unwrap();
        assert!(position("syn") < position("par"));
        assert!(position("par") < position("drc"));
        assert!(position("par") < position("lvs"));
        assert!(position("drc") < position("signoff"));
        assert!(position("lvs") < position("signoff"));
    }

    #[test]
    fn pinned_steps_are_skipped_along_with_their_dependencies() {
        let never = acting("never", vec![], || panic!("upstream of a pin must not run"));
        let par = pinned("par", vec![never]);
        let drc = step("drc", vec![par]);

        let summary = config().run_arc(drc).unwrap();

        // `never` is not even part of the graph: pinning truncates the walk.
        assert_eq!(summary.total, 2);
        assert_eq!(summary.skipped, 1);
        assert_eq!(summary.executed, 1);
    }

    #[test]
    fn duplicate_dependency_edges_do_not_stall() {
        let par = step("par", vec![]);
        let drc = step("drc", vec![Arc::clone(&par), Arc::clone(&par)]);

        let summary = config().run_arc(drc).unwrap();

        assert_eq!(summary.total, 2);
        assert_eq!(summary.executed, 2);
    }

    #[test]
    fn cycles_are_reported_instead_of_hanging() {
        let a = Arc::new(TestStep {
            name: "a".into(),
            deps: Mutex::new(vec![]),
            pinned: false,
            action: Box::new(|| {}),
        });
        let b = Arc::new(TestStep {
            name: "b".into(),
            deps: Mutex::new(vec![Arc::clone(&a) as Arc<dyn Step>]),
            pinned: false,
            action: Box::new(|| {}),
        });
        // Close the loop: a now depends on b.
        a.deps.lock().unwrap().push(Arc::clone(&b) as Arc<dyn Step>);

        let error = config().run_arc(b).unwrap_err();

        match error {
            ExecuteError::Cycle(labels) => {
                assert!(labels.contains(&"a".to_string()), "got {labels:?}");
                assert!(labels.contains(&"b".to_string()), "got {labels:?}");
            }
            other => panic!("expected a cycle, got {other:?}"),
        }
    }

    #[test]
    fn a_failing_step_stops_its_dependents() {
        let ran_after = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&ran_after);

        let par = acting("par", vec![], || panic!("boom"));
        let drc = acting("drc", vec![par], move || {
            counter.fetch_add(1, AtomicOrdering::SeqCst);
        });

        let error = config().run_arc(drc).unwrap_err();

        match error {
            ExecuteError::Failed(failures) => {
                assert_eq!(failures.len(), 1);
                assert_eq!(failures[0].label, "par");
                assert!(
                    failures[0].message.contains("boom"),
                    "got {:?}",
                    failures[0]
                );
            }
            other => panic!("expected a failure, got {other:?}"),
        }
        assert_eq!(ran_after.load(AtomicOrdering::SeqCst), 0);
    }

    #[test]
    fn labels_default_to_the_step_type_name() {
        let step = TestStep {
            name: "x".into(),
            deps: Mutex::new(vec![]),
            pinned: false,
            action: Box::new(|| {}),
        };
        // `TestStep` overrides `label`, so check the default via a type that
        // does not.
        #[derive(Debug)]
        struct Plain;
        impl Step for Plain {
            fn deps(&self) -> Vec<Arc<dyn Step>> {
                vec![]
            }
            fn pinned(&self) -> bool {
                false
            }
            fn execute(&self) {}
        }
        assert_eq!(step.label(), "x");
        assert_eq!(Plain.label(), "Plain");
        let erased: Arc<dyn Step> = Arc::new(Plain);
        assert_eq!(erased.label(), "Plain");
    }
}
