//! The screen the live display draws on, and the keys typed at it.
//!
//! [`crate::progress`] decides what a run looks like — which steps there are,
//! what each one's line says, how far along the run is. This owns the screen it
//! all appears on: the pages, the cursor's travel through them, the log being
//! read, the keys. The two meet at [`Paint`], which is everything this module
//! knows about a flow.
//!
//! # A screen of its own
//!
//! The display takes the terminal's alternate screen, the way an editor does,
//! and keeps it until it is dismissed. A run that has finished stays on screen
//! to be looked through — every step under the cursor, a failure's log a
//! keypress away — instead of turning into terminal history the moment it
//! ends. `q` gives the screen back, and the run's record is left in the
//! ordinary scrollback then: one line per step, in the order things happened,
//! so the terminal still shows what the run did.
//!
//! While the run is going, the display is where it is controlled from: `q`
//! cancels it, after asking, by sending the run the same interrupt `^C` would
//! — to the whole process group, so the tools the steps are running die with
//! it — and the run ends as an interrupted one. A run that is not wanted on
//! screen at all is started with the display turned off
//! ([`ExecuteConfig::progress`](crate::ExecuteConfig::progress)), and reports
//! plainly instead.
//!
//! There are two pages. The list is every step in the run, in the order the run
//! is expected to take them and staying in it, with a cursor on one of them;
//! `enter`
//! opens that step, which is its log as it is written, with the step's own line
//! and the run's summary underneath; `esc` closes it again. The files a step's
//! page can read — the output of the tool it is running, its own log, and the
//! run's `rivet.log` after them — are a `tab` apart, `L` goes straight to the
//! run's, and `y` copies a command to open one in a terminal of one's own.
//! `L` from the list opens the run's log on a page of its own.
//!
//! # Reading a log
//!
//! Whichever log is open, all of it is there to read, however long it is: a
//! page is a pager, not the last few thousand lines of one. It follows the end
//! as the file grows until it is scrolled off it, and `G` (or `F`) goes back to
//! following; `g` goes to the first line, `space` and `b` move by the screen,
//! and `/` searches — `?` backwards, `n` and `N` on to the next match and back.
//! A search reads the whole file, wrapping round the end, and says in the
//! footer while it is still looking or if it found nothing. What matched is
//! picked out wherever it is on screen.
//!
//! None of this holds the file in memory: see [`Source`].
//!
//! Because the display owns the whole screen, it can be redrawn from nothing at
//! any moment. The terminal being resized redraws it, and so does `^L`, for
//! when something has written over it: a stray `println!` in flow code lands on
//! the alternate screen, and is gone with it.
//!
//! # The terminal's mode
//!
//! Reading a key as it is typed means raw mode, and crossterm's raw mode is
//! `cfmakeraw`, which turns off the signal keys along with everything else.
//! That would be wrong here: `^C` has to stay a signal, because the terminal
//! sends it to the whole foreground process group and the tools a step is
//! running are in that group. Interrupting a run must keep killing them — at
//! once, and whatever state the display is in, which is what makes `^C` the
//! way out of a display that has stopped answering. So `ISIG` is put back
//! immediately afterwards, and `^C` and `^Z` go on meaning what they always
//! did. `q` asks before it cancels, and then sends the group the same signal.
//!
//! What is then left to do is tidy up after them. [`signal_hook`] notices the
//! interrupt, the display gives the terminal back, and the run exits; a second
//! `^C` gives up on being tidy and exits at once. `^Z` is noticed the same way,
//! so that the screen can be handed back before the process stops — otherwise
//! the shell's prompt would land on the alternate screen — and taken again when
//! it is continued.
//!
//! # The wheel
//!
//! The wheel does what `↑` and `↓` do, on either page: moves the cursor
//! through the list, and scrolls an open log. That means asking the terminal
//! to report the mouse, which is asked for along with the screen and given
//! back with it.
//!
//! Asking is not free. A terminal reporting the mouse hands the display every
//! click and drag over it, the drags that would otherwise have selected text
//! included. That is the trade tmux makes with `mouse on`, and it answers it
//! by being a terminal in its own right: it has a selection of its own to put
//! in place of the one it took. So does this — see below — and the hint line
//! says so, because a drag that quietly stopped working would be worse than a
//! wheel that never worked. `shift` is still there for the terminal's own
//! selection, which is what a terminal does with a drag it is told to keep:
//! useful for taking a whole screen at once, and for a terminal whose
//! selection reaches somewhere this one's cannot.
//!
//! # Selecting
//!
//! Dragging selects, and letting go copies. What is selected is the screen —
//! the frame is marked where the drag covers, and read back out of the same
//! buffer when the button comes up — so it is whatever was drawn there: a
//! log's lines, a step's failure in the list, a path out of the banner. There
//! is nothing to teach it about pages, and nothing for a page to remember.
//!
//! The clipboard it reaches is the watcher's, not the run's. Both are asked
//! ([`crate::clipboard`]): an OSC 52 escape sequence, which the terminal
//! answers wherever it is running, and a local tool for the terminals that do
//! not. An EDA run is nearly always watched over ssh, and the first is what
//! carries the text back across it.
//!
//! A log that is following its end would go on scrolling out from under a
//! selection being made from it, so starting one holds the log where it is;
//! putting the selection away — a click, a key, the wheel — lets it follow
//! again.
//!
//! Only normal tracking is asked for, not the any-event tracking that would
//! report the pointer merely moving whether anything wanted it or not: see
//! [`ReportMouse`]. What arrives is the wheel, the buttons, and the drags
//! between them, all of which are things to answer.

use std::collections::VecDeque;
use std::fs::{self, File, Metadata};
use std::io::{self, Read, Seek, SeekFrom, Stderr, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use ratatui::backend::CrosstermBackend;
use ratatui::buffer::Buffer;
use ratatui::crossterm::event::{
    self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent,
    MouseEventKind,
};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style, Stylize};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{
    Block, Borders, List, ListItem, ListState, Paragraph, Scrollbar, ScrollbarOrientation,
    ScrollbarState,
};
use ratatui::{Frame, Terminal};
use unicode_width::UnicodeWidthChar;

use crate::clipboard;
use crate::progress::clean;

/// How often the screen is redrawn. The spinners and the elapsed times move on
/// their own, so this is a frame rate rather than a reaction to events; a key
/// is drawn as soon as it lands.
const FRAME: Duration = Duration::from_millis(100);

/// What a run exits with when it is interrupted, the shell's convention.
pub(crate) const INTERRUPTED: i32 = 130;

/// How long the hint line holds a message before going back to the keys.
const FLASH_FOR: Duration = Duration::from_secs(4);

/// Longer, for a message that has to be read rather than glanced at.
const FLASH_LONG: Duration = Duration::from_secs(12);

/// The name over the list, three rows of block letters.
const WORDMARK: [&str; 3] = [
    "█▀▄ █ █ █ █▀▀ ▀█▀",
    "█▀▄ █ ▀▄▀ █▀▀  █ ",
    "▀ ▀ ▀  ▀  ▀▀▀  ▀ ",
];

/// Fewest rows a terminal needs before the banner gets the wordmark; below
/// this it is one line, and below [`TITLE_ROWS`] nothing at all. The list is
/// what the screen is for, and the banner is not to crowd it out.
const BANNER_ROWS: u16 = 16;
const TITLE_ROWS: u16 = 8;

/// Columns the continuation rows of a wrapped step line are indented by: the
/// width of the cursor and the glyph, so the text lines up under itself.
const HANG: usize = 4;

/// Most rows a step's line may take on its own page.
const MAX_STEP_ROWS: usize = 6;

/// Rows a log scrolls, or steps the cursor moves, per notch of the wheel.
const WHEEL: usize = 3;

/// How many lines apart a log's index puts its anchors. Reaching a line costs
/// a seek to the anchor at or before it and a scan of at most this many lines
/// on from there, so it trades the index's size off against that scan.
const INDEX_EVERY: u64 = 256;

/// How many lines of a log are held decoded around the part being read. Enough
/// that scrolling crosses the edge of it rarely, and that a screen's worth is
/// always in it.
const WINDOW: usize = 4 * INDEX_EVERY as usize;

/// Most of a line kept. A tool that writes a megabyte without a newline has
/// not written a line, and nobody is going to read the end of it on a screen.
const LINE_BYTES: usize = 64 * 1024;

/// Most bytes of a log indexed in one poll: enough that anything of an
/// ordinary size is done before its first frame, little enough that a very
/// long one costs no single frame more than that.
const INDEX_BUDGET: u64 = 64 * 1024 * 1024;

/// How much of a file is read at a time.
const CHUNK: usize = 64 * 1024;

/// How many lines are searched per frame. A search of a long log is spread
/// over as many frames as it takes rather than holding one up; this is how
/// much of it each frame does.
const SEARCH_LINES: u64 = 50_000;

// ---------------------------------------------------------------------------
// What the display is told
// ---------------------------------------------------------------------------

/// A move of the cursor through the list of steps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Motion {
    Up(usize),
    Down(usize),
    First,
    Last,
}

/// The run, as the banner over the list describes it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct About {
    /// The steps the run was asked for, which nothing else waits on.
    pub targets: Vec<String>,
    pub steps: usize,
    pub workers: usize,
    /// Where `rivet.log` is being written, if it is.
    pub log_dir: Option<PathBuf>,
}

/// The list page: every step so far, and how the run is going.
#[derive(Default)]
pub(crate) struct Screen {
    /// The run itself, for the banner.
    pub about: About,
    /// One line per step, in the order they are drawn.
    pub steps: Vec<StepLine>,
    /// Which of them the cursor is on. The list scrolls to keep it in view.
    pub selected: Option<usize>,
    /// The bar and counts.
    pub summary: Line<'static>,
    /// Whether the run is over, and the display only waiting to be dismissed.
    pub done: bool,
}

/// One step's line in the list.
pub(crate) struct StepLine {
    /// The id the cursor knows the step by.
    pub id: usize,
    pub line: Line<'static>,
    /// Whether the line is still changing. A running step's line is drawn on
    /// one row and cut at the edge, so the list does not jump as its status
    /// changes length; a finished step's line is settled, and can be wrapped
    /// so that all of it — a failure's message most of all — can be read.
    pub running: bool,
}

/// One step, for its own page.
pub(crate) struct Detail {
    pub label: String,
    /// The step's line, as it appears in the list.
    pub line: Line<'static>,
    /// Every file the step has written or is writing, the most useful first.
    pub files: Vec<PathBuf>,
    /// What a command to read the step's log should open: the files the tool it
    /// is running is writing now, or its own log until it runs one.
    pub follow: Vec<PathBuf>,
    /// Whether the step is still going, which is what decides whether a file
    /// that is not there yet is worth waiting for.
    pub running: bool,
    /// Whether the step has yet to start, which is why it has no files.
    pub pending: bool,
}

/// What the display draws, and what it does when a key is typed.
///
/// Implemented by [`crate::progress::Reporter`], and held weakly: the drawing
/// must not be what keeps a finished run alive.
pub(crate) trait Paint: Send + Sync {
    /// What the list page should show now, on a terminal `width` columns wide.
    ///
    /// The width is for fitting: a line that fits is drawn as it is, and one
    /// that does not is made to.
    fn screen(&self, width: usize) -> Screen;

    /// One step, by the id its line in the list carried, for its own page on a
    /// terminal `width` columns wide.
    fn detail(&self, id: usize, width: usize) -> Option<Detail>;

    /// Move the cursor through the list.
    fn move_cursor(&self, motion: Motion);

    /// Put the cursor on one step.
    fn select(&self, id: usize);

    /// Whether the run is over.
    fn done(&self) -> bool;

    /// Stop drawing. Everything that has happened so far comes back as lines
    /// for the terminal's scrollback, and everything after this is reported
    /// there directly.
    fn detach(&self) -> Vec<Line<'static>>;
}

// ---------------------------------------------------------------------------
// The terminal
// ---------------------------------------------------------------------------

/// The terminal, for as long as a run is drawing on it.
pub(crate) struct Tui {
    stage: Arc<Stage>,
    painter: Weak<dyn Paint>,
    thread: Option<JoinHandle<()>>,
}

impl Tui {
    /// Take the screen and start drawing `painter` on it.
    ///
    /// Returns `None` if the terminal will not have it, in which case the run
    /// falls back to plain line-by-line logging.
    pub(crate) fn start(painter: Weak<dyn Paint>) -> Option<Tui> {
        // A terminal that says it has no rows is one nothing can be drawn on.
        // Terminals do report this — a pty whose size was never set is the
        // usual way — so it is asked before anything else happens.
        if !has_room() {
            return None;
        }
        enable_raw_mode().ok()?;
        signals::keep_keys();
        if execute!(io::stderr(), EnterAlternateScreen, ReportMouse(true)).is_err() {
            let _ = disable_raw_mode();
            return None;
        }

        let terminal = match Terminal::new(CrosstermBackend::new(io::stderr())) {
            Ok(terminal) => terminal,
            Err(_) => {
                // The mouse was asked for along with the screen, and goes back
                // with it.
                let _ = execute!(io::stderr(), ReportMouse(false), LeaveAlternateScreen);
                let _ = disable_raw_mode();
                return None;
            }
        };

        let interrupted = Arc::new(AtomicBool::new(false));
        let stopped = Arc::new(AtomicBool::new(false));
        let resumed = Arc::new(AtomicBool::new(false));
        let stage = Arc::new(Stage {
            terminal: Mutex::new(terminal),
            closed: AtomicBool::new(false),
            // Asked for along with the screen, just above.
            mouse: AtomicBool::new(true),
            signals: Mutex::new(signals::catch(&interrupted, &stopped, &resumed)),
            interrupted,
            stopped,
            resumed,
        });

        let thread = thread::Builder::new()
            .name("rivet-tui".into())
            .spawn({
                let (stage, painter) = (Arc::clone(&stage), painter.clone());
                move || run_loop(&stage, &painter)
            })
            .ok();
        if thread.is_none() {
            stage.close(Vec::new());
            return None;
        }

        Some(Tui {
            stage,
            painter,
            thread,
        })
    }

    /// Whether an interrupt has arrived.
    ///
    /// For the run to ask as it ends: a run cut short by `^C` ends the moment
    /// the tools it killed report back, which can be before the display has
    /// had a chance to notice the signal itself, and it must still end as an
    /// interrupted run rather than a finished one.
    pub(crate) fn interrupted(&self) -> bool {
        self.stage.interrupted.load(Ordering::SeqCst)
    }

    /// Wait for the display to be dismissed, and make sure the terminal has
    /// been handed back once it is.
    ///
    /// Ordinarily the display hands it back itself, record and all, on its way
    /// out. This is for the run to wait on, and for the one case where it did
    /// not get to.
    pub(crate) fn wait(mut self) {
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        if !self.stage.closed.load(Ordering::SeqCst) {
            let record = self
                .painter
                .upgrade()
                .map(|paint| paint.detach())
                .unwrap_or_default();
            self.stage.close(record);
        }
    }

    /// Run `f` with the screen put away and the terminal in its ordinary mode,
    /// then take the screen again.
    pub(crate) fn suspend<R>(&self, f: impl FnOnce() -> R) -> R {
        let mut terminal = self.stage.hold();
        if self.stage.closed.load(Ordering::SeqCst) {
            return f();
        }
        let _ = terminal.show_cursor();
        // Whatever runs next is not to be sent mouse reports.
        self.stage.set_mouse(false);
        let _ = execute!(io::stderr(), LeaveAlternateScreen);
        let _ = disable_raw_mode();

        let result = f();

        self.stage.retake(&mut terminal);
        result
    }
}

/// Ask the terminal to report the mouse, or to stop.
///
/// Three modes, and the choice of them is the whole point of not using
/// crossterm's [`EnableMouseCapture`]:
///
/// - `1000`, normal tracking: the buttons and the wheel.
/// - `1002`, button-event tracking: motion **while a button is held**, which
///   is what a drag is. Without it a drag is invisible — the press arrives,
///   then the release, and nothing in between — so a selection could never
///   grow past the cell it started on.
/// - `1006`, SGR encoding: coordinates that stay unambiguous past column 223.
///
/// What is left out is `1003`, any-event tracking, which reports the pointer
/// moving whether a button is down or not. The display is up for as long as
/// the run is, and a flow that takes an hour would spend it being handed
/// events about a pointer that is not doing anything. `1000` and `1002`
/// together report only what someone is actually doing to the display, which
/// is the same set tmux asks for with `mouse on`.
///
/// [`EnableMouseCapture`]: ratatui::crossterm::event::EnableMouseCapture
struct ReportMouse(bool);

impl ratatui::crossterm::Command for ReportMouse {
    fn write_ansi(&self, f: &mut impl std::fmt::Write) -> std::fmt::Result {
        f.write_str(if self.0 {
            concat!("\x1b[?1000h", "\x1b[?1002h", "\x1b[?1006h")
        } else {
            // Undone in the order they were done.
            concat!("\x1b[?1006l", "\x1b[?1002l", "\x1b[?1000l")
        })
    }

    /// Windows has no such modes: its console is either reporting the mouse or
    /// it is not, which is what crossterm's own commands set.
    #[cfg(windows)]
    fn execute_winapi(&self) -> io::Result<()> {
        use ratatui::crossterm::event::{DisableMouseCapture, EnableMouseCapture};
        use ratatui::crossterm::Command;
        if self.0 {
            EnableMouseCapture.execute_winapi()
        } else {
            DisableMouseCapture.execute_winapi()
        }
    }
}

/// The terminal, and whose turn it is with it.
///
/// Holding it is what it means to have the terminal: the display's thread takes
/// it for the moment a frame or a key takes, and [`Tui::suspend`] for as long as
/// it needs.
struct Stage {
    terminal: Mutex<Terminal<CrosstermBackend<Stderr>>>,
    /// Set once the terminal has been handed back. Nothing draws after that.
    closed: AtomicBool,
    /// Whether the terminal is reporting the mouse. True for as long as the
    /// display has the screen. See [`Stage::set_mouse`].
    mouse: AtomicBool,
    signals: Mutex<Vec<signals::Id>>,
    /// Set by the signals, read by the display's thread. Acting on them —
    /// redrawing, restoring a terminal — is far more than a signal handler may
    /// do.
    interrupted: Arc<AtomicBool>,
    stopped: Arc<AtomicBool>,
    resumed: Arc<AtomicBool>,
}

type Term = Terminal<CrosstermBackend<Stderr>>;

impl Stage {
    fn hold(&self) -> MutexGuard<'_, Term> {
        // A panic while the terminal was held must not leave the run unable to
        // give it back.
        self.terminal
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Start or stop the terminal reporting the mouse, unless it already is
    /// as `want`.
    ///
    /// Asked for as long as the display has the screen, and given back
    /// whenever the screen is — a suspended run's shell is not to be sent
    /// mouse reports.
    ///
    /// The terminal must be held: this writes to the stream the frames are
    /// drawn on.
    fn set_mouse(&self, want: bool) {
        if self.mouse.load(Ordering::SeqCst) == want {
            return;
        }
        // A terminal that will not report the mouse is one the wheel does not
        // work on. The keys still scroll, so there is nothing to say about it.
        let asked = execute!(io::stderr(), ReportMouse(want));
        self.mouse.store(want && asked.is_ok(), Ordering::SeqCst);
    }

    /// Hand the terminal back as it was found, leaving `record` in its
    /// scrollback. Done once; asking again does nothing.
    fn close(&self, record: Vec<Line<'static>>) {
        let mut terminal = self.hold();
        if self.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        let _ = terminal.show_cursor();
        self.set_mouse(false);
        let _ = execute!(io::stderr(), LeaveAlternateScreen);
        let _ = disable_raw_mode();
        // The signals are the shell's again.
        signals::forget(std::mem::take(&mut *self.signals.lock().unwrap()));
        print_record(&record);
    }

    /// Take the screen again after something else has had the terminal.
    ///
    /// Whatever ran has changed the mode and drawn where it liked, so nothing
    /// is assumed: the mode is set again, the screen taken again, and the next
    /// frame draws everything.
    fn retake(&self, terminal: &mut Term) {
        if enable_raw_mode().is_ok() {
            signals::keep_keys();
        }
        // The mouse was given back before the terminal was, and this is a
        // fresh mode, so it is asked for again here rather than assumed.
        self.mouse.store(false, Ordering::SeqCst);
        let _ = execute!(io::stderr(), EnterAlternateScreen);
        self.set_mouse(true);
        let _ = terminal.clear();
    }

    /// Hand the terminal back and stop, for a `^Z`, then take it again once
    /// continued.
    fn pause(&self, terminal: &mut Term) {
        let _ = terminal.show_cursor();
        self.set_mouse(false);
        let _ = execute!(io::stderr(), LeaveAlternateScreen);
        let _ = disable_raw_mode();
        signals::stop_now();
        // Back: the shell had the terminal in the meantime, so nothing on
        // screen is ours and nothing is still set up.
        self.retake(terminal);
    }
}

/// Whether the terminal has any room to draw in.
fn has_room() -> bool {
    ratatui::crossterm::terminal::size().is_ok_and(|(columns, rows)| columns > 0 && rows > 0)
}

// ---------------------------------------------------------------------------
// The loop
// ---------------------------------------------------------------------------

/// Draw frames, and act on keys between them, until the display is dismissed.
fn run_loop(stage: &Stage, painter: &Weak<dyn Paint>) {
    let mut view = View::default();
    let mut next_frame = Instant::now();

    loop {
        // The signals first, ahead of anything the keys asked for.
        if stage.interrupted.load(Ordering::SeqCst) {
            // The tools this run started have had the same signal from the
            // terminal already; all that is left is to hand the terminal back
            // before the run goes. Once the run is over an interrupt is just a
            // way of leaving, and the run ends the way it ended.
            let paint = painter.upgrade();
            let done = paint.as_ref().is_some_and(|paint| paint.done());
            let record = paint.map(|paint| paint.detach()).unwrap_or_default();
            stage.close(record);
            if !done {
                std::process::exit(INTERRUPTED);
            }
            return;
        }
        if stage.stopped.swap(false, Ordering::SeqCst) {
            stage.pause(&mut stage.hold());
            stage.resumed.store(false, Ordering::SeqCst);
        }
        if stage.resumed.swap(false, Ordering::SeqCst) {
            stage.retake(&mut stage.hold());
        }

        // The run this was drawing has gone without saying so; there is nothing
        // left to draw.
        let Some(paint) = painter.upgrade() else {
            stage.close(Vec::new());
            return;
        };

        // Keys, until it is time for a frame. `poll` waits without taking
        // anything: the terminal can be given away between a key arriving and
        // this being able to read it.
        let wait = next_frame.saturating_duration_since(Instant::now());
        match event::poll(wait) {
            Ok(true) => {
                let event = {
                    let _terminal = stage.hold();
                    // Asked again now that the terminal is this thread's: if it
                    // was suspended in between, whatever was typed belongs to
                    // whatever had it, and has already been read by it.
                    match event::poll(Duration::ZERO) {
                        Ok(true) => event::read().ok(),
                        _ => None,
                    }
                };
                if let Some(event) = event {
                    let at_once = worth_a_frame(&event);
                    match view.event(event, &*paint) {
                        Action::None => {}
                        Action::Redraw => {
                            let _ = stage.hold().clear();
                        }
                        Action::Copy(files) => view.copy(stage, &files),
                        Action::Quit => {
                            let record = paint.detach();
                            stage.close(record);
                            return;
                        }
                        // The interrupt reaches this process too, and is dealt
                        // with at the top of the loop like any other.
                        Action::Cancel => {
                            tracing::warn!("run cancelled from the display");
                            signals::interrupt_run(&stage.interrupted);
                        }
                    }
                    // Drawn straight away, so the key is seen to land — but
                    // not for the pointer merely moving over the screen, which
                    // nothing on it answers.
                    if at_once {
                        next_frame = Instant::now();
                    }
                }
            }
            Ok(false) => {}
            // Keys cannot be read. The display still can be drawn, so it goes
            // on, but without spinning.
            Err(_) => thread::sleep(wait),
        }

        if Instant::now() >= next_frame {
            let mut terminal = stage.hold();
            if stage.closed.load(Ordering::SeqCst) {
                return;
            }
            let copied = view.draw(&mut terminal, &*paint);
            next_frame = Instant::now() + FRAME;
            if let Some(text) = copied {
                // The terminal is held, which is what asking it to copy needs,
                // and the frame saying so is worth drawing at once.
                view.copy_text(&text);
                next_frame = Instant::now();
            }
        }
    }
}

/// Whether an event has earned a frame of its own, rather than waiting for the
/// next one.
///
/// Nearly everything has: a key is to be seen to land, and a drag pulls the
/// selection along behind it, which is worth watching happen rather than
/// catching up ten times a second. The pointer merely moving is the exception
/// — it changes nothing on screen, and a terminal that reports it reports it
/// continuously, which would redraw the display as fast as a hand could move.
/// [`ReportMouse`] does not ask for it, and this is what would keep a terminal
/// that sent it anyway from being able to spin the display.
fn worth_a_frame(event: &Event) -> bool {
    !matches!(
        event,
        Event::Mouse(MouseEvent {
            kind: MouseEventKind::Moved,
            ..
        })
    )
}

// ---------------------------------------------------------------------------
// The pages
// ---------------------------------------------------------------------------

/// What a key asks the loop to do that the view cannot do itself.
enum Action {
    None,
    /// Draw everything again from nothing.
    Redraw,
    /// Copy a command to read these files.
    Copy(Vec<PathBuf>),
    /// Give the screen back, the run being over.
    Quit,
    /// Cancel the run, confirmed.
    Cancel,
}

/// A search being typed, which is what the hint line says while it is.
struct Prompt {
    text: String,
    backwards: bool,
}

/// Everything about the display that is not the run: which page is open, how
/// far the list has scrolled, what the hint line is saying.
#[derive(Default)]
struct View {
    page: Page,
    list: ListState,
    /// Rows the list had last frame, which is how far a page key moves.
    list_rows: usize,
    /// Whether the list had a scrollbar last frame, which takes a column off
    /// what its lines can use.
    list_scrollbar: bool,
    /// The column this frame's scrollbar took, if it had one. A selection
    /// leaves it out: a scrollbar is furniture rather than text, and dragging
    /// across a log to the edge of the screen should not paste a column of
    /// scrollbar into whatever it is pasted into.
    scrollbar: Option<u16>,
    /// The step under the cursor as of the last frame, by id.
    selected: Option<usize>,
    /// Something to say on the hint line, until the moment it expires.
    flash: Option<(Line<'static>, Instant)>,
    /// `q` was pressed while the run was going, and the next key decides
    /// whether the run is cancelled.
    confirming: bool,
    /// Text being selected with the mouse, or just selected.
    selection: Option<Selection>,
    /// A search being typed, which has the keyboard until it is done.
    prompt: Option<Prompt>,
}

#[derive(Default)]
enum Page {
    #[default]
    List,
    /// A log being read: a step's, or the run's own.
    Log(Box<Pager>),
}

/// The keys that move within a log or search it, which both pages that read
/// one share. `less`'s, where `less` has one.
///
/// Returns the search to open a prompt for, if the key asked for one: the
/// prompt belongs to the display rather than to the page under it.
fn log_key(code: KeyCode, ctrl: bool, page: isize, pane: &mut Pane) -> Option<Prompt> {
    use KeyCode::*;
    match code {
        Up | Char('k') => pane.scroll(-1),
        Down | Char('j') => pane.scroll(1),
        PageUp | Char('b') => pane.scroll(-page),
        PageDown | Char(' ') => pane.scroll(page),
        Char('u') if ctrl => pane.scroll(-(page / 2).max(1)),
        Char('d') if ctrl => pane.scroll((page / 2).max(1)),
        Home | Char('g') => pane.scroll_to_top(),
        End | Char('G') | Char('F') => pane.follow(),
        Char('n') => pane.again(true),
        Char('N') => pane.again(false),
        Char('/') => {
            return Some(Prompt {
                text: String::new(),
                backwards: false,
            })
        }
        Char('?') => {
            return Some(Prompt {
                text: String::new(),
                backwards: true,
            })
        }
        _ => {}
    }
    None
}

impl View {
    fn event(&mut self, event: Event, paint: &dyn Paint) -> Action {
        match event {
            // A terminal that reports releases as well as presses would
            // otherwise act twice per keystroke.
            Event::Key(key) if key.kind == KeyEventKind::Press => self.key(key, paint),
            Event::Mouse(mouse) => self.mouse(mouse, paint),
            // Everything moves, so whatever was selected is not where it was.
            // The next frame measures the terminal again and draws it all.
            Event::Resize(..) => {
                self.deselect();
                Action::None
            }
            _ => Action::None,
        }
    }

    /// What the mouse does: the wheel moves, and dragging selects.
    ///
    /// The wheel does on either page what `↑` and `↓` do — moves the cursor
    /// through the list, scrolls an open log — and moving is enough to mean
    /// the selection is finished with, so it clears one.
    ///
    /// A drag selects the screen it is drawn over, and letting go copies. A
    /// click that goes nowhere is how a selection is put away again. Nothing
    /// on screen is meant to be clicked otherwise, so no other button does
    /// anything.
    fn mouse(&mut self, mouse: MouseEvent, paint: &dyn Paint) -> Action {
        let at = (mouse.column, mouse.row);
        match mouse.kind {
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                let up = mouse.kind == MouseEventKind::ScrollUp;
                self.deselect();
                let by = WHEEL as isize;
                match self.pane() {
                    Some(pane) => pane.scroll(if up { -by } else { by }),
                    None => paint.move_cursor(if up {
                        Motion::Up(WHEEL)
                    } else {
                        Motion::Down(WHEEL)
                    }),
                }
            }
            MouseEventKind::Down(MouseButton::Left) => {
                // A log that is following its end would go on moving under the
                // selection, so it is held where it is until the selection is
                // done with.
                self.deselect();
                if let Some(pane) = self.pane() {
                    pane.pin();
                }
                self.selection = Some(Selection::new(at));
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if let Some(selection) = &mut self.selection {
                    if selection.dragging {
                        selection.head = at;
                    }
                }
            }
            MouseEventKind::Up(MouseButton::Left) => {
                if let Some(selection) = &mut self.selection {
                    // Where the button came up is the far end, whether or not
                    // the drags on the way were reported: a terminal that
                    // sends none still says where the release was.
                    selection.head = at;
                    selection.dragging = false;
                    if selection.head == selection.anchor {
                        // A click that went nowhere is how a selection is put
                        // away, rather than a selection of the one cell.
                        self.deselect();
                    } else {
                        // Read off the next frame, which is the one that knows
                        // what the selected cells say.
                        selection.copy = true;
                    }
                }
            }
            _ => {}
        }
        Action::None
    }

    /// Put a selection away, and let a log follow its end again.
    fn deselect(&mut self) {
        if self.selection.take().is_some() {
            if let Some(pane) = self.pane() {
                pane.unpin();
            }
        }
    }

    /// The log on the open page, if the page is reading one.
    fn pane(&mut self) -> Option<&mut Pane> {
        match &mut self.page {
            Page::List => None,
            Page::Log(pager) => Some(&mut pager.pane),
        }
    }

    fn key(&mut self, key: KeyEvent, paint: &dyn Paint) -> Action {
        use KeyCode::*;
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

        // Whatever the key turns out to do, typing means the selection on
        // screen is finished with.
        self.deselect();

        // A search being typed has the keyboard: every key is the pattern,
        // until `enter` runs it or `esc` puts it away.
        if let Some(prompt) = &mut self.prompt {
            match key.code {
                Esc => self.prompt = None,
                Enter => {
                    let prompt = self.prompt.take().expect("a prompt is open");
                    if let Some(pane) = self.pane() {
                        pane.look(&prompt.text, prompt.backwards);
                    }
                }
                Backspace => {
                    prompt.text.pop();
                    // Backspacing away the last of it is a way of changing
                    // one's mind, as it is in a shell.
                    if prompt.text.is_empty() {
                        self.prompt = None;
                    }
                }
                Char(c) if !ctrl => prompt.text.push(c),
                _ => {}
            }
            return Action::None;
        }

        // The answer to "cancel the run?": `y` (or `q` again) does, and any
        // other key is a no that is otherwise ignored.
        if self.confirming {
            self.confirming = false;
            return match key.code {
                Char('y') | Char('Y') | Char('q') | Enter => Action::Cancel,
                _ => Action::None,
            };
        }

        match key.code {
            Char('l') if ctrl => return Action::Redraw,
            // The run's log, from wherever: what the run says about itself
            // belongs to no one step, and is as much worth reading beside a
            // tool's output as from the list. On a page that can already
            // reach it, this is the key that goes straight to it.
            Char('L') => {
                match &mut self.page {
                    Page::Log(pager) => pager.open_run(),
                    Page::List => self.page = Page::Log(Box::new(Pager::run())),
                }
                return Action::None;
            }
            // Once the run is over there is nothing to cancel, and `q` is just
            // the way out. Before that, it is a question first.
            Char('q') if paint.done() => return Action::Quit,
            Char('q') => {
                self.confirming = true;
                return Action::None;
            }
            _ => {}
        }

        match &mut self.page {
            Page::List => {
                let page = self.list_rows.max(1);
                match key.code {
                    Up | Char('k') => paint.move_cursor(Motion::Up(1)),
                    Down | Char('j') => paint.move_cursor(Motion::Down(1)),
                    PageUp => paint.move_cursor(Motion::Up(page)),
                    PageDown => paint.move_cursor(Motion::Down(page)),
                    Char('u') if ctrl => paint.move_cursor(Motion::Up(page.div_ceil(2))),
                    Char('d') if ctrl => paint.move_cursor(Motion::Down(page.div_ceil(2))),
                    Home | Char('g') => paint.move_cursor(Motion::First),
                    End | Char('G') => paint.move_cursor(Motion::Last),
                    Enter | Right | Char('l') => {
                        if let Some(id) = self.selected {
                            self.page = Page::Log(Box::new(Pager::step(id)));
                        }
                    }
                    Char('y') => {
                        let files = self
                            .selected
                            .and_then(|id| paint.detail(id, usize::MAX))
                            .map(|detail| detail.follow)
                            .unwrap_or_default();
                        return Action::Copy(files);
                    }
                    _ => {}
                }
            }
            Page::Log(pager) => {
                let page = pager.pane.page();
                match key.code {
                    Esc | Backspace | Left | Char('h') => {
                        // Back to where the list was left, on the step that
                        // was open: the cursor may have moved on by itself in
                        // the meantime, if that step finished.
                        if let Some(id) = pager.step {
                            paint.select(id);
                        }
                        self.page = Page::List;
                    }
                    Tab | Char(']') => pager.next_file(1),
                    BackTab | Char('[') => pager.next_file(-1),
                    Char('y') => return Action::Copy(pager.pane.files()),
                    code => self.prompt = log_key(code, ctrl, page, &mut pager.pane),
                }
            }
        }
        Action::None
    }

    /// Copy a command for reading `files` in full.
    ///
    /// This screen shows the end of a log as it grows. What someone wants
    /// beyond that is the whole of it, in a terminal of their own, and this
    /// hands over the command for it.
    fn copy(&mut self, stage: &Stage, files: &[PathBuf]) {
        if files.is_empty() {
            self.flash(
                Line::from(span("  nothing to read yet", Style::new().yellow())),
                FLASH_FOR,
            );
            return;
        }
        let command = view_command(files);
        tracing::info!(%command, "copied a command to read the log");

        // Held still while it writes: asking the terminal to copy is an escape
        // sequence on the same stream the display draws on.
        let copied = {
            let _terminal = stage.hold();
            clipboard::copy(&command)
        };
        if copied {
            self.flash(
                Line::from(vec![
                    span("  ✔ copied ", Style::new().green().bold()),
                    span(command, Style::new().dim()),
                ]),
                FLASH_FOR,
            );
        } else {
            // Nowhere to copy it to, so leave it on screen long enough to be
            // read off.
            self.flash(
                Line::from(vec![
                    span("  no clipboard to copy to: ", Style::new().yellow()),
                    span(command, Style::new()),
                ]),
                FLASH_LONG,
            );
        }
    }

    /// Put selected text on the clipboard, and say what went.
    ///
    /// Unlike [`Self::copy`], the terminal is already held by the frame this
    /// follows, so it is not asked for again.
    fn copy_text(&mut self, text: &str) {
        let lines = text.lines().count();
        let copied = clipboard::copy(text);
        tracing::info!(lines, copied, "copied a selection");

        // One line is short enough to show back, which is the clearest way of
        // saying what was caught; more than one is counted instead.
        let what = match text.lines().next() {
            Some(line) if lines == 1 => format!("\"{}\"", clip(line, 48)),
            _ => format!("{lines} lines"),
        };
        if copied {
            self.flash(
                Line::from(vec![
                    span("  ✔ copied ", Style::new().green().bold()),
                    span(what, Style::new().dim()),
                ]),
                FLASH_FOR,
            );
        } else {
            self.flash(
                Line::from(span(
                    format!("  no clipboard to copy {what} to"),
                    Style::new().yellow(),
                )),
                FLASH_FOR,
            );
        }
    }

    fn flash(&mut self, line: Line<'static>, for_: Duration) {
        self.flash = Some((line, Instant::now() + for_));
    }

    /// The bottom line: the question being asked, what was just done, or what
    /// the keys do.
    fn hint(&mut self, done: bool, width: usize) -> Line<'static> {
        if self.confirming {
            return Line::from(span(confirm_text(width), Style::new().yellow().bold()));
        }
        if let Some((line, until)) = &self.flash {
            if Instant::now() < *until {
                return line.clone();
            }
            self.flash = None;
        }
        // A search being typed is what the line is for while it is being
        // typed: the keys it would otherwise list all belong to the pattern.
        if let Some(prompt) = &self.prompt {
            let mark = if prompt.backwards { '?' } else { '/' };
            return Line::from(vec![
                span(format!("  {mark}{}", prompt.text), Style::new()),
                span("▌", Style::new().dim()),
            ]);
        }
        let page = match &self.page {
            Page::List => Hint::List,
            Page::Log(pager) if pager.step.is_some() => Hint::Step,
            Page::Log(_) => Hint::Run,
        };
        Line::from(span(hint_text(page, done, width), Style::new().dim()))
    }

    /// Draw a frame, and hand back any text the selection on it has just been
    /// let go of over.
    ///
    /// The selection is marked on the frame after the page has drawn, so that
    /// it picks out whatever ended up there, and read back from the same
    /// frame: what is copied is what was on screen to be seen.
    fn draw(&mut self, terminal: &mut Term, paint: &dyn Paint) -> Option<String> {
        let width = terminal
            .size()
            .map(|size| size.width as usize)
            .unwrap_or(80)
            .max(1);
        let screen = paint.screen(width - usize::from(self.list_scrollbar));
        // For a step's page, what the step says about itself — `None` for the
        // run's page, which is nobody's step, and `Some(None)` for a step that
        // is no longer in the run.
        let detail = match &self.page {
            Page::Log(pager) => pager.step.map(|id| paint.detail(id, width)),
            Page::List => None,
        };
        let log = matches!(self.page, Page::Log(_));
        let mut copied = None;
        let _ = terminal.draw(|frame| {
            match log {
                true => self.draw_log(frame, screen, detail),
                false => self.draw_list(frame, screen),
            }
            let skip = self.scrollbar;
            if let Some(selection) = &mut self.selection {
                copied = selection.mark(frame.buffer_mut(), skip);
            }
        });
        copied
    }

    /// The list page: the banner, the steps, the summary, the hint.
    fn draw_list(&mut self, frame: &mut Frame, screen: Screen) {
        let banner = banner_lines(&screen.about, frame.area().height);
        let [banner_area, list_area, summary_area, hint_area] = Layout::vertical([
            Constraint::Length(banner.len() as u16),
            Constraint::Fill(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .areas(frame.area());
        frame.render_widget(Paragraph::new(Text::from(banner)), banner_area);

        self.list_rows = list_area.height as usize;
        self.selected = screen
            .selected
            .and_then(|index| screen.steps.get(index))
            .map(|step| step.id);
        self.list.select(screen.selected);

        let count = screen.steps.len();
        let width = list_area.width as usize;
        let items = screen.steps.into_iter().map(|step| {
            if step.running {
                ListItem::new(step.line)
            } else {
                ListItem::new(Text::from(wrap_line(&step.line, width, HANG)))
            }
        });
        frame.render_stateful_widget(List::new(items), list_area, &mut self.list);
        self.scrollbar = scrollbar(frame, list_area, count, self.list.offset());
        self.list_scrollbar = self.scrollbar.is_some();

        frame.render_widget(Paragraph::new(screen.summary), summary_area);
        let hint = self.hint(screen.done, hint_area.width as usize);
        frame.render_widget(Paragraph::new(hint), hint_area);
    }

    /// A log's page: the log itself, the step's line under it if it is a
    /// step's page, and the run's summary under that.
    fn draw_log(&mut self, frame: &mut Frame, screen: Screen, detail: Option<Option<Detail>>) {
        // The step's line in full, wrapped, however long a failure's message
        // made it — within reason, so the log keeps most of the screen.
        let width = frame.area().width as usize;
        let step_rows: Vec<Line<'static>> = detail
            .as_ref()
            .and_then(|detail| detail.as_ref())
            .map(|detail| wrap_line(&detail.line, width, HANG))
            .unwrap_or_default()
            .into_iter()
            .take(MAX_STEP_ROWS)
            .collect();
        // A step's page keeps the row even when the step has gone, to say so.
        let step_height = match detail.is_some() {
            true => step_rows.len().max(1) as u16,
            false => 0,
        };
        let [log_area, step_area, summary_area, hint_area] = Layout::vertical([
            Constraint::Fill(1),
            Constraint::Length(step_height),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .areas(frame.area());

        let run = run_log(&screen.about);
        if let Page::Log(pager) = &mut self.page {
            let detail = detail.flatten();
            let files = detail.as_ref().map(|detail| &*detail.files).unwrap_or(&[]);
            pager.sync(files, run);
            match (&detail, pager.step) {
                // A step's page whose step is no longer in the run.
                (None, Some(_)) => {
                    frame.render_widget(
                        Paragraph::new(span("  no such step", Style::new().dim())),
                        log_area,
                    );
                    self.scrollbar = None;
                }
                _ => {
                    self.scrollbar = pager.draw(frame, log_area, detail.as_ref());
                    frame.render_widget(Paragraph::new(Text::from(step_rows)), step_area);
                }
            }
        }

        frame.render_widget(Paragraph::new(screen.summary), summary_area);
        let hint = self.hint(screen.done, hint_area.width as usize);
        frame.render_widget(Paragraph::new(hint), hint_area);
    }
}

/// The run's own log, where the run said it was logging — which is where the
/// banner says it is. A run told not to log has none.
fn run_log(about: &About) -> Option<PathBuf> {
    about
        .log_dir
        .as_ref()
        .map(|dir| dir.join(crate::log::RUN_LOG))
}

/// The banner over the list, for a terminal `height` rows tall: the wordmark
/// with the run's facts beside it, a single line where there is less room,
/// nothing where there is none.
fn banner_lines(about: &About, height: u16) -> Vec<Line<'static>> {
    let targets = if about.targets.is_empty() {
        "nothing to run".to_string()
    } else {
        about.targets.join(", ")
    };
    let counts = format!(
        "{} step{} · {} worker{}",
        about.steps,
        if about.steps == 1 { "" } else { "s" },
        about.workers,
        if about.workers == 1 { "" } else { "s" },
    );
    let logs = match &about.log_dir {
        Some(dir) => format!("logs in {}", dir.display()),
        None => "logging off".to_string(),
    };

    if height >= BANNER_ROWS {
        let facts = [
            span(targets, Style::new().bold()),
            span(counts, Style::new()),
            span(logs, Style::new().dim()),
        ];
        let mut lines: Vec<Line<'static>> = WORDMARK
            .iter()
            .zip(facts)
            .map(|(row, fact)| {
                Line::from(vec![
                    span(format!("  {row}   "), Style::new().cyan().bold()),
                    fact,
                ])
            })
            .collect();
        // A blank row, so the list does not sit right under it.
        lines.push(Line::default());
        lines
    } else if height >= TITLE_ROWS {
        vec![Line::from(vec![
            span("  rivet", Style::new().cyan().bold()),
            span(format!(" · {targets} · {counts}"), Style::new()),
        ])]
    } else {
        Vec::new()
    }
}

/// Which page's keys the hint line is saying, which is all the pages differ by
/// as far as it is concerned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Hint {
    List,
    Step,
    Run,
}

/// What the keys do, said at whatever length fits in `width` columns: in full
/// where there is room, and more tersely where there is not.
///
/// Both pages say what a drag does as well as what the keys do. The mouse is
/// being reported on either, so a drag no longer selects the way it would have
/// in a terminal left alone, and what it does instead has to be said or it is
/// a thing that simply stopped working. It outlasts most of the keys on the
/// way down: only the narrowest terminal, with room for nothing but the keys
/// themselves, goes without it.
fn hint_text(page: Hint, done: bool, width: usize) -> String {
    let quit = if done { "q quit" } else { "q cancel the run" };
    let quit_short = if done { "q quit" } else { "q cancel" };
    let tiers: Vec<String> = match page {
        Hint::List => vec![
            format!(
                "  ↑/↓ or wheel move · enter open a step · L run log · drag copies · \
                 y copy a less command · {quit}"
            ),
            format!(
                "  ↑/↓/wheel move · enter open · L run log · drag copies · y copy · {quit_short}"
            ),
            format!("  ↑/↓/wheel · enter · L log · drag copies · y · {quit_short}"),
            "  ↑/↓ · enter · drag copies · y · q".to_string(),
            "  ↑/↓ · enter · y · q".to_string(),
        ],
        Hint::Step => vec![
            format!(
                "  esc back · ↑/↓ or wheel scroll · / search · G follow · tab next file · \
                 L run log · drag copies · y copy a less command · {quit}"
            ),
            format!(
                "  esc back · ↑/↓ or wheel · / search · G follow · tab file · L run log · \
                 drag copies · y copy · {quit_short}"
            ),
            format!(
                "  esc back · ↑/↓/wheel · / search · G follow · tab file · drag copies · {quit_short}"
            ),
            format!("  esc · ↑/↓/wheel · G · tab · drag copies · {quit_short}"),
            "  esc · ↑/↓ · G · tab · drag copies · q".to_string(),
            "  esc · ↑/↓ · G · tab · y · q".to_string(),
        ],
        // No `tab`: this page has the one file, and no `L` either, being it.
        Hint::Run => vec![
            format!(
                "  esc back · ↑/↓ or wheel scroll · / search · G follow · drag copies · \
                 y copy a less command · {quit}"
            ),
            format!(
                "  esc back · ↑/↓ or wheel · / search · G follow · drag copies · y copy · {quit_short}"
            ),
            format!("  esc back · ↑/↓/wheel · / search · G follow · drag copies · {quit_short}"),
            format!("  esc · ↑/↓/wheel · G · drag copies · {quit_short}"),
            "  esc · ↑/↓ · G · drag copies · q".to_string(),
            "  esc · ↑/↓ · G · y · q".to_string(),
        ],
    };
    tiers
        .iter()
        .find(|tier| columns(tier) <= width)
        .or_else(|| tiers.last())
        .cloned()
        .unwrap_or_default()
}

/// The question `q` asks while the run is going, at whatever length fits.
fn confirm_text(width: usize) -> String {
    let tiers = [
        "  cancel the run? this kills the tools it is running · y cancels · any other key keeps going",
        "  cancel the run and kill its tools? y cancels · any other key keeps going",
        "  cancel the run? y/n",
    ];
    tiers
        .iter()
        .find(|tier| columns(tier) <= width)
        .unwrap_or(&tiers[2])
        .to_string()
}

/// How many columns `text` takes.
fn columns(text: &str) -> usize {
    text.chars().map(|c| c.width().unwrap_or(0)).sum()
}

/// `text` cut to `max` columns, with an ellipsis for what went: for showing a
/// line back to whoever just copied it.
fn clip(text: &str, max: usize) -> String {
    if columns(text) <= max {
        return text.to_string();
    }
    let mut kept = String::new();
    let mut used = 0;
    for c in text.chars() {
        let w = c.width().unwrap_or(0);
        if used + w > max.saturating_sub(1) {
            break;
        }
        kept.push(c);
        used += w;
    }
    kept.push('…');
    kept
}

/// `text` cut to `max` columns from the left, with an ellipsis for what went:
/// for a path, whose end is the part that tells one from another.
fn shorten_left(text: &str, max: usize) -> String {
    if columns(text) <= max {
        return text.to_string();
    }
    let room = max.saturating_sub(1);
    let mut kept: Vec<char> = Vec::new();
    let mut used = 0;
    for c in text.chars().rev() {
        let w = c.width().unwrap_or(0);
        if used + w > room {
            break;
        }
        kept.push(c);
        used += w;
    }
    let tail: String = kept.into_iter().rev().collect();
    format!("…{tail}")
}

/// A scrollbar down the right of `area`, if `count` items do not fit in it.
/// Says which column it took, for whatever has to work around it.
fn scrollbar(frame: &mut Frame, area: Rect, count: usize, offset: usize) -> Option<u16> {
    if count <= area.height as usize {
        return None;
    }
    let mut state = ScrollbarState::new(count).position(offset);
    frame.render_stateful_widget(
        Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(None)
            .end_symbol(None),
        area,
        &mut state,
    );
    Some(area.right().saturating_sub(1))
}

// ---------------------------------------------------------------------------
// Selecting with the mouse
// ---------------------------------------------------------------------------

/// Text being selected with the mouse, as places on the screen.
///
/// Kept as screen cells rather than as places in a log, because the screen is
/// what it selects from: a step's log, the list's lines, the banner's counts —
/// whatever has been drawn. What lands on the clipboard is read back out of
/// the frame, so what was readable is what is copied.
///
/// The shape is a terminal's, not a spreadsheet's: from the cell it started on
/// to the cell it is on now, taking whole rows in between, rather than the
/// rectangle of the two.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Selection {
    anchor: (u16, u16),
    head: (u16, u16),
    /// Whether the button is still down. A selection that has been let go of
    /// stays on screen, and the next thing that happens clears it.
    dragging: bool,
    /// Set when the button comes up, and taken by the frame that copies.
    copy: bool,
}

impl Selection {
    fn new(at: (u16, u16)) -> Self {
        Self {
            anchor: at,
            head: at,
            dragging: true,
            copy: false,
        }
    }

    /// The two ends in reading order.
    fn ends(&self) -> ((u16, u16), (u16, u16)) {
        let (a, b) = (self.anchor, self.head);
        // By row first: a selection running up the screen is the same one as
        // the selection running down it.
        if (a.1, a.0) <= (b.1, b.0) {
            (a, b)
        } else {
            (b, a)
        }
    }

    /// Whether the selection covers a cell, both ends included.
    fn covers(&self, x: u16, y: u16) -> bool {
        let ((sx, sy), (ex, ey)) = self.ends();
        if y < sy || y > ey {
            return false;
        }
        // One row: between the two. Otherwise the first row runs to the edge
        // and the last runs from it, the way a terminal's own selection does.
        match (y == sy, y == ey) {
            (true, true) => (sx..=ex).contains(&x),
            (true, false) => x >= sx,
            (false, true) => x <= ex,
            (false, false) => true,
        }
    }

    /// Draw the selection over the frame, and read it back if it is to be
    /// copied.
    ///
    /// Reversing what is there keeps whatever the page drew — a failure's red,
    /// a step's bold — and marks it as picked out, without needing to know
    /// what any of it was.
    fn mark(&mut self, buffer: &mut Buffer, skip: Option<u16>) -> Option<String> {
        let area = buffer.area;
        let wanted = std::mem::take(&mut self.copy);
        let mut text = String::new();
        let ((_, sy), (_, ey)) = self.ends();

        for y in sy.max(area.y)..=ey.min(area.bottom().saturating_sub(1)) {
            let mut row = String::new();
            for x in area.x..area.right() {
                if !self.covers(x, y) || skip == Some(x) {
                    continue;
                }
                let Some(cell) = buffer.cell_mut((x, y)) else {
                    continue;
                };
                cell.modifier |= Modifier::REVERSED;
                if wanted {
                    // The cells a wide character runs into hold nothing, so
                    // taking every symbol gives the text back once.
                    row.push_str(cell.symbol());
                }
            }
            if wanted {
                // Trailing blanks are the screen's, not the text's.
                text.push_str(row.trim_end());
                text.push('\n');
            }
        }

        if !wanted {
            return None;
        }
        // A selection of nothing but blank screen is not worth copying, and
        // saying so would only be in the way.
        let text = text.trim_end_matches('\n');
        (!text.trim().is_empty()).then(|| text.to_string())
    }
}

// ---------------------------------------------------------------------------
// A step's page
// ---------------------------------------------------------------------------

/// One step's page: which of its files is being read, and where in it.
struct Pager {
    /// The step whose page this is, or `None` for the run's own log, which is
    /// nobody's step.
    step: Option<usize>,
    /// What this page can read, as of the last frame: the step's files, and
    /// the run's log after them.
    files: Vec<PathBuf>,
    /// Where the run's log is among them, for the key that jumps to it.
    run: Option<PathBuf>,
    /// The file picked with `tab`, or `None` to read whichever the step most
    /// wants read — which changes as it starts tools.
    chosen: Option<PathBuf>,
    pane: Pane,
}

/// A log on a page: the file being read, and the size of the area it was last
/// drawn in, which is what the keys that scroll by a page need.
///
/// Both pages that read a file have one of these. What differs is which file:
/// a step's page follows the step, and the run's page reads `rivet.log`.
struct Pane {
    /// The file being read, once there is one.
    log: Option<LogView>,
    columns: u16,
    rows: u16,
}

impl Default for Pane {
    fn default() -> Self {
        Self {
            log: None,
            columns: 80,
            rows: 24,
        }
    }
}

impl Pane {
    /// Read `path`, starting again if it is not the file being read already.
    fn sync(&mut self, path: Option<&Path>) {
        match path {
            Some(path) => {
                if self.log.as_ref().map(|log| log.source.path.as_path()) != Some(path) {
                    self.log = Some(LogView::new(path.to_path_buf()));
                }
            }
            None => self.log = None,
        }
    }

    /// How far the keys that move by a screenful move.
    fn page(&self) -> isize {
        self.rows.max(1) as isize
    }

    /// The file being read, for the `less` command `y` copies.
    fn files(&self) -> Vec<PathBuf> {
        self.log
            .as_ref()
            .map(|log| vec![log.source.path.clone()])
            .into_iter()
            .flatten()
            .collect()
    }

    /// The area the log was last drawn in, which the scrolling keys measure
    /// themselves against.
    fn measure(&mut self, inner: Rect) {
        self.columns = inner.width;
        self.rows = inner.height;
    }

    fn scroll(&mut self, by: isize) {
        if let Some(log) = &mut self.log {
            log.scroll(by, self.columns, self.rows);
        }
    }

    fn scroll_to_top(&mut self) {
        if let Some(log) = &mut self.log {
            log.scroll_to_top();
        }
    }

    /// Hold the log still, so that a selection being made from what is on
    /// screen does not have the screen move under it.
    fn pin(&mut self) {
        let (columns, rows) = (self.columns, self.rows);
        if let Some(log) = &mut self.log {
            log.pin(columns, rows);
        }
    }

    /// Let it follow its end again, if that is where it was.
    fn unpin(&mut self) {
        if let Some(log) = &mut self.log {
            log.unpin();
        }
    }

    fn follow(&mut self) {
        if let Some(log) = &mut self.log {
            log.follow();
        }
    }

    /// Look for `pattern` from where the screen is.
    fn look(&mut self, pattern: &str, backwards: bool) {
        let (columns, rows) = (self.columns as usize, self.rows as usize);
        if let Some(log) = &mut self.log {
            log.look(pattern, backwards, columns, rows);
        }
    }

    /// Look for the same thing again, the same way round or the other one.
    fn again(&mut self, same_way: bool) {
        let (columns, rows) = (self.columns as usize, self.rows as usize);
        if let Some(log) = &mut self.log {
            log.again(same_way, columns, rows);
        }
    }
}

impl Pager {
    /// A step's page.
    fn step(id: usize) -> Self {
        Self {
            step: Some(id),
            ..Self::run()
        }
    }

    /// The run's own page, which is over the one file.
    fn run() -> Self {
        Self {
            step: None,
            files: Vec::new(),
            run: None,
            chosen: None,
            pane: Pane::default(),
        }
    }

    /// Take in what there is to read — the step's files, and the run's log
    /// after them — and read whichever of them this page is on.
    ///
    /// Done every frame, because a step picks up files as it starts tools.
    fn sync(&mut self, files: &[PathBuf], run: Option<PathBuf>) {
        self.files = files.to_vec();
        if let Some(run) = &run {
            if !self.files.contains(run) {
                self.files.push(run.clone());
            }
        }
        self.run = run;
        let wanted = match &self.chosen {
            Some(chosen) if self.files.contains(chosen) => Some(chosen),
            _ => self.files.first(),
        };
        self.pane.sync(wanted.map(PathBuf::as_path));
    }

    /// Switch to the next (or previous) of the files this page can read.
    fn next_file(&mut self, by: isize) {
        if self.files.is_empty() {
            return;
        }
        let current = self.at().unwrap_or(0) as isize;
        let next = (current + by).rem_euclid(self.files.len() as isize) as usize;
        self.choose(self.files[next].clone());
    }

    /// Read the run's log, wherever in the list it is.
    fn open_run(&mut self) {
        if let Some(run) = self.run.clone() {
            self.choose(run);
        }
    }

    fn choose(&mut self, file: PathBuf) {
        self.chosen = Some(file);
        let (files, run) = (self.files.clone(), self.run.clone());
        self.sync(&files, run);
    }

    /// Which of the files is being read.
    fn at(&self) -> Option<usize> {
        let log = self.pane.log.as_ref()?;
        self.files.iter().position(|file| *file == log.source.path)
    }

    /// The log, framed: what it is above it, where in it below.
    ///
    /// `detail` is the step this page is for, if it is for one and the step is
    /// still there to describe.
    fn draw(&mut self, frame: &mut Frame, area: Rect, detail: Option<&Detail>) -> Option<u16> {
        let border = log_border();
        let inner = border.inner(area);
        self.pane.measure(inner);

        let name = match detail {
            Some(detail) => detail.label.clone(),
            None => "run log".to_string(),
        };
        let mut title = vec![span(format!(" {name} "), Style::new().bold())];
        let mut footer = Vec::new();
        // Which lines the scrollbar stands for, and where it is in them.
        let mut scroll = None;
        let at = self.at();
        let files = self.files.len();

        let body = match (&mut self.pane.log, detail) {
            (None, Some(detail)) => {
                title.push(span(" no log ", Style::new().dim()));
                Text::from(span(
                    if detail.pending {
                        "  this step has not started"
                    } else {
                        "  this step has no files: it has no directory of its own (Step::log_dir)"
                    },
                    Style::new().dim(),
                ))
            }
            (None, None) => Text::from(span(
                "  this run is not logging: no log directory was given (ExecuteConfig::log_dir)",
                Style::new().dim(),
            )),
            (Some(log), detail) => {
                // The path gives way to the name, from the left: its end is
                // what tells one file from another.
                let room = (area.width as usize).saturating_sub(columns(&name) + 4);
                let path = shorten_left(&log.source.path.display().to_string(), room);
                title.push(span(format!(" {path} "), Style::new().dim()));
                if files > 1 {
                    footer.push(span(
                        format!(" {}/{files} files (tab) ", at.map_or(0, |at| at + 1)),
                        Style::new().dim(),
                    ));
                }

                let waiting = match detail {
                    Some(detail) if !detail.running => "no such file",
                    Some(_) => "waiting for the file to appear",
                    None => "waiting for the log to be written",
                };
                let (body, where_) = log_body(log, inner, waiting, &mut footer);
                scroll = where_;
                body
            }
        };

        draw_framed(frame, area, inner, border, title, footer, body);
        scroll.and_then(|(count, top)| scrollbar(frame, inner, count, top))
    }
}

/// How a search of this log is going, for the footer to say: still looking,
/// or nothing to find, or found something round the far end of the file.
///
/// Nothing while a search has simply found what it was looking for: the
/// highlight on screen says that better than the footer could.
fn searching(log: &LogView, count: u64) -> Option<Span<'static>> {
    let find = log.find.as_ref()?;
    if find.next.is_some() {
        let done = 100 - (find.left * 100 / count.max(1)).min(100);
        return Some(span(
            format!(" searching {}… {done}% ", find.pattern),
            Style::new().dim(),
        ));
    }
    if find.missing {
        return Some(span(
            format!(" no match for {} ", find.pattern),
            Style::new().yellow(),
        ));
    }
    find.wrapped
        .then(|| span(" wrapped ", Style::new().yellow()))
}

/// The frame a log is read in: a rule above it and below, with what it is over
/// the top and where in it under the bottom.
fn log_border() -> Block<'static> {
    Block::new()
        .borders(Borders::TOP | Borders::BOTTOM)
        .border_style(Style::new().dim())
}

fn draw_framed(
    frame: &mut Frame,
    area: Rect,
    inner: Rect,
    border: Block<'static>,
    title: Vec<Span<'static>>,
    footer: Vec<Span<'static>>,
    body: Text<'static>,
) {
    frame.render_widget(
        border
            .title(Line::from(title))
            .title_bottom(Line::from(footer).right_aligned()),
        area,
    );
    frame.render_widget(Paragraph::new(body), inner);
}

/// What a log shows for whatever state its file is in, and — appended to
/// `footer` — where in the file the screen is. The lines the scrollbar stands
/// for come back with it.
///
/// `waiting` is what to say while the file is not there: whether that is worth
/// waiting for is the page's business rather than the log's.
fn log_body(
    log: &mut LogView,
    inner: Rect,
    waiting: &str,
    footer: &mut Vec<Span<'static>>,
) -> (Text<'static>, Option<(usize, usize)>) {
    log.poll();
    match &log.source.state {
        SourceState::Waiting => (
            Text::from(span(format!("  {waiting}"), Style::new().dim())),
            None,
        ),
        SourceState::Failed(error) => (
            Text::from(span(format!("  cannot read: {error}"), Style::new().red())),
            None,
        ),
        SourceState::Reading if log.source.count() == 0 => {
            (Text::from(span("  (empty)", Style::new().dim())), None)
        }
        SourceState::Reading => {
            let shown = log.render(inner.width, inner.height);
            let count = log.source.count();
            // Said while the index is still catching up, because the count is
            // of what it has reached rather than of the file.
            let indexing = if log.source.indexing() {
                " · indexing"
            } else {
                ""
            };
            footer.push(span(
                if shown.following {
                    format!(" following · {count} lines{indexing} ")
                } else {
                    format!(" line {} of {count}{indexing} ", shown.bottom + 1)
                },
                Style::new().dim(),
            ));
            if let Some(note) = searching(log, count) {
                footer.push(note);
            }
            (
                Text::from(shown.rows),
                Some((count as usize, shown.top.line as usize)),
            )
        }
    }
}

// ---------------------------------------------------------------------------
// Reading a log
// ---------------------------------------------------------------------------

/// A file being read as it is written, and where in it the reader is.
struct LogView {
    source: Source,
    /// What is being looked for in it, once anything has been.
    find: Option<Find>,
    /// The first row on screen, or `None` to follow the end as it grows.
    top: Option<Top>,
    /// Whether the end is to be followed no further, whatever [`Self::top`]
    /// says. Set while a selection is being made from what is on screen, which
    /// a log growing underneath would otherwise carry away.
    pinned: bool,
    /// Whether the log was following its end when it was pinned, and so is to
    /// go back to following when the selection is let go of. Holding a log
    /// still is the selection's doing, not something that was asked for, so it
    /// is not left behind once the selection is gone.
    resume: bool,
}

/// A search over a log: what is being looked for, which way, and how far the
/// looking has got.
///
/// Scanned a slice at a time from [`LogView::poll`] rather than run to the
/// end: a long log takes longer to search than a frame is worth, and a display
/// that stopped drawing until it had an answer would be worse than one that
/// takes a moment to give it.
struct Find {
    pattern: String,
    /// Matched regardless of case, unless the pattern has a capital in it —
    /// which is how a search that is typed in a hurry is meant to behave.
    fold: bool,
    backwards: bool,
    /// The line the last match was on.
    at: Option<u64>,
    /// The line to look at next, while there is still looking to do.
    next: Option<u64>,
    /// Lines still to look at before every one has been looked at once.
    left: u64,
    /// Whether the looking has come round past an end of the file.
    wrapped: bool,
    /// Whether it went all the way round and found nothing.
    missing: bool,
}

/// A place in a wrapped file: a line, and a row within it once wrapped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Top {
    line: u64,
    row: usize,
}

/// What a frame of the log showed.
struct Shown {
    rows: Vec<Line<'static>>,
    top: Top,
    /// The last line with anything on screen.
    bottom: u64,
    following: bool,
}

impl LogView {
    fn new(path: PathBuf) -> Self {
        Self {
            source: Source::new(path),
            find: None,
            top: None,
            pinned: false,
            resume: false,
        }
    }

    fn poll(&mut self) {
        self.source.poll();
        self.step();
    }

    /// Look for `pattern` from the top of the screen, whichever way.
    fn look(&mut self, pattern: &str, backwards: bool, width: usize, height: usize) {
        let count = self.source.count();
        let top = self.top(width, height).0.line;
        // From the line after the one on top, so that a search does not keep
        // finding what is already on screen — and round the far end of the
        // file if that was the last line there was.
        let (from, wrapped) = beyond(top, backwards, count);
        self.start(pattern.to_string(), backwards, from, count, wrapped);
    }

    /// Look for the same thing again: on from the last match, the same way
    /// round or the other one.
    fn again(&mut self, same_way: bool, width: usize, height: usize) {
        let Some(find) = &self.find else { return };
        let backwards = find.backwards == same_way;
        let pattern = find.pattern.clone();
        let count = self.source.count();
        let Some(at) = find.at else {
            return self.look(&pattern, backwards, width, height);
        };
        let (from, wrapped) = beyond(at, backwards, count);
        self.start(pattern, backwards, from, count, wrapped);
    }

    fn start(&mut self, pattern: String, backwards: bool, from: u64, count: u64, wrapped: bool) {
        let fold = !pattern.chars().any(char::is_uppercase);
        self.find = Some(Find {
            pattern,
            fold,
            backwards,
            at: None,
            next: (count > 0).then_some(from),
            left: count,
            wrapped,
            missing: count == 0,
        });
        self.step();
    }

    /// One frame's worth of looking, which either finds something, runs out of
    /// file, or leaves the rest for the next frame.
    fn step(&mut self) {
        let count = self.source.count();
        let Some(find) = self.find.as_mut() else {
            return;
        };
        let Some(mut at) = find.next else { return };
        for _ in 0..SEARCH_LINES {
            if find.left == 0 || count == 0 {
                find.next = None;
                find.missing = true;
                return;
            }
            if contains(self.source.line(at), &find.pattern, find.fold) {
                find.at = Some(at);
                find.next = None;
                // At the top of the screen, where `less` puts it: what is
                // wanted with a match is usually what follows it.
                self.top = Some(Top { line: at, row: 0 });
                self.pinned = false;
                return;
            }
            find.left -= 1;
            at = match (find.backwards, at) {
                (true, 0) => {
                    find.wrapped = true;
                    count - 1
                }
                (true, at) => at - 1,
                (false, at) if at + 1 >= count => {
                    find.wrapped = true;
                    0
                }
                (false, at) => at + 1,
            };
            find.next = Some(at);
        }
    }

    fn follow(&mut self) {
        self.top = None;
        self.pinned = false;
    }

    fn scroll_to_top(&mut self) {
        self.top = Some(Top { line: 0, row: 0 });
        self.pinned = false;
    }

    /// Stay where the screen is now, however the file grows.
    fn pin(&mut self, width: u16, height: u16) {
        if self.pinned {
            return;
        }
        let (width, height) = (width.max(1) as usize, height as usize);
        self.resume = self.top.is_none();
        self.top = Some(self.top.unwrap_or_else(|| self.follow_top(width, height)));
        self.pinned = true;
    }

    /// Let go, and follow the end again if that is what it was doing.
    fn unpin(&mut self) {
        if !self.pinned {
            return;
        }
        self.pinned = false;
        if self.resume {
            self.top = None;
        }
    }

    /// Where the top of the screen is when following: far enough back from
    /// the end to fill `height` rows.
    fn follow_top(&mut self, width: usize, height: usize) -> Top {
        let count = self.source.count();
        if count == 0 {
            return Top { line: 0, row: 0 };
        }
        let mut line = count - 1;
        let mut need = height.max(1);
        loop {
            let rows = row_count(self.source.line(line), width);
            if rows >= need {
                return Top {
                    line,
                    row: rows - need,
                };
            }
            need -= rows;
            if line == 0 {
                return Top { line, row: 0 };
            }
            line -= 1;
        }
    }

    /// Where the top of the screen is now, given where it was asked to be.
    ///
    /// A place at or past where following would put it means following.
    fn top(&mut self, width: usize, height: usize) -> (Top, bool) {
        let follow = self.follow_top(width, height);
        match self.top {
            Some(top) if top >= follow && !self.pinned => {
                self.top = None;
                (follow, true)
            }
            Some(top) => (top, false),
            None => (follow, true),
        }
    }

    fn render(&mut self, width: u16, height: u16) -> Shown {
        let (width, height) = (width.max(1) as usize, height as usize);
        let (top, following) = self.top(width, height);

        let mut rows = Vec::with_capacity(height);
        let mut line = top.line;
        let mut skip = top.row;
        let mut bottom = top.line;
        let find = self
            .find
            .as_ref()
            .map(|find| (find.pattern.clone(), find.fold));
        while rows.len() < height && line < self.source.count() {
            for row in wrap(self.source.line(line), width).into_iter().skip(skip) {
                if rows.len() == height {
                    break;
                }
                let find = find.as_ref().map(|(pattern, fold)| (&**pattern, *fold));
                rows.push(highlight(row, find));
                bottom = line;
            }
            skip = 0;
            line += 1;
        }

        Shown {
            rows,
            top,
            bottom,
            following,
        }
    }

    /// Move `by` rows down (up, if negative), and follow the end again when
    /// that reaches it.
    fn scroll(&mut self, by: isize, width: u16, height: u16) {
        self.pinned = false;
        let (width, height) = (width.max(1) as usize, height as usize);
        let follow = self.follow_top(width, height);
        let (mut top, _) = self.top(width, height);
        let count = self.source.count();

        if by < 0 {
            for _ in 0..by.unsigned_abs() {
                if top.row > 0 {
                    top.row -= 1;
                } else if top.line > 0 {
                    top.line -= 1;
                    top.row = row_count(self.source.line(top.line), width).saturating_sub(1);
                } else {
                    break;
                }
            }
        } else {
            for _ in 0..by {
                if top.row + 1 < row_count(self.source.line(top.line), width) {
                    top.row += 1;
                } else if top.line + 1 < count {
                    top.line += 1;
                    top.row = 0;
                } else {
                    break;
                }
            }
        }
        self.top = (top < follow).then_some(top);
    }
}

/// The line a search starts at, given the one it is starting from, and
/// whether getting there went round an end of the file.
fn beyond(from: u64, backwards: bool, count: u64) -> (u64, bool) {
    match backwards {
        true if from == 0 => (count.saturating_sub(1), true),
        true => (from - 1, false),
        false if from + 1 >= count => (0, true),
        false => (from + 1, false),
    }
}

/// Whether `text` has `needle` in it, ignoring case where `fold` says to.
///
/// Case is folded for ASCII only, which keeps a match's bytes where they were:
/// lowercasing a whole line first would move them, and the highlight is drawn
/// from the offsets this finds.
fn contains(text: &str, needle: &str, fold: bool) -> bool {
    !needle.is_empty() && occurrences(text, needle, fold).next().is_some()
}

/// Where `needle` is in `text`, as byte ranges, left to right.
fn occurrences<'a>(
    text: &'a str,
    needle: &'a str,
    fold: bool,
) -> impl Iterator<Item = (usize, usize)> + 'a {
    let (hay, pin) = (text.as_bytes(), needle.as_bytes());
    let mut at = 0;
    std::iter::from_fn(move || {
        while at + pin.len() <= hay.len() {
            let end = at + pin.len();
            let same = if fold {
                hay[at..end].eq_ignore_ascii_case(pin)
            } else {
                &hay[at..end] == pin
            };
            // On a character boundary at both ends, so that what is found can
            // be sliced out of the text it was found in.
            if same && text.is_char_boundary(at) && text.is_char_boundary(end) {
                at = end.max(at + 1);
                return Some((end - pin.len(), end));
            }
            at += 1;
        }
        None
    })
}

/// A row with whatever matched picked out of it.
fn highlight(row: String, find: Option<(&str, bool)>) -> Line<'static> {
    let Some((needle, fold)) = find.filter(|(needle, _)| !needle.is_empty()) else {
        return Line::from(row);
    };
    let mut spans = Vec::new();
    let mut cut = 0;
    for (from, to) in occurrences(&row, needle, fold) {
        if from < cut {
            continue;
        }
        if from > cut {
            spans.push(span(row[cut..from].to_string(), Style::new()));
        }
        spans.push(span(
            row[from..to].to_string(),
            Style::new().black().on_yellow(),
        ));
        cut = to;
    }
    if spans.is_empty() {
        return Line::from(row);
    }
    if cut < row.len() {
        spans.push(span(row[cut..].to_string(), Style::new()));
    }
    Line::from(spans)
}

/// A line broken into rows of at most `width` columns.
///
/// By column rather than by word: this is tool output, where a line is as
/// often a table row or a path as prose, and where the end of a long line is
/// the part that matters.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut rows = Vec::new();
    let mut row = String::new();
    let mut used = 0;
    for c in text.chars() {
        let w = c.width().unwrap_or(0);
        if used + w > width && used > 0 {
            rows.push(std::mem::take(&mut row));
            used = 0;
        }
        row.push(c);
        used += w;
    }
    rows.push(row);
    rows
}

/// A styled line broken into rows of at most `width` columns, keeping its
/// styling, with every row after the first indented by `hang`.
fn wrap_line(line: &Line<'static>, width: usize, hang: usize) -> Vec<Line<'static>> {
    let width = width.max(1);
    let hang = hang.min(width - 1);
    let mut rows: Vec<Line<'static>> = Vec::new();
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut piece = String::new();
    let mut used = 0;
    let mut room = width;

    for span in &line.spans {
        for c in span.content.chars() {
            let w = c.width().unwrap_or(0);
            if used + w > room && used > 0 {
                if !piece.is_empty() {
                    spans.push(Span::styled(std::mem::take(&mut piece), span.style));
                }
                rows.push(Line::from(std::mem::take(&mut spans)).style(line.style));
                spans.push(Span::raw(" ".repeat(hang)));
                used = 0;
                room = width - hang;
            }
            piece.push(c);
            used += w;
        }
        if !piece.is_empty() {
            spans.push(Span::styled(std::mem::take(&mut piece), span.style));
        }
    }
    rows.push(Line::from(spans).style(line.style));
    rows
}

/// How many rows [`wrap`] would give, without giving them.
fn row_count(text: &str, width: usize) -> usize {
    let width = width.max(1);
    let mut rows = 1;
    let mut used = 0;
    for c in text.chars() {
        let w = c.width().unwrap_or(0);
        if used + w > width && used > 0 {
            rows += 1;
            used = 0;
        }
        used += w;
    }
    rows
}

/// A file being read as it is written, and read back through: `tail -F` and
/// `less` at once.
///
/// All of it is reachable however long it is, because none of it is kept. What
/// is kept is an index — the byte offset of every [`INDEX_EVERY`]th line — and
/// a window of decoded lines around wherever is being read. Any line is then a
/// seek to the nearest anchor and a short scan on from there, which costs the
/// same whether it is the first line of the file or the last.
///
/// The index is built by scanning for newlines, at most [`INDEX_BUDGET`] bytes
/// per poll: a log of an ordinary size is indexed before its first frame, and
/// a very long one fills in over the next few without any of them costing more
/// than that. [`Source::indexing`] says while that is still going on, and the
/// count is of what has been indexed so far.
struct Source {
    path: PathBuf,
    file: Option<File>,
    /// Which file this is, so that a replacement under the same name is
    /// noticed.
    identity: Option<(u64, u64)>,
    /// What the file measured at the last poll.
    len: u64,
    /// Where lines 0, [`INDEX_EVERY`], 2 × [`INDEX_EVERY`] … start.
    anchors: Vec<u64>,
    /// Lines finished by a newline, which is every line before
    /// [`Source::line_start`].
    lines: u64,
    /// Where the line that has no newline yet starts.
    line_start: u64,
    /// How far the index has scanned. Ahead of [`Source::line_start`] only
    /// inside a line longer than what one poll reads.
    scanned: u64,
    /// How much one poll may scan. [`INDEX_BUDGET`], except in tests, which
    /// would otherwise have to write 64 MiB to reach a second poll.
    budget: u64,
    /// The lines held decoded, and the number of the first of them.
    window: VecDeque<String>,
    from: u64,
    /// Whether the last line in the window had no newline when it was read,
    /// and what the file measured then. Every other line is finished and can
    /// never say anything different; that one grows.
    window_partial: bool,
    window_len: u64,
    state: SourceState,
}

enum SourceState {
    /// There is no such file, yet.
    Waiting,
    Reading,
    Failed(String),
}

impl Source {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            file: None,
            identity: None,
            len: 0,
            anchors: vec![0],
            lines: 0,
            line_start: 0,
            scanned: 0,
            budget: INDEX_BUDGET,
            window: VecDeque::new(),
            from: 0,
            window_partial: false,
            window_len: 0,
            state: SourceState::Waiting,
        }
    }

    /// Take in whatever has been written since last time.
    fn poll(&mut self) {
        let meta = match fs::metadata(&self.path) {
            Ok(meta) => meta,
            Err(error) => {
                self.file = None;
                self.state = if error.kind() == io::ErrorKind::NotFound {
                    SourceState::Waiting
                } else {
                    SourceState::Failed(error.to_string())
                };
                return;
            }
        };
        if !meta.is_file() {
            self.state = SourceState::Failed("not a file".into());
            return;
        }

        // New, replaced, or truncated: everything known about it was about a
        // file that is no longer there.
        let identity = identity(&meta);
        if self.file.is_none() || self.identity != Some(identity) || meta.len() < self.scanned {
            match File::open(&self.path) {
                Ok(file) => self.file = Some(file),
                Err(error) => {
                    self.file = None;
                    self.state = SourceState::Failed(error.to_string());
                    return;
                }
            }
            self.identity = Some(identity);
            self.forget();
        }

        // The line that had no newline when the window was read has had more
        // of itself written since. It is the only line that can have changed,
        // so it is the only one dropped.
        if self.window_partial && meta.len() != self.window_len {
            self.window.pop_back();
            self.window_partial = false;
        }

        self.len = meta.len();
        self.index();
        if !matches!(self.state, SourceState::Failed(_)) {
            self.state = SourceState::Reading;
        }
    }

    /// Everything read so far, forgotten: a different file is being read now.
    fn forget(&mut self) {
        self.anchors = vec![0];
        self.lines = 0;
        self.line_start = 0;
        self.scanned = 0;
        self.window.clear();
        self.from = 0;
        self.window_partial = false;
        self.window_len = 0;
    }

    /// Scan on from where the last scan stopped, for as many bytes as one poll
    /// is allowed, noting where every [`INDEX_EVERY`]th line starts.
    fn index(&mut self) {
        let until = self.len.min(self.scanned + self.budget);
        if self.scanned >= until {
            return;
        }
        let Some(file) = &mut self.file else { return };
        if let Err(error) = file.seek(SeekFrom::Start(self.scanned)) {
            self.state = SourceState::Failed(error.to_string());
            return;
        }

        let mut buffer = vec![0u8; CHUNK];
        while self.scanned < until {
            let want = ((until - self.scanned) as usize).min(CHUNK);
            match file.read(&mut buffer[..want]) {
                Ok(0) => break,
                Ok(read) => {
                    let base = self.scanned;
                    for (at, byte) in buffer[..read].iter().enumerate() {
                        if *byte != b'\n' {
                            continue;
                        }
                        // The line ends here, so the next one starts after it
                        // — and is the one an anchor would point at.
                        self.lines += 1;
                        self.line_start = base + at as u64 + 1;
                        if self.lines.is_multiple_of(INDEX_EVERY) {
                            self.anchors.push(self.line_start);
                        }
                    }
                    self.scanned += read as u64;
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => {
                    self.state = SourceState::Failed(error.to_string());
                    return;
                }
            }
        }
    }

    /// Whether the index is still catching up with the file.
    ///
    /// What is on screen is right either way; what is still growing is the
    /// count of how much there is, which the page says rather than pretending
    /// to a number it does not have yet.
    fn indexing(&self) -> bool {
        self.scanned < self.len
    }

    /// How many lines there are, the one still being written included.
    fn count(&self) -> u64 {
        self.lines + u64::from(self.len > self.line_start)
    }

    /// Line `number`, which must be below [`Source::count`].
    ///
    /// Takes `&mut self` because a line outside the window is read in, which
    /// is the whole point of the window: it is the reading that is bounded,
    /// not what can be read.
    fn line(&mut self, number: u64) -> &str {
        if number < self.from || number >= self.from + self.window.len() as u64 {
            self.load(number);
        }
        number
            .checked_sub(self.from)
            .and_then(|index| self.window.get(index as usize))
            .map(String::as_str)
            .unwrap_or_default()
    }

    /// Decode the window of lines that `number` falls in, from the anchor at
    /// or before it.
    fn load(&mut self, number: u64) {
        self.window.clear();
        self.window_partial = false;
        self.window_len = self.len;
        let anchor = (number / INDEX_EVERY).min(self.anchors.len().saturating_sub(1) as u64);
        let Some(&offset) = self.anchors.get(anchor as usize) else {
            return;
        };
        self.from = anchor * INDEX_EVERY;

        let Some(file) = &mut self.file else { return };
        if file.seek(SeekFrom::Start(offset)).is_err() {
            return;
        }

        let mut buffer = vec![0u8; CHUNK];
        let mut line: Vec<u8> = Vec::new();
        let mut read_from = offset;
        let mut cut = false;
        while self.window.len() < WINDOW && read_from < self.len {
            let want = ((self.len - read_from) as usize).min(CHUNK);
            let read = match file.read(&mut buffer[..want]) {
                Ok(0) => break,
                Ok(read) => read,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            };
            read_from += read as u64;
            for byte in &buffer[..read] {
                if *byte == b'\n' {
                    self.window.push_back(decode(&line, cut));
                    line.clear();
                    cut = false;
                    if self.window.len() == WINDOW {
                        break;
                    }
                } else if line.len() < LINE_BYTES {
                    line.push(*byte);
                } else {
                    // A line longer than anyone will read the end of on a
                    // screen. The rest of it is skipped rather than held.
                    cut = true;
                }
            }
        }
        // Whatever is left over has no newline yet: the line being written.
        if !line.is_empty() && self.window.len() < WINDOW {
            self.window.push_back(decode(&line, cut));
            self.window_partial = true;
        }
    }
}

/// A line's bytes as they are shown: cleaned of anything that would move the
/// cursor, and marked if it was too long to keep whole.
fn decode(line: &[u8], cut: bool) -> String {
    let mut text = clean(&String::from_utf8_lossy(line));
    if cut {
        text.push('…');
    }
    text
}

#[cfg(unix)]
fn identity(meta: &Metadata) -> (u64, u64) {
    use std::os::unix::fs::MetadataExt;
    (meta.dev(), meta.ino())
}

#[cfg(not(unix))]
fn identity(_: &Metadata) -> (u64, u64) {
    (0, 0)
}

// ---------------------------------------------------------------------------
// Reading a log elsewhere
// ---------------------------------------------------------------------------

/// `less` over the files a step has written or is writing.
///
/// The whole log, rather than a tail of it: the tail is what this screen
/// already shows. Given several files, `less` opens the first and moves to the
/// next with `:n`.
fn view_command(files: &[PathBuf]) -> String {
    let files: Vec<String> = files.iter().map(|file| quote(&full_path(file))).collect();
    format!("less {}", files.join(" "))
}

/// A path as it has to be to be pasted somewhere else, which is not necessarily
/// a shell sitting in the directory this run was started from.
fn full_path(path: &Path) -> String {
    fs::canonicalize(path)
        .unwrap_or_else(|_| path.to_path_buf())
        .display()
        .to_string()
}

/// Quote a path for a shell: a step's own log is named after the step, and step
/// labels have spaces in them.
fn quote(text: &str) -> String {
    let plain = !text.is_empty()
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._-+=/:,@%".contains(c));
    if plain {
        text.to_string()
    } else {
        format!("'{}'", text.replace('\'', r"'\''"))
    }
}

// ---------------------------------------------------------------------------
// Leaving the record behind
// ---------------------------------------------------------------------------

/// Write `lines` to the terminal, styled, once the screen has been given back.
fn print_record(lines: &[Line<'static>]) {
    if lines.is_empty() {
        return;
    }
    let mut out = String::new();
    for line in lines {
        out.push_str(&ansi(line));
        out.push('\n');
    }
    let mut stderr = io::stderr().lock();
    let _ = stderr.write_all(out.as_bytes());
    let _ = stderr.flush();
}

/// A line as escape sequences, for a terminal that is being written to rather
/// than drawn on.
fn ansi(line: &Line) -> String {
    use ratatui::crossterm::style::{Attribute, ContentStyle, StyledContent};
    use std::fmt::Write as _;

    let mut out = String::new();
    for span in &line.spans {
        let mut style = ContentStyle::new();
        style.foreground_color = span.style.fg.map(Into::into);
        style.background_color = span.style.bg.map(Into::into);
        let modifiers = span.style.add_modifier;
        for (modifier, attribute) in [
            (Modifier::BOLD, Attribute::Bold),
            (Modifier::DIM, Attribute::Dim),
            (Modifier::ITALIC, Attribute::Italic),
            (Modifier::UNDERLINED, Attribute::Underlined),
            (Modifier::REVERSED, Attribute::Reverse),
            (Modifier::CROSSED_OUT, Attribute::CrossedOut),
        ] {
            if modifiers.contains(modifier) {
                style.attributes.set(attribute);
            }
        }
        let _ = write!(out, "{}", StyledContent::new(style, span.content.as_ref()));
    }
    out
}

/// One piece of a line, in the display's own styling.
fn span(text: impl Into<String>, style: Style) -> Span<'static> {
    Span::styled(text.into(), style)
}

/// A line as plain text.
#[cfg(test)]
fn plain(line: &Line) -> String {
    line.spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect()
}

// ---------------------------------------------------------------------------
// Signals
// ---------------------------------------------------------------------------

/// Signals and terminal modes, which only unix has.
mod signals {
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;

    /// A signal this module has taken an interest in.
    #[cfg(unix)]
    pub(super) type Id = signal_hook::SigId;
    #[cfg(not(unix))]
    pub(super) type Id = ();

    /// Notice an interrupt, so the terminal can be handed back before the run
    /// ends; a `^Z`, so it can be handed back before the process stops; and
    /// coming back from one.
    ///
    /// The flags are read by the display's thread rather than acted on where
    /// they are set: redrawing and restoring a terminal are both far more than
    /// a signal handler may do.
    #[cfg(unix)]
    pub(super) fn catch(
        interrupted: &Arc<AtomicBool>,
        stopped: &Arc<AtomicBool>,
        resumed: &Arc<AtomicBool>,
    ) -> Vec<Id> {
        use signal_hook::consts::{SIGCONT, SIGINT, SIGTERM, SIGTSTP};
        use signal_hook::flag;

        let mut ids = Vec::new();
        for signal in [SIGINT, SIGTERM] {
            // Registered first, so it runs first: a second interrupt, arriving
            // while the first is still being tidied up, gives up on being tidy.
            let shutdown = flag::register_conditional_shutdown(
                signal,
                super::INTERRUPTED,
                interrupted.clone(),
            );
            ids.extend(shutdown);
            ids.extend(flag::register(signal, interrupted.clone()));
        }
        // Having a handler at all is what stops the process being stopped on
        // the spot; the display stops it itself once the screen is put away.
        ids.extend(flag::register(SIGTSTP, stopped.clone()));
        ids.extend(flag::register(SIGCONT, resumed.clone()));
        ids
    }

    #[cfg(not(unix))]
    pub(super) fn catch(_: &Arc<AtomicBool>, _: &Arc<AtomicBool>, _: &Arc<AtomicBool>) -> Vec<Id> {
        Vec::new()
    }

    /// Stop this process, as the `^Z` it is standing in for would have. Returns
    /// once it has been continued.
    #[cfg(unix)]
    pub(super) fn stop_now() {
        let _ = signal_hook::low_level::raise(signal_hook::consts::SIGSTOP);
    }

    #[cfg(not(unix))]
    pub(super) fn stop_now() {}

    /// Put back the signal keys that raw mode took away.
    ///
    /// See the module docs: `^C` has to reach the tools a step is running, and
    /// only the terminal can send it to them at once.
    #[cfg(unix)]
    pub(super) fn keep_keys() {
        use rustix::termios::{tcgetattr, tcsetattr, LocalModes, OptionalActions};

        // The display draws on stderr, which has already been found to be a
        // terminal, and a terminal's mode belongs to the device rather than to
        // any one of the handles open on it.
        let tty = std::io::stderr();
        if let Ok(mut mode) = tcgetattr(&tty) {
            mode.local_modes |= LocalModes::ISIG;
            let _ = tcsetattr(&tty, OptionalActions::Now, &mode);
        }
    }

    #[cfg(not(unix))]
    pub(super) fn keep_keys() {}

    /// Interrupt the run, as a `^C` would have: the whole process group gets
    /// the signal, this process included, so the tools the steps are running
    /// die with it and the run ends as an interrupted one.
    ///
    /// The flag is left to the signal to set. Setting it here as well would
    /// make the signal look like a second interrupt, and a second interrupt
    /// exits on the spot, terminal and all. Only if the signal cannot be sent
    /// is the flag set by hand, so that the display still ends.
    #[cfg(unix)]
    pub(super) fn interrupt_run(interrupted: &AtomicBool) {
        use std::sync::atomic::Ordering;
        if rustix::process::kill_current_process_group(rustix::process::Signal::Int).is_err() {
            interrupted.store(true, Ordering::SeqCst);
        }
    }

    #[cfg(not(unix))]
    pub(super) fn interrupt_run(interrupted: &AtomicBool) {
        use std::sync::atomic::Ordering;
        interrupted.store(true, Ordering::SeqCst);
    }

    /// Stop taking an interest, now that the run has the terminal no longer.
    #[cfg(unix)]
    pub(super) fn forget(ids: Vec<Id>) {
        for id in ids {
            signal_hook::low_level::unregister(id);
        }
    }

    #[cfg(not(unix))]
    pub(super) fn forget(_: Vec<Id>) {}
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- wrapping -----------------------------------------------------------

    #[test]
    fn lines_wrap_by_column() {
        assert_eq!(wrap("", 10), [""]);
        assert_eq!(wrap("abc", 10), ["abc"]);
        assert_eq!(wrap("abcdefghij", 10), ["abcdefghij"]);
        assert_eq!(wrap("abcdefghijk", 10), ["abcdefghij", "k"]);
        assert_eq!(wrap("abcdef", 2), ["ab", "cd", "ef"]);
        for text in [
            "",
            "abc",
            "abcdefghijk",
            "abcdef",
            "x".repeat(1000).as_str(),
        ] {
            for width in [1, 2, 7, 80] {
                assert_eq!(
                    row_count(text, width),
                    wrap(text, width).len(),
                    "{text:?} at {width}"
                );
            }
        }
    }

    #[test]
    fn styled_lines_wrap_with_a_hanging_indent_and_keep_their_styling() {
        let line = Line::from(vec![
            span("✖ ", Style::new().red()),
            span("decoder lvs", Style::new().red().bold()),
            span(
                "  did not match; see build/decoder.lvs.out",
                Style::new().red(),
            ),
        ]);
        let rows = wrap_line(&line, 20, 4);
        let text: Vec<String> = rows.iter().map(plain).collect();
        assert_eq!(
            text,
            [
                "✖ decoder lvs  did n",
                "    ot match; see bu",
                "    ild/decoder.lvs.",
                "    out",
            ]
        );
        // Every row fits, and the styling survives the cut.
        assert!(rows.iter().all(|row| row.width() <= 20));
        assert!(rows[1].spans[1].style.fg == Some(ratatui::style::Color::Red));
        assert_eq!(rows[0].spans[1].content, "decoder lvs");
        assert!(rows[0].spans[1].style.add_modifier.contains(Modifier::BOLD));

        // A line that fits is one row, untouched.
        assert_eq!(wrap_line(&line, 200, 4).len(), 1);
        assert_eq!(plain(&wrap_line(&Line::from(""), 10, 4)[0]), "");
    }

    #[test]
    fn wide_characters_take_two_columns_and_never_split() {
        // Three CJK characters are six columns wide.
        assert_eq!(wrap("漢字表", 6), ["漢字表"]);
        assert_eq!(wrap("漢字表", 5), ["漢字", "表"]);
        // Too narrow for even one: each still gets a row rather than looping.
        assert_eq!(wrap("漢字", 1), ["漢", "字"]);
        assert_eq!(row_count("漢字表", 5), 2);
    }

    // -- reading a file as it grows -----------------------------------------

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("rivet-tui-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Every line of a source, which is all of them: nothing is out of reach.
    fn lines(source: &mut Source) -> Vec<String> {
        (0..source.count())
            .map(|n| source.line(n).to_string())
            .collect()
    }

    /// A scratch directory no other test is using: [`scratch`] empties the one
    /// it is given, and the tests run at the same time as each other.
    fn own_dir(name: &str) -> PathBuf {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        scratch(&format!("{name}{}", NEXT.fetch_add(1, Ordering::Relaxed)))
    }

    /// A file of `count` lines, numbered from zero, in a directory of its own.
    fn numbered(count: usize) -> PathBuf {
        let dir = own_dir("lines");
        let path = dir.join("log");
        let mut text = String::new();
        for n in 0..count {
            text.push_str(&format!("{n}\n"));
        }
        fs::write(&path, text).unwrap();
        path
    }

    fn append(path: &Path, text: &str) {
        let mut file = fs::OpenOptions::new().append(true).open(path).unwrap();
        file.write_all(text.as_bytes()).unwrap();
    }

    #[test]
    fn a_source_waits_for_its_file_then_reads_what_is_appended() {
        let dir = scratch("grows");
        let path = dir.join("tool.out");
        let mut source = Source::new(path.clone());

        source.poll();
        assert!(matches!(source.state, SourceState::Waiting));
        assert_eq!(source.count(), 0);

        fs::write(&path, "one\ntwo\npart").unwrap();
        source.poll();
        assert!(matches!(source.state, SourceState::Reading));
        // The unfinished line is shown while it waits for the rest of itself.
        assert_eq!(lines(&mut source), ["one", "two", "part"]);
        assert_eq!(source.count(), 3);

        append(&path, "ial\r\nthree\n");
        source.poll();
        assert_eq!(lines(&mut source), ["one", "two", "partial", "three"]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_source_starts_over_when_its_file_is_truncated_or_replaced() {
        let dir = scratch("replaced");
        let path = dir.join("tool.out");
        let mut source = Source::new(path.clone());

        fs::write(&path, "old one\nold two\n").unwrap();
        source.poll();
        assert_eq!(lines(&mut source), ["old one", "old two"]);

        // Truncated and rewritten shorter.
        fs::write(&path, "new\n").unwrap();
        source.poll();
        assert_eq!(lines(&mut source), ["new"]);

        // Replaced by a different file under the same name, the way a tool
        // that rotates its log does it.
        let other = dir.join("tool.out.new");
        fs::write(&other, "replacement one\nreplacement two\n").unwrap();
        fs::rename(&other, &path).unwrap();
        source.poll();
        assert_eq!(lines(&mut source), ["replacement one", "replacement two"]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn every_line_of_a_long_file_is_reachable_at_the_same_cost() {
        // Longer than the index's spacing several times over, so that lines
        // are reached through anchors rather than from the top.
        let count = 5 * INDEX_EVERY as usize + 7;
        let path = numbered(count);
        let mut source = Source::new(path);
        source.poll();

        assert_eq!(source.count(), count as u64);
        assert!(!source.indexing());
        assert_eq!(source.line(0), "0");
        assert_eq!(source.line(count as u64 - 1), format!("{}", count - 1));

        // Out of order, backwards, and across every anchor: the window is
        // reloaded as needed and none of it is wrong.
        for n in [count as u64 - 1, 0, INDEX_EVERY, INDEX_EVERY - 1, 3, 999] {
            let n = n.min(count as u64 - 1);
            assert_eq!(source.line(n), format!("{n}"), "line {n}");
        }
        for n in (0..count as u64).rev().step_by(97) {
            assert_eq!(source.line(n), format!("{n}"), "line {n}");
        }
        // An anchor every INDEX_EVERY lines, and one for the first line.
        assert_eq!(source.anchors.len(), count / INDEX_EVERY as usize + 1);
    }

    /// A file too long to index in one poll is indexed over several, and is
    /// readable throughout — the count says how far it has got, and the page
    /// says that is what it is.
    #[test]
    fn a_file_longer_than_one_polls_worth_is_indexed_over_several() {
        let count = 4 * INDEX_EVERY as usize;
        let path = numbered(count);
        let all = fs::metadata(&path).unwrap().len();

        let mut source = Source::new(path);
        source.budget = all / 4;
        source.poll();
        assert!(source.indexing(), "done in one poll");
        let reached = source.count();
        assert!(reached > 0 && reached < count as u64, "{reached}");
        // What it has reached is right, however much is left.
        assert_eq!(source.line(0), "0");
        assert_eq!(source.line(reached - 1), format!("{}", reached - 1));

        // Polling on finishes it, and the count is the file's.
        while source.indexing() {
            source.poll();
        }
        assert_eq!(source.count(), count as u64);
        assert_eq!(source.line(count as u64 - 1), format!("{}", count - 1));
    }

    #[test]
    fn a_line_too_long_to_keep_is_cut_and_says_so() {
        let dir = scratch("longline");
        let path = dir.join("tool.out");
        fs::write(&path, format!("{}\nshort\n", "y".repeat(LINE_BYTES + 500))).unwrap();

        let mut source = Source::new(path);
        source.poll();
        assert_eq!(source.count(), 2);
        let long = source.line(0).to_string();
        assert_eq!(long.chars().count(), LINE_BYTES + 1);
        assert!(long.ends_with('…'), "{}", &long[long.len() - 8..]);
        assert_eq!(source.line(1), "short");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn tool_output_is_cleaned_before_it_is_shown() {
        let dir = scratch("cleaned");
        let path = dir.join("tool.out");
        fs::write(
            &path,
            b"\x1b[1;31m**ERROR\x1b[0m: bad\tthing\r\n\xff\xfe raw\n",
        )
        .unwrap();

        let mut source = Source::new(path);
        source.poll();
        assert_eq!(
            lines(&mut source),
            ["**ERROR: bad    thing", "\u{fffd}\u{fffd} raw"]
        );
        let _ = fs::remove_dir_all(&dir);
    }

    // -- scrolling ----------------------------------------------------------

    /// A view over `count` one-row lines, numbered from zero.
    fn view(count: usize) -> LogView {
        let mut view = LogView::new(numbered(count));
        view.poll();
        view
    }

    /// Another line written to what a view is reading.
    fn grew(view: &mut LogView, text: &str) {
        append(&view.source.path.clone(), &format!("{text}\n"));
        view.poll();
    }

    fn shown(view: &mut LogView, height: u16) -> Vec<String> {
        view.render(80, height).rows.iter().map(plain).collect()
    }

    #[test]
    fn a_log_follows_its_end_until_scrolled_and_again_once_scrolled_back() {
        let mut view = view(100);
        assert_eq!(shown(&mut view, 3), ["97", "98", "99"]);
        assert!(view.render(80, 3).following);

        view.scroll(-1, 80, 3);
        assert_eq!(shown(&mut view, 3), ["96", "97", "98"]);
        assert!(!view.render(80, 3).following);

        // New lines arrive; the view stays where it was put.
        grew(&mut view, "100");
        assert_eq!(shown(&mut view, 3), ["96", "97", "98"]);

        // Down to the end again, and it follows again.
        view.scroll(2, 80, 3);
        assert_eq!(shown(&mut view, 3), ["98", "99", "100"]);
        assert!(view.render(80, 3).following);
        grew(&mut view, "101");
        assert_eq!(shown(&mut view, 3), ["99", "100", "101"]);
    }

    #[test]
    fn a_log_scrolls_to_its_top_and_no_further_and_back_to_its_end() {
        let mut view = view(10);
        view.scroll_to_top();
        assert_eq!(shown(&mut view, 3), ["0", "1", "2"]);
        view.scroll(-5, 80, 3);
        assert_eq!(shown(&mut view, 3), ["0", "1", "2"]);
        view.scroll(4, 80, 3);
        assert_eq!(shown(&mut view, 3), ["4", "5", "6"]);
        view.follow();
        assert_eq!(shown(&mut view, 3), ["7", "8", "9"]);
    }

    /// The top of a file that has scrolled far past it, which is what nothing
    /// being forgotten is for: `g` reaches line 0 of a long log.
    #[test]
    fn the_top_of_a_long_log_is_still_there_to_scroll_back_to() {
        let count = 3 * INDEX_EVERY as usize;
        let mut view = view(count);
        assert_eq!(
            shown(&mut view, 2),
            [format!("{}", count - 2), format!("{}", count - 1)]
        );

        view.scroll_to_top();
        assert_eq!(shown(&mut view, 2), ["0", "1"]);
        assert!(!view.render(80, 2).following);

        // And on down through it, a screen at a time, arriving where it ends.
        view.follow();
        assert_eq!(
            shown(&mut view, 2),
            [format!("{}", count - 2), format!("{}", count - 1)]
        );
    }

    #[test]
    fn a_log_shorter_than_the_screen_starts_at_the_top_and_follows() {
        let mut view = view(2);
        assert_eq!(shown(&mut view, 5), ["0", "1"]);
        assert!(view.render(80, 5).following);
        view.scroll(-1, 80, 5);
        assert!(view.render(80, 5).following);
    }

    #[test]
    fn scrolling_moves_by_wrapped_rows_not_lines() {
        let dir = scratch("wrapped");
        let path = dir.join("log");
        fs::write(&path, "aaaa\nbbbbbbbb\ncc\n").unwrap(); // the middle one is two rows at width 4
        let mut view = LogView::new(path);
        view.poll();

        let rows = |view: &mut LogView| -> Vec<String> {
            view.render(4, 2).rows.iter().map(plain).collect()
        };
        assert_eq!(rows(&mut view), ["bbbb", "cc"]);
        view.scroll(-1, 4, 2);
        assert_eq!(rows(&mut view), ["bbbb", "bbbb"]);
        view.scroll(-1, 4, 2);
        assert_eq!(rows(&mut view), ["aaaa", "bbbb"]);
        view.scroll(1, 4, 2);
        assert_eq!(rows(&mut view), ["bbbb", "bbbb"]);
        let _ = fs::remove_dir_all(&dir);
    }

    // -- searching ----------------------------------------------------------

    /// A view over lines that are worth searching, three rows tall.
    fn haystack() -> LogView {
        let path = own_dir("haystack").join("log");
        fs::write(
            &path,
            "alpha\nbeta\nERROR: first\ngamma\nerror: second\ndelta\nepsilon\n",
        )
        .unwrap();
        let mut view = LogView::new(path);
        view.poll();
        view
    }

    /// The line a search left on top of the screen.
    fn found(view: &LogView) -> Option<u64> {
        view.find.as_ref().and_then(|find| find.at)
    }

    #[test]
    fn a_search_puts_the_line_it_found_on_top_of_the_screen() {
        let mut view = haystack();
        view.scroll_to_top();
        view.look("gamma", false, 80, 3);
        assert_eq!(found(&view), Some(3));
        assert_eq!(shown(&mut view, 3), ["gamma", "error: second", "delta"]);
        assert!(!view.render(80, 3).following);
    }

    #[test]
    fn a_search_ignores_case_unless_the_pattern_has_a_capital_in_it() {
        let mut view = haystack();
        view.scroll_to_top();
        // All lowercase: either line will do, and the first one is line 2.
        view.look("error", false, 80, 3);
        assert_eq!(found(&view), Some(2));

        // A capital means it was meant: only the line that has it.
        view.scroll_to_top();
        view.look("ERROR", false, 80, 3);
        assert_eq!(found(&view), Some(2));
        view.scroll_to_top();
        view.look("Error", false, 80, 3);
        assert!(view.find.as_ref().unwrap().missing);
    }

    #[test]
    fn a_search_wraps_round_the_end_of_the_file_and_says_it_did() {
        let mut view = haystack();
        // From the last line, forwards: what it finds is round the far end.
        view.top = Some(Top { line: 6, row: 0 });
        view.look("alpha", false, 80, 3);
        assert_eq!(found(&view), Some(0));
        assert!(view.find.as_ref().unwrap().wrapped);

        // And backwards from the top, the same the other way.
        view.scroll_to_top();
        view.look("epsilon", true, 80, 3);
        assert_eq!(found(&view), Some(6));
        assert!(view.find.as_ref().unwrap().wrapped);
    }

    #[test]
    fn a_search_that_finds_nothing_says_so_and_leaves_the_screen_alone() {
        let mut view = haystack();
        view.scroll_to_top();
        view.look("nowhere", false, 80, 3);
        let find = view.find.as_ref().unwrap();
        assert!(find.missing);
        assert_eq!(find.at, None);
        assert_eq!(shown(&mut view, 3), ["alpha", "beta", "ERROR: first"]);
    }

    #[test]
    fn n_goes_on_to_the_next_and_shift_n_back_to_the_one_before() {
        let mut view = haystack();
        view.scroll_to_top();
        view.look("e", false, 80, 3);
        let first = found(&view).unwrap();

        view.again(true, 80, 3);
        let second = found(&view).unwrap();
        assert!(second > first, "{first} then {second}");

        // The other way round, which is back where it was.
        view.again(false, 80, 3);
        assert_eq!(found(&view), Some(first));
    }

    #[test]
    fn a_search_of_a_long_log_is_spread_over_frames_and_finishes() {
        // Longer than one frame's worth, with what it is looking for at the
        // very end, so the first pass cannot reach it.
        let count = SEARCH_LINES as usize + 100;
        let path = numbered(count);
        append(&path, "needle\n");
        let mut view = LogView::new(path);
        view.poll();
        view.scroll_to_top();

        view.look("needle", false, 80, 3);
        assert!(
            view.find.as_ref().unwrap().next.is_some(),
            "searched it all in one frame"
        );
        assert_eq!(found(&view), None);

        // Each frame does the next slice, and it is found without the display
        // ever having stopped.
        for _ in 0..10 {
            view.poll();
            if found(&view).is_some() {
                break;
            }
        }
        assert_eq!(found(&view), Some(count as u64));
    }

    #[test]
    fn what_matched_is_picked_out_of_the_row_it_is_in() {
        let plain_row = highlight("nothing here".to_string(), None);
        assert_eq!(plain_row.spans.len(), 1);

        let row = highlight("a beta and a Beta".to_string(), Some(("beta", true)));
        assert_eq!(plain(&row), "a beta and a Beta");
        let lit: Vec<&str> = row
            .spans
            .iter()
            .filter(|span| span.style.bg.is_some())
            .map(|span| span.content.as_ref())
            .collect();
        // Both, folded — and each of them whole, with its own case kept.
        assert_eq!(lit, ["beta", "Beta"]);

        // A capital in the pattern means only the one.
        let row = highlight("a beta and a Beta".to_string(), Some(("Beta", false)));
        let lit: Vec<&str> = row
            .spans
            .iter()
            .filter(|span| span.style.bg.is_some())
            .map(|span| span.content.as_ref())
            .collect();
        assert_eq!(lit, ["Beta"]);

        // Text either side of a match is left as it was.
        let row = highlight("xxfindxx".to_string(), Some(("find", false)));
        assert_eq!(plain(&row), "xxfindxx");
        assert_eq!(row.spans.len(), 3);
    }

    #[test]
    fn a_search_typed_at_the_prompt_has_the_keyboard_until_it_is_run() {
        let paint = OneStep::default();
        let mut page = run_page(100);

        page.key(press(KeyCode::Char('/')), &paint);
        assert!(page.prompt.is_some(), "no prompt");
        // Keys that would otherwise do something are the pattern instead: `q`
        // does not ask to cancel the run, and `j` does not scroll.
        for c in "q9j".chars() {
            page.key(press(KeyCode::Char(c)), &paint);
        }
        assert_eq!(page.prompt.as_ref().unwrap().text, "q9j");
        assert!(!page.confirming, "asked to cancel the run");
        assert_eq!(run_rows(&mut page), ["97", "98", "99"]);
        assert!(plain(&page.hint(true, 80)).starts_with("  /q9j"));

        // Backspaced away to nothing, it is gone.
        for _ in 0..3 {
            page.key(press(KeyCode::Backspace), &paint);
        }
        assert!(page.prompt.is_none());

        // Typed and run, it searches — and the page is still the page.
        page.key(press(KeyCode::Char('/')), &paint);
        for c in "42".chars() {
            page.key(press(KeyCode::Char(c)), &paint);
        }
        page.key(press(KeyCode::Enter), &paint);
        assert!(page.prompt.is_none());
        assert_eq!(run_rows(&mut page), ["42", "43", "44"]);

        // And `esc` puts a prompt away without closing the page under it.
        page.key(press(KeyCode::Char('?')), &paint);
        assert!(page.prompt.as_ref().unwrap().backwards);
        page.key(press(KeyCode::Esc), &paint);
        assert!(page.prompt.is_none());
        assert!(matches!(page.page, Page::Log(_)));
    }

    #[test]
    fn the_less_keys_page_and_follow() {
        let paint = OneStep::default();
        let mut page = run_page(100);
        let rows = 3;

        page.key(press(KeyCode::Char('b')), &paint);
        assert_eq!(run_rows(&mut page), ["94", "95", "96"]);
        page.key(press(KeyCode::Char(' ')), &paint);
        assert_eq!(run_rows(&mut page), ["97", "98", "99"]);

        page.key(press(KeyCode::Char('b')), &paint);
        assert_eq!(run_rows(&mut page).len(), rows);
        page.key(press(KeyCode::Char('F')), &paint);
        assert_eq!(run_rows(&mut page), ["97", "98", "99"]);
    }

    // -- the banner ---------------------------------------------------------

    #[test]
    fn the_banner_shrinks_with_the_terminal() {
        let about = About {
            targets: vec!["decoder signoff".into()],
            steps: 7,
            workers: 4,
            log_dir: Some(PathBuf::from("/build")),
        };

        // Tall: the wordmark, the facts beside it, and a blank row under.
        let tall = banner_lines(&about, 40);
        assert_eq!(tall.len(), WORDMARK.len() + 1);
        let text: Vec<String> = tall.iter().map(plain).collect();
        assert!(text[0].ends_with("decoder signoff"), "{:?}", text[0]);
        assert!(text[1].ends_with("7 steps · 4 workers"), "{:?}", text[1]);
        assert!(text[2].ends_with("logs in /build"), "{:?}", text[2]);
        assert_eq!(text[3], "");
        // The wordmark's rows are all the same width, so the facts line up.
        let marks: Vec<usize> = WORDMARK.iter().map(|row| columns(row)).collect();
        assert!(marks.iter().all(|&width| width == marks[0]), "{marks:?}");

        // Shorter: one line with the essentials.
        let short = banner_lines(&about, 12);
        assert_eq!(short.len(), 1);
        assert_eq!(
            plain(&short[0]),
            "  rivet · decoder signoff · 7 steps · 4 workers"
        );

        // Too short for anything.
        assert!(banner_lines(&about, 6).is_empty());

        // Two targets, one worker, no logging.
        let about = About {
            targets: vec!["drc".into(), "lvs".into()],
            steps: 1,
            workers: 1,
            log_dir: None,
        };
        let text: Vec<String> = banner_lines(&about, 40).iter().map(plain).collect();
        assert!(text[0].ends_with("drc, lvs"), "{:?}", text[0]);
        assert!(text[1].ends_with("1 step · 1 worker"), "{:?}", text[1]);
        assert!(text[2].ends_with("logging off"), "{:?}", text[2]);
    }

    // -- the wheel ----------------------------------------------------------

    /// A run of one finished step, which remembers where it was asked to put
    /// its cursor.
    #[derive(Default)]
    struct OneStep {
        moves: Mutex<Vec<Motion>>,
    }

    impl Paint for OneStep {
        fn screen(&self, _width: usize) -> Screen {
            Screen {
                steps: vec![StepLine {
                    id: 7,
                    line: Line::from("decoder lvs"),
                    running: false,
                }],
                selected: Some(0),
                done: true,
                ..Screen::default()
            }
        }
        fn detail(&self, _id: usize, _width: usize) -> Option<Detail> {
            None
        }
        fn move_cursor(&self, motion: Motion) {
            self.moves.lock().unwrap().push(motion);
        }
        fn select(&self, _id: usize) {}
        fn done(&self) -> bool {
            true
        }
        fn detach(&self) -> Vec<Line<'static>> {
            Vec::new()
        }
    }

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn mouse(kind: MouseEventKind) -> Event {
        Event::Mouse(MouseEvent {
            kind,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        })
    }

    /// A step's page, open on a log of `count` one-row lines, three rows tall.
    fn page(count: usize) -> View {
        opened(Pager::step(7), count)
    }

    /// The run's page, open on a log of `count` one-row lines.
    fn run_page(count: usize) -> View {
        opened(Pager::run(), count)
    }

    fn opened(mut pager: Pager, count: usize) -> View {
        let log = view(count);
        pager.files = vec![log.source.path.clone()];
        pager.pane.log = Some(log);
        pager.pane.columns = 80;
        pager.pane.rows = 3;
        View {
            page: Page::Log(Box::new(pager)),
            ..View::default()
        }
    }

    /// The page's pager, whichever kind of page it is.
    fn pager(page: &mut View) -> &mut Pager {
        let Page::Log(pager) = &mut page.page else {
            panic!("not a log's page");
        };
        pager
    }

    /// What the log on an open page is showing.
    fn log_rows(page: &mut View) -> Vec<String> {
        shown(pager(page).pane.log.as_mut().unwrap(), 3)
    }

    /// The same, for a page opened on the run's log.
    fn run_rows(page: &mut View) -> Vec<String> {
        log_rows(page)
    }

    fn is_following(page: &mut View) -> bool {
        pager(page)
            .pane
            .log
            .as_mut()
            .unwrap()
            .render(80, 3)
            .following
    }

    #[test]
    fn the_wheel_scrolls_an_open_log_and_follows_its_end_again_at_the_bottom() {
        let mut page = page(100);
        assert_eq!(log_rows(&mut page), ["97", "98", "99"]);

        // A notch is WHEEL rows, and the log stops following its end.
        page.event(mouse(MouseEventKind::ScrollUp), &OneStep::default());
        assert_eq!(log_rows(&mut page), ["94", "95", "96"]);
        page.event(mouse(MouseEventKind::ScrollUp), &OneStep::default());
        assert_eq!(log_rows(&mut page), ["91", "92", "93"]);
        assert!(!is_following(&mut page));

        // Back down to the end, and it follows again.
        for _ in 0..2 {
            page.event(mouse(MouseEventKind::ScrollDown), &OneStep::default());
        }
        assert_eq!(log_rows(&mut page), ["97", "98", "99"]);
        assert!(is_following(&mut page));
    }

    #[test]
    fn the_wheel_moves_the_cursor_on_the_list() {
        let paint = OneStep::default();
        let mut view = View::default();

        view.event(mouse(MouseEventKind::ScrollUp), &paint);
        view.event(mouse(MouseEventKind::ScrollDown), &paint);
        assert_eq!(
            *paint.moves.lock().unwrap(),
            [Motion::Up(WHEEL), Motion::Down(WHEEL)]
        );
        // Still the list: the wheel does not open anything.
        assert!(matches!(view.page, Page::List));
    }

    #[test]
    fn nothing_else_the_mouse_does_counts_on_either_page() {
        let idle = [
            MouseEventKind::Moved,
            MouseEventKind::Down(MouseButton::Left),
            MouseEventKind::Up(MouseButton::Left),
            MouseEventKind::Drag(MouseButton::Left),
            MouseEventKind::Down(MouseButton::Right),
            // The wheel tilted sideways: there is nothing to scroll across,
            // because a log's lines are wrapped rather than run off the edge.
            MouseEventKind::ScrollLeft,
            MouseEventKind::ScrollRight,
        ];

        // Nothing on screen is meant to be clicked, and a drag is someone
        // reaching for the terminal's selection rather than for the display.
        let paint = OneStep::default();
        let mut view = View::default();
        for kind in idle {
            assert!(matches!(view.event(mouse(kind), &paint), Action::None));
        }
        assert!(paint.moves.lock().unwrap().is_empty());
        assert!(matches!(view.page, Page::List));

        let mut page = page(100);
        page.event(mouse(MouseEventKind::ScrollUp), &paint);
        for kind in idle {
            page.event(mouse(kind), &paint);
        }
        assert_eq!(log_rows(&mut page), ["94", "95", "96"]);
    }

    #[test]
    fn only_the_pointer_moving_does_not_earn_a_frame_of_its_own() {
        // It changes nothing on screen, and a terminal reporting it does so
        // continuously. Nothing asks for it, and this is the guard if one
        // arrives anyway.
        assert!(!worth_a_frame(&mouse(MouseEventKind::Moved)));

        // A drag pulls the selection with it, which is worth seeing happen.
        assert!(worth_a_frame(&mouse(MouseEventKind::Drag(
            MouseButton::Left
        ))));
        assert!(worth_a_frame(&mouse(MouseEventKind::Down(
            MouseButton::Left
        ))));
        assert!(worth_a_frame(&mouse(MouseEventKind::ScrollUp)));
        assert!(worth_a_frame(&mouse(MouseEventKind::ScrollDown)));
        assert!(worth_a_frame(&Event::Key(press(KeyCode::Char('j')))));
        assert!(worth_a_frame(&Event::Resize(80, 24)));
    }

    #[test]
    fn the_keys_still_open_and_close_a_step_page() {
        let paint = OneStep::default();
        // The cursor is ordinarily put here by the frame that drew the list.
        let mut view = View {
            selected: Some(7),
            ..View::default()
        };
        view.key(press(KeyCode::Enter), &paint);
        assert!(matches!(view.page, Page::Log(_)));
        view.key(press(KeyCode::Esc), &paint);
        assert!(matches!(view.page, Page::List));
    }

    #[test]
    fn the_run_log_opens_from_the_list_and_closes_back_to_it() {
        let paint = OneStep::default();
        let mut view = View {
            selected: Some(7),
            ..View::default()
        };

        view.key(press(KeyCode::Char('L')), &paint);
        let Page::Log(pager) = &view.page else {
            panic!("not a log's page");
        };
        assert_eq!(pager.step, None, "opened as a step's page");
        view.key(press(KeyCode::Esc), &paint);
        assert!(matches!(view.page, Page::List));

        // A step's page is a step's, and closing it goes back to that step.
        view.key(press(KeyCode::Enter), &paint);
        let Page::Log(pager) = &view.page else {
            panic!("not a log's page");
        };
        assert_eq!(pager.step, Some(7));
    }

    #[test]
    fn a_steps_page_reaches_the_run_log_by_tab_and_by_key() {
        let paint = OneStep::default();
        let mut page = page(100);
        let out = numbered(4);
        let err = numbered(5);
        let run = numbered(6);

        // What a frame does: hand the page the step's files and the run's log.
        pager(&mut page).sync(&[out.clone(), err.clone()], Some(run.clone()));
        assert_eq!(
            pager(&mut page).files,
            [out.clone(), err.clone(), run.clone()]
        );
        let open = |page: &mut View| pager(page).pane.log.as_ref().unwrap().source.path.clone();
        assert_eq!(open(&mut page), out);

        // `tab` goes through the step's files and on into the run's log,
        // which is one of the files this page can read.
        page.key(press(KeyCode::Tab), &paint);
        assert_eq!(open(&mut page), err);
        page.key(press(KeyCode::Tab), &paint);
        assert_eq!(open(&mut page), run);
        page.key(press(KeyCode::Tab), &paint);
        assert_eq!(open(&mut page), out, "did not come round");
        page.key(press(KeyCode::BackTab), &paint);
        assert_eq!(open(&mut page), run, "did not go back round");

        // And `L` goes straight to it from wherever in the cycle.
        page.key(press(KeyCode::Tab), &paint);
        assert_eq!(open(&mut page), out);
        page.key(press(KeyCode::Char('L')), &paint);
        assert_eq!(open(&mut page), run);
        // Still the step's page: the step is what it is a page for.
        assert_eq!(pager(&mut page).step, Some(7));
    }

    #[test]
    fn pressing_the_run_logs_key_on_the_run_log_leaves_it_where_it_is() {
        let paint = OneStep::default();
        let mut page = run_page(100);
        page.key(press(KeyCode::Char('k')), &paint);
        assert_eq!(run_rows(&mut page), ["96", "97", "98"]);

        // Not reopened from the top, which would throw away where it was read
        // to: `L` is how it is opened, and `esc` how it is left.
        page.key(press(KeyCode::Char('L')), &paint);
        assert!(matches!(page.page, Page::Log(_)));
        assert_eq!(run_rows(&mut page), ["96", "97", "98"]);
    }

    #[test]
    fn the_run_log_scrolls_and_follows_the_way_a_steps_log_does() {
        let paint = OneStep::default();
        let mut page = run_page(100);
        assert_eq!(run_rows(&mut page), ["97", "98", "99"]);

        page.key(press(KeyCode::Char('k')), &paint);
        page.key(press(KeyCode::Char('k')), &paint);
        assert_eq!(run_rows(&mut page), ["95", "96", "97"]);

        page.key(press(KeyCode::Home), &paint);
        assert_eq!(run_rows(&mut page), ["0", "1", "2"]);

        page.key(press(KeyCode::PageDown), &paint);
        assert_eq!(run_rows(&mut page), ["3", "4", "5"]);

        page.key(press(KeyCode::Char('G')), &paint);
        assert_eq!(run_rows(&mut page), ["97", "98", "99"]);

        // And the wheel, which reaches whichever log is open.
        page.event(mouse(MouseEventKind::ScrollUp), &paint);
        assert_eq!(run_rows(&mut page), ["94", "95", "96"]);

        // `y` offers the file it is reading, so it can be read in full
        // somewhere else.
        let Action::Copy(files) = page.key(press(KeyCode::Char('y')), &paint) else {
            panic!("y copies");
        };
        let path = pager(&mut page)
            .pane
            .log
            .as_ref()
            .unwrap()
            .source
            .path
            .clone();
        assert_eq!(files, [path]);
    }

    #[test]
    fn the_run_page_reads_the_log_the_banner_names() {
        let mut about = About {
            targets: vec!["decoder signoff".into()],
            steps: 7,
            workers: 4,
            log_dir: Some(PathBuf::from("/build")),
        };
        assert_eq!(run_log(&about), Some(PathBuf::from("/build/rivet.log")));

        // A run that is not logging has nothing for the page to read, which is
        // what it says instead of showing an empty one.
        about.log_dir = None;
        assert_eq!(run_log(&about), None);
    }

    #[test]
    fn a_pane_reads_the_file_it_is_pointed_at_until_pointed_at_another() {
        let mut pane = Pane::default();
        assert!(pane.log.is_none());
        assert!(pane.files().is_empty());

        let one = numbered(3);
        let two = numbered(7);
        pane.sync(Some(&one));
        assert_eq!(pane.files(), std::slice::from_ref(&one));

        // The same file again is the same read, not a fresh one from the top.
        pane.log.as_mut().unwrap().poll();
        pane.log.as_mut().unwrap().scroll_to_top();
        pane.sync(Some(&one));
        assert!(pane.log.as_ref().unwrap().top.is_some(), "started again");

        // A different one starts again, at its end.
        pane.sync(Some(&two));
        pane.log.as_mut().unwrap().poll();
        assert_eq!(pane.log.as_ref().unwrap().source.count(), 7);
        assert!(pane.log.as_ref().unwrap().top.is_none(), "not following");

        // Nothing to read — a run that was told not to log.
        pane.sync(None);
        assert!(pane.log.is_none());
    }

    // -- selecting ----------------------------------------------------------

    fn click(kind: MouseEventKind, at: (u16, u16)) -> Event {
        Event::Mouse(MouseEvent {
            kind,
            column: at.0,
            row: at.1,
            modifiers: KeyModifiers::NONE,
        })
    }

    /// A buffer with `rows` written into it, one per line from the top left.
    fn painted(rows: &[&str], width: u16) -> Buffer {
        let mut buffer = Buffer::empty(Rect::new(0, 0, width, rows.len() as u16));
        for (y, row) in rows.iter().enumerate() {
            for (x, c) in row.chars().enumerate() {
                if let Some(cell) = buffer.cell_mut((x as u16, y as u16)) {
                    cell.set_symbol(&c.to_string());
                }
            }
        }
        buffer
    }

    /// The text a drag from `from` to `to` takes off `rows`.
    fn dragged(rows: &[&str], width: u16, from: (u16, u16), to: (u16, u16)) -> Option<String> {
        let mut buffer = painted(rows, width);
        let mut selection = Selection::new(from);
        selection.head = to;
        selection.copy = true;
        selection.mark(&mut buffer, None)
    }

    #[test]
    fn a_selection_takes_whole_rows_between_its_ends_the_way_a_terminal_does() {
        let rows = [
            "**ERROR: bad thing",
            "  at decoder.v:12",
            "  and again here",
        ];

        // Within one row, it is the run between the two cells, both included.
        assert_eq!(dragged(&rows, 20, (2, 0), (6, 0)).unwrap(), "ERROR");

        // Across rows, the first runs to the edge and the last runs from it.
        assert_eq!(
            dragged(&rows, 20, (9, 0), (16, 1)).unwrap(),
            "bad thing\n  at decoder.v:12"
        );
        assert_eq!(
            dragged(&rows, 20, (0, 0), (15, 2)).unwrap(),
            "**ERROR: bad thing\n  at decoder.v:12\n  and again here"
        );

        // Dragged back up the screen it is the same selection.
        assert_eq!(
            dragged(&rows, 20, (16, 1), (9, 0)).unwrap(),
            dragged(&rows, 20, (9, 0), (16, 1)).unwrap()
        );

        // The blanks past the end of a line are the screen's, not the text's.
        assert_eq!(
            dragged(&rows, 40, (0, 2), (39, 2)).unwrap(),
            "  and again here"
        );

        // Nor is a scrollbar down the edge text: dragging a log to the right
        // of the screen must not paste a column of furniture along with it.
        let mut buffer = painted(&["one    ║", "two    █"], 8);
        let mut selection = Selection::new((0, 0));
        selection.head = (7, 1);
        selection.copy = true;
        assert_eq!(selection.mark(&mut buffer, Some(7)).unwrap(), "one\ntwo");
        // And a selection of nothing but screen is not worth copying.
        assert!(dragged(&["", ""], 20, (5, 0), (9, 1)).is_none());
    }

    #[test]
    fn a_selection_reverses_what_it_covers_and_leaves_the_rest() {
        let mut buffer = painted(&["abcdef", "ghijkl"], 6);
        let mut selection = Selection::new((4, 0));
        selection.head = (1, 1);
        assert!(
            selection.mark(&mut buffer, None).is_none(),
            "not asked to copy yet"
        );

        let reversed = |x: u16, y: u16| {
            buffer
                .cell((x, y))
                .unwrap()
                .modifier
                .contains(Modifier::REVERSED)
        };
        // The first row from the anchor to the edge, the last up to the head.
        assert!(!reversed(3, 0));
        assert!(reversed(4, 0) && reversed(5, 0));
        assert!(reversed(0, 1) && reversed(1, 1));
        assert!(!reversed(2, 1));
    }

    #[test]
    fn dragging_selects_and_letting_go_asks_for_the_copy() {
        let paint = OneStep::default();
        let mut view = View::default();

        view.event(
            click(MouseEventKind::Down(MouseButton::Left), (3, 4)),
            &paint,
        );
        assert_eq!(view.selection.unwrap().anchor, (3, 4));
        view.event(
            click(MouseEventKind::Drag(MouseButton::Left), (9, 6)),
            &paint,
        );
        let selection = view.selection.unwrap();
        assert_eq!((selection.anchor, selection.head), ((3, 4), (9, 6)));
        assert!(selection.dragging && !selection.copy);

        // Letting go leaves it on screen, marked for the next frame to copy.
        view.event(click(MouseEventKind::Up(MouseButton::Left), (9, 6)), &paint);
        let selection = view.selection.unwrap();
        assert!(!selection.dragging && selection.copy);

        // A click that goes nowhere is how it is put away again.
        view.event(
            click(MouseEventKind::Down(MouseButton::Left), (1, 1)),
            &paint,
        );
        view.event(click(MouseEventKind::Up(MouseButton::Left), (1, 1)), &paint);
        assert!(view.selection.is_none());
    }

    #[test]
    fn moving_on_puts_a_selection_away() {
        let paint = OneStep::default();
        for moved_on in [
            click(MouseEventKind::ScrollUp, (0, 0)),
            click(MouseEventKind::ScrollDown, (0, 0)),
            Event::Key(press(KeyCode::Char('j'))),
            Event::Key(press(KeyCode::Enter)),
        ] {
            let mut view = View::default();
            view.event(
                click(MouseEventKind::Down(MouseButton::Left), (3, 4)),
                &paint,
            );
            view.event(
                click(MouseEventKind::Drag(MouseButton::Left), (9, 6)),
                &paint,
            );
            assert!(view.selection.is_some());
            view.event(moved_on, &paint);
            assert!(view.selection.is_none(), "still selected after moving on");
        }
    }

    #[test]
    fn starting_a_selection_holds_a_following_log_still() {
        let paint = OneStep::default();
        let mut page = page(100);
        assert!(is_following(&mut page));
        assert_eq!(log_rows(&mut page), ["97", "98", "99"]);

        // Pressing the button pins it where it is...
        page.event(
            click(MouseEventKind::Down(MouseButton::Left), (2, 1)),
            &paint,
        );
        // ...so the lines that arrive during the drag do not carry it away.
        let log = pager(&mut page).pane.log.as_mut().unwrap();
        let path = log.source.path.clone();
        for n in 100..110 {
            append(&path, &format!("{n}\n"));
        }
        log.poll();
        assert_eq!(log_rows(&mut page), ["97", "98", "99"]);
        assert!(!is_following(&mut page));

        // Putting the selection away lets it follow again.
        page.event(click(MouseEventKind::Up(MouseButton::Left), (2, 1)), &paint);
        assert!(page.selection.is_none());
        assert_eq!(log_rows(&mut page), ["107", "108", "109"]);
        assert!(is_following(&mut page));
    }

    #[test]
    fn reporting_the_mouse_asks_for_drags_but_not_for_idle_motion() {
        use ratatui::crossterm::Command;

        let sequence = |on: bool| {
            let mut out = String::new();
            ReportMouse(on).write_ansi(&mut out).unwrap();
            out
        };
        let on = sequence(true);
        // Without button-event tracking a drag is never reported, and a
        // selection could never grow past the cell it started on.
        assert!(on.contains("\x1b[?1002h"), "no drags: {on:?}");
        assert!(on.contains("\x1b[?1000h"), "no buttons or wheel: {on:?}");
        assert!(on.contains("\x1b[?1006h"), "no SGR coordinates: {on:?}");
        // Any-event tracking would report the pointer for the whole run.
        assert!(!on.contains("1003"), "asked for idle motion: {on:?}");

        // Everything asked for is given back, and nothing else is.
        let off = sequence(false);
        for mode in ["1000", "1002", "1006"] {
            assert!(on.contains(&format!("?{mode}h")), "{mode} not set: {on:?}");
            assert!(
                off.contains(&format!("?{mode}l")),
                "{mode} not unset: {off:?}"
            );
        }
        assert_eq!(on.matches('\x1b').count(), off.matches('\x1b').count());
    }

    #[test]
    fn a_release_away_from_the_press_selects_even_with_no_drags_reported() {
        // A terminal that reports the buttons but not the motion between them
        // still says where the release was, and that is the far end.
        let paint = OneStep::default();
        let mut view = View::default();
        view.event(
            click(MouseEventKind::Down(MouseButton::Left), (4, 2)),
            &paint,
        );
        view.event(
            click(MouseEventKind::Up(MouseButton::Left), (30, 5)),
            &paint,
        );

        let selection = view.selection.expect("nothing selected");
        assert_eq!((selection.anchor, selection.head), ((4, 2), (30, 5)));
        assert!(selection.copy && !selection.dragging);
    }

    // -- fitting the width --------------------------------------------------

    #[test]
    fn the_hint_is_said_at_the_longest_length_that_fits() {
        let long = hint_text(Hint::List, false, 200);
        assert!(long.contains("enter open a step"), "{long}");
        assert_eq!(hint_text(Hint::List, false, columns(&long)), long);

        let medium = hint_text(Hint::List, false, columns(&long) - 1);
        assert!(columns(&medium) < columns(&long));
        assert!(medium.contains("enter open"), "{medium}");
        assert!(medium.contains("q cancel"), "{medium}");

        let short = hint_text(Hint::List, false, 30);
        assert!(columns(&short) <= 30, "{short}");
        assert!(short.ends_with("· q"), "{short}");
        // Narrower than even the shortest: the shortest, for the terminal to
        // cut.
        assert_eq!(hint_text(Hint::List, false, 3), short);

        // Once the run is done, `q` quits, at every length.
        for width in [200, 60] {
            assert!(hint_text(Hint::List, true, width).ends_with("q quit"));
            assert!(!hint_text(Hint::List, true, width).contains("cancel"));
        }

        // The question `q` asks, at every length, says what `y` does.
        for width in [200, 80, 20, 3] {
            let question = confirm_text(width);
            assert!(question.contains("cancel the run"), "{question}");
            assert!(question.contains('y'), "{question}");
            assert!(columns(&question) <= width || width < 25, "{question}");
        }
        assert!(hint_text(Hint::Step, true, 200).starts_with("  esc back"));
        assert!(columns(&hint_text(Hint::Step, true, 40)) <= 40);

        // The run log is offered from the pages that are not it, and its own
        // page says nothing about the `tab` it has no files to cycle.
        assert!(hint_text(Hint::List, true, 200).contains("L run log"));
        assert!(hint_text(Hint::Step, true, 200).contains("L run log"));
        assert!(!hint_text(Hint::Run, true, 200).contains("run log"));
        assert!(!hint_text(Hint::Run, true, 200).contains("tab"));
        assert!(hint_text(Hint::Run, true, 200).starts_with("  esc back"));

        // The longer tiers are written across two source lines, and a string
        // continuation that did not eat its indentation would show up as a gap
        // in the middle of the line.
        for page in [Hint::List, Hint::Step, Hint::Run] {
            for width in [200, 120, 95, 80, 50, 20] {
                let hint = hint_text(page, false, width);
                assert!(!hint.trim_start().contains("  "), "{width}: {hint}");
            }
        }

        // The mouse is reported on both pages, so both own up to `shift`
        // wherever there is room for more than the keys themselves.
        for page in [Hint::List, Hint::Step, Hint::Run] {
            for done in [true, false] {
                for width in [200, 120, 100, 95, 80, 63, 50, 40] {
                    let hint = hint_text(page, done, width);
                    assert!(columns(&hint) <= width, "{page:?} {width}: {hint}");
                    assert!(hint.contains("drag copies"), "{page:?} {width}: {hint}");
                    // Nothing offers to cancel a run that is already over.
                    assert!(!done || !hint.contains("cancel"), "{width}: {hint}");
                }
            }
            assert!(hint_text(page, false, 200).contains("wheel"));

            // Below that, the keys are all that fits — the pages with fewer of
            // them to list hold on a little longer.
            let floor = match page {
                Hint::List => 35,
                Hint::Step => 39,
                Hint::Run => 33,
            };
            assert!(
                hint_text(page, false, floor).contains("drag copies"),
                "{page:?}"
            );
            assert!(
                !hint_text(page, false, floor - 1).contains("drag"),
                "{page:?}"
            );
        }
    }

    #[test]
    fn a_long_path_is_cut_from_the_left() {
        assert_eq!(
            shorten_left("/build/decoder.par.out", 30),
            "/build/decoder.par.out"
        );
        assert_eq!(
            shorten_left("/build/decoder.par.out", 22),
            "/build/decoder.par.out"
        );
        assert_eq!(
            shorten_left("/build/decoder.par.out", 21),
            "…uild/decoder.par.out"
        );
        assert_eq!(shorten_left("/build/decoder.par.out", 8), "…par.out");
        assert_eq!(shorten_left("漢字表", 3), "…表");
    }

    // -- what y copies ------------------------------------------------------

    #[test]
    fn the_copied_command_opens_every_file_it_is_given_in_less() {
        assert_eq!(
            view_command(&[PathBuf::from("/build/decoder par.rivet.log")]),
            r"less '/build/decoder par.rivet.log'"
        );
        assert_eq!(
            view_command(&[
                PathBuf::from("/build/decoder.par.out"),
                PathBuf::from("/build/decoder.par.err"),
            ]),
            "less /build/decoder.par.out /build/decoder.par.err"
        );
    }

    #[test]
    fn paths_are_quoted_for_the_shell_they_are_pasted_into() {
        assert_eq!(quote("/build/decoder.par.out"), "/build/decoder.par.out");
        // Step labels have spaces in them, and their log files are named after
        // them.
        assert_eq!(
            quote("/build/decoder par.rivet.log"),
            "'/build/decoder par.rivet.log'"
        );
        assert_eq!(quote("/build/it's.log"), r"'/build/it'\''s.log'");
        assert_eq!(quote(""), "''");
    }

    // -- the record ---------------------------------------------------------

    #[test]
    fn the_record_is_written_with_its_styling_and_reset_after() {
        let line = Line::from(vec![
            span("✔ ", Style::new().green()),
            span("decoder par", Style::new().bold()),
            span("  1m04s", Style::new().dim()),
        ]);
        let text = ansi(&line);
        // ratatui's plain green is crossterm's dark green, colour 2.
        assert!(text.contains("\x1b[38;5;2m✔ "), "{text:?}");
        assert!(text.contains("\x1b[1mdecoder par"), "{text:?}");
        assert!(text.contains("\x1b[2m  1m04s"), "{text:?}");
        assert!(text.ends_with("\x1b[0m"), "{text:?}");
        assert_eq!(ansi(&Line::from("plain")), "plain");
    }
}
