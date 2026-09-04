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
//! 2026-08-31T18:02:11.401942Z  INFO step{name=decoder par}: rivet::executor: started
//! 2026-08-31T18:02:11.402813Z  INFO step{name=decoder par}: rivet::exec: running command="innovus" "-files" "par.tcl" stdout=build/decoder/par/decoder.par.out stderr=build/decoder/par/decoder.par.err
//! 2026-08-31T18:06:12.884406Z  INFO step{name=decoder par}: rivet::exec: exited code=0 success=true
//! 2026-08-31T18:06:13.002115Z  INFO step{name=decoder par}: rivet::executor: completed elapsed=4m1.6s
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
//! # Which step a line belongs to
//!
//! The step is the one whose line is on screen: this module asks
//! [`crate::progress::current_step`] rather than tracking a second copy of the
//! same fact, and a step's log file hangs off the same handle its progress line
//! does.
//!
//! It cannot come from the `tracing` span instead. The formatter picks its
//! writer through `MakeWriter`, which is handed the event's `Metadata` and
//! never the span context, so routing per step through spans would mean writing
//! an event formatter as well — `FmtContext` is not constructible outside
//! `tracing-subscriber`. The consequence is that a thread a step spawns for
//! itself is not that step: its events reach `rivet.log` but not the step's own
//! file, even if it carries the span.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, RwLock};

use tracing::Subscriber;
use tracing_appender::rolling::{RollingFileAppender, Rotation};
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

/// A file rivet writes events to.
///
/// The file handling is `tracing-appender`'s: it creates the directory, opens
/// for append, and synchronises writes internally so a sink can be shared. Only
/// [`Rotation::NEVER`] is used — these files are named for the run or the step
/// they describe, so there is nothing to roll over, and rotation is the one part
/// of that crate that reports errors by printing them, which is the single thing
/// this module must never do.
pub(crate) struct LogFile {
    appender: RollingFileAppender,
    /// Where it was opened, so the display can offer a command to read it.
    path: PathBuf,
}

impl LogFile {
    /// Opens `dir/name`, or gives up.
    ///
    /// There is nowhere to report a failure to: printing it is what this module
    /// exists to avoid, and failing a step because its log file would not open
    /// would be worse than losing the log. `build` is used rather than
    /// [`tracing_appender::rolling::never`] for the same reason — that one
    /// panics.
    fn open(dir: &Path, name: &str) -> Option<Self> {
        RollingFileAppender::builder()
            .rotation(Rotation::NEVER)
            .filename_prefix(name)
            .build(dir)
            .ok()
            .map(|appender| Self {
                appender,
                path: dir.join(name),
            })
    }

    /// The file being written. With [`Rotation::NEVER`] it does not change.
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// Writes one whole event.
    ///
    /// A single `write_all` per event into a file opened for append is what
    /// keeps steps running at the same time from interleaving mid-line.
    fn write(&self, bytes: &[u8]) {
        let _ = self.appender.make_writer().write_all(bytes);
    }
}

/// `rivet.log` for the run in progress, if there is one.
static RUN: LazyLock<RwLock<Option<Arc<LogFile>>>> = LazyLock::new(|| RwLock::new(None));

/// A panic while writing a log line should not disable logging for the rest of
/// the run, so a poisoned lock is read anyway.
fn run_log() -> Option<Arc<LogFile>> {
    RUN.read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

// ---------------------------------------------------------------------------
// The writer
// ---------------------------------------------------------------------------

/// Hands each formatted event to the files it belongs in.
struct Fanout;

impl<'a> MakeWriter<'a> for Fanout {
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
        if let Some(run) = run_log() {
            run.write(&self.buf);
        }
        if let Some(step) = crate::progress::current_step_log() {
            step.write(&self.buf);
        }
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
        .with_writer(Fanout)
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

    // Asked before opening, because opening creates the file: this is the last
    // moment at which an earlier run's log can be told apart from an empty one.
    let appended_to = fs::metadata(dir.join(RUN_LOG))
        .map(|meta| meta.len() > 0)
        .unwrap_or(false);
    let file = LogFile::open(dir, RUN_LOG);
    if let Some(file) = &file {
        if appended_to {
            // A blank line between runs, so the log reads as a stack of runs
            // rather than one undifferentiated stream.
            file.write(b"\n");
        }
    }

    let mut run = RUN.write().unwrap_or_else(|poisoned| poisoned.into_inner());
    *run = file.map(Arc::new);
    RunLog { active: true }
}

pub(crate) struct RunLog {
    active: bool,
}

impl Drop for RunLog {
    fn drop(&mut self) {
        if self.active {
            let mut run = RUN.write().unwrap_or_else(|poisoned| poisoned.into_inner());
            *run = None;
        }
    }
}

/// Opens the log file for a step working in `dir`.
///
/// The executor hangs the result on the step's handle, which is what makes it
/// the file events land in while that step runs. A step with no directory of
/// its own reaches `rivet.log` and nothing else.
pub(crate) fn open_step_log(dir: Option<PathBuf>, label: &str) -> Option<Arc<LogFile>> {
    let dir = dir?;
    let name = step_log_name(label);
    // The appender only appends, and this file describes one run of the step:
    // the same rule as the `.out` and `.err` files it sits beside.
    let _ = fs::remove_file(dir.join(&name));
    LogFile::open(&dir, &name).map(Arc::new)
}

/// Where the log file for a step working in `dir` is, whether or not this run
/// writes it.
///
/// For a step that is not run this time — pinned, or blocked by a failure —
/// this is where the last run that did run it left its log, which is the only
/// log there is to offer for it.
pub(crate) fn step_log_path(dir: &Path, label: &str) -> PathBuf {
    dir.join(step_log_name(label))
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
    use std::sync::Mutex;

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

        fn set_pinned(&mut self, _pinned: bool) {
            unreachable!("nothing pins a Talker")
        }

        fn label(&self) -> String {
            self.label.clone()
        }

        fn log_dir(&self) -> Option<PathBuf> {
            Some(self.work_dir.clone())
        }
    }

    /// A step that says something both ways round.
    #[derive(Debug)]
    struct Speaker {
        label: String,
        work_dir: PathBuf,
    }

    impl Speaker {
        fn build(label: &str, work_dir: &Path) -> StepRef<Self> {
            StepRef::new(Self {
                label: label.to_string(),
                work_dir: work_dir.to_path_buf(),
            })
        }
    }

    impl Step for Speaker {
        fn execute(&self) -> StepResult {
            crate::progress::note("said in passing");
            crate::progress::warn("worth noticing");
            Ok(())
        }

        fn deps(&self) -> Vec<StepRef<dyn Step>> {
            Vec::new()
        }

        fn pinned(&self) -> bool {
            false
        }

        fn set_pinned(&mut self, _pinned: bool) {
            unreachable!("nothing pins a Speaker")
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

    /// What [`crate::progress::note`] and [`crate::progress::warn`] say is kept
    /// here and nowhere else: they are not held in memory for the display to
    /// show later, so a run that is not being watched — or one being watched on
    /// a screen that has moved on — has this to read back.
    #[test]
    fn what_a_step_says_out_loud_is_kept_in_the_log() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let dir = temp_dir("notes");

        crate::Executor::new()
            .progress(false)
            .log_dir(&dir)
            .target(Speaker::build("speaker", &dir))
            .run()
            .expect("run");

        let run = fs::read_to_string(dir.join(RUN_LOG)).expect("rivet.log");
        assert!(run.contains("INFO"), "{run}");
        assert!(run.contains("said in passing"), "{run}");
        assert!(run.contains("WARN"), "{run}");
        assert!(run.contains("worth noticing"), "{run}");

        // Both are the step's own, so the step's log has them too.
        let step = fs::read_to_string(dir.join("speaker.rivet.log")).expect("speaker.rivet.log");
        assert!(step.contains("said in passing"), "{step}");
        assert!(step.contains("worth noticing"), "{step}");
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
    fn a_second_run_is_appended_to_the_first() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let dir = temp_dir("append");

        for message in ["first run", "second run"] {
            crate::Executor::new()
                .progress(false)
                .log_dir(&dir)
                .target(Talker::build("talker", &dir, message))
                .run()
                .expect("run");
        }

        let run = fs::read_to_string(dir.join(RUN_LOG)).expect("rivet.log");
        assert!(run.contains("first run"), "{run}");
        assert!(run.contains("second run"), "{run}");

        // The step's own log describes the run it just did, not the one before.
        let step = fs::read_to_string(dir.join("talker.rivet.log")).expect("talker.rivet.log");
        assert!(step.contains("second run"), "{step}");
        assert!(!step.contains("first run"), "{step}");
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
