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
//! Two separately constructed steps are two steps, however identical they look.
//!
//! A failure takes down the branch below it and nothing else. The steps that
//! depended on the failed one can never run, so they are dropped from the run;
//! every other branch keeps going, and steps that had not started yet are still
//! dispatched as long as they do not depend on anything that failed. The run
//! ends when the graph is exhausted, reporting every failure it collected.

use std::any::Any;
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::panic::{self, AssertUnwindSafe, PanicHookInfo};
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use by_address::ByThinAddress;

use crate::log;
use crate::progress::{self, Outcome, Reporter};
use crate::{Step, StepRef};

/// Settings for a run.
///
/// ```no_run
/// # use rivet::{ExecuteConfig, Step, StepRef};
/// # fn demo(target: StepRef<impl Step>) -> Result<(), rivet::ExecuteError> {
/// ExecuteConfig::new().concurrency(2).run(target)?;
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct ExecuteConfig {
    concurrency: usize,
    progress: bool,
    logging: bool,
    log_dir: PathBuf,
}

impl Default for ExecuteConfig {
    fn default() -> Self {
        Self {
            concurrency: default_concurrency(),
            progress: true,
            logging: true,
            log_dir: PathBuf::from("."),
        }
    }
}

impl ExecuteConfig {
    pub fn new() -> Self {
        Self::default()
    }

    /// Maximum number of steps to run at once. Values below 1 are treated as 1.
    ///
    /// Defaults to the number of available cores. Tools that hold licences or
    /// saturate a machine on their own are usually worth capping explicitly.
    pub fn concurrency(mut self, concurrency: usize) -> Self {
        self.concurrency = concurrency.max(1);
        self
    }

    /// Whether to draw the live progress display. Defaults to on.
    ///
    /// The display takes the screen for the whole run and is where the run is
    /// controlled from: steps under a cursor, their logs a key away, `q` to
    /// cancel. Turn it off for a run that should just report, one line per
    /// event, and leave the terminal alone — which is also what happens when
    /// stderr is not a terminal.
    pub fn progress(mut self, progress: bool) -> Self {
        self.progress = progress;
        self
    }

    /// Directory the run's log file, [`rivet.log`](crate::log::RUN_LOG), is
    /// written in. Defaults to the current directory.
    ///
    /// This is the whole run in one file. A step that says where it lives
    /// ([`Step::log_dir`]) also gets its own events next to it; see
    /// [`crate::log`].
    pub fn log_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.log_dir = dir.into();
        self
    }

    /// Whether to write log files at all. Defaults to on.
    ///
    /// Turning it off leaves the `tracing` subscriber installed but gives it
    /// nowhere to write, so a run leaves nothing on disk.
    pub fn logging(mut self, logging: bool) -> Self {
        self.logging = logging;
        self
    }

    /// Execute `target` and everything it depends on.
    pub fn run<T: Step>(&self, target: StepRef<T>) -> Result<Summary, ExecuteError> {
        self.run_dyn(target.into_dyn())
    }

    /// [`ExecuteConfig::run`] for an already-erased step.
    pub fn run_dyn(&self, target: StepRef<dyn Step>) -> Result<Summary, ExecuteError> {
        run(self, vec![target])
    }

    /// Execute several targets, and everything they depend on, as one graph.
    pub fn run_all(&self, targets: Vec<StepRef<dyn Step>>) -> Result<Summary, ExecuteError> {
        run(self, targets)
    }
}

/// Runs one or more target steps.
///
/// Targets are collected into a single graph, so work shared between them
/// happens once and independent branches of either still run concurrently.
///
/// ```no_run
/// # use rivet::{Executor, Step, StepRef};
/// # fn demo(drc: StepRef<impl Step>, lvs: StepRef<impl Step>) -> Result<(), rivet::ExecuteError> {
/// Executor::new().concurrency(2).target(drc).target(lvs).run()?;
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Default)]
#[must_use = "an Executor does nothing until `run` is called"]
pub struct Executor {
    config: ExecuteConfig,
    targets: Vec<StepRef<dyn Step>>,
}

impl Executor {
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace the settings for this run.
    pub fn config(mut self, config: ExecuteConfig) -> Self {
        self.config = config;
        self
    }

    /// See [`ExecuteConfig::concurrency`].
    pub fn concurrency(mut self, concurrency: usize) -> Self {
        self.config = self.config.concurrency(concurrency);
        self
    }

    /// See [`ExecuteConfig::progress`].
    pub fn progress(mut self, progress: bool) -> Self {
        self.config = self.config.progress(progress);
        self
    }

    /// See [`ExecuteConfig::log_dir`].
    pub fn log_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.config = self.config.log_dir(dir);
        self
    }

    /// See [`ExecuteConfig::logging`].
    pub fn logging(mut self, logging: bool) -> Self {
        self.config = self.config.logging(logging);
        self
    }

    /// Add a step to run. Nothing happens until [`Executor::run`] is called.
    pub fn target<T: Step>(self, step: StepRef<T>) -> Self {
        self.target_dyn(step.into_dyn())
    }

    /// [`Executor::target`] for an already-erased step.
    pub fn target_dyn(mut self, step: StepRef<dyn Step>) -> Self {
        self.targets.push(step);
        self
    }

    /// [`Executor::target`] for several steps at once, as a flow hands them
    /// out: `Executor::new().targets(block.signoff()).run()`.
    pub fn targets(mut self, steps: impl IntoIterator<Item = StepRef<dyn Step>>) -> Self {
        self.targets.extend(steps);
        self
    }

    /// Run every target that was added, and everything they depend on.
    pub fn run(self) -> Result<Summary, ExecuteError> {
        run(&self.config, self.targets)
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

/// A step that could never run because a step it depended on failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockedStep {
    pub label: String,
    /// The failed step that made this one unreachable. With more than one
    /// failure upstream, this is whichever one was reported first.
    pub blame: String,
}

impl fmt::Display for BlockedStep {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} (blocked by {})", self.label, self.blame)
    }
}

/// A step that did not succeed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepFailure {
    pub label: String,
    pub message: String,
    /// The step panicked instead of returning an error, which usually means a
    /// bug rather than an expected failure.
    pub panicked: bool,
    /// The status the step had set, if any.
    pub status: Option<String>,
    /// The substep parsed from its output, if any.
    pub substep: Option<String>,
}

impl fmt::Display for StepFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.label)?;
        let location: Vec<&str> = [self.status.as_deref(), self.substep.as_deref()]
            .into_iter()
            .flatten()
            .collect();
        if !location.is_empty() {
            write!(f, " (during {})", location.join(progress::REGION_SEP))?;
        }
        write!(f, ": ")?;
        if self.panicked {
            write!(f, "panicked: ")?;
        }
        write!(f, "{}", self.message)
    }
}

/// Why a run did not finish.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecuteError {
    /// One or more steps failed. The rest of the graph was run anyway, so
    /// this can hold more than one failure.
    Failed {
        failures: Vec<StepFailure>,
        /// Steps that never ran because something they depended on failed.
        blocked: Vec<BlockedStep>,
    },
    /// The graph contains a dependency cycle; the listed steps could never
    /// become runnable.
    Cycle(Vec<String>),
}

impl fmt::Display for ExecuteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Failed { failures, blocked } => {
                write!(f, "{} step(s) failed:", failures.len())?;
                for failure in failures {
                    write!(f, "\n  {failure}")?;
                }
                if !blocked.is_empty() {
                    let labels: Vec<&str> =
                        blocked.iter().map(|step| step.label.as_str()).collect();
                    write!(
                        f,
                        "\n{} step(s) never ran: {}",
                        blocked.len(),
                        labels.join(", ")
                    )?;
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
pub fn execute<T: Step>(target: StepRef<T>) {
    if let Err(error) = Executor::new().target(target).run() {
        panic!("{error}");
    }
}

fn default_concurrency() -> usize {
    thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
}

// ---------------------------------------------------------------------------
// Graph
// ---------------------------------------------------------------------------

struct Node {
    step: StepRef<dyn Step>,
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
    fn flatten(roots: Vec<StepRef<dyn Step>>) -> Self {
        struct Builder {
            nodes: Vec<Node>,
            // Keyed on the data address alone. `ByAddress` would compare the
            // whole fat pointer, vtable included, and Rust does not promise one
            // vtable per (type, trait) — two `into_dyn` calls in different
            // codegen units can produce two for the same step, which would
            // intern it twice and run it twice, concurrently, under one label.
            // See https://github.com/rust-lang/rust/issues/46139.
            index: HashMap<ByThinAddress<StepRef<dyn Step>>, usize>,
        }

        impl Builder {
            fn intern(&mut self, step: StepRef<dyn Step>) -> usize {
                let key = ByThinAddress(step.clone());
                if let Some(&existing) = self.index.get(&key) {
                    return existing;
                }

                // Read the shape of the step once, up front, so the lock is
                // not touched again until the step runs.
                let (pinned, label, deps) = {
                    let guard = step.read();
                    (guard.pinned(), guard.label(), guard.deps())
                };
                let index = self.nodes.len();
                self.index.insert(key, index);
                self.nodes.push(Node {
                    step: step.clone(),
                    label,
                    pinned,
                    deps: Vec::new(),
                    dependents: Vec::new(),
                });

                let deps = if pinned { Vec::new() } else { deps };
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
        for root in roots {
            builder.intern(root);
        }
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
    /// Steps taken out of the run because something upstream of them failed.
    blocked: Vec<bool>,
    stuck: bool,
    failures: Vec<StepFailure>,
    blocked_steps: Vec<BlockedStep>,
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
        if let Some(index) = self.ready.pop_front() {
            self.in_flight += 1;
            return Work::Run(index);
        }
        if self.in_flight == 0 {
            return Work::Stuck;
        }
        Work::Wait
    }

    /// Take everything below a failed step out of the run.
    ///
    /// Those steps can never become runnable — the dependency they are waiting
    /// on will never finish — so they are dropped here, which is also what
    /// keeps `remaining` honest and stops the scheduler mistaking them for a
    /// cycle. Nothing outside the failed step's dependents is touched: other
    /// branches keep running, and steps that have not started yet are still
    /// dispatched.
    ///
    /// Returns the steps dropped by this call, so they can be reported once
    /// the lock is free.
    fn block_dependents(&mut self, graph: &Graph, failed: usize) -> Vec<usize> {
        let mut dropped = Vec::new();
        let mut queue: VecDeque<usize> = graph.nodes[failed].dependents.iter().copied().collect();
        while let Some(index) = queue.pop_front() {
            // A step under two failures is only dropped once, and is blamed on
            // the failure that reached it first.
            if self.blocked[index] {
                continue;
            }
            // A dependent cannot have started: it only becomes ready once every
            // dependency has *succeeded*, so this never double-counts a step
            // that already finished or is in flight.
            self.blocked[index] = true;
            self.remaining -= 1;
            dropped.push(index);
            queue.extend(graph.nodes[index].dependents.iter().copied());
        }
        dropped
    }
}

fn run(config: &ExecuteConfig, roots: Vec<StepRef<dyn Step>>) -> Result<Summary, ExecuteError> {
    let graph = Graph::flatten(roots);
    let total = graph.nodes.len();

    // Before the reporter, so `rivet.log` opens with the run it describes.
    let _run_log = log::start_run(&config.log_dir, config.logging);

    // The display is told the whole plan up front, so that it can show what is
    // still to come as well as what is running. A step that is not run this
    // time still has a log to offer: the one the last run to run it left.
    let plan = graph
        .nodes
        .iter()
        .map(|node| progress::Planned {
            label: node.label.clone(),
            pinned: node.pinned,
            deps: node.deps.clone(),
            log: node
                .step
                .read()
                .log_dir()
                .map(|dir| log::step_log_path(&dir, &node.label)),
        })
        .collect();
    let workers = config.concurrency.max(1).min(total.max(1));
    let log_dir = config.logging.then(|| config.log_dir.clone());
    let reporter = Reporter::new(plan, workers, log_dir, config.progress);
    progress::set_active_reporter(Some(Arc::clone(&reporter)));

    let unfinished_deps: Vec<usize> = graph.nodes.iter().map(|node| node.deps.len()).collect();
    let ready: VecDeque<usize> = (0..total).filter(|&i| unfinished_deps[i] == 0).collect();

    let shared = Mutex::new(Shared {
        ready,
        unfinished_deps,
        in_flight: 0,
        remaining: total,
        blocked: vec![false; total],
        stuck: false,
        failures: Vec::new(),
        blocked_steps: Vec::new(),
    });
    let condvar = Condvar::new();

    let started = Instant::now();
    tracing::info!(steps = total, workers, "run started");

    let logging = config.logging;
    let hook_guard = PanicHookGuard::install();
    thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| worker(&graph, &shared, &condvar, &reporter, logging));
        }
    });
    drop(hook_guard);

    let elapsed = started.elapsed();
    reporter.finish_all(elapsed);
    progress::set_active_reporter(None);

    // Ahead of the error paths below, so the log records how every run ended
    // and not just the ones that succeeded.
    let counts = reporter.counts();
    tracing::info!(
        executed = counts.executed(),
        skipped = counts.skipped,
        blocked = counts.blocked,
        failed = counts.failed,
        ?elapsed,
        "run finished"
    );

    let shared = shared.into_inner().unwrap();
    if !shared.failures.is_empty() {
        return Err(ExecuteError::Failed {
            failures: shared.failures,
            blocked: shared.blocked_steps,
        });
    }
    if shared.stuck {
        // Steps dropped after a failure also sit at a non-zero dependency
        // count, so they are excluded here; without a failure there are none.
        let cycle = (0..total)
            .filter(|&i| shared.unfinished_deps[i] > 0 && !shared.blocked[i])
            .map(|i| graph.nodes[i].label.clone())
            .collect();
        return Err(ExecuteError::Cycle(cycle));
    }

    Ok(Summary {
        total,
        executed: counts.executed(),
        skipped: counts.skipped,
        elapsed,
    })
}

fn worker(
    graph: &Graph,
    shared: &Mutex<Shared>,
    condvar: &Condvar,
    reporter: &Arc<Reporter>,
    logging: bool,
) {
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
                    condvar.notify_all();
                    return;
                }
            }
        };
        drop(guard);

        let node = &graph.nodes[index];
        let outcome = run_node(index, node, reporter, logging);

        let mut guard = shared.lock().unwrap();
        guard.in_flight -= 1;
        guard.remaining -= 1;
        // Reported after the lock is dropped: `Reporter` draws, and drawing
        // under the scheduler lock would stall every other worker.
        let mut dropped = Vec::new();
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
                // The branch below this step is gone, but the run is not: every
                // other branch carries on and unstarted steps that do not
                // depend on this one are still dispatched.
                dropped = guard.block_dependents(graph, index);
                guard.failures.push(failure);
                guard
                    .blocked_steps
                    .extend(dropped.iter().map(|&i| BlockedStep {
                        label: graph.nodes[i].label.clone(),
                        blame: node.label.clone(),
                    }));
            }
        }
        condvar.notify_all();
        drop(guard);

        for index in dropped {
            let blocked = &graph.nodes[index];
            reporter.block(index, &node.label);
            tracing::warn!(step = %blocked.label, blame = %node.label, "blocked by a failed dependency");
        }
    }
}

fn run_node(
    index: usize,
    node: &Node,
    reporter: &Arc<Reporter>,
    logging: bool,
) -> Result<(), StepFailure> {
    if node.pinned {
        reporter.skip(index);
        tracing::info!(step = %node.label, "pinned, so not run");
        return Ok(());
    }

    // Where the step's own log file goes, asked for before it starts so the
    // read guard is not held while it runs.
    let log_dir = if logging {
        node.step.read().log_dir()
    } else {
        None
    };

    let handle = reporter.start(index, log::open_step_log(log_dir, &node.label));
    // Guards, because there are several ways out of this function: the step
    // stops being the current one, and its events stop being logged as its own,
    // whichever one is taken.
    let _current = progress::enter_step(handle.clone());
    let step_span = tracing::info_span!("step", name = %node.label);
    let _span = step_span.enter();
    tracing::info!("started");
    take_panic_message();

    let result = panic::catch_unwind(AssertUnwindSafe(|| node.step.read().execute()));

    let (message, panicked) = match result {
        Ok(Ok(())) => {
            let elapsed = handle.elapsed();
            reporter.finish(&handle, Outcome::Completed, None);
            tracing::info!(?elapsed, "completed");
            return Ok(());
        }
        Ok(Err(error)) => (error.to_string(), false),
        Err(payload) => (
            take_panic_message().unwrap_or_else(|| payload_message(&*payload)),
            true,
        ),
    };

    // Both halves of where it was, the same pair the display names: which of
    // them caused the failure is exactly what is not known here.
    let elapsed = handle.elapsed();
    match handle.location() {
        Some(location) => tracing::error!(panicked, ?elapsed, %location, "{message}"),
        None => tracing::error!(panicked, ?elapsed, "{message}"),
    }

    let detail = if panicked {
        format!("panicked: {message}")
    } else {
        message.clone()
    };
    reporter.finish(&handle, Outcome::Failed, Some(&detail));

    Err(StepFailure {
        label: node.label.clone(),
        message,
        panicked,
        status: handle.status().map(|status| status.describe()),
        substep: handle.substep().map(|substep| substep.describe()),
    })
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
    use crate::StepResult;
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

    struct TestStep {
        name: String,
        deps: Mutex<Vec<StepRef<dyn Step>>>,
        pinned: bool,
        action: Box<dyn Fn() -> StepResult + Send + Sync>,
    }

    impl fmt::Debug for TestStep {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.debug_struct("TestStep")
                .field("name", &self.name)
                .finish()
        }
    }

    impl Step for TestStep {
        fn deps(&self) -> Vec<StepRef<dyn Step>> {
            self.deps.lock().unwrap().clone()
        }

        fn pinned(&self) -> bool {
            self.pinned
        }

        fn set_pinned(&mut self, pinned: bool) {
            self.pinned = pinned;
        }

        fn execute(&self) -> StepResult {
            (self.action)()
        }

        fn label(&self) -> String {
            self.name.clone()
        }
    }

    fn step(name: &str, deps: Vec<StepRef<dyn Step>>) -> StepRef<dyn Step> {
        acting(name, deps, || Ok(()))
    }

    fn acting(
        name: &str,
        deps: Vec<StepRef<dyn Step>>,
        action: impl Fn() -> StepResult + Send + Sync + 'static,
    ) -> StepRef<dyn Step> {
        StepRef::new(TestStep {
            name: name.to_string(),
            deps: Mutex::new(deps),
            pinned: false,
            action: Box::new(action),
        })
        .into_dyn()
    }

    fn pinned(name: &str, deps: Vec<StepRef<dyn Step>>) -> StepRef<dyn Step> {
        StepRef::new(TestStep {
            name: name.to_string(),
            deps: Mutex::new(deps),
            pinned: true,
            action: Box::new(|| Err("a pinned step must not run".into())),
        })
        .into_dyn()
    }

    fn config() -> ExecuteConfig {
        // No log files: these tests care about scheduling, and would otherwise
        // drop a `rivet.log` into whatever directory `cargo test` ran in.
        ExecuteConfig::new().progress(false).logging(false)
    }

    #[test]
    fn shared_dependency_runs_exactly_once() {
        let runs = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&runs);
        let par = acting("par", vec![], move || {
            counter.fetch_add(1, AtomicOrdering::SeqCst);
            Ok(())
        });
        let drc = step("drc", vec![par.clone()]);
        let lvs = step("lvs", vec![par.clone()]);
        let signoff = step("signoff", vec![drc, lvs]);

        let summary = config().run_dyn(signoff).unwrap();

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
        let mut checks: Vec<StepRef<dyn Step>> = Vec::new();
        for name in ["drc", "lvs"] {
            let gate = Arc::clone(&gate);
            let overlaps = Arc::clone(&overlaps);
            checks.push(acting(name, vec![par.clone()], move || {
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
                Ok(())
            }));
        }
        let signoff = step("signoff", checks);

        config().concurrency(4).run_dyn(signoff).unwrap();

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
            move || {
                order.lock().unwrap().push(name);
                Ok(())
            }
        };

        let syn = acting("syn", vec![], record(&order, "syn"));
        let par = acting("par", vec![syn.clone()], record(&order, "par"));
        let drc = acting("drc", vec![par.clone()], record(&order, "drc"));
        let lvs = acting("lvs", vec![par.clone()], record(&order, "lvs"));
        let signoff = acting("signoff", vec![drc, lvs], record(&order, "signoff"));

        config().concurrency(4).run_dyn(signoff).unwrap();

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
        let never = acting("never", vec![], || {
            Err("upstream of a pin must not run".into())
        });
        let par = pinned("par", vec![never]);
        let drc = step("drc", vec![par]);

        let summary = config().run_dyn(drc).unwrap();

        // `never` is not even part of the graph: pinning truncates the walk.
        assert_eq!(summary.total, 2);
        assert_eq!(summary.skipped, 1);
        assert_eq!(summary.executed, 1);
    }

    #[test]
    fn duplicate_dependency_edges_do_not_stall() {
        let par = step("par", vec![]);
        let drc = step("drc", vec![par.clone(), par.clone()]);

        let summary = config().run_dyn(drc).unwrap();

        assert_eq!(summary.total, 2);
        assert_eq!(summary.executed, 2);
    }

    #[test]
    fn cycles_are_reported_instead_of_hanging() {
        let a = StepRef::new(TestStep {
            name: "a".into(),
            deps: Mutex::new(vec![]),
            pinned: false,
            action: Box::new(|| Ok(())),
        });
        let b = StepRef::new(TestStep {
            name: "b".into(),
            deps: Mutex::new(vec![a.clone().into_dyn()]),
            pinned: false,
            action: Box::new(|| Ok(())),
        });
        // Close the loop: a now depends on b.
        a.read().deps.lock().unwrap().push(b.clone().into_dyn());

        let error = config().run(b).unwrap_err();

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

        let par = acting("par", vec![], || {
            Err("LVS mismatch: 3 unmatched nets".into())
        });
        let drc = acting("drc", vec![par], move || {
            counter.fetch_add(1, AtomicOrdering::SeqCst);
            Ok(())
        });

        let error = config().run_dyn(drc).unwrap_err();

        match error {
            ExecuteError::Failed { failures, blocked } => {
                assert_eq!(failures.len(), 1);
                assert_eq!(failures[0].label, "par");
                assert_eq!(failures[0].message, "LVS mismatch: 3 unmatched nets");
                assert!(!failures[0].panicked, "an Err is not a panic");
                assert_eq!(blocked.len(), 1);
                assert_eq!(blocked[0].label, "drc");
                assert_eq!(blocked[0].blame, "par");
            }
            other => panic!("expected a failure, got {other:?}"),
        }
        assert_eq!(ran_after.load(AtomicOrdering::SeqCst), 0);
    }

    #[test]
    fn everything_below_a_failure_is_dropped_transitively() {
        let par = acting("par", vec![], || Err("router gave up".into()));
        let drc = acting("drc", vec![par.clone()], || {
            Err("a step under a failure must not run".into())
        });
        let signoff = acting("signoff", vec![drc], || {
            Err("a step under a failure must not run".into())
        });

        let error = config().run_dyn(signoff).unwrap_err();

        match error {
            ExecuteError::Failed { failures, blocked } => {
                assert_eq!(failures.len(), 1, "only `par` ran, so only `par` failed");
                let labels: Vec<&str> = blocked.iter().map(|s| s.label.as_str()).collect();
                assert_eq!(labels, ["drc", "signoff"]);
                // Both are blamed on the failure itself, not on the step
                // immediately above them.
                assert!(blocked.iter().all(|s| s.blame == "par"), "got {blocked:?}");
            }
            other => panic!("expected a failure, got {other:?}"),
        }
    }

    #[test]
    fn a_step_under_two_failures_is_reported_once() {
        let drc = acting("drc", vec![], || Err("drc failed".into()));
        let lvs = acting("lvs", vec![], || Err("lvs failed".into()));
        let signoff = acting("signoff", vec![drc, lvs], || {
            Err("a step under a failure must not run".into())
        });

        let error = config().concurrency(2).run_dyn(signoff).unwrap_err();

        match error {
            ExecuteError::Failed { failures, blocked } => {
                assert_eq!(failures.len(), 2);
                assert_eq!(blocked.len(), 1, "got {blocked:?}");
                assert_eq!(blocked[0].label, "signoff");
            }
            other => panic!("expected a failure, got {other:?}"),
        }
    }

    #[test]
    fn unrelated_steps_still_start_after_a_failure() {
        // A failure takes down its own dependents and nothing else: steps
        // already running finish, and steps that had not started yet are still
        // dispatched as long as they do not depend on what failed.
        let gate = Arc::new((Mutex::new(0usize), Condvar::new()));
        let started_late = Arc::new(AtomicUsize::new(0));

        // Both fail, but only once each has seen the other start, so the second
        // failure is guaranteed to arrive after the first has been reported.
        let mut roots: Vec<StepRef<dyn Step>> = Vec::new();
        for name in ["lvs", "drc"] {
            let gate = Arc::clone(&gate);
            roots.push(acting(name, vec![], move || {
                let (lock, condvar) = &*gate;
                let mut started = lock.lock().unwrap();
                *started += 1;
                condvar.notify_all();
                while *started < 2 {
                    started = condvar.wait(started).unwrap();
                }
                Err("did not match".into())
            }));
        }
        let counter = Arc::clone(&started_late);
        roots.push(acting("signoff", vec![], move || {
            counter.fetch_add(1, AtomicOrdering::SeqCst);
            Ok(())
        }));

        // Two slots for three roots: `signoff` can only start once one of the
        // failing steps has freed a worker, so it is dispatched strictly after
        // the first failure was reported.
        let error = config().concurrency(2).run_all(roots).unwrap_err();

        match error {
            ExecuteError::Failed { failures, blocked } => {
                assert_eq!(failures.len(), 2, "both in-flight steps should report");
                assert!(blocked.is_empty(), "nothing depended on either failure");
            }
            other => panic!("expected two failures, got {other:?}"),
        }
        assert_eq!(
            started_late.load(AtomicOrdering::SeqCst),
            1,
            "an independent step must still start after a failure"
        );
    }

    #[test]
    fn a_panicking_step_is_caught_and_flagged() {
        let par = acting("par", vec![], || panic!("index out of bounds"));

        let error = config().run_dyn(par).unwrap_err();

        match error {
            ExecuteError::Failed { failures, .. } => {
                assert_eq!(failures.len(), 1);
                assert!(failures[0].panicked, "a panic should be flagged as one");
                assert!(
                    failures[0].message.contains("index out of bounds"),
                    "got {:?}",
                    failures[0]
                );
            }
            other => panic!("expected a failure, got {other:?}"),
        }
    }

    #[test]
    fn a_failure_reports_the_substep_it_died_in() {
        let par = acting("par", vec![], || {
            // As a tool would report it: a banner in the output stream.
            progress::log_line(progress::banner(3, 5, "place_opt_design"));
            Err("router gave up".into())
        });

        let error = config().run_dyn(par).unwrap_err();

        match error {
            ExecuteError::Failed { failures, .. } => {
                assert_eq!(failures[0].status, None);
                assert_eq!(
                    failures[0].substep.as_deref(),
                    Some("place_opt_design (3/5)")
                );
                assert_eq!(
                    failures[0].to_string(),
                    "par (during place_opt_design (3/5)): router gave up"
                );
            }
            other => panic!("expected a failure, got {other:?}"),
        }
    }

    #[test]
    fn a_failure_reports_both_halves_when_both_are_known() {
        let par = acting("par", vec![], || {
            progress::status_progress(7, 12, "merging gds");
            progress::log_line(progress::banner(3, 5, "place_opt_design"));
            Err("router gave up".into())
        });

        let error = config().run_dyn(par).unwrap_err();

        match error {
            ExecuteError::Failed { failures, .. } => {
                // Which half caused it is exactly what is not known, so both
                // are kept.
                assert_eq!(failures[0].status.as_deref(), Some("merging gds (7/12)"));
                assert_eq!(
                    failures[0].substep.as_deref(),
                    Some("place_opt_design (3/5)")
                );
                assert_eq!(
                    failures[0].to_string(),
                    "par (during merging gds (7/12) │ place_opt_design (3/5)): router gave up"
                );
            }
            other => panic!("expected a failure, got {other:?}"),
        }
    }

    #[test]
    fn a_clean_tool_run_stops_being_the_failure_location() {
        let par = acting("par", vec![], || {
            // A tool reports substeps, exits cleanly, and the step then fails
            // doing its own work. The finished substep must not be blamed.
            progress::log_line(progress::banner(5, 5, "add_fillers"));
            progress::clear_substep();
            progress::status("checking gds");
            Err("gds is missing a top cell".into())
        });

        let error = config().run_dyn(par).unwrap_err();

        match error {
            ExecuteError::Failed { failures, .. } => {
                assert_eq!(failures[0].substep, None);
                assert_eq!(failures[0].status.as_deref(), Some("checking gds"));
                assert_eq!(
                    failures[0].to_string(),
                    "par (during checking gds): gds is missing a top cell"
                );
            }
            other => panic!("expected a failure, got {other:?}"),
        }
    }

    #[test]
    fn labels_default_to_the_step_type_name() {
        let step = TestStep {
            name: "x".into(),
            deps: Mutex::new(vec![]),
            pinned: false,
            action: Box::new(|| Ok(())),
        };
        // `TestStep` overrides `label`, so check the default via a type that
        // does not.
        #[derive(Debug)]
        struct Plain;
        impl Step for Plain {
            fn deps(&self) -> Vec<StepRef<dyn Step>> {
                vec![]
            }
            fn pinned(&self) -> bool {
                false
            }
            fn set_pinned(&mut self, _pinned: bool) {
                unreachable!("nothing pins a Plain")
            }
            fn execute(&self) -> StepResult {
                Ok(())
            }
        }
        assert_eq!(step.label(), "x");
        assert_eq!(Plain.label(), "Plain");
        let erased: StepRef<dyn Step> = StepRef::new(Plain).into_dyn();
        assert_eq!(erased.read().label(), "Plain");
    }
}
