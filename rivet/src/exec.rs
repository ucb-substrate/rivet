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

use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use crate::progress::{self, StepHandle};

/// Run `command`, writing its stdout and stderr to the given log files and
/// surfacing them on the running step's progress line.
pub fn run_logged(
    command: &mut Command,
    stdout_log: impl AsRef<Path>,
    stderr_log: impl AsRef<Path>,
) -> std::io::Result<ExitStatus> {
    let stdout_log = stdout_log.as_ref();
    let stderr_log = stderr_log.as_ref();
    let stdout_file = File::create(stdout_log)?;
    let stderr_file = File::create(stderr_log)?;

    // Named at `info`, because "which command was this and where did its output
    // go" is the first thing anyone reading the log wants.
    tracing::info!(
        command = ?command,
        stdout = %stdout_log.display(),
        stderr = %stderr_log.display(),
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
    let stdout_handle = handle.clone();
    let stderr_handle = handle.clone();

    // Each pump holds a sender it never sends on, so the channel closes exactly
    // when both have returned — the same thing joining them would wait for, but
    // waited for with a timeout, which leaves somewhere to put the check below.
    let (ended, both_ended) = mpsc::channel::<()>();
    let out_ended = ended.clone();
    let out_thread = thread::spawn(move || pump(stdout, stdout_file, stdout_handle, out_ended));
    let err_thread = thread::spawn(move || pump(stderr, stderr_file, stderr_handle, ended));

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
    while both_ended.recv_timeout(QUIET_CHECK) != Err(mpsc::RecvTimeoutError::Disconnected) {
        let Some(handle) = &handle else { continue };
        let Some(quiet) = handle.quiet_for() else {
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

    Ok(status)
}

/// [`run_logged`] with log files named `{basename}.out` and `{basename}.err`
/// inside `log_dir`.
pub fn run_logged_in(
    command: &mut Command,
    log_dir: impl AsRef<Path>,
    basename: &str,
) -> std::io::Result<ExitStatus> {
    let log_dir = log_dir.as_ref();
    run_logged(
        command,
        log_dir.join(format!("{basename}.out")),
        log_dir.join(format!("{basename}.err")),
    )
}

/// How often a waiting [`run_logged`] looks up from the child to check whether
/// its output has dried up. Finer than [`progress::QUIET_AFTER`], so the notice
/// is not late by as much as the thing it is reporting.
const QUIET_CHECK: Duration = Duration::from_secs(30);

/// `_ended` is dropped when this returns, which is how [`run_logged`] learns
/// that this stream is finished. It is never sent on.
fn pump<R: BufRead>(
    reader: R,
    mut file: File,
    handle: Option<StepHandle>,
    _ended: mpsc::Sender<()>,
) {
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
        if let Some(handle) = &handle {
            handle.output_line(&String::from_utf8_lossy(&bytes));
        }
    }
    let _ = file.flush();
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
        let status =
            run_logged(&mut command, dir.join("t.out"), dir.join("t.err")).unwrap();

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

        let status =
            run_logged(&mut command, dir.join("t.out"), dir.join("t.err")).unwrap();

        assert_eq!(status.code(), Some(3));
        let _ = std::fs::remove_dir_all(&dir);
    }
}

