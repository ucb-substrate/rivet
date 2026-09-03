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
//! so the terminal still shows what the run did. `q` before the run is over
//! does the same and leaves the run going, reporting plainly from then on.
//!
//! There are two pages. The list is every step, in the order they started,
//! with a cursor on one of them; `enter` opens that step, which is its log as
//! it is written, with the step's own line and the run's summary underneath;
//! `esc` closes it again. The step's files — the output of the tool it is
//! running, and its own log — are a `tab` apart.
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
//! running are in that group. Interrupting a run must keep killing them. So
//! `ISIG` is put back immediately afterwards, and `^C` and `^Z` go on meaning
//! what they always did.
//!
//! What is then left to do is tidy up after them. [`signal_hook`] notices the
//! interrupt, the display gives the terminal back, and the run exits; a second
//! `^C` gives up on being tidy and exits at once. `^Z` is noticed the same way,
//! so that the screen can be handed back before the process stops — otherwise
//! the shell's prompt would land on the alternate screen — and taken again when
//! it is continued.

use std::collections::VecDeque;
use std::fs::{self, File, Metadata};
use std::io::{self, Read, Seek, SeekFrom, Stderr, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
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

/// How much of a step's log a copied command shows before it starts following.
const TAIL_LINES: usize = 100;

/// Most notes kept on screen under the list.
const MAX_NOTES: usize = 3;

/// Columns the continuation rows of a wrapped step line are indented by: the
/// width of the cursor and the glyph, so the text lines up under itself.
const HANG: usize = 4;

/// Most rows a step's line may take on its own page.
const MAX_STEP_ROWS: usize = 6;

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

/// The list page: every step so far, and how the run is going.
#[derive(Default)]
pub(crate) struct Screen {
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
    /// What a command to follow the step should follow: the files the tool it is
    /// running is writing now, or its own log until it runs one.
    pub follow: Vec<PathBuf>,
    /// Whether the step is still going, which is what decides whether a file
    /// that is not there yet is worth waiting for.
    pub running: bool,
}

/// What the display draws, and what it does when a key is typed.
///
/// Implemented by [`crate::progress::Reporter`], and held weakly: the drawing
/// must not be what keeps a finished run alive.
pub(crate) trait Paint: Send + Sync {
    /// What the list page should show now.
    fn screen(&self) -> Screen;

    /// One step, by the id its line in the list carried, for its own page.
    fn detail(&self, id: usize) -> Option<Detail>;

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
        if execute!(io::stderr(), EnterAlternateScreen).is_err() {
            let _ = disable_raw_mode();
            return None;
        }

        let terminal = match Terminal::new(CrosstermBackend::new(io::stderr())) {
            Ok(terminal) => terminal,
            Err(_) => {
                let _ = execute!(io::stderr(), LeaveAlternateScreen);
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
        let _ = execute!(io::stderr(), LeaveAlternateScreen);
        let _ = disable_raw_mode();

        let result = f();

        Stage::retake(&mut terminal);
        result
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

    /// Hand the terminal back as it was found, leaving `record` in its
    /// scrollback. Done once; asking again does nothing.
    fn close(&self, record: Vec<Line<'static>>) {
        let mut terminal = self.hold();
        if self.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        let _ = terminal.show_cursor();
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
    fn retake(terminal: &mut Term) {
        if enable_raw_mode().is_ok() {
            signals::keep_keys();
        }
        let _ = execute!(io::stderr(), EnterAlternateScreen);
        let _ = terminal.clear();
    }

    /// Hand the terminal back and stop, for a `^Z`, then take it again once
    /// continued.
    fn pause(&self, terminal: &mut Term) {
        let _ = terminal.show_cursor();
        let _ = execute!(io::stderr(), LeaveAlternateScreen);
        let _ = disable_raw_mode();
        signals::stop_now();
        // Back: the shell had the terminal in the meantime, so nothing on
        // screen is ours and nothing is still set up.
        Self::retake(terminal);
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
            Stage::retake(&mut stage.hold());
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
                    }
                    // Drawn at once, so the key is seen to land.
                    next_frame = Instant::now();
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
            view.draw(&mut terminal, &*paint);
            next_frame = Instant::now() + FRAME;
        }
    }
}

// ---------------------------------------------------------------------------
// The pages
// ---------------------------------------------------------------------------

/// What a key asks the loop to do that the view cannot do itself.
enum Action {
    None,
    /// Draw everything again from nothing.
    Redraw,
    /// Copy a command to follow these files.
    Copy(Vec<PathBuf>),
    /// Give the screen back.
    Quit,
}

/// Everything about the display that is not the run: which page is open, how
/// far the list has scrolled, what the hint line is saying.
#[derive(Default)]
struct View {
    page: Page,
    list: ListState,
    /// Rows the list had last frame, which is how far a page key moves.
    list_rows: usize,
    /// The step under the cursor as of the last frame, by id.
    selected: Option<usize>,
    /// Something to say on the hint line, until the moment it expires.
    flash: Option<(Line<'static>, Instant)>,
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
            // The next frame measures the terminal again and draws everything.
            _ => Action::None,
        }
    }

    fn key(&mut self, key: KeyEvent, paint: &dyn Paint) -> Action {
        use KeyCode::*;
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

        match key.code {
            Char('l') if ctrl => return Action::Redraw,
            Char('q') => return Action::Quit,
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
                            .and_then(|id| paint.detail(id))
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

    /// Copy a command for reading `files` as they are written.
    ///
    /// What someone watching a step wants beyond this screen is the same log
    /// in a terminal of their own, and this hands over the command for it.
    fn copy(&mut self, stage: &Stage, files: &[PathBuf]) {
        if files.is_empty() {
            self.flash(
                Line::from(span("  nothing to follow yet", Style::new().yellow())),
                FLASH_FOR,
            );
            return;
        }
        let command = follow_command(files);
        tracing::info!(%command, "copied a command to follow the log");

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

    fn flash(&mut self, line: Line<'static>, for_: Duration) {
        self.flash = Some((line, Instant::now() + for_));
    }

    /// The bottom line: what was just done, or what the keys do.
    fn hint(&mut self, done: bool) -> Line<'static> {
        if let Some((line, until)) = &self.flash {
            if Instant::now() < *until {
                return line.clone();
            }
            self.flash = None;
        }
        let quit = if done {
            "q quit"
        } else {
            "q leave the display (the run goes on)"
        };
        let keys = match self.page {
            Page::List => {
                format!("  ↑/↓ or j/k move · enter open a step · y copy a tail command · {quit}")
            }
            Page::Detail(_) => {
                format!("  esc back · ↑/↓ scroll · G follow · tab next file · y copy a tail command · {quit}")
            }
        };
        Line::from(span(keys, Style::new().dim()))
    }

    fn draw(&mut self, terminal: &mut Term, paint: &dyn Paint) {
        let screen = paint.screen();
        let detail = match &self.page {
            Page::Detail(watch) => Some(paint.detail(watch.id)),
            Page::List => None,
        };
        let _ = terminal.draw(|frame| match detail {
            None => self.draw_list(frame, screen),
            Some(detail) => self.draw_detail(frame, screen, detail),
        });
    }

    /// The list page: the steps, the last few notes, the summary, the hint.
    fn draw_list(&mut self, frame: &mut Frame, screen: Screen) {
        let notes = screen.notes.len().min(MAX_NOTES);
        let [list_area, notes_area, summary_area, hint_area] = Layout::vertical([
            Constraint::Fill(1),
            Constraint::Length(notes as u16),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .areas(frame.area());

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
        scrollbar(frame, list_area, count, self.list.offset());

        let recent = screen.notes.len() - notes;
        frame.render_widget(
            Paragraph::new(Text::from(screen.notes[recent..].to_vec())),
            notes_area,
        );
        frame.render_widget(Paragraph::new(screen.summary), summary_area);
        let hint = self.hint(screen.done);
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
                watch.draw(frame, log_area, &detail);
                frame.render_widget(Paragraph::new(Text::from(step_rows)), step_area);
            }
            None => frame.render_widget(
                Paragraph::new(span("  no such step", Style::new().dim())),
                log_area,
            ),
        }
        frame.render_widget(Paragraph::new(screen.summary), summary_area);
        let hint = self.hint(screen.done);
        frame.render_widget(Paragraph::new(hint), hint_area);
    }
}

/// A scrollbar down the right of `area`, if `count` items do not fit in it.
fn scrollbar(frame: &mut Frame, area: Rect, count: usize, offset: usize) {
    if count <= area.height as usize {
        return;
    }
    let mut state = ScrollbarState::new(count).position(offset);
    frame.render_stateful_widget(
        Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(None)
            .end_symbol(None),
        area,
        &mut state,
    );
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
        let Some(files) = paint.detail(self.id).map(|detail| detail.files) else {
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

    fn follow(&mut self) {
        if let Some(log) = &mut self.log {
            log.follow();
        }
    }

    /// The log, framed: the step and the file above it, where in the file
    /// below.
    fn draw(&mut self, frame: &mut Frame, area: Rect, detail: &Detail) {
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
                    "  this step has no files: it has no directory of its own (Step::log_dir)",
                    Style::new().dim(),
                ))
            }
            Some(log) => {
                title.push(span(
                    format!(" {} ", log.tail.path.display()),
                    Style::new().dim(),
                ));
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
        if let Some((count, top)) = scroll {
            scrollbar(frame, inner, count, top);
        }
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
        }
    }

    fn poll(&mut self) {
        self.tail.poll();
    }

    fn follow(&mut self) {
        self.top = None;
    }

    fn scroll_to_top(&mut self) {
        self.top = Some(Top {
            line: self.tail.first(),
            row: 0,
        });
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
            Some(top) if top >= follow => {
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
// Following a log elsewhere
// ---------------------------------------------------------------------------

/// `tail` over the files a step is writing.
///
/// `-F` rather than `-f`, so the command works pasted into a terminal before
/// the file exists — a step that has not reached that tool yet — and keeps
/// working when a tool replaces its log rather than appending to it.
fn follow_command(files: &[PathBuf]) -> String {
    let files: Vec<String> = files.iter().map(|file| quote(&full_path(file))).collect();
    format!("tail -n {TAIL_LINES} -F {}", files.join(" "))
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

    /// Put back the signal keys that raw mode took away.
    ///
    /// See the module docs: `^C` has to reach the tools a step is running, and
    /// only the terminal can send it to them.
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
            span("  did not match; see build/decoder.lvs.out", Style::new().red()),
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

    // -- what y copies ------------------------------------------------------

    #[test]
    fn the_follow_command_tails_every_file_it_is_given() {
        assert_eq!(
            follow_command(&[PathBuf::from("/build/decoder par.rivet.log")]),
            r"tail -n 100 -F '/build/decoder par.rivet.log'"
        );
        assert_eq!(
            follow_command(&[
                PathBuf::from("/build/decoder.par.out"),
                PathBuf::from("/build/decoder.par.err"),
            ]),
            "tail -n 100 -F /build/decoder.par.out /build/decoder.par.err"
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
