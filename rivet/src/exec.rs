//! Running subprocesses from inside a [`Step`](crate::Step).
//!
//! Steps should not let a child process write straight to the terminal: with
//! several steps running at once the output interleaves and corrupts the live
//! progress display. These helpers capture the child's output instead, tee it
//! to log files, and hand each line to the reporter for the running step.

use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};
use std::thread;

use crate::progress::{self, StepHandle};

/// Run `command`, writing its stdout and stderr to the given log files and
/// surfacing them on the running step's progress line.
pub fn run_logged(
    command: &mut Command,
    stdout_log: impl AsRef<Path>,
    stderr_log: impl AsRef<Path>,
) -> std::io::Result<ExitStatus> {
    let stdout_file = File::create(stdout_log.as_ref())?;
    let stderr_file = File::create(stderr_log.as_ref())?;

    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    let stdout = BufReader::new(child.stdout.take().expect("stdout is piped"));
    let stderr = BufReader::new(child.stderr.take().expect("stderr is piped"));

    // The reader threads are not the worker thread, so they cannot look the
    // handle up themselves.
    let handle = progress::current_step();
    let stdout_handle = handle.clone();
    let stderr_handle = handle.clone();

    let out_thread = thread::spawn(move || pump(stdout, stdout_file, stdout_handle, false));
    let err_thread = thread::spawn(move || pump(stderr, stderr_file, stderr_handle, true));

    let _ = out_thread.join();
    let _ = err_thread.join();

    let status = child.wait()?;

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

fn pump<R: BufRead>(reader: R, mut file: File, handle: Option<StepHandle>, is_stderr: bool) {
    // Split on bytes rather than using `lines()`: EDA tools are not reliably
    // UTF-8 clean, and a stray byte should not kill the step.
    for chunk in reader.split(b'\n') {
        let Ok(bytes) = chunk else { break };
        let _ = file.write_all(&bytes);
        let _ = file.write_all(b"\n");

        let text = String::from_utf8_lossy(&bytes);
        match &handle {
            Some(handle) => handle.output_line(&text),
            None if is_stderr => eprintln!("{text}"),
            None => println!("{text}"),
        }
    }
    let _ = file.flush();
}
