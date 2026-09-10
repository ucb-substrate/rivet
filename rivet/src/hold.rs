//! Tools held at their own prompt after they fail, instead of being let go.
//!
//! A Cadence tool that hits an error in the TCL it was given exits, and takes
//! the design it had in memory with it. What is left behind is the log and
//! whatever db was written last, which answers most questions and none of the
//! ones worth a session of its own: what a `get_db` says about the state the
//! tool was actually in when it stopped, what the timing looked like before
//! anything was rerun, which of the things that were supposed to be true were
//! not. Answering those means the tool still being there.
//!
//! So a tool can be held instead. It stays where it failed, taking commands
//! one line at a time from a fifo rivet made for it, while the step it
//! belonged to fails exactly as it always would: the run carries on, the steps
//! waiting on it are blocked, and its line says what went wrong. What is
//! different is that the tool is still behind that line, with a script beside
//! its logs that attaches a terminal to it — a second terminal, because the
//! display owns the one the run is on.
//!
//! # What the tool has to do
//!
//! Nothing here can hold a tool. All rivet can do is offer somewhere to be
//! held, notice a tool that takes the offer, and keep what is left alive:
//!
//! 1. Before the tool starts, rivet makes a fifo beside the tool's logs and
//!    names it in the [`CONTROL`] environment variable. A tool that finds
//!    nothing there was given nowhere to be held, and must exit on an error as
//!    it always did — the run may be one with no display to say it on, or on a
//!    machine with no fifos.
//! 2. On an error the tool prints [`MARKER`] on its output and then reads
//!    lines from that fifo, evaluating each one and printing what came of it.
//!    It leaves when it reads [`LEAVE`], and exits as the failure it was.
//! 3. rivet sees the marker, gives the step its failure there and then, and
//!    adopts what the step left running: the tool goes on writing to the same
//!    log files, so the conversation with it is in the log the step's page is
//!    already showing.
//!
//! `cadence::hold_tcl` writes exactly that loop into the TCL it generates for
//! Genus and Innovus.
//!
//! # What it costs
//!
//! A held tool is a tool that has not exited: it keeps its licence, its
//! memory and its work directory for as long as it is held. That is worth it
//! for someone who is going to attach to it and not otherwise, so holding is
//! on only when there is a live display to say it on, and [`RIVET_HOLD`] turns
//! it off, or on, by hand.
//!
//! A session lasts as long as the run's display: `x` on a held step lets its
//! tool go, and everything still held is let go when the display is dismissed
//! — which the display asks about first, since somebody may be in the middle
//! of using one. That is not tidiness. Nothing but this process is draining
//! the tool's output, so a tool left running once rivet has gone would fill
//! its pipe and stop dead there, holding a licence, with nobody left to
//! notice; ending it is the only honest thing to do with it. A rivet that is
//! killed outright cannot do even that, and leaves a tool sitting at a prompt
//! nothing will ever write to.

use std::fmt;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use indoc::formatdoc;

use crate::progress::fmt_duration;
use crate::tui::{full_path, quote, signals};

/// The line a tool prints on its output to say it has failed and is holding
/// itself at its own prompt rather than exiting.
///
/// Printed once, when the tool has decided it is not going on. Everything the
/// tool says after it belongs to the session rather than to the step, and is
/// not offered to the step's line — a debug session's output is not progress.
///
/// Matched anywhere in a line, as a substep banner is, so that a tool which
/// prefixes what it prints still gets to say it. A script must therefore not
/// contain it written out: see [`marker_halves`].
pub const MARKER: &str = "<<rivet:held>>";

/// [`MARKER`] in two halves, for a script that has to print it without
/// containing it.
///
/// A Cadence tool echoes the source of the script it is reading as it reads
/// it, so a marker written out in one would be on the tool's output before the
/// tool had run anything at all — and rivet, which looks for it anywhere in a
/// line, would take the reading of the script for the tool failing in it.
/// Assembled from these two, in the script, it can only appear when the script
/// says it.
///
/// Both halves together are [`MARKER`]; neither alone is anything.
pub fn marker_halves() -> (&'static str, &'static str) {
    let at = MARKER.find(':').map(|colon| colon + 1).unwrap_or_default();
    MARKER.split_at(at)
}

/// The line that ends a session, read by the tool from its control channel.
///
/// The word a shell would have used, because a terminal attached to the
/// session is typing into the same channel: someone who types `exit` at a held
/// tool means what rivet means by it.
pub const LEAVE: &str = "exit";

/// The environment variable naming the fifo a tool takes commands from while
/// it is held. Set by rivet on the tool it is offering to hold, and set to
/// nothing otherwise.
pub const CONTROL: &str = "RIVET_CONTROL";

/// Whether to hold a failed tool at its prompt: `1` to hold whatever the run
/// looks like, `0` never to. Unset — the default — holds when the run has a
/// live display, since a session nobody is watching for is a licence held for
/// nothing.
pub const RIVET_HOLD: &str = "RIVET_HOLD";

/// How long a tool asked to let go is given before it is signalled, and again
/// before it is signalled for real.
///
/// Long enough for the way out it was asked to take: leaving the prompt puts a
/// Cadence tool through its own shutdown, which takes a few seconds on a large
/// design even when it goes willingly.
const LET_GO_AFTER: Duration = Duration::from_secs(5);

/// A tool that failed and was held at its own prompt.
///
/// One per tool, for as long as the run lasts. The session outlives the step
/// that started the tool — that is the whole point of it — so it is reached
/// through the step it belonged to, or through [`live`].
pub struct Session {
    /// The step whose tool this was, to say what a terminal is attaching to.
    label: String,
    /// What the tool is: the program the step ran.
    tool: String,
    pid: u32,
    /// The fifo the tool is taking its commands from.
    control: PathBuf,
    /// The script that attaches a terminal to it.
    script: PathBuf,
    /// This end of the control channel, held open: see [`Control::channel`].
    channel: Mutex<File>,
    /// Whether the tool is still there, set by the thread waiting on it.
    alive: AtomicBool,
    /// Whether it has been asked to let go, so it is only asked once however
    /// many times `x` is typed at it.
    ending: AtomicBool,
    since: Instant,
}

impl Session {
    /// The step whose tool this was.
    pub fn label(&self) -> &str {
        &self.label
    }

    /// The program the step ran.
    pub fn tool(&self) -> &str {
        &self.tool
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// The command that attaches a terminal to the session, for pasting into
    /// one — a full path, since the terminal it is pasted into is not
    /// necessarily sitting where the run was started.
    pub fn attach_command(&self) -> String {
        format!("sh {}", quote(&full_path(&self.script)))
    }

    /// Whether the tool is still there.
    pub fn alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }

    /// How long it has been held.
    pub fn held_for(&self) -> Duration {
        self.since.elapsed()
    }

    /// Let the tool go: ask it to leave its prompt, and make it if it will not.
    ///
    /// Nothing waits. The tool's own way out is the one worth taking — it
    /// leaves the loop, ends as the failure it was and writes whatever it
    /// writes on the way — so it is asked first, and given [`LET_GO_AFTER`] to
    /// take it before it is signalled, and as long again before it is
    /// signalled for real. Asking twice does nothing.
    pub fn end(self: &Arc<Self>) {
        if !self.alive() || self.ending.swap(true, Ordering::SeqCst) {
            return;
        }
        tracing::warn!(
            step = %self.label,
            tool = %self.tool,
            pid = self.pid,
            held = %fmt_duration(self.held_for()),
            "letting the held tool go"
        );
        self.ask();

        // On a thread of its own: whoever asked for this — the display, or the
        // end of the run — has something else to be doing.
        let session = Arc::clone(self);
        thread::spawn(move || {
            for hard in [false, true] {
                if !session.wait_for(LET_GO_AFTER) {
                    return;
                }
                tracing::warn!(
                    step = %session.label,
                    pid = session.pid,
                    hard,
                    "the held tool has not gone; signalling it"
                );
                signals::signal_process(session.pid, hard);
            }
        });
    }

    /// Ask the tool to leave its prompt, by writing to the control channel
    /// exactly what an attached terminal would have typed.
    fn ask(&self) {
        let mut channel = self.channel.lock().unwrap();
        let _ = writeln!(channel, "{LEAVE}");
        let _ = channel.flush();
    }

    /// Wait up to `patience` for the tool to go, and say whether it is still
    /// there afterwards.
    fn wait_for(&self, patience: Duration) -> bool {
        let until = Instant::now() + patience;
        while self.alive() && Instant::now() < until {
            thread::sleep(Duration::from_millis(50));
        }
        self.alive()
    }

    /// The tool has gone: nothing more can be attached to it, so the fifo goes
    /// too rather than sitting in the work directory looking like a session.
    fn done(&self, status: io::Result<std::process::ExitStatus>) {
        self.alive.store(false, Ordering::SeqCst);
        let _ = fs::remove_file(&self.control);
        tracing::info!(
            step = %self.label,
            tool = %self.tool,
            pid = self.pid,
            code = status.as_ref().ok().and_then(|status| status.code()),
            held = %fmt_duration(self.held_for()),
            "the held tool has gone"
        );
    }
}

impl fmt::Debug for Session {
    /// What the session is, and not what it is made of: the fifo and the fd
    /// are how it works, and a step's error message is what this ends up in.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Session")
            .field("label", &self.label)
            .field("tool", &self.tool)
            .field("pid", &self.pid)
            .field("alive", &self.alive())
            .finish()
    }
}

/// Every tool the run is holding now.
pub fn live() -> Vec<Arc<Session>> {
    HELD.lock()
        .unwrap()
        .iter()
        .filter(|session| session.alive())
        .cloned()
        .collect()
}

/// Let go of everything still held.
///
/// Called when the run's display has been dismissed, because a tool nothing is
/// left to drain would only stop dead in its own pipe; see [the module
/// docs](self#what-it-costs). Waited for, unlike [`Session::end`]: this is the
/// process on its way out, and a tool still running when it goes is a tool
/// nobody is coming back for.
pub(crate) fn end_all() {
    let held = live();
    // Asked all at once and waited for afterwards, so the wait is one tool's
    // worth however many there are.
    for session in &held {
        session.end();
    }
    for session in &held {
        // The thread `end` left behind is doing the signalling; this only has
        // to be here when it does. `end` gives the tool two goes of
        // `LET_GO_AFTER` and the last of them is a `SIGKILL`, so waiting a
        // moment past that is waiting for something that cannot not happen.
        session.wait_for(2 * LET_GO_AFTER + Duration::from_secs(1));
    }
}

static HELD: LazyLock<Mutex<Vec<Arc<Session>>>> = LazyLock::new(|| Mutex::new(Vec::new()));

/// Somewhere for a tool that is about to run to be held, if anything is to be
/// held at all.
///
/// `None` says nothing will be: the tool is told nothing, and holds itself
/// nowhere. See [`RIVET_HOLD`] for what decides it.
pub(crate) fn control(stdout_log: &Path, stderr_log: &Path) -> io::Result<Option<Control>> {
    if !wanted() {
        return Ok(None);
    }
    Control::open(stdout_log, stderr_log).map(Some)
}

/// Whether a failed tool is to be held: what [`RIVET_HOLD`] says, or, unset,
/// whether there is a live display to say it on.
fn wanted() -> bool {
    wanted_from(knob().as_deref(), crate::progress::showing())
}

/// What [`RIVET_HOLD`] says, which is the environment's to say.
///
/// Except in a test: the environment belongs to the whole process, every other
/// test in it is spawning children that read it, and changing it underneath
/// one of those is a race rather than a way of asking for a hold.
fn knob() -> Option<String> {
    #[cfg(test)]
    if testing::forced() {
        return Some("1".to_string());
    }
    std::env::var(RIVET_HOLD).ok()
}

/// [`wanted`] given what it goes on, which is what makes it something a test
/// can ask about without an opinion on the whole process's environment.
fn wanted_from(knob: Option<&str>, showing: bool) -> bool {
    if !cfg!(unix) {
        return false;
    }
    match knob {
        // Set is the whole of the answer: someone who says to hold a tool in a
        // run with no display has their reasons, and someone who says not to
        // does not want to be asked again.
        Some(knob) => !matches!(
            knob.trim().to_ascii_lowercase().as_str(),
            "" | "0" | "no" | "off" | "false"
        ),
        None => showing,
    }
}

/// A control channel a tool has been offered but has not taken yet.
///
/// Made before the tool starts, because a running process cannot be handed
/// one, and thrown away with the tool that never needed it.
pub(crate) struct Control {
    /// The fifo the tool is told to take its commands from.
    path: PathBuf,
    /// This end of it, open for reading and writing at once — which on Linux
    /// is a fifo that never ends. The tool sees a channel that stays open
    /// however many terminals attach to it and leave, and this is also how the
    /// tool is asked to let go.
    channel: File,
    /// Where the script that attaches a terminal goes, written when the tool
    /// is actually held: it names the tool's pid, and a script for a tool that
    /// never failed would only be a session that was never there.
    script: PathBuf,
    /// The logs the session says to follow: everything the tool says while it
    /// is held goes on going to them.
    logs: Vec<PathBuf>,
    /// Whether the tool took the offer, in which case the fifo is the
    /// session's and is not removed with this.
    taken: bool,
}

impl Control {
    fn open(stdout_log: &Path, stderr_log: &Path) -> io::Result<Control> {
        let path = stdout_log.with_extension("control");
        let channel = mkfifo(&path)?;
        Ok(Control {
            path,
            channel,
            script: stdout_log.with_extension("attach.sh"),
            logs: vec![stdout_log.to_path_buf(), stderr_log.to_path_buf()],
            taken: false,
        })
    }

    /// Offer the channel to the tool `command` is about to run.
    pub(crate) fn offer(&self, command: &mut Command) {
        command.env(CONTROL, &self.path);
    }

    /// Where the channel is, for the log line that says a tool was offered one.
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// The tool has taken the offer: keep it alive as a session of its own.
    ///
    /// `pumps` are the threads writing the tool's output to its logs, which go
    /// on doing so while it is held — a tool whose output nothing drains stops
    /// at its first full pipe — and are waited for by the same thread that
    /// waits for the tool.
    pub(crate) fn hold(
        &mut self,
        label: &str,
        tool: &str,
        mut child: Child,
        pumps: Vec<JoinHandle<()>>,
    ) -> io::Result<Arc<Session>> {
        let session = match self.session(label, tool, child.id()) {
            Ok(session) => session,
            // Nothing came of the session, so there is nothing to reach the
            // tool with — and a tool waiting at a prompt nobody can type at is
            // a licence held for nothing at all. Stopped here rather than left
            // for the end of a run that has no idea it is there.
            Err(error) => {
                tracing::error!(
                    %error,
                    step = %label,
                    pid = child.id(),
                    "cannot hold the tool; stopping it instead"
                );
                signals::signal_process(child.id(), true);
                let _ = child.wait();
                return Err(error);
            }
        };
        // The fifo is the session's now: dropping this must not take it away
        // from the tool that is reading it.
        self.taken = true;

        HELD.lock().unwrap().push(Arc::clone(&session));
        let watched = Arc::clone(&session);
        thread::spawn(move || {
            let status = child.wait();
            // Said before the pumps are waited for: the tool is what a session
            // is, and one whose grandchild is still holding its pipe open has
            // gone all the same.
            watched.done(status);
            for pump in pumps {
                let _ = pump.join();
            }
        });
        Ok(session)
    }

    /// The session itself, or why there cannot be one: this end of the channel
    /// to keep the tool's end open, and the script a terminal is attached with.
    fn session(&self, label: &str, tool: &str, pid: u32) -> io::Result<Arc<Session>> {
        let session = Arc::new(Session {
            label: label.to_string(),
            tool: tool.to_string(),
            pid,
            control: self.path.clone(),
            script: self.script.clone(),
            channel: Mutex::new(self.channel.try_clone()?),
            alive: AtomicBool::new(true),
            ending: AtomicBool::new(false),
            since: Instant::now(),
        });
        write_script(&session, &self.logs)?;
        Ok(session)
    }
}

impl Drop for Control {
    fn drop(&mut self) {
        if self.taken {
            return;
        }
        // The tool it was made for has gone without being held. A fifo left in
        // a work directory would only look like a session that is there.
        let _ = fs::remove_file(&self.path);
    }
}

/// Make the fifo and open this end of it; see [`Control::channel`].
#[cfg(unix)]
fn mkfifo(path: &Path) -> io::Result<File> {
    use rustix::fs::{mknodat, FileType, Mode, CWD};

    // Whatever is there is the last run's, and this run's tool is about to be
    // told to read it.
    let _ = fs::remove_file(path);
    // Nobody else's: a line written here is a command run inside the session,
    // with everything the person who started the run can reach.
    mknodat(CWD, path, FileType::Fifo, Mode::RUSR | Mode::WUSR, 0)
        .map_err(|error| io::Error::from_raw_os_error(error.raw_os_error()))?;
    fs::OpenOptions::new().read(true).write(true).open(path)
}

#[cfg(not(unix))]
fn mkfifo(_path: &Path) -> io::Result<File> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "no fifos to hold a tool with",
    ))
}

/// Write the script that attaches a terminal to `session`.
///
/// A script rather than something rivet does itself, because the terminal it
/// wants is one rivet has never seen: another window, or another ssh session
/// altogether. What it needs is a fifo to write and two files to follow, and
/// `sh` is the shortest way from a person to both.
fn write_script(session: &Session, logs: &[PathBuf]) -> io::Result<()> {
    let control = quote(&full_path(&session.control));
    let logs: Vec<String> = logs.iter().map(|log| quote(&full_path(log))).collect();
    let (label, tool, pid) = (&session.label, &session.tool, session.pid);
    let script = formatdoc! {r#"
        #!/bin/sh
        # Attach a terminal to the {tool} that {label} was running when it
        # failed, which rivet is holding at its own prompt rather than letting
        # go. Written by rivet when it held it; see the `rivet::hold` docs.
        #
        # What you type goes to the tool one line at a time, as commands in its
        # own language. What comes back comes back through its log, which this
        # follows: the command, what it returned, and whatever the tool printed
        # for itself. `^D` leaves the tool where it is, to come back to;
        # `{LEAVE}` lets it go, and it exits as the failure it was. It is let go
        # in any case when the run holding it is quit.
        control={control}
        if [ ! -p "$control" ]; then
            echo "that session has ended: $control is gone" >&2
            exit 1
        fi
        if ! kill -0 {pid} 2>/dev/null; then
            echo "that {tool} has gone (pid {pid})" >&2
            exit 1
        fi
        echo "attached to {tool} (pid {pid}), held where {label} failed" >&2
        echo "type its commands · ^D leaves it held · {LEAVE} lets it go" >&2
        # Quietly, and from here on: which of the two streams a tool chose to
        # say something on means nothing, and a header every time it changes
        # its mind would be most of what a conversation looks like.
        tail -n 0 -q -F {logs} &
        following=$!
        trap 'kill "$following" 2>/dev/null' EXIT HUP INT TERM
        cat > "$control"
        # Which of the two ways out this was: `^D` here, or the tool having
        # been let go from under it.
        sleep 1
        if kill -0 {pid} 2>/dev/null; then
            echo "detached · it is still held · attach again with $0" >&2
        else
            echo "the session has ended" >&2
        fi
    "#, logs = logs.join(" ")};

    fs::write(&session.script, script)?;
    runnable(&session.script)
}

/// Make a script something `sh` will run, where that is a thing a file has to
/// be told to be.
#[cfg(unix)]
fn runnable(script: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut mode = fs::metadata(script)?.permissions();
    mode.set_mode(0o755);
    fs::set_permissions(script, mode)
}

#[cfg(not(unix))]
fn runnable(_script: &Path) -> io::Result<()> {
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// A stand-in for a tool that holds itself, for the tests here and in the
/// modules that show a session; see [`testing::session`].
#[cfg(test)]
pub(crate) mod testing {
    use super::*;
    use std::process::Stdio;

    /// A directory of its own for a test to leave things in.
    pub(crate) fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("rivet-hold-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// The shell a stand-in tool runs: it says it is held, then takes lines
    /// from the control channel and echoes what it was given until it is let
    /// go, which is everything a held tool looks like from rivet's side.
    ///
    /// A shell rather than a Cadence tool for the obvious reason. The protocol
    /// is the whole of what is being tested, and it is the same protocol.
    pub(crate) fn holds_itself() -> Command {
        let mut command = Command::new("bash");
        command.arg("-c").arg(formatdoc! {r#"
            if [ -z "${{{control}:-}}" ]; then
                echo "nowhere to be held"
                exit 3
            fi
            echo "{marker}"
            while IFS= read -r line; do
                [ "$line" = "{leave}" ] && break
                echo "ran $line"
            done < "${{{control}}}"
            echo "let go"
            exit 1
        "#, control = CONTROL, marker = MARKER, leave = LEAVE});
        command
    }

    /// Turn holding on, and keep the run's one registry of held tools to this
    /// test until the guard is dropped.
    ///
    /// Held tools are the run's rather than any one step's — how many are held
    /// is on screen, and letting them all go is what the end of a run does —
    /// so a test that holds one has to be the only test holding one. Every
    /// test that ends up with a session takes this first.
    pub(crate) fn alone() -> Alone {
        // A test that panicked holding this said nothing about the sessions of
        // the ones after it, which are the only thing it was keeping to itself.
        let guard = ONE_AT_A_TIME
            .lock()
            .unwrap_or_else(|held| held.into_inner());
        FORCED.store(true, Ordering::SeqCst);
        Alone(guard)
    }

    /// The lock on holding, held for as long as the test is. The guard is
    /// the whole of it: nothing reads it, and dropping it is what it is for.
    pub(crate) struct Alone(#[allow(dead_code)] std::sync::MutexGuard<'static, ()>);

    impl Drop for Alone {
        fn drop(&mut self) {
            FORCED.store(false, Ordering::SeqCst);
        }
    }

    /// Whether a test is standing in for [`RIVET_HOLD`]; see [`super::knob`].
    pub(crate) fn forced() -> bool {
        FORCED.load(Ordering::SeqCst)
    }

    static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());
    static FORCED: AtomicBool = AtomicBool::new(false);

    /// A session over a stand-in tool, held and ready to be driven. Take
    /// [`alone`] first.
    pub(crate) fn session(dir: &Path, label: &str) -> Arc<Session> {
        let mut control = control(&dir.join("tool.out"), &dir.join("tool.err"))
            .unwrap()
            .expect("holding is on");
        let mut command = holds_itself();
        control.offer(&mut command);
        let child = command
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        control.hold(label, "bash", child, Vec::new()).unwrap()
    }

    /// Wait for a session to end, and say whether it did.
    pub(crate) fn gone(session: &Arc<Session>) -> bool {
        !session.wait_for(3 * LET_GO_AFTER)
    }
}

#[cfg(test)]
mod tests {
    use super::testing::*;
    use super::*;

    /// The shape of a session: a tool that is there, a fifo that reaches it,
    /// and a script that attaches a terminal to it.
    #[test]
    fn a_held_tool_is_there_until_it_is_let_go() {
        let _alone = alone();
        let dir = scratch("session");
        let session = session(&dir, "decoder par");

        assert!(session.alive());
        assert!(session.control.exists(), "no control channel");
        assert!(session.script.exists(), "no attach script");
        assert!(
            live().iter().any(|other| other.pid() == session.pid()),
            "the run does not know it is holding it"
        );
        // The command is the script, in full, for a terminal that is not
        // sitting where the run was started.
        let attach = session.attach_command();
        assert!(attach.starts_with("sh /"), "{attach}");

        session.end();
        assert!(gone(&session), "still there");
        // Nothing left to attach to, so nothing left that looks like it.
        assert!(
            !session.control.exists(),
            "the control channel is still there"
        );
        assert!(live().is_empty(), "the run still thinks it is holding it");
        let _ = fs::remove_dir_all(&dir);
    }

    /// The script is what someone runs in another terminal, so it has to be
    /// runnable, and has to say what it is attaching to and how to leave.
    #[test]
    fn the_attach_script_is_runnable_and_says_what_it_is_for() {
        let _alone = alone();
        let dir = scratch("script");
        let session = session(&dir, "decoder par");

        let script = fs::read_to_string(&session.script).unwrap();
        assert!(script.starts_with("#!/bin/sh"), "{script}");
        assert!(script.contains(&session.pid.to_string()), "{script}");
        assert!(script.contains("decoder par"), "{script}");
        assert!(script.contains(LEAVE), "{script}");
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(&session.script).unwrap().permissions().mode();
        assert_eq!(mode & 0o111, 0o111, "not executable: {mode:o}");
        // `sh -n` reads the whole of it without running any of it, which is
        // the one thing worth checking about a script rivet writes.
        let checked = Command::new("sh")
            .arg("-n")
            .arg(&session.script)
            .status()
            .unwrap();
        assert!(checked.success(), "the script does not parse");

        session.end();
        assert!(gone(&session));
        let _ = fs::remove_dir_all(&dir);
    }

    /// Asking twice is what happens when someone types `x` twice, and must not
    /// turn into two sets of signals chasing one process.
    #[test]
    fn a_session_is_only_ended_once() {
        let _alone = alone();
        let dir = scratch("twice");
        let session = session(&dir, "decoder par");

        session.end();
        session.end();
        assert!(gone(&session));
        let _ = fs::remove_dir_all(&dir);
    }

    /// The halves are the marker and nothing else is, which is the whole
    /// point of them: a script that contains either is a script a tool can
    /// echo without appearing to have failed.
    #[test]
    fn the_marker_is_only_ever_said_by_both_halves() {
        let (first, second) = marker_halves();
        assert_eq!(format!("{first}{second}"), MARKER);
        assert!(!first.is_empty() && !second.is_empty());
        assert!(!first.contains(MARKER) && !second.contains(MARKER));
    }

    /// The knob, which is the whole of the answer when it is set. Asked of
    /// [`wanted_from`] rather than of the environment, which the tests share
    /// one of between them.
    #[test]
    fn holding_is_what_the_environment_says_it_is() {
        for on in ["1", "yes", "on", "true", "always"] {
            assert!(wanted_from(Some(on), false), "{on:?} did not hold");
        }
        for off in ["0", "no", "off", "false", "", " "] {
            assert!(!wanted_from(Some(off), true), "{off:?} did not turn it off");
        }
        // Unset is not "off": it is whether anybody is watching the run.
        assert!(wanted_from(None, true));
        assert!(!wanted_from(None, false));
    }
}
