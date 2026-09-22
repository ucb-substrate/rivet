//! Runs written down as they happen, so that one can be opened again after it
//! is over.
//!
//! The live display ([`crate::progress`]) *is* the run while it lasts: every
//! step, how each one ended, and the log behind each of them a keypress away.
//! It lasts exactly as long as the process does. A run that is interrupted —
//! `^C`, a dropped ssh session, a machine going down — takes its display with
//! it, and what is left on disk is a scatter of `.out`, `.err` and
//! `.rivet.log` files with nothing to say which step wrote which, which tool
//! wrote them in what order, or how any of it ended.
//!
//! So a run writes down what the display knows, as it learns it: the plan, what
//! each step is writing, and how each step ended. That is a session, and
//! [`open`] puts it back on the screen — the same list, the same pages, the
//! same keys — with whatever was still running marked unfinished.
//!
//! ```text
//! build/
//!   rivet.log             every run that has logged here, in order
//!   rivet.session.toml    the last one, to open again
//! ```
//!
//! One file, beside the `rivet.log` of the run's log directory
//! ([`ExecuteConfig::log_dir`](crate::ExecuteConfig::log_dir), the current
//! directory by default), and the next run in that directory writes over it.
//! [`ExecuteConfig::sessions`](crate::ExecuteConfig::sessions) turns the
//! writing off altogether.
//!
//! The last run rather than a stack of them, because a session is only worth as
//! much as the logs it points at, and those are the last run's too: a step
//! rewrites its `{step}.rivet.log` and its tools' `.out` and `.err` every time
//! it runs. A session kept from the run before would name files that have since
//! been written over — it would look like a record of that run and read like
//! this one. So it goes the way the logs go, all together.
//!
//! # Reopening one
//!
//! ```text
//! rivet               the run that logged in the current directory
//! rivet -C build      the run that logged in build
//! ```
//!
//! [`open`] is the same thing from Rust, over [`path`].
//!
//! # It is an index, not a copy
//!
//! A session holds what a step *is*, not what it *said*: its label, where it
//! came in the plan, how it ended, and the paths of the files it wrote. The
//! output itself stays where the tool wrote it, and the reopened display reads
//! it from there — which is what makes reopening a run cost nothing to write
//! and nothing to keep, however many gigabytes of innovus log it is over.
//!
//! The other side of that bargain: a session is only as good as the files it
//! points at. Clean the build directory and what comes back is the shape of the
//! run with nothing behind it, each page saying `no such file`.
//!
//! # What is written, and when
//!
//! The file is rewritten whenever something happens that could not be worked
//! out again afterwards: a step starting, a step ending, a step starting a tool
//! and so writing somewhere new. Not on progress — a status or a substep banner
//! moves several times a second and means nothing once the run is over.
//!
//! Each rewrite is a whole file, written beside the old one and renamed over
//! it, so a session file is never half a run: it is the run as of some moment,
//! or it is the moment before.
//!
//! A run that ends says how long it took. A run that is killed never gets to,
//! and that is what tells the two apart when the file is read back: a session
//! with no elapsed time is one whose run did not finish, and the steps it has
//! as running are the ones it was in the middle of.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::progress::Planned;
use crate::tui::About;

/// The session file, written beside the run's
/// [`rivet.log`](crate::log::RUN_LOG).
pub const SESSION: &str = "rivet.session.toml";

/// What is written in a session file, so that a later rivet can tell whether it
/// understands one.
pub const VERSION: u32 = 1;

/// The session of the run that last logged in `log_dir`.
pub fn path(log_dir: impl AsRef<Path>) -> PathBuf {
    log_dir.as_ref().join(SESSION)
}

// ---------------------------------------------------------------------------
// What a session is
// ---------------------------------------------------------------------------

/// A run, as it was when this was last written.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    /// The format the file was written in; see [`VERSION`].
    pub version: u32,
    /// When the run started, in seconds since the Unix epoch.
    pub started: u64,
    /// When the file was last written, which for a run that was killed is the
    /// last thing that happened before it was.
    pub saved: u64,
    /// The process that ran it.
    pub pid: u32,
    /// How many steps the run could have going at once.
    pub workers: usize,
    /// How long the run took, in seconds. `None` for a run that never finished:
    /// see [`Session::complete`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub elapsed: Option<f64>,
    /// How many warnings the run logged, for the summary to point at.
    #[serde(default)]
    pub warnings: usize,
    /// Where the run's `rivet.log` went, if it was logging.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_dir: Option<PathBuf>,
    /// The steps the run was asked for, in the order they were asked for.
    #[serde(default)]
    pub targets: Vec<String>,
    /// Every step in the run, numbered as the plan numbered them, which is what
    /// [`Step::deps`] are indices into.
    ///
    /// Last, because everything above it has to be written before it: these are
    /// TOML tables, and a value after a table belongs to the table.
    #[serde(default)]
    pub steps: Vec<Step>,
}

/// One step of a run, as the session records it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Step {
    /// What the step is called on its line.
    pub label: String,
    /// Where it got to.
    pub state: State,
    /// Whether it was pinned, and so never going to run.
    #[serde(default, skip_serializing_if = "is_false")]
    pub pinned: bool,
    /// The steps it waits for, by their index in [`Session::steps`].
    #[serde(default)]
    pub deps: Vec<usize>,
    /// The step's own `{label}.rivet.log`, if it has a directory to keep one
    /// in. For a step that did not run this time, this is where the run that
    /// last ran it left one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log: Option<PathBuf>,
    /// When it started, in seconds since the Unix epoch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started: Option<f64>,
    /// When it stopped, however it stopped.
    ///
    /// To the fraction of a second, unlike the run's own times: several steps
    /// end in the same second all the time — a failure and the steps it blocks
    /// end in the same instant — and this is what puts the record back in the
    /// order the run made it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended: Option<f64>,
    /// How long it ran for, in seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub elapsed: Option<f64>,
    /// Why it ended the way it did: a failure's message, or the step to blame
    /// for a blocked one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Where it was when it failed: its status, its substep, or both.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    /// Whether it was killed from the display, which is why it failed.
    #[serde(default, skip_serializing_if = "is_false")]
    pub killed: bool,
    /// Everything the step wrote, in the order its page offers them: the tool
    /// it was running last first, then earlier tools, then its own log.
    #[serde(default)]
    pub files: Vec<PathBuf>,
    /// The files a command copied from its page would follow: whatever tool it
    /// was running, or its own log until it ran one.
    #[serde(default)]
    pub follow: Vec<PathBuf>,
}

/// How far a step got.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    /// It had not started.
    Pending,
    /// It was running. In a session whose run is over, that means it never
    /// finished: the run was killed while it was going.
    Running,
    /// It ran and succeeded.
    Completed,
    /// It was pinned, so it was not run.
    Skipped,
    /// Something it depended on failed, so it never became runnable.
    Blocked,
    /// It failed.
    Failed,
}

impl State {
    /// Whether the step will not be run again, however it got there.
    pub fn over(self) -> bool {
        !matches!(self, Self::Pending | Self::Running)
    }
}

impl Session {
    /// Whether the run this describes finished.
    ///
    /// A run writes its elapsed time only once every step has ended, so a
    /// session without one is a run that was killed — or, if it is being read
    /// while its run is still going, a snapshot of one still in flight. Either
    /// way, what is in it is as far as the run got.
    pub fn complete(&self) -> bool {
        self.elapsed.is_some()
    }

    /// How long the run took, or had been going when it was cut short.
    pub fn elapsed(&self) -> Duration {
        match self.elapsed {
            Some(seconds) => Duration::from_secs_f64(seconds.max(0.0)),
            None => Duration::from_secs(self.saved.saturating_sub(self.started)),
        }
    }

    /// When the run started, as `2026-09-11 18:02Z`.
    pub fn when(&self) -> String {
        moment(self.started)
    }

    /// The steps that were still running when this was written: what the run
    /// was in the middle of when it was killed.
    pub fn unfinished(&self) -> usize {
        self.count(State::Running)
    }

    /// How many steps ended in `state`.
    pub fn count(&self, state: State) -> usize {
        self.steps.iter().filter(|step| step.state == state).count()
    }

    /// Put this run back on the screen, and stay there until it is dismissed.
    ///
    /// The same display the run itself had, with the same keys: `↑`/`↓` between
    /// the steps, `enter` to read one's log, `L` for the run's, `q` to leave.
    /// Nothing in it can be killed or cancelled — there is nothing running —
    /// and whatever was running when the run died is marked unfinished.
    ///
    /// With no terminal to draw on, the run's record is printed instead, one
    /// line per step, as a run with the display turned off reports.
    pub fn show(&self) {
        crate::progress::Reporter::replay(self).present();
    }
}

// ---------------------------------------------------------------------------
// Reading them back
// ---------------------------------------------------------------------------

/// Read one session file.
pub fn read(path: impl AsRef<Path>) -> io::Result<Session> {
    let path = path.as_ref();
    let text = fs::read_to_string(path)?;
    let session: Session = toml::from_str(&text).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{} is not a rivet session: {error}", path.display()),
        )
    })?;
    // A file from a rivet that knows more about sessions than this one does:
    // what is in it may not mean what this one would take it to mean, and a run
    // drawn wrong would be worse than one that could not be drawn.
    if session.version > VERSION {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "{} was written by a newer rivet: it is a version {} session, and this rivet reads version {VERSION}",
                path.display(),
                session.version,
            ),
        ));
    }
    Ok(session)
}

/// The run that last logged in `log_dir`, if it wrote itself down.
pub fn of(log_dir: impl AsRef<Path>) -> io::Result<Session> {
    read(path(log_dir))
}

/// Open a saved run on the screen, and stay there until it is dismissed.
///
/// See [`Session::show`].
pub fn open(path: impl AsRef<Path>) -> io::Result<()> {
    read(path)?.show();
    Ok(())
}

// ---------------------------------------------------------------------------
// Writing them
// ---------------------------------------------------------------------------

/// The session file of the run in progress, kept up to date as it goes.
///
/// Held by the [`Reporter`](crate::progress::Reporter), which is told everything
/// this has to record in the course of drawing it.
pub(crate) struct Recorder {
    path: PathBuf,
    /// The session as it stands, and the file it is written to, under one lock:
    /// two steps ending at once must not write the file in the other order from
    /// the one they changed it in.
    session: Mutex<Session>,
    /// Whether a write has already failed, so the next one that does is not
    /// another warning about the same thing.
    complained: AtomicBool,
}

impl Recorder {
    /// Start a session for the run `about` describes, in `log_dir`, over
    /// whatever the last run there left.
    ///
    /// `None` if there is nowhere to write it: a run is not to fail, or even to
    /// say much, because its session file could not be opened. The run is the
    /// point; the session is a convenience for afterwards.
    pub(crate) fn start(log_dir: &Path, about: &About, plan: &[Planned]) -> Option<Self> {
        fs::create_dir_all(log_dir).ok()?;
        let started = now();
        let pid = std::process::id();
        let session = Session {
            version: VERSION,
            started,
            saved: started,
            pid,
            workers: about.workers,
            elapsed: None,
            warnings: 0,
            log_dir: about.log_dir.clone(),
            targets: about.targets.clone(),
            steps: plan
                .iter()
                .map(|step| Step {
                    label: step.label.clone(),
                    state: State::Pending,
                    pinned: step.pinned,
                    deps: step.deps.clone(),
                    log: step.log.clone(),
                    started: None,
                    ended: None,
                    elapsed: None,
                    detail: None,
                    location: None,
                    killed: false,
                    files: Vec::new(),
                    follow: Vec::new(),
                })
                .collect(),
        };

        let recorder = Self {
            path: path(log_dir),
            session: Mutex::new(session),
            complained: AtomicBool::new(false),
        };
        // Written now rather than at the first step. A run that dies in its
        // first moments is still a run somebody can look at; the last run's
        // session, which is about to stop being true as this run rewrites the
        // logs under it, is gone from the first moment; and a directory that
        // will not be written to is found out about here.
        recorder.save(&recorder.session.lock().unwrap());
        if recorder.complained.load(Ordering::Relaxed) {
            return None;
        }
        Some(recorder)
    }

    /// Where the session is being written.
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// Record that a step has started, and where its own log is going.
    pub(crate) fn started(&self, id: usize, log: Option<&Path>) {
        self.update(id, |step| {
            step.state = State::Running;
            step.started = Some(instant());
            step.log = log.map(Path::to_path_buf);
            step.files = log.map(Path::to_path_buf).into_iter().collect();
            step.follow = step.files.clone();
        });
    }

    /// Record what a step is writing, as the display would offer it.
    pub(crate) fn files(&self, id: usize, files: Vec<PathBuf>, follow: Vec<PathBuf>) {
        self.update(id, |step| {
            step.files = files;
            step.follow = follow;
        });
    }

    /// Record how a step ended.
    pub(crate) fn ended(
        &self,
        id: usize,
        state: State,
        elapsed: Option<Duration>,
        detail: Option<String>,
        location: Option<String>,
        killed: bool,
    ) {
        self.update(id, |step| {
            step.state = state;
            step.ended = Some(instant());
            step.elapsed = elapsed.map(|elapsed| elapsed.as_secs_f64());
            step.detail = detail;
            step.location = location;
            step.killed = killed;
        });
    }

    /// Record that the run is over, which is also what says it was not killed.
    pub(crate) fn finished(&self, elapsed: Duration, warnings: usize) {
        let mut session = self.session.lock().unwrap();
        session.elapsed = Some(elapsed.as_secs_f64());
        session.warnings = warnings;
        self.save(&session);
    }

    /// Change one step and write the file, under the one lock.
    fn update(&self, id: usize, change: impl FnOnce(&mut Step)) {
        let mut session = self.session.lock().unwrap();
        let Some(step) = session.steps.get_mut(id) else {
            return;
        };
        change(step);
        self.save(&session);
    }

    /// Write the whole file, beside the old one and then over it.
    ///
    /// Renaming is what keeps a session file whole: a reader either sees the
    /// run as it was a moment ago or as it is now, never a file that was being
    /// written when the run was killed.
    fn save(&self, session: &Session) {
        let saved = Session {
            saved: now(),
            ..session.clone()
        };
        if let Err(error) = write(&self.path, &saved) {
            // Once. A directory that cannot be written to now will not be
            // writable at every step of the run either, and a warning per step
            // would bury the run's own in its log.
            if !self.complained.swap(true, Ordering::Relaxed) {
                tracing::warn!(
                    path = %self.path.display(),
                    %error,
                    "cannot write the session; this run will not be one to reopen"
                );
            }
        }
    }
}

/// Write a session to `path`, atomically.
fn write(path: &Path, session: &Session) -> io::Result<()> {
    let text = toml::to_string(session)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    let temporary = path.with_extension("toml.writing");
    fs::write(&temporary, text)?;
    // Same directory, so this is a rename rather than a copy, and the file at
    // `path` is replaced whole or not at all.
    fs::rename(&temporary, path)
}

// ---------------------------------------------------------------------------
// Telling the time
// ---------------------------------------------------------------------------

/// Now, in seconds since the Unix epoch.
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_secs())
        .unwrap_or_default()
}

/// Now, to the fraction of a second: [`now`] where the order of two things in
/// the same second matters.
fn instant() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_secs_f64())
        .unwrap_or_default()
}

/// `2026-09-11 18:02Z`, to read.
///
/// UTC, like the timestamps in `rivet.log`: a flow is watched from somewhere
/// other than the machine it runs on often enough that a local time would be
/// two different answers to the same question.
fn moment(unix: u64) -> String {
    let (year, month, day, hour, minute) = utc(unix);
    format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}Z")
}

/// Unix seconds as a UTC date and time.
///
/// The date is Howard Hinnant's `civil_from_days`, which counts from an era
/// starting in March so that a leap day falls at the end of one. Worth the
/// twenty lines: the alternative is a date library, and this is the only thing
/// in rivet that has ever needed to know what day it is.
fn utc(unix: u64) -> (i64, u32, u32, u32, u32) {
    let days = (unix / 86_400) as i64;
    let rest = unix % 86_400;

    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;

    (
        year + i64::from(month <= 2),
        month,
        day,
        (rest / 3_600) as u32,
        (rest % 3_600 / 60) as u32,
    )
}

/// For `skip_serializing_if`: a flag that is not set is left out of the file.
fn is_false(flag: &bool) -> bool {
    !*flag
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{StepRef, StepResult};
    use std::sync::atomic::AtomicUsize;

    fn temp_dir(name: &str) -> PathBuf {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "rivet-session-{name}-{}-{unique}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    /// A step that runs a command, so it has output files to be recorded.
    #[derive(Debug)]
    struct Talker {
        label: String,
        work_dir: PathBuf,
        fails: bool,
        deps: Vec<StepRef<dyn crate::Step>>,
    }

    impl Talker {
        fn build(label: &str, dir: &Path, deps: Vec<StepRef<dyn crate::Step>>) -> StepRef<Self> {
            StepRef::new(Self {
                label: label.to_string(),
                work_dir: dir.to_path_buf(),
                fails: false,
                deps,
            })
        }

        fn failing(label: &str, dir: &Path) -> StepRef<Self> {
            StepRef::new(Self {
                label: label.to_string(),
                work_dir: dir.to_path_buf(),
                fails: true,
                deps: Vec::new(),
            })
        }
    }

    impl crate::Step for Talker {
        fn execute(&self) -> StepResult {
            let mut command = std::process::Command::new("/bin/bash");
            command.args(["-c", "echo working"]);
            let status = crate::exec::run_logged_in(&mut command, &self.work_dir, &self.label)?;
            if self.fails || !status.success() {
                return Err("did not match".into());
            }
            Ok(())
        }

        fn deps(&self) -> Vec<StepRef<dyn crate::Step>> {
            self.deps.clone()
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

    /// The whole of it: a run writes itself down, and what it wrote is the run.
    #[test]
    fn a_run_is_written_down_as_it_goes() {
        let _serial = crate::ONE_RUN_AT_A_TIME
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = temp_dir("run");
        let first = Talker::build("first", &dir, vec![]);
        let second = Talker::build("second", &dir, vec![first.clone().into_dyn()]);

        crate::Executor::new()
            .progress(false)
            .log_dir(&dir)
            .target(second)
            .run()
            .expect("run");

        let session = of(&dir).expect("the run wrote itself down");
        assert_eq!(path(&dir), dir.join(SESSION), "beside the run's rivet.log");
        assert_eq!(session.version, VERSION);
        assert!(session.complete(), "the run finished, so it said so");
        assert_eq!(session.targets, ["second"]);
        assert_eq!(session.log_dir.as_deref(), Some(dir.as_path()));
        assert_eq!(session.steps.len(), 2);
        assert_eq!(session.unfinished(), 0);

        let step = session
            .steps
            .iter()
            .find(|step| step.label == "first")
            .expect("the step is in the session");
        assert_eq!(step.state, State::Completed);
        assert!(step.elapsed.is_some(), "how long it took");
        // What it wrote, which is the point: the tool's output first, then the
        // step's own log, all of them files that are there to be read.
        assert!(
            step.files.contains(&dir.join("first.out")),
            "{:?}",
            step.files
        );
        assert!(
            step.files.contains(&dir.join("first.rivet.log")),
            "{:?}",
            step.files
        );
        assert_eq!(step.follow, [dir.join("first.out"), dir.join("first.err")]);
        for file in &step.files {
            assert!(
                file.exists(),
                "{} was recorded but not written",
                file.display()
            );
        }
    }

    /// A failure is recorded with what it said, so that the line it had can be
    /// drawn again rather than guessed at.
    #[test]
    fn a_failure_keeps_its_message_and_what_it_took_down() {
        let _serial = crate::ONE_RUN_AT_A_TIME
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = temp_dir("failure");
        let lvs = Talker::failing("lvs", &dir).into_dyn();
        let signoff = Talker::build("signoff", &dir, vec![lvs]);

        crate::Executor::new()
            .progress(false)
            .log_dir(&dir)
            .target(signoff)
            .run()
            .expect_err("lvs fails, so the run does");

        let session = of(&dir).expect("a session");
        let step = |label: &str| {
            session
                .steps
                .iter()
                .find(|step| step.label == label)
                .unwrap_or_else(|| panic!("{label} is in the session"))
        };
        assert_eq!(step("lvs").state, State::Failed);
        assert_eq!(step("lvs").detail.as_deref(), Some("did not match"));
        assert_eq!(step("signoff").state, State::Blocked);
        // Blamed on the step that failed, which is what its line says.
        assert_eq!(step("signoff").detail.as_deref(), Some("lvs"));
        assert_eq!(session.count(State::Failed), 1);
    }

    /// The case the whole thing is for: the process goes, and what it left says
    /// where the run had got to.
    #[test]
    fn a_run_that_never_finishes_keeps_the_steps_it_was_in_the_middle_of() {
        let dir = temp_dir("killed");
        let about = About {
            targets: vec!["par".into()],
            steps: 2,
            workers: 1,
            log_dir: Some(dir.clone()),
            saved: None,
        };
        let plan = vec![
            Planned {
                label: "syn".into(),
                pinned: false,
                deps: Vec::new(),
                log: None,
            },
            Planned {
                label: "par".into(),
                pinned: false,
                deps: vec![0],
                log: None,
            },
        ];
        let recorder = Recorder::start(&dir.join("sessions"), &about, &plan).expect("a recorder");

        recorder.started(0, Some(&dir.join("syn.rivet.log")));
        recorder.ended(
            0,
            State::Completed,
            Some(Duration::from_secs(74)),
            None,
            None,
            false,
        );
        recorder.started(1, Some(&dir.join("par.rivet.log")));
        recorder.files(1, vec![dir.join("par.out")], vec![dir.join("par.out")]);
        // And here the process dies: nothing says the run ended.

        let session = read(recorder.path()).expect("the session reads back");
        assert!(!session.complete(), "the run never said it was done");
        assert_eq!(session.unfinished(), 1);
        assert_eq!(session.steps[1].state, State::Running);
        assert_eq!(session.steps[1].files, [dir.join("par.out")]);
        // The step that did finish is still how it finished.
        assert_eq!(session.steps[0].state, State::Completed);
        assert_eq!(session.steps[0].elapsed, Some(74.0));
    }

    /// A session is never half a file, whatever happens while it is being
    /// written: every write lands whole or not at all.
    #[test]
    fn a_session_is_rewritten_in_place() {
        let dir = temp_dir("rewrite");
        let about = About {
            targets: vec!["one".into()],
            steps: 1,
            workers: 1,
            log_dir: Some(dir.clone()),
            saved: None,
        };
        let plan = vec![Planned {
            label: "one".into(),
            pinned: false,
            deps: Vec::new(),
            log: None,
        }];
        let recorder = Recorder::start(&dir, &about, &plan).expect("a recorder");

        recorder.started(0, None);
        recorder.ended(
            0,
            State::Completed,
            Some(Duration::from_secs(1)),
            None,
            None,
            false,
        );
        recorder.finished(Duration::from_secs(2), 3);

        // One file, not one per write, and nothing left half written beside it.
        assert_eq!(recorder.path(), path(&dir));
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
        let session = read(recorder.path()).expect("read");
        assert!(session.complete());
        assert_eq!(session.warnings, 3);
        assert_eq!(session.elapsed(), Duration::from_secs(2));
    }

    /// One session per directory, and it is the last run's: the logs it names
    /// are the last run's too, since a step rewrites them every time it runs.
    #[test]
    fn a_run_writes_over_the_session_of_the_run_before_it() {
        let _serial = crate::ONE_RUN_AT_A_TIME
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = temp_dir("replace");

        for label in ["first", "second"] {
            crate::Executor::new()
                .progress(false)
                .log_dir(&dir)
                .target(Talker::build(label, &dir, vec![]))
                .run()
                .expect("run");
        }

        let session = of(&dir).expect("a session");
        assert_eq!(session.targets, ["second"], "the run that logged last");
        assert_eq!(session.steps.len(), 1);
        // One file, however many runs there have been.
        let sessions = fs::read_dir(&dir)
            .expect("read the log directory")
            .flatten()
            .filter(|entry| entry.file_name() == SESSION)
            .count();
        assert_eq!(sessions, 1);
    }

    #[test]
    fn a_run_says_when_it_happened() {
        // 2026-09-10T18:02:11Z, and the epoch itself.
        assert_eq!(moment(1_789_063_331), "2026-09-10 18:02Z");
        assert_eq!(moment(0), "1970-01-01 00:00Z");
        // A leap day, which is where a date that counts days goes wrong.
        assert_eq!(moment(1_709_208_000), "2024-02-29 12:00Z");
        assert_eq!(moment(1_709_251_200), "2024-03-01 00:00Z");
        // And the turn of a century that is not a leap year.
        assert_eq!(moment(4_107_542_400), "2100-03-01 00:00Z");
    }

    /// Where a session goes: beside the `rivet.log` of the run that wrote it.
    #[test]
    fn a_session_sits_beside_the_runs_log() {
        assert_eq!(path("build"), PathBuf::from("build/rivet.session.toml"));
    }

    /// A run told to leave the disk alone leaves nothing, sessions included.
    #[test]
    fn a_run_that_is_not_logging_writes_no_session() {
        let _serial = crate::ONE_RUN_AT_A_TIME
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = temp_dir("off");
        crate::Executor::new()
            .progress(false)
            .logging(false)
            .log_dir(&dir)
            .target(Talker::build("quiet", &dir, vec![]))
            .run()
            .expect("run");
        assert!(of(&dir).is_err());
        assert!(!path(&dir).exists());
    }

    /// And one told not to write sessions writes no session, but still logs.
    #[test]
    fn sessions_can_be_turned_off_on_their_own() {
        let _serial = crate::ONE_RUN_AT_A_TIME
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = temp_dir("nosession");
        crate::Executor::new()
            .progress(false)
            .sessions(false)
            .log_dir(&dir)
            .target(Talker::build("quiet", &dir, vec![]))
            .run()
            .expect("run");
        assert!(of(&dir).is_err());
        assert!(!path(&dir).exists());
        assert!(dir.join(crate::log::RUN_LOG).exists(), "still logging");
    }
}
