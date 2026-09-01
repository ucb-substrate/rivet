//! Structured logging for flow code, written to files and never to the
//! terminal.
//!
//! Rivet's display owns stderr while a flow runs, so a log line printed to a
//! stream would corrupt it (see [`crate::progress`]). Logging therefore goes to
//! two files, and an event reaches both:
//!
//! * **`rivet.log`**, in the run's log directory
//!   ([`ExecuteConfig::log_dir`](crate::ExecuteConfig::log_dir), the current
//!   directory by default) — every event from the whole run, in order, from
//!   every step and from rivet itself.
//! * **`{step}.rivet.log`**, in the step's own directory
//!   ([`Step::log_dir`](crate::Step::log_dir)) — only what was logged while
//!   that step was running, sitting next to the `.out` and `.err` files of the
//!   tools it drove.
//!
//! A step that does not say where it lives logs only to `rivet.log`, as does
//! anything logged outside a step.
//!
//! # Using it
//!
//! There is nothing to set up: the executor installs the subscriber and points
//! it at the right files for the duration of a run. Flow code just uses the
//! `tracing` macros.
//!
//! ```
//! # let unmatched = 3;
//! tracing::info!(unmatched, "LVS did not match");
//! ```
//!
//! Events are tagged with the step that emitted them, so `rivet.log` reads as
//! one narrative of a run even with several steps in flight:
//!
//! ```text
//! 2026-08-31T18:02:11.401Z  INFO rivet::executor: step{name="decoder par"}: started
//! 2026-08-31T18:02:11.402Z  INFO rivet::exec: step{name="decoder par"}: running command="innovus" "-files" "par.tcl" stdout="build/decoder/par/decoder.par.out" stderr="build/decoder/par/decoder.par.err"
//! 2026-08-31T18:06:12.884Z  INFO rivet::exec: step{name="decoder par"}: exited code=0 success=true
//! 2026-08-31T18:06:13.002Z  INFO rivet::executor: step{name="decoder par"}: completed elapsed=4m1.6s
//! ```
//!
//! # What is logged
//!
//! Set [`FILTER_ENV`] to change it, using the usual `EnvFilter` syntax:
//! `RIVET_LOG=debug`, or `RIVET_LOG=rivet=info,cadence=debug` to turn one
//! plugin up. It defaults to `info`.
//!
//! # This is not where tool output goes
//!
//! Rivet keeps three channels apart, and this is the third:
//!
//! | | goes to | written by |
//! |---|---|---|
//! | raw tool output | `{basename}.out` / `.err` | the tool, captured by [`crate::exec`] |
//! | the live display | stderr | [`crate::progress::status`] and substep banners |
//! | the run's own record | `rivet.log`, `{step}.rivet.log` | `tracing`, this module |
//!
//! A tool's chatter stays in its own files: it is far too much of it to be
//! worth folding into a log that is meant to stay readable. What rivet records
//! here is which command a step ran, where that output went, and how it ended.
//!
//! # Threads
//!
//! The step a log line belongs to is tracked per thread, the same way
//! [`crate::progress::current_step`] is. A thread a step spawns for itself does
//! not inherit it, so its events reach `rivet.log` but not the step's own file.

use std::cell::RefCell;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex, MutexGuard};

use tracing::Subscriber;
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer};

/// The run-wide log file, written in the directory set by
/// [`ExecuteConfig::log_dir`](crate::ExecuteConfig::log_dir).
pub const RUN_LOG: &str = "rivet.log";

/// Suffix on a step's own log file, `{label}.rivet.log`.
pub const STEP_LOG_SUFFIX: &str = ".rivet.log";

/// Environment variable setting what is logged, in `EnvFilter` syntax.
pub const FILTER_ENV: &str = "RIVET_LOG";

/// What is logged when [`FILTER_ENV`] is not set.
const DEFAULT_FILTER: &str = "info";

// ---------------------------------------------------------------------------
// Sinks
// ---------------------------------------------------------------------------

/// A log file opened the first time something is written to it.
///
/// Opening lazily is what keeps the filesystem honest: a step that logs nothing
/// leaves no file behind, and a run whose subscriber never got installed does
/// not drop an empty `rivet.log` into someone's directory.
struct LazyFile {
    path: PathBuf,
    /// Keep what is already in the file rather than truncating it.
    append: bool,
    file: Option<File>,
    /// Set once opening or writing has failed, so a broken sink is not retried
    /// on every line.
    failed: bool,
}

impl LazyFile {
    /// A file that keeps what earlier runs wrote.
    fn appending(path: PathBuf) -> Self {
        Self {
            path,
            append: true,
            file: None,
            failed: false,
        }
    }

    /// A file that starts empty each time.
    fn truncating(path: PathBuf) -> Self {
        Self {
            path,
            append: false,
            file: None,
            failed: false,
        }
    }

    fn write(&mut self, bytes: &[u8]) {
        if self.failed {
            return;
        }
        if self.file.is_none() {
            match self.open() {
                Ok(file) => self.file = Some(file),
                // There is nowhere to report this. Printing it is the one thing
                // this module exists to avoid, and failing a step because its
                // log file could not be opened would be worse than losing the
                // log.
                Err(_) => {
                    self.failed = true;
                    return;
                }
            }
        }
        let file = self.file.as_mut().expect("just opened");
        if file.write_all(bytes).is_err() {
            self.failed = true;
        }
    }

    fn open(&self) -> io::Result<File> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        if !self.append {
            return File::create(&self.path);
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        // A blank line before a run appends to an existing log, so the file
        // reads as a stack of runs rather than one undifferentiated stream.
        if file.metadata().map(|meta| meta.len() > 0).unwrap_or(false) {
            let _ = file.write_all(b"\n");
        }
        Ok(file)
    }
}

/// `rivet.log` for the run in progress, if there is one.
static RUN: LazyLock<Mutex<Option<LazyFile>>> = LazyLock::new(|| Mutex::new(None));

thread_local! {
    /// The log file of the step running on this thread, if any.
    static STEP: RefCell<Option<LazyFile>> = const { RefCell::new(None) };
}

/// A panic while writing a log line should not disable logging for the rest of
/// the run, so a poisoned lock is taken anyway.
fn run_sink() -> MutexGuard<'static, Option<LazyFile>> {
    RUN.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

// ---------------------------------------------------------------------------
// The writer
// ---------------------------------------------------------------------------

/// Hands each formatted event to the files it belongs in.
struct Sink;

impl<'a> MakeWriter<'a> for Sink {
    type Writer = Record;

    fn make_writer(&'a self) -> Record {
        Record { buf: Vec::new() }
    }
}

/// One event, buffered until it is complete.
///
/// The formatter writes an event in several pieces. Buffering them and writing
/// the whole line in one call per file is what keeps steps running at the same
/// time from interleaving mid-line.
struct Record {
    buf: Vec<u8>,
}

impl Write for Record {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.buf.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for Record {
    /// The formatter builds a writer per event and drops it once the line is
    /// written, which is what makes this the point where the line is complete.
    fn drop(&mut self) {
        if self.buf.is_empty() {
            return;
        }
        if let Some(run) = run_sink().as_mut() {
            run.write(&self.buf);
        }
        // `try_with` and `try_borrow_mut`: writing a log line must not panic,
        // whatever else the thread is in the middle of.
        let _ = STEP.try_with(|step| {
            if let Ok(mut step) = step.try_borrow_mut() {
                if let Some(step) = step.as_mut() {
                    step.write(&self.buf);
                }
            }
        });
    }
}

// ---------------------------------------------------------------------------
// Installation
// ---------------------------------------------------------------------------

fn filter() -> EnvFilter {
    EnvFilter::try_from_env(FILTER_ENV).unwrap_or_else(|_| EnvFilter::new(DEFAULT_FILTER))
}

/// Rivet's logging as a `tracing` layer, filtered by [`FILTER_ENV`].
///
/// The executor installs this itself. Reach for it only to add rivet's log
/// files to a subscriber an application builds for its own reasons — whatever
/// else that subscriber does, it must not write to the terminal while a flow is
/// running.
///
/// ```no_run
/// use tracing_subscriber::prelude::*;
///
/// tracing_subscriber::registry()
///     .with(rivet::log::layer())
///     .init();
/// ```
pub fn layer<S>() -> impl Layer<S>
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    tracing_subscriber::fmt::layer()
        .with_writer(Sink)
        // These are files, not a terminal.
        .with_ansi(false)
        .with_filter(filter())
}

/// Install rivet's logging as the process-wide subscriber.
///
/// Returns `false` if a subscriber was already installed — by an earlier run,
/// or by an application that built its own — in which case this leaves it
/// alone. An application that wants both should use [`layer`].
pub fn install() -> bool {
    tracing_subscriber::registry()
        .with(layer())
        .try_init()
        .is_ok()
}

// ---------------------------------------------------------------------------
// Routing
// ---------------------------------------------------------------------------

/// Points `rivet.log` at `dir` until the returned guard is dropped.
///
/// The sink is process-wide, like the subscriber it feeds: runs in the same
/// process share it rather than nesting.
pub(crate) fn start_run(dir: &Path, enabled: bool) -> RunLog {
    if !enabled {
        return RunLog { active: false };
    }
    install();
    *run_sink() = Some(LazyFile::appending(dir.join(RUN_LOG)));
    RunLog { active: true }
}

pub(crate) struct RunLog {
    active: bool,
}

impl Drop for RunLog {
    fn drop(&mut self) {
        if self.active {
            *run_sink() = None;
        }
    }
}

/// Also send everything logged on this thread to `dir/{label}.rivet.log`, until
/// the returned guard is dropped.
///
/// With no directory the step logs only to `rivet.log`.
pub(crate) fn start_step(dir: Option<PathBuf>, label: &str) -> StepLog {
    let sink = dir.map(|dir| LazyFile::truncating(dir.join(step_log_name(label))));
    let _ = STEP.try_with(|step| *step.borrow_mut() = sink);
    StepLog
}

pub(crate) struct StepLog;

impl Drop for StepLog {
    fn drop(&mut self) {
        let _ = STEP.try_with(|step| *step.borrow_mut() = None);
    }
}

/// What a step's log file is called.
///
/// A label is a display string, not a file name: it usually has a space in it
/// and nothing stops it having a separator. Anything that would send the file
/// somewhere other than the step's own directory is replaced.
fn step_log_name(label: &str) -> String {
    let cleaned: String = label
        .chars()
        .map(|c| {
            if std::path::is_separator(c) || c.is_control() {
                '_'
            } else {
                c
            }
        })
        .collect();
    let name = cleaned.trim();
    let name = if name.is_empty() { "step" } else { name };
    format!("{name}{STEP_LOG_SUFFIX}")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Step, StepRef, StepResult};
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// The run sink is process-wide, so these take turns. They still assert on
    /// what a file *contains* rather than on all of it: a flow running in
    /// another test at the same time logs into whatever sink is active.
    static SERIAL: Mutex<()> = Mutex::new(());

    fn temp_dir(name: &str) -> PathBuf {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("rivet-log-{name}-{}-{unique}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    /// A step that logs one message and knows where it lives.
    #[derive(Debug)]
    struct Talker {
        label: String,
        work_dir: PathBuf,
        message: String,
    }

    impl Talker {
        fn build(label: &str, work_dir: &Path, message: &str) -> StepRef<Self> {
            StepRef::new(Self {
                label: label.to_string(),
                work_dir: work_dir.to_path_buf(),
                message: message.to_string(),
            })
        }
    }

    impl Step for Talker {
        fn execute(&self) -> StepResult {
            tracing::info!("{}", self.message);
            Ok(())
        }

        fn deps(&self) -> Vec<StepRef<dyn Step>> {
            Vec::new()
        }

        fn pinned(&self) -> bool {
            false
        }

        fn label(&self) -> String {
            self.label.clone()
        }

        fn log_dir(&self) -> Option<PathBuf> {
            Some(self.work_dir.clone())
        }
    }

    #[test]
    fn an_event_reaches_both_the_run_log_and_the_step_log() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let dir = temp_dir("both");

        crate::Executor::new()
            .progress(false)
            .log_dir(&dir)
            .target(Talker::build("talker", &dir, "hello from the step"))
            .run()
            .expect("run");

        let run = fs::read_to_string(dir.join(RUN_LOG)).expect("rivet.log");
        assert!(run.contains("hello from the step"), "{run}");
        assert!(run.contains("run started"), "{run}");

        let step = fs::read_to_string(dir.join("talker.rivet.log")).expect("talker.rivet.log");
        assert!(step.contains("hello from the step"), "{step}");
    }

    #[test]
    fn a_step_log_holds_only_that_step() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let dir = temp_dir("apart");
        let one = dir.join("one");
        let two = dir.join("two");

        crate::Executor::new()
            .progress(false)
            .concurrency(1)
            .log_dir(&dir)
            .target(Talker::build("one", &one, "only in one"))
            .target(Talker::build("two", &two, "only in two"))
            .run()
            .expect("run");

        let first = fs::read_to_string(one.join("one.rivet.log")).expect("one.rivet.log");
        assert!(first.contains("only in one"), "{first}");
        assert!(!first.contains("only in two"), "{first}");

        let second = fs::read_to_string(two.join("two.rivet.log")).expect("two.rivet.log");
        assert!(second.contains("only in two"), "{second}");
        assert!(!second.contains("only in one"), "{second}");
    }

    #[test]
    fn logging_can_be_turned_off() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let dir = temp_dir("off");

        crate::Executor::new()
            .progress(false)
            .logging(false)
            .log_dir(&dir)
            .target(Talker::build("quiet", &dir, "should not be written"))
            .run()
            .expect("run");

        assert!(!dir.join(RUN_LOG).exists(), "rivet.log should not exist");
        assert!(
            !dir.join("quiet.rivet.log").exists(),
            "step log should not exist"
        );
    }

    #[test]
    fn a_label_becomes_a_file_name_in_the_step_directory() {
        assert_eq!(step_log_name("decoder par"), "decoder par.rivet.log");
        assert_eq!(step_log_name("a/b"), "a_b.rivet.log");
        assert_eq!(step_log_name(""), "step.rivet.log");
    }
}
