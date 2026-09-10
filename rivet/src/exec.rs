//! Running subprocesses from inside a [`Step`](crate::Step).
//!
//! Steps should not let a child process write straight to the terminal: with
//! several steps running at once the output interleaves and corrupts the live
//! progress display. These helpers capture the child's output instead and write
//! it to log files.
//!
//! Nothing a child prints is shown on screen. Each line is offered to the
//! running step, which takes it only if it carries a substep banner; see
//! [`crate::progress`]. stdout and stderr are treated the same way — plenty of
//! tools put all their chatter on stderr — and differ only in which file they
//! land in.
//!
//! A tool's own output stays in those files and is not folded into
//! [`rivet.log`](crate::log): there is far too much of it for a log meant to
//! stay readable. What is recorded there instead is the command a step ran,
//! where its output went, and how it exited.
//!
//! Any `Command` a step runs some other way must have its stdio piped or
//! redirected for the same reason; if it genuinely needs the terminal, wrap it
//! in [`crate::progress::suspend`].
//!
//! A tool that can hold itself at its own prompt when it fails is run with
//! [`run_held`] instead, which gives it somewhere to be held and returns as
//! soon as it says it has been: the step fails there and then, and the tool
//! goes on writing to the same log files until it is let go. See
//! [`crate::hold`].

use std::fmt;
use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use crate::hold::{self, Session};
use crate::progress::{self, StepHandle};

/// How a tool run ended.
///
/// A tool that exited did so for its own reasons, good or bad, and there is
/// nothing left of it. A tool that was held is still there: it failed, said so,
/// and stayed at its own prompt to be attached to. See [`crate::hold`].
#[derive(Debug)]
pub enum Finish {
    /// The tool exited, with this status.
    Exited(ExitStatus),
    /// The tool failed and is being held at its own prompt, in this session.
    Held(Arc<Session>),
}

impl Finish {
    /// Whether the tool ran to completion.
    ///
    /// A held tool has failed by definition: holding itself is what it does
    /// instead of exiting on an error.
    pub fn success(&self) -> bool {
        matches!(self, Finish::Exited(status) if status.success())
    }

    /// How it exited, if it exited at all.
    pub fn status(&self) -> Option<ExitStatus> {
        match self {
            Finish::Exited(status) => Some(*status),
            Finish::Held(_) => None,
        }
    }

    /// The session the tool is being held in, if it is being held.
    pub fn session(&self) -> Option<&Arc<Session>> {
        match self {
            Finish::Exited(_) => None,
            Finish::Held(session) => Some(session),
        }
    }
}

impl fmt::Display for Finish {
    /// How a step's error message says what became of its tool.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Finish::Exited(status) => write!(f, "exited with {status}"),
            Finish::Held(session) => write!(
                f,
                "failed and is held at its own prompt (pid {}); attach with {}",
                session.pid(),
                session.attach_command()
            ),
        }
    }
}

/// Run `command`, writing its stdout and stderr to the given log files and
/// surfacing them on the running step's progress line.
pub fn run_logged(
    command: &mut Command,
    stdout_log: impl AsRef<Path>,
    stderr_log: impl AsRef<Path>,
) -> std::io::Result<ExitStatus> {
    match run(command, stdout_log.as_ref(), stderr_log.as_ref(), false)? {
        Finish::Exited(status) => Ok(status),
        // A tool holds itself only where it has been given somewhere to be
        // held, and this run gave it nowhere.
        Finish::Held(_) => unreachable!("no control channel, so nothing to hold"),
    }
}

/// [`run_logged`] for a tool that holds itself at its own prompt when it fails,
/// rather than exiting.
///
/// The tool is offered somewhere to be held — see [`crate::hold`] for what it
/// has to do with the offer — and this returns as soon as it says it has taken
/// it, with the session it left behind. The step should fail on that as it
/// would on any other failure: the tool is not the step's any more, and goes on
/// writing to the same log files until the session is let go.
///
/// Whether anything is offered at all is [`hold::RIVET_HOLD`]'s to decide. A
/// run that offers nothing gets a tool that exits on an error exactly as it
/// always did, so this is what a tool that can be held is always run with.
pub fn run_held(
    command: &mut Command,
    stdout_log: impl AsRef<Path>,
    stderr_log: impl AsRef<Path>,
) -> std::io::Result<Finish> {
    run(command, stdout_log.as_ref(), stderr_log.as_ref(), true)
}

/// [`run_logged`] with log files named `{basename}.out` and `{basename}.err`
/// inside `log_dir`.
pub fn run_logged_in(
    command: &mut Command,
    log_dir: impl AsRef<Path>,
    basename: &str,
) -> std::io::Result<ExitStatus> {
    let (stdout_log, stderr_log) = logs_in(log_dir.as_ref(), basename);
    run_logged(command, stdout_log, stderr_log)
}

/// [`run_held`] with log files named `{basename}.out` and `{basename}.err`
/// inside `log_dir`, and the session's control channel beside them.
pub fn run_held_in(
    command: &mut Command,
    log_dir: impl AsRef<Path>,
    basename: &str,
) -> std::io::Result<Finish> {
    let (stdout_log, stderr_log) = logs_in(log_dir.as_ref(), basename);
    run_held(command, stdout_log, stderr_log)
}

/// Where `{basename}`'s two log files go in `log_dir`.
fn logs_in(log_dir: &Path, basename: &str) -> (PathBuf, PathBuf) {
    (
        log_dir.join(format!("{basename}.out")),
        log_dir.join(format!("{basename}.err")),
    )
}

/// Run `command` with its output captured, offering it somewhere to be held if
/// it fails when `offer_hold` says to.
fn run(
    command: &mut Command,
    stdout_log: &Path,
    stderr_log: &Path,
    offer_hold: bool,
) -> std::io::Result<Finish> {
    let stdout_file = File::create(stdout_log)?;
    let stderr_file = File::create(stderr_log)?;

    // Before the tool starts, because a tool already running cannot be handed
    // one. Kept until this returns, so the fifo goes with the tool that never
    // needed it.
    let mut control = match offer_hold {
        true => hold::control(stdout_log, stderr_log)?,
        false => None,
    };
    if let Some(control) = &control {
        control.offer(command);
    }
    // What the tool is, for the session that may be left of it. Taken before
    // the spawn so that nothing borrows `command` afterwards.
    let tool = command.get_program().to_string_lossy().into_owned();

    // Named at `info`, because "which command was this and where did its output
    // go" is the first thing anyone reading the log wants.
    tracing::info!(
        command = ?command,
        stdout = %stdout_log.display(),
        stderr = %stderr_log.display(),
        control = ?control.as_ref().map(|control| control.path()),
        "running"
    );

    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    let stdout = BufReader::new(child.stdout.take().expect("stdout is piped"));
    let stderr = BufReader::new(child.stderr.take().expect("stderr is piped"));

    // The reader threads are not the worker thread, so they cannot look the
    // handle up themselves.
    let handle = progress::current_step();
    // Held until the child has been waited for, which is what lets the display
    // kill this step on its own without touching the rest of the run. See
    // `progress::StepHandle::watch_child`.
    let _child = handle.as_ref().map(|handle| handle.watch_child(child.id()));
    if let Some(handle) = &handle {
        // Not to show — none of this is shown — but so that someone watching
        // the run can be handed a command to follow these two files while the
        // tool is writing them. See `progress::StepHandle::set_output_files`.
        handle.set_output_files(vec![stdout_log.to_path_buf(), stderr_log.to_path_buf()]);
    }

    // Each pump holds a sender it never sends on unless the tool holds itself,
    // so the channel closes exactly when both have returned — the same thing
    // joining them would wait for, but waited for with a timeout, which leaves
    // somewhere to put the check below.
    let (ended, note) = mpsc::channel::<Note>();
    // Shared by both pumps: the marker arrives on one of the two streams, and
    // from then on neither of them belongs to the step.
    let held = control.is_some().then(|| Arc::new(AtomicBool::new(false)));
    let out = Pumped {
        handle: handle.clone(),
        held: held.clone(),
        ended: ended.clone(),
    };
    let err = Pumped {
        handle: handle.clone(),
        held,
        ended,
    };
    let out_thread = thread::spawn(move || pump(stdout, stdout_file, out));
    let err_thread = thread::spawn(move || pump(stderr, stderr_file, err));

    // A running step draws a spinner, which says the tool is still there and
    // nothing about whether it is getting anywhere. A tool can stop dead with
    // its process alive and healthy-looking — waiting on a license, wedged in
    // a crash handler, blocked on a filesystem — and the only sign is that it
    // has stopped writing. Watch for it, and say so when it has been quiet too
    // long. Said again each time the wait doubles — 10m, 20m, 40m and so on.
    // How long it has been is the whole content of the warning and it keeps
    // growing, so one line at the threshold would be stale within the hour;
    // doubling keeps the number current without repeating itself for hours
    // over a long stall.
    let mut say_at = progress::QUIET_AFTER;
    loop {
        let quiet = match note.recv_timeout(QUIET_CHECK) {
            // The tool has failed and stayed at its prompt. The step is
            // finished with it, and what is left of it is a session that
            // outlives the step: the pumps go on writing its output to the
            // same files, and are waited for by whoever waits for the tool.
            Ok(Note::Held) => {
                let control = control
                    .as_mut()
                    .expect("only a tool offered a channel says it is held");
                let label = handle
                    .as_ref()
                    .map(|handle| handle.label().to_string())
                    .unwrap_or_else(|| tool.clone());
                let session = control.hold(&label, &tool, child, vec![out_thread, err_thread])?;
                if let Some(handle) = &handle {
                    handle.hold(Arc::clone(&session));
                }
                // On screen the step's line says it, and its page says how to
                // reach it. This is the log, and the plain output of a run with
                // no display to say it on.
                progress::warn(format!(
                    "{label}: {tool} failed and is held at its own prompt (pid {}) — attach with {}",
                    session.pid(),
                    session.attach_command()
                ));
                return Ok(Finish::Held(session));
            }
            // Both pumps have returned: the tool's output is over, so the tool
            // is over.
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                handle.as_ref().and_then(|handle| handle.quiet_for())
            }
        };
        let Some(handle) = &handle else { continue };
        let Some(quiet) = quiet else {
            // Writing again. Whatever it does next is a fresh stall, and gets
            // reported from the threshold rather than from wherever the last
            // one had doubled its way to.
            say_at = progress::QUIET_AFTER;
            continue;
        };
        if quiet < say_at {
            continue;
        }
        say_at = quiet * 2;

        let message = format!(
            "{}: nothing written for {} — still running, last output in {}",
            handle.label(),
            progress::fmt_duration(quiet),
            stdout_log.display()
        );
        // On screen the step's own line says it, in yellow, and takes it back
        // the moment the tool writes again. What this adds is the log, and the
        // plain output of a run with no display to say it on — both of which
        // `progress::warn` does, and neither of which goes stale.
        progress::warn(message);
    }

    let _ = out_thread.join();
    let _ = err_thread.join();

    let status = child.wait()?;
    tracing::info!(code = status.code(), success = status.success(), "exited");

    // Only on success. If the tool failed, the substep it failed in is the
    // whole point, and the caller is about to turn that into an error.
    if status.success() {
        if let Some(handle) = &handle {
            handle.clear_substep();
        }
    }

    Ok(Finish::Exited(status))
}

/// How often a waiting [`run_logged`] looks up from the child to check whether
/// its output has dried up. Finer than [`progress::QUIET_AFTER`], so the notice
/// is not late by as much as the thing it is reporting.
const QUIET_CHECK: Duration = Duration::from_secs(30);

/// What a pump has to do with a line besides writing it to the file.
struct Pumped {
    /// The step to offer the line to, if there is one.
    handle: Option<StepHandle>,
    /// Whether the tool has said it is held, shared with the other pump.
    /// `None` for a tool that was never offered anywhere to be held, whose
    /// output is only ever the step's.
    held: Option<Arc<AtomicBool>>,
    /// Dropped when the pump returns, which is how [`run`] learns that this
    /// stream is finished. Sent on once, if the tool says it is held.
    ended: mpsc::Sender<Note>,
}

/// What a pump has to tell [`run`] before its stream is over.
enum Note {
    /// The tool has printed [`hold::MARKER`]: it has failed and is waiting at
    /// its own prompt.
    Held,
}

fn pump<R: BufRead>(reader: R, mut file: File, mut pumped: Pumped) {
    // Split on bytes rather than using `lines()`: EDA tools are not reliably
    // UTF-8 clean, and a stray byte should not kill the step.
    for chunk in reader.split(b'\n') {
        let Ok(bytes) = chunk else { break };
        let _ = file.write_all(&bytes);
        let _ = file.write_all(b"\n");

        // The log file has the line either way. Offering it to the step is
        // only how a substep banner gets picked out of it, so with no step to
        // offer it to — `run_logged` called off a worker thread, or outside a
        // run — there is nothing left to do with it.
        let line = String::from_utf8_lossy(&bytes);
        if !pumped.take_hold(&line) {
            if let Some(handle) = &pumped.handle {
                handle.output_line(&line);
            }
        }
    }
    let _ = file.flush();
}

impl Pumped {
    /// Whether this line belongs to a session rather than to the step: the
    /// marker that says the tool is held, and everything after it.
    ///
    /// What a held tool prints is a conversation with whoever attached to it.
    /// It is written to the log, where they are reading it, and offered to
    /// nothing: a banner in it is not where the step got to, and the step has
    /// finished with the tool in any case.
    fn take_hold(&mut self, line: &str) -> bool {
        let Some(held) = &self.held else { return false };
        if held.load(Ordering::SeqCst) {
            return true;
        }
        if !line.contains(hold::MARKER) {
            return false;
        }
        held.store(true, Ordering::SeqCst);
        let _ = self.ended.send(Note::Held);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("rivet-exec-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// The ordinary path, which the quiet check sits in the middle of: a child
    /// that says its piece and exits is still waited for exactly, on both
    /// streams, and still returns as soon as it is done.
    #[test]
    fn a_child_that_exits_is_waited_for_and_no_longer() {
        let dir = scratch("clean");
        let mut command = Command::new("bash");
        command.args(["-c", "echo working; echo also working >&2"]);

        let started = std::time::Instant::now();
        let status = run_logged(&mut command, dir.join("t.out"), dir.join("t.err")).unwrap();

        assert!(status.success());
        assert!(started.elapsed() < QUIET_CHECK, "waited for a timeout tick");
        assert_eq!(
            std::fs::read_to_string(dir.join("t.out")).unwrap().trim(),
            "working"
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("t.err")).unwrap().trim(),
            "also working"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// And a failing child is reported as failing, not swallowed by the loop.
    #[test]
    fn a_child_that_fails_still_fails() {
        let dir = scratch("fail");
        let mut command = Command::new("bash");
        command.args(["-c", "echo nope >&2; exit 3"]);

        let status = run_logged(&mut command, dir.join("t.out"), dir.join("t.err")).unwrap();

        assert_eq!(status.code(), Some(3));
        let _ = std::fs::remove_dir_all(&dir);
    }

    // -- a tool that holds itself -------------------------------------------

    /// The whole of the held path: the tool says it is holding itself, this
    /// returns without waiting for it, and what is left is a session that can
    /// be driven and then let go.
    #[test]
    fn a_tool_that_holds_itself_is_returned_still_running() {
        let _alone = hold::testing::alone();
        let dir = scratch("held");
        let mut command = hold::testing::holds_itself();

        let started = std::time::Instant::now();
        let finish = run_held_in(&mut command, &dir, "tool").unwrap();

        // Not waited for: the tool is still sitting at its prompt, and the
        // step is meant to fail on it now rather than whenever it is let go.
        assert!(started.elapsed() < QUIET_CHECK, "waited for the tool");
        assert!(!finish.success(), "a held tool has failed by definition");
        assert!(finish.status().is_none(), "it has not exited");
        let session = finish.session().expect("held").clone();
        assert!(session.alive());
        // What a step's error message says: where to read, and how to reach it.
        let said = finish.to_string();
        assert!(said.contains("held"), "{said}");
        assert!(said.contains(&session.attach_command()), "{said}");

        // Driven the way an attached terminal drives it — a line at a time
        // into the control channel — and answered in the log it is writing.
        let mut channel = std::fs::OpenOptions::new()
            .write(true)
            .open(dir.join("tool.control"))
            .unwrap();
        writeln!(channel, "report_timing").unwrap();
        let answered = std::time::Instant::now();
        while !read(&dir.join("tool.out")).contains("ran report_timing") {
            assert!(answered.elapsed() < QUIET_CHECK, "no answer in the log");
            thread::sleep(Duration::from_millis(20));
        }

        session.end();
        assert!(hold::testing::gone(&session), "still there");
        assert!(
            read(&dir.join("tool.out")).contains("let go"),
            "left in a hurry"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A tool run the ordinary way is offered nowhere to be held, and holds
    /// itself nowhere: the same tool exits on its error as it always did.
    #[test]
    fn a_tool_offered_nowhere_to_be_held_exits() {
        // Holding is on, and the point is that this run offers nothing
        // anyway: `run_logged` is the way a tool is run when it is not to be
        // held, whatever the knob says.
        let _alone = hold::testing::alone();
        let dir = scratch("unheld");
        let mut command = hold::testing::holds_itself();

        let status = run_logged(&mut command, dir.join("t.out"), dir.join("t.err")).unwrap();

        assert_eq!(status.code(), Some(3), "it was offered somewhere");
        assert!(read(&dir.join("t.out")).contains("nowhere to be held"));
        // And nothing is left in the work directory that looks like a session.
        assert!(
            !dir.join("t.control").exists(),
            "a control channel was left"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The channel goes with a tool that never needed it, which is every tool
    /// that did what it was asked.
    #[test]
    fn a_tool_that_does_not_fail_leaves_no_channel_behind() {
        let _alone = hold::testing::alone();
        let dir = scratch("clean-held");
        let mut command = Command::new("bash");
        command.args(["-c", "echo done"]);

        let finish = run_held_in(&mut command, &dir, "tool").unwrap();

        assert!(finish.success());
        assert!(
            !dir.join("tool.control").exists(),
            "a control channel was left"
        );
        assert!(
            !dir.join("tool.attach.sh").exists(),
            "an attach script was left"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn read(path: &std::path::Path) -> String {
        std::fs::read_to_string(path).unwrap_or_default()
    }
}
