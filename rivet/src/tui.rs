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
//! There are two pages. The list is every step in the run — pinned, finished,
//! running, and greyed, still to come — with a cursor on one of them; `enter`
//! opens that step, which is its log as it is written, with the step's own line
//! and the run's summary underneath; `esc` closes it again. The step's files —
//! the output of the tool it is running, and its own log — are a `tab` apart,
//! and `y` copies a command to open them in a terminal of one's own.
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

/// Most notes kept on screen under the list.
const MAX_NOTES: usize = 3;

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

/// Most lines of a log kept in memory to scroll back through.
const MAX_LINES: usize = 10_000;

/// Most of a file read in one go. A log that is further behind than this — one
/// that was already large when opened, or a tool writing faster than it can be
/// read — is picked up from this close to its end, and says so.
const BACKLOG: u64 = 2 * 1024 * 1024;

/// How much of a file is read at a time.
const CHUNK: usize = 64 * 1024;

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
    /// Things said during the run that are not steps, most recent last.
    pub notes: Vec<Line<'static>>,
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
}

#[derive(Default)]
enum Page {
    #[default]
    List,
    Detail(Box<Watch>),
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
                match &mut self.page {
                    Page::List => paint.move_cursor(if up {
                        Motion::Up(WHEEL)
                    } else {
                        Motion::Down(WHEEL)
                    }),
                    Page::Detail(watch) => {
                        let by = WHEEL as isize;
                        watch.scroll(if up { -by } else { by });
                    }
                }
            }
            MouseEventKind::Down(MouseButton::Left) => {
                // A log that is following its end would go on moving under the
                // selection, so it is held where it is until the selection is
                // done with.
                self.deselect();
                if let Page::Detail(watch) = &mut self.page {
                    watch.pin();
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
            if let Page::Detail(watch) = &mut self.page {
                watch.unpin();
            }
        }
    }

    fn key(&mut self, key: KeyEvent, paint: &dyn Paint) -> Action {
        use KeyCode::*;
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

        // Whatever the key turns out to do, typing means the selection on
        // screen is finished with.
        self.deselect();

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
                            self.page = Page::Detail(Box::new(Watch::new(id)));
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
            Page::Detail(watch) => {
                let page = watch.rows.max(1) as isize;
                match key.code {
                    Esc | Backspace | Left | Char('h') => {
                        // Back to where the list was left, on the step that
                        // was open: the cursor may have moved on by itself in
                        // the meantime, if that step finished.
                        paint.select(watch.id);
                        self.page = Page::List;
                    }
                    Up | Char('k') => watch.scroll(-1),
                    Down | Char('j') => watch.scroll(1),
                    PageUp => watch.scroll(-page),
                    PageDown => watch.scroll(page),
                    Char('u') if ctrl => watch.scroll(-(page / 2).max(1)),
                    Char('d') if ctrl => watch.scroll((page / 2).max(1)),
                    Home | Char('g') => watch.scroll_to_top(),
                    End | Char('G') => watch.follow(),
                    Tab | Char(']') => watch.next_file(1, paint),
                    BackTab | Char('[') => watch.next_file(-1, paint),
                    Char('y') => {
                        let files = watch.log.as_ref().map(|log| vec![log.tail.path.clone()]);
                        return Action::Copy(files.unwrap_or_default());
                    }
                    _ => {}
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
        let list = matches!(self.page, Page::List);
        Line::from(span(hint_text(list, done, width), Style::new().dim()))
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
        let detail = match &self.page {
            Page::Detail(watch) => Some(paint.detail(watch.id, width)),
            Page::List => None,
        };
        let mut copied = None;
        let _ = terminal.draw(|frame| {
            match detail {
                None => self.draw_list(frame, screen),
                Some(detail) => self.draw_detail(frame, screen, detail),
            }
            let skip = self.scrollbar;
            if let Some(selection) = &mut self.selection {
                copied = selection.mark(frame.buffer_mut(), skip);
            }
        });
        copied
    }

    /// The list page: the banner, the steps, the last few notes, the summary,
    /// the hint.
    fn draw_list(&mut self, frame: &mut Frame, screen: Screen) {
        let banner = banner_lines(&screen.about, frame.area().height);
        let notes = screen.notes.len().min(MAX_NOTES);
        let [banner_area, list_area, notes_area, summary_area, hint_area] = Layout::vertical([
            Constraint::Length(banner.len() as u16),
            Constraint::Fill(1),
            Constraint::Length(notes as u16),
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

        let recent = screen.notes.len() - notes;
        frame.render_widget(
            Paragraph::new(Text::from(screen.notes[recent..].to_vec())),
            notes_area,
        );
        frame.render_widget(Paragraph::new(screen.summary), summary_area);
        let hint = self.hint(screen.done, hint_area.width as usize);
        frame.render_widget(Paragraph::new(hint), hint_area);
    }

    /// A step's page: its log, with its own line and the run's summary under it.
    fn draw_detail(&mut self, frame: &mut Frame, screen: Screen, detail: Option<Detail>) {
        let Page::Detail(watch) = &mut self.page else {
            return;
        };
        // The step's line in full, wrapped, however long a failure's message
        // made it — within reason, so the log keeps most of the screen.
        let width = frame.area().width as usize;
        let step_rows = detail
            .as_ref()
            .map(|detail| wrap_line(&detail.line, width, HANG))
            .unwrap_or_default();
        let step_rows: Vec<Line<'static>> = step_rows.into_iter().take(MAX_STEP_ROWS).collect();
        let [log_area, step_area, summary_area, hint_area] = Layout::vertical([
            Constraint::Fill(1),
            Constraint::Length(step_rows.len().max(1) as u16),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .areas(frame.area());

        match detail {
            Some(detail) => {
                watch.sync(&detail.files);
                let column = watch.draw(frame, log_area, &detail);
                frame.render_widget(Paragraph::new(Text::from(step_rows)), step_area);
                self.scrollbar = column;
            }
            None => {
                frame.render_widget(
                    Paragraph::new(span("  no such step", Style::new().dim())),
                    log_area,
                );
                self.scrollbar = None;
            }
        }
        frame.render_widget(Paragraph::new(screen.summary), summary_area);
        let hint = self.hint(screen.done, hint_area.width as usize);
        frame.render_widget(Paragraph::new(hint), hint_area);
    }
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

/// What the keys do, said at whatever length fits in `width` columns: in full
/// where there is room, and more tersely where there is not.
///
/// Both pages say what a drag does as well as what the keys do. The mouse is
/// being reported on either, so a drag no longer selects the way it would have
/// in a terminal left alone, and what it does instead has to be said or it is
/// a thing that simply stopped working. It outlasts most of the keys on the
/// way down: only the narrowest terminal, with room for nothing but the keys
/// themselves, goes without it.
fn hint_text(list: bool, done: bool, width: usize) -> String {
    let quit = if done { "q quit" } else { "q cancel the run" };
    let quit_short = if done { "q quit" } else { "q cancel" };
    let tiers: Vec<String> = if list {
        vec![
            format!(
                "  ↑/↓ or wheel move · enter open a step · drag copies · \
                 y copy a less command · {quit}"
            ),
            format!("  ↑/↓/wheel move · enter open · drag copies · y copy · {quit_short}"),
            format!("  ↑/↓/wheel · enter · drag copies · y · {quit_short}"),
            "  ↑/↓ · enter · drag copies · y · q".to_string(),
            "  ↑/↓ · enter · y · q".to_string(),
        ]
    } else {
        vec![
            format!(
                "  esc back · ↑/↓ or wheel scroll · G follow · tab next file · drag copies · \
                 y copy a less command · {quit}"
            ),
            format!(
                "  esc back · ↑/↓ or wheel scroll · G follow · tab file · drag copies · \
                 y copy · {quit_short}"
            ),
            format!("  esc back · ↑/↓/wheel · G follow · tab file · drag copies · {quit_short}"),
            format!("  esc · ↑/↓/wheel · G · tab · drag copies · {quit_short}"),
            "  esc · ↑/↓ · G · tab · drag copies · q".to_string(),
            "  esc · ↑/↓ · G · tab · y · q".to_string(),
        ]
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
struct Watch {
    id: usize,
    /// The file picked with `tab`, or `None` to read whichever the step most
    /// wants read — which changes as it starts tools.
    chosen: Option<PathBuf>,
    /// The file being read, once there is one.
    log: Option<LogView>,
    /// The size of the log's area last frame, for the keys that scroll by it.
    columns: u16,
    rows: u16,
}

impl Watch {
    fn new(id: usize) -> Self {
        Self {
            id,
            chosen: None,
            log: None,
            columns: 80,
            rows: 24,
        }
    }

    /// Read the file this page should be reading, given what the step has.
    fn sync(&mut self, files: &[PathBuf]) {
        let wanted = match &self.chosen {
            Some(chosen) if files.contains(chosen) => Some(chosen),
            _ => files.first(),
        };
        match wanted {
            Some(wanted) => {
                if self.log.as_ref().map(|log| &log.tail.path) != Some(wanted) {
                    self.log = Some(LogView::new(wanted.clone()));
                }
            }
            None => self.log = None,
        }
    }

    /// Switch to the next (or previous) of the step's files.
    fn next_file(&mut self, by: isize, paint: &dyn Paint) {
        let Some(files) = paint.detail(self.id, usize::MAX).map(|detail| detail.files) else {
            return;
        };
        if files.is_empty() {
            return;
        }
        let current = self
            .log
            .as_ref()
            .and_then(|log| files.iter().position(|file| *file == log.tail.path))
            .unwrap_or(0) as isize;
        let next = (current + by).rem_euclid(files.len() as isize) as usize;
        self.chosen = Some(files[next].clone());
        self.sync(&files);
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

    /// The log, framed: the step and the file above it, where in the file
    /// below.
    fn draw(&mut self, frame: &mut Frame, area: Rect, detail: &Detail) -> Option<u16> {
        let border = Block::new()
            .borders(Borders::TOP | Borders::BOTTOM)
            .border_style(Style::new().dim());
        let inner = border.inner(area);
        self.columns = inner.width;
        self.rows = inner.height;

        let mut title = vec![span(format!(" {} ", detail.label), Style::new().bold())];
        let mut footer = Vec::new();
        // Which lines the scrollbar stands for, and where it is in them.
        let mut scroll = None;

        let body = match &mut self.log {
            None => {
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
            Some(log) => {
                // The path gives way to the label, from the left: its end is
                // what tells one file from another.
                let room = (area.width as usize).saturating_sub(columns(&detail.label) + 4);
                let path = shorten_left(&log.tail.path.display().to_string(), room);
                title.push(span(format!(" {path} "), Style::new().dim()));
                if detail.files.len() > 1 {
                    let index = detail
                        .files
                        .iter()
                        .position(|file| *file == log.tail.path)
                        .map_or(0, |index| index + 1);
                    footer.push(span(
                        format!(" {index}/{} files (tab) ", detail.files.len()),
                        Style::new().dim(),
                    ));
                }

                log.poll();
                match &log.tail.state {
                    TailState::Waiting if detail.running => {
                        Text::from(span("  waiting for the file to appear", Style::new().dim()))
                    }
                    TailState::Waiting => Text::from(span("  no such file", Style::new().dim())),
                    TailState::Failed(error) => {
                        Text::from(span(format!("  cannot read: {error}"), Style::new().red()))
                    }
                    TailState::Reading if log.tail.count() == 0 => {
                        Text::from(span("  (empty)", Style::new().dim()))
                    }
                    TailState::Reading => {
                        let shown = log.render(inner.width, inner.height);
                        // Numbered within what is here to read, which is the
                        // whole file unless it was too long to keep.
                        let of = if log.tail.truncated() {
                            format!("the last {}", log.tail.kept())
                        } else {
                            log.tail.kept().to_string()
                        };
                        footer.push(span(
                            if shown.following {
                                format!(" following · {of} lines ")
                            } else {
                                format!(" line {} of {of} ", shown.bottom + 1 - log.tail.first())
                            },
                            Style::new().dim(),
                        ));
                        let top = shown.top.line.saturating_sub(log.tail.first()) as usize;
                        scroll = Some((log.tail.lines.len(), top));
                        Text::from(shown.rows)
                    }
                }
            }
        };

        frame.render_widget(
            border
                .title(Line::from(title))
                .title_bottom(Line::from(footer).right_aligned()),
            area,
        );
        frame.render_widget(Paragraph::new(body), inner);
        scroll.and_then(|(count, top)| scrollbar(frame, inner, count, top))
    }
}

// ---------------------------------------------------------------------------
// Reading a log
// ---------------------------------------------------------------------------

/// A file being read as it is written, and where in it the reader is.
struct LogView {
    tail: Tail,
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
            tail: Tail::new(path),
            top: None,
            pinned: false,
            resume: false,
        }
    }

    fn poll(&mut self) {
        self.tail.poll();
    }

    fn follow(&mut self) {
        self.top = None;
        self.pinned = false;
    }

    fn scroll_to_top(&mut self) {
        self.top = Some(Top {
            line: self.tail.first(),
            row: 0,
        });
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
    fn follow_top(&self, width: usize, height: usize) -> Top {
        let first = self.tail.first();
        let count = self.tail.count();
        if count == 0 {
            return Top {
                line: first,
                row: 0,
            };
        }
        let mut line = count - 1;
        let mut need = height.max(1);
        loop {
            let rows = row_count(self.tail.line(line), width);
            if rows >= need {
                return Top {
                    line,
                    row: rows - need,
                };
            }
            need -= rows;
            if line == first {
                return Top { line, row: 0 };
            }
            line -= 1;
        }
    }

    /// Where the top of the screen is now, given where it was asked to be.
    ///
    /// A place at or past where following would put it means following; a
    /// place in lines that have since been forgotten means the oldest kept.
    fn top(&mut self, width: usize, height: usize) -> (Top, bool) {
        let follow = self.follow_top(width, height);
        match self.top {
            Some(top) if top >= follow && !self.pinned => {
                self.top = None;
                (follow, true)
            }
            Some(top) if top.line < self.tail.first() => {
                let top = Top {
                    line: self.tail.first(),
                    row: 0,
                };
                self.top = Some(top);
                (top, false)
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
        while rows.len() < height && line < self.tail.count() {
            for row in wrap(self.tail.line(line), width).into_iter().skip(skip) {
                if rows.len() == height {
                    break;
                }
                rows.push(Line::from(row));
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
        let first = self.tail.first();
        let count = self.tail.count();

        if by < 0 {
            for _ in 0..by.unsigned_abs() {
                if top.row > 0 {
                    top.row -= 1;
                } else if top.line > first {
                    top.line -= 1;
                    top.row = row_count(self.tail.line(top.line), width) - 1;
                } else {
                    break;
                }
            }
        } else {
            for _ in 0..by {
                if top.row + 1 < row_count(self.tail.line(top.line), width) {
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

/// A file being read as it grows, the way `tail -F` reads one.
///
/// Keeps the last [`MAX_LINES`] lines, starts again if the file is replaced or
/// truncated, and waits for a file that is not there yet.
struct Tail {
    path: PathBuf,
    file: Option<File>,
    /// Which file this is, so that a replacement under the same name is
    /// noticed.
    identity: Option<(u64, u64)>,
    /// Bytes read so far.
    offset: u64,
    /// The bytes after the last newline, waiting for the rest of their line.
    partial: Vec<u8>,
    /// The same, as text, to show while it waits.
    partial_text: String,
    /// Bytes are being skipped up to the next newline, after a jump forward.
    resync: bool,
    /// Bytes never read, because the file was too far ahead.
    skipped: u64,
    lines: VecDeque<String>,
    /// Lines forgotten off the front, which is the number of the first kept.
    dropped: u64,
    state: TailState,
}

enum TailState {
    /// There is no such file, yet.
    Waiting,
    Reading,
    Failed(String),
}

impl Tail {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            file: None,
            identity: None,
            offset: 0,
            partial: Vec::new(),
            partial_text: String::new(),
            resync: false,
            skipped: 0,
            lines: VecDeque::new(),
            dropped: 0,
            state: TailState::Waiting,
        }
    }

    /// Read whatever has been written since last time.
    fn poll(&mut self) {
        let meta = match fs::metadata(&self.path) {
            Ok(meta) => meta,
            Err(error) => {
                self.file = None;
                self.state = if error.kind() == io::ErrorKind::NotFound {
                    TailState::Waiting
                } else {
                    TailState::Failed(error.to_string())
                };
                return;
            }
        };
        if !meta.is_file() {
            self.state = TailState::Failed("not a file".into());
            return;
        }

        // New, replaced, or truncated: start again from the top.
        let identity = identity(&meta);
        if self.file.is_none() || self.identity != Some(identity) || meta.len() < self.offset {
            match File::open(&self.path) {
                Ok(file) => self.file = Some(file),
                Err(error) => {
                    self.file = None;
                    self.state = TailState::Failed(error.to_string());
                    return;
                }
            }
            self.identity = Some(identity);
            self.offset = 0;
            self.partial.clear();
            self.partial_text.clear();
            self.resync = false;
            self.skipped = 0;
            self.lines.clear();
            self.dropped = 0;
        }

        let len = meta.len();
        if len - self.offset > BACKLOG {
            // Too far behind to catch up on all of it: pick up from near the
            // end, at a whole line.
            self.skipped += len - BACKLOG - self.offset;
            self.offset = len - BACKLOG;
            self.partial.clear();
            self.partial_text.clear();
            self.resync = true;
            if let Some(file) = &mut self.file {
                if let Err(error) = file.seek(SeekFrom::Start(self.offset)) {
                    self.state = TailState::Failed(error.to_string());
                    return;
                }
            }
        }

        let mut buffer = vec![0u8; CHUNK];
        while self.offset < len {
            let Some(file) = &mut self.file else { break };
            match file.read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => {
                    self.offset += n as u64;
                    self.consume(&buffer[..n]);
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => {
                    self.state = TailState::Failed(error.to_string());
                    return;
                }
            }
        }
        self.state = TailState::Reading;
    }

    fn consume(&mut self, mut bytes: &[u8]) {
        while let Some(newline) = bytes.iter().position(|&b| b == b'\n') {
            if self.resync {
                self.resync = false;
            } else {
                self.partial.extend_from_slice(&bytes[..newline]);
                let text = clean(&String::from_utf8_lossy(&self.partial));
                self.partial.clear();
                self.push(text);
            }
            bytes = &bytes[newline + 1..];
        }
        if !self.resync {
            self.partial.extend_from_slice(bytes);
        }
        self.partial_text = if self.partial.is_empty() {
            String::new()
        } else {
            clean(&String::from_utf8_lossy(&self.partial))
        };
    }

    fn push(&mut self, line: String) {
        self.lines.push_back(line);
        while self.lines.len() > MAX_LINES {
            self.lines.pop_front();
            self.dropped += 1;
        }
    }

    /// The number of the oldest line still kept.
    fn first(&self) -> u64 {
        self.dropped
    }

    /// Whether the start of the file is no longer here to scroll back to.
    fn truncated(&self) -> bool {
        self.dropped > 0 || self.skipped > 0
    }

    /// How many lines are here to read.
    fn kept(&self) -> u64 {
        self.count() - self.first()
    }

    /// How many lines there have been, the one still being written included.
    fn count(&self) -> u64 {
        self.dropped + self.lines.len() as u64 + u64::from(!self.partial_text.is_empty())
    }

    /// Line `number`, which must be between [`Tail::first`] and [`Tail::count`].
    fn line(&self, number: u64) -> &str {
        let index = number.saturating_sub(self.dropped) as usize;
        self.lines
            .get(index)
            .map(String::as_str)
            .unwrap_or(&self.partial_text)
    }
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

    fn lines(tail: &Tail) -> Vec<&str> {
        (tail.first()..tail.count()).map(|n| tail.line(n)).collect()
    }

    #[test]
    fn a_tail_waits_for_its_file_then_reads_what_is_appended() {
        let dir = scratch("grows");
        let path = dir.join("tool.out");
        let mut tail = Tail::new(path.clone());

        tail.poll();
        assert!(matches!(tail.state, TailState::Waiting));
        assert_eq!(tail.count(), 0);

        fs::write(&path, "one\ntwo\npart").unwrap();
        tail.poll();
        assert!(matches!(tail.state, TailState::Reading));
        // The unfinished line is shown while it waits for the rest of itself.
        assert_eq!(lines(&tail), ["one", "two", "part"]);
        assert_eq!(tail.count(), 3);

        let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(b"ial\r\nthree\n").unwrap();
        drop(file);
        tail.poll();
        assert_eq!(lines(&tail), ["one", "two", "partial", "three"]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_tail_starts_over_when_its_file_is_truncated_or_replaced() {
        let dir = scratch("replaced");
        let path = dir.join("tool.out");
        let mut tail = Tail::new(path.clone());

        fs::write(&path, "old one\nold two\n").unwrap();
        tail.poll();
        assert_eq!(lines(&tail), ["old one", "old two"]);

        // Truncated and rewritten shorter.
        fs::write(&path, "new\n").unwrap();
        tail.poll();
        assert_eq!(lines(&tail), ["new"]);

        // Replaced by a different file under the same name, the way a tool
        // that rotates its log does it.
        let other = dir.join("tool.out.new");
        fs::write(&other, "replacement one\nreplacement two\n").unwrap();
        fs::rename(&other, &path).unwrap();
        tail.poll();
        assert_eq!(lines(&tail), ["replacement one", "replacement two"]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_tail_that_is_far_behind_skips_to_near_the_end_at_a_whole_line() {
        let dir = scratch("behind");
        let path = dir.join("tool.out");
        let mut contents = String::new();
        let mut number = 0;
        while (contents.len() as u64) < BACKLOG + 100_000 {
            contents.push_str(&format!("line {number}\n"));
            number += 1;
        }
        fs::write(&path, &contents).unwrap();

        let mut tail = Tail::new(path.clone());
        tail.poll();
        let read = lines(&tail);
        // Picked up at a whole line, not the tail of one, and read to the end.
        assert!(read[0].starts_with("line "), "{:?}", read[0]);
        assert!(
            read[0]["line ".len()..].parse::<u64>().is_ok(),
            "{:?}",
            read[0]
        );
        assert_eq!(*read.last().unwrap(), format!("line {}", number - 1));
        assert_eq!(tail.kept(), read.len() as u64);
        assert!(tail.truncated());

        // Whereas a file that fits is all there.
        let small = dir.join("small.out");
        fs::write(&small, "a\nb\n").unwrap();
        let mut tail = Tail::new(small);
        tail.poll();
        assert!(!tail.truncated());
        assert_eq!(tail.kept(), 2);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_tail_keeps_only_the_last_lines_and_numbers_them_from_the_start() {
        let mut tail = Tail::new(PathBuf::from("/nonexistent"));
        for n in 0..(MAX_LINES + 5) {
            tail.push(format!("line {n}"));
        }
        assert_eq!(tail.lines.len(), MAX_LINES);
        assert_eq!(tail.first(), 5);
        assert_eq!(tail.count(), MAX_LINES as u64 + 5);
        assert_eq!(tail.line(5), "line 5");
        assert_eq!(
            tail.line(tail.count() - 1),
            format!("line {}", MAX_LINES + 4)
        );
    }

    #[test]
    fn tool_output_is_cleaned_before_it_is_shown() {
        let mut tail = Tail::new(PathBuf::from("/nonexistent"));
        tail.consume(b"\x1b[1;31m**ERROR\x1b[0m: bad\tthing\r\n\xff\xfe raw\n");
        assert_eq!(
            lines(&tail),
            ["**ERROR: bad    thing", "\u{fffd}\u{fffd} raw"]
        );
    }

    // -- scrolling ----------------------------------------------------------

    /// A view over `count` one-row lines, numbered from zero.
    fn view(count: usize) -> LogView {
        let mut view = LogView::new(PathBuf::from("/nonexistent"));
        for n in 0..count {
            view.tail.push(format!("{n}"));
        }
        view.tail.state = TailState::Reading;
        view
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
        view.tail.push("100".into());
        assert_eq!(shown(&mut view, 3), ["96", "97", "98"]);

        // Down to the end again, and it follows again.
        view.scroll(2, 80, 3);
        assert_eq!(shown(&mut view, 3), ["98", "99", "100"]);
        assert!(view.render(80, 3).following);
        view.tail.push("101".into());
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
        let mut view = LogView::new(PathBuf::from("/nonexistent"));
        view.tail.push("aaaa".into());
        view.tail.push("bbbbbbbb".into()); // two rows at width 4
        view.tail.push("cc".into());
        view.tail.state = TailState::Reading;

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
    }

    #[test]
    fn a_place_in_forgotten_lines_becomes_the_oldest_kept() {
        let mut view = view(10);
        view.scroll_to_top();
        assert_eq!(shown(&mut view, 2), ["0", "1"]);
        for n in 10..(MAX_LINES + 20) {
            view.tail.push(format!("{n}"));
        }
        // Lines 0 to 19 have gone; the view is on line 20, not following.
        assert_eq!(view.tail.first(), 20);
        assert_eq!(shown(&mut view, 2), ["20", "21"]);
        assert!(!view.render(80, 2).following);
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
        let mut watch = Watch::new(7);
        watch.log = Some(view(count));
        watch.columns = 80;
        watch.rows = 3;
        View {
            page: Page::Detail(Box::new(watch)),
            ..View::default()
        }
    }

    /// What the log on an open step's page is showing.
    fn log_rows(page: &mut View) -> Vec<String> {
        let Page::Detail(watch) = &mut page.page else {
            panic!("not a step's page");
        };
        shown(watch.log.as_mut().unwrap(), 3)
    }

    fn is_following(page: &mut View) -> bool {
        let Page::Detail(watch) = &mut page.page else {
            panic!("not a step's page");
        };
        watch.log.as_mut().unwrap().render(80, 3).following
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
        assert!(matches!(view.page, Page::Detail(_)));
        view.key(press(KeyCode::Esc), &paint);
        assert!(matches!(view.page, Page::List));
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
        let Page::Detail(watch) = &mut page.page else {
            panic!("not a step's page");
        };
        // ...so the lines that arrive during the drag do not carry it away.
        for n in 100..110 {
            watch.log.as_mut().unwrap().tail.push(n.to_string());
        }
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
        let long = hint_text(true, false, 200);
        assert!(long.contains("enter open a step"), "{long}");
        assert_eq!(hint_text(true, false, columns(&long)), long);

        let medium = hint_text(true, false, columns(&long) - 1);
        assert!(columns(&medium) < columns(&long));
        assert!(medium.contains("enter open"), "{medium}");
        assert!(medium.contains("q cancel"), "{medium}");

        let short = hint_text(true, false, 30);
        assert!(columns(&short) <= 30, "{short}");
        assert!(short.ends_with("· q"), "{short}");
        // Narrower than even the shortest: the shortest, for the terminal to
        // cut.
        assert_eq!(hint_text(true, false, 3), short);

        // Once the run is done, `q` quits, at every length.
        for width in [200, 60] {
            assert!(hint_text(true, true, width).ends_with("q quit"));
            assert!(!hint_text(true, true, width).contains("cancel"));
        }

        // The question `q` asks, at every length, says what `y` does.
        for width in [200, 80, 20, 3] {
            let question = confirm_text(width);
            assert!(question.contains("cancel the run"), "{question}");
            assert!(question.contains('y'), "{question}");
            assert!(columns(&question) <= width || width < 25, "{question}");
        }
        assert!(hint_text(false, true, 200).starts_with("  esc back"));
        assert!(columns(&hint_text(false, true, 40)) <= 40);

        // The longer tiers are written across two source lines, and a string
        // continuation that did not eat its indentation would show up as a gap
        // in the middle of the line.
        for list in [true, false] {
            for width in [200, 120, 95, 80, 50, 20] {
                let hint = hint_text(list, false, width);
                assert!(!hint.trim_start().contains("  "), "{width}: {hint}");
            }
        }

        // The mouse is reported on both pages, so both own up to `shift`
        // wherever there is room for more than the keys themselves.
        for list in [true, false] {
            for done in [true, false] {
                for width in [200, 120, 100, 95, 80, 63, 50, 40] {
                    let hint = hint_text(list, done, width);
                    assert!(columns(&hint) <= width, "{list} {width}: {hint}");
                    assert!(hint.contains("drag copies"), "{list} {width}: {hint}");
                    // Nothing offers to cancel a run that is already over.
                    assert!(!done || !hint.contains("cancel"), "{width}: {hint}");
                }
            }
            assert!(hint_text(list, false, 200).contains("wheel"));

            // Below that, the keys are all that fits — the list holds on a
            // little longer, having fewer of them to list.
            let floor = if list { 35 } else { 39 };
            assert!(hint_text(list, false, floor).contains("drag copies"));
            assert!(!hint_text(list, false, floor - 1).contains("drag"));
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
