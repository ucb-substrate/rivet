//! `rivet`: open a run that has already happened, to read its logs.
//!
//! A run writes itself down as it goes (see [`rivet::session`]), so that the
//! display it had is not the only copy of what it did. This puts one back on
//! the screen — the same list of steps, the same pages over the same log files,
//! the same keys — for a run that was interrupted, or one that ended hours ago.
//!
//! ```text
//! rivet                     the run that logged in the current directory
//! rivet -C build            the run that logged in build
//! rivet build               the same, said the short way
//! rivet build/rivet.session.toml
//! ```

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Parser;
use rivet::session;

#[derive(Parser)]
#[command(
    name = "rivet",
    about = "Open the run rivet last did in a directory, to read its logs",
    long_about = "Open the run rivet last did in a directory, to read its logs.\n\n\
                  Runs write themselves down as they go, so one that was \
                  interrupted — or one that simply ended — can be put back on \
                  the screen: every step, how it ended, and its logs a keypress \
                  away. Nothing is re-run, and nothing is changed."
)]
struct Cli {
    /// Where the run logged: the directory its `rivet.log` went in, or a
    /// session file to open directly. The current directory by default.
    #[arg(value_name = "DIR")]
    which: Option<PathBuf>,

    /// The same thing, said the way the rest of rivet says it.
    #[arg(short = 'C', long, value_name = "DIR")]
    dir: Option<PathBuf>,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let asked = cli.dir.or(cli.which).unwrap_or_else(|| PathBuf::from("."));

    // A directory is where a run logged; anything else is taken for the session
    // file itself, which is what the line a run leaves in the terminal on its
    // way out hands over, and what someone who has one from another machine
    // has.
    let path = match asked.is_dir() {
        true => session::path(&asked),
        false => asked.clone(),
    };

    match session::read(&path) {
        Ok(session) => {
            session.show();
            ExitCode::SUCCESS
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => fail(&missing(&asked, &path)),
        Err(error) => fail(&format!("{error}")),
    }
}

/// What to say when there is no session where one was asked for.
///
/// Where it looked, in the words the person used: a directory that is there but
/// has no run in it is a different thing from a path that is not there at all,
/// and only one of them is answered by looking somewhere else.
fn missing(asked: &Path, path: &Path) -> String {
    if !asked.exists() {
        return format!("{}: no such file or directory", asked.display());
    }
    format!(
        "no run to open in {}\n\
         rivet writes {} as it runs, beside the run's {} — point it at the \
         directory the run logged in.",
        asked.display(),
        path.file_name()
            .unwrap_or(session::SESSION.as_ref())
            .display(),
        rivet::log::RUN_LOG,
    )
}

/// Say what went wrong, on stderr, and exit non-zero.
fn fail(message: &str) -> ExitCode {
    eprintln!("rivet: {message}");
    ExitCode::FAILURE
}
