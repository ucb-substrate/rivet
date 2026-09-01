//! The terminal the live display draws on, and the keys typed at it.
//!
//! [`crate::progress`] decides what a run looks like; this owns the terminal it
//! appears on. The two meet at [`Paint`], which is all this module knows about
//! a flow: some lines to keep on screen, some lines to leave behind, and three
//! keys.
//!
//! # An inline viewport, not a screen
//!
//! Rivet is a build tool, not an editor: a run's output belongs in the
//! scrollback afterwards, next to the command that started it. So the display
//! is ratatui's [`Viewport::Inline`] — a fixed block of rows at the bottom of
//! the ordinary terminal — and finished steps are pushed above it with
//! [`Terminal::insert_before`], where they stay for good. Nothing takes over
//! the screen, and nothing is erased when the run ends.
//!
//! The block is a fixed height because an inline viewport's height is fixed
//! when it is created. It is sized for the most steps that can run at once, so
//! a run that is not using all of its workers leaves the top rows blank rather
//! than making everything below it jump around.
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
//! What is then left to do is tidy up after them: [`signal_hook`] catches the
//! interrupt, the display gives the terminal back, and the run exits. A second
//! `^C` gives up on being tidy and exits at once.

use std::io::{self, Stderr, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use ratatui::layout::{Constraint, Layout};
use ratatui::text::{Line, Text};
use ratatui::widgets::{List, ListItem, ListState, Paragraph, Widget, Wrap};
use ratatui::{Terminal, TerminalOptions, Viewport};

/// How often the live area is redrawn. The spinners and the elapsed times move
/// on their own, so this is a frame rate rather than a reaction to events.
const FRAME: Duration = Duration::from_millis(100);

/// What a run exits with when it is interrupted, the shell's convention.
const INTERRUPTED: i32 = 130;

/// A key the display does something with. Everything else is dropped.
///
/// `^C` and `^Z` are not among them on purpose: they are signals here, not
/// keys, and the kernel delivers them to the whole process group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Key {
    Up,
    Down,
    Enter,
}

/// One frame of the live area.
///
/// The whole of what this module knows about a flow: a list with a cursor on
/// it, and some lines underneath.
#[derive(Default)]
pub(crate) struct Screen {
    /// One line per running step, in the order they are drawn.
    pub steps: Vec<Line<'static>>,
    /// Which of them the cursor is on. The list scrolls to keep it in view.
    pub selected: Option<usize>,
    /// Drawn under the steps and always visible: the summary, and the hint.
    pub footer: Vec<Line<'static>>,
}

/// What the display draws, and what it does when a key is typed.
///
/// Implemented by [`crate::progress::Reporter`], and held weakly: the drawing
/// must not be what keeps a finished run alive.
pub(crate) trait Paint: Send + Sync {
    /// What the live area should show now.
    fn screen(&self) -> Screen;

    /// Lines to leave in the terminal's scrollback for good, taken as they are
    /// read.
    fn scrollback(&self) -> Vec<Line<'static>>;

    /// A key was typed at the display.
    fn key(&self, key: Key);
}

/// The terminal, for as long as a run is drawing on it.
pub(crate) struct Tui {
    stage: Arc<Stage>,
    /// How tall the live area is, for when it has to be taken again.
    rows: u16,
    stop: Arc<AtomicBool>,
    /// Set when a signal that ends the run has arrived. Kept here as well as in
    /// the drawing thread because it may arrive while that thread is being
    /// stopped, and an interrupt must not be lost with it.
    interrupted: Arc<AtomicBool>,
    threads: Vec<JoinHandle<()>>,
    signals: Vec<signals::Id>,
}

impl Tui {
    /// Take the terminal and start drawing `painter` on it.
    ///
    /// `rows` is the height of the live area. Returns `None` if the terminal
    /// will not give up raw mode, in which case the run falls back to plain
    /// line-by-line logging.
    pub(crate) fn start(painter: Weak<dyn Paint>, rows: u16) -> Option<Tui> {
        // A terminal that says it has no rows is one nothing can be drawn on,
        // and ratatui's `insert_before` does not come back from being asked to
        // try: it draws a screenful at a time, and a screenful of no rows makes
        // no progress. Terminals do report this — a pty whose size was never
        // set is the usual way — so it is asked before anything else happens.
        if !has_room() {
            return None;
        }
        enable_raw_mode().ok()?;
        signals::keep_keys();

        let terminal = Terminal::with_options(
            CrosstermBackend::new(io::stderr()),
            TerminalOptions {
                viewport: Viewport::Inline(rows),
            },
        );
        let terminal = match terminal {
            Ok(terminal) => terminal,
            Err(_) => {
                let _ = disable_raw_mode();
                return None;
            }
        };

        let stage = Arc::new(Stage {
            terminal: Mutex::new(terminal),
        });
        let stop = Arc::new(AtomicBool::new(false));
        let interrupted = Arc::new(AtomicBool::new(false));
        let resumed = Arc::new(AtomicBool::new(false));
        let signals = signals::catch(&interrupted, &resumed);

        let mut threads = Vec::new();
        threads.push(spawn("rivet-draw", {
            let (stage, painter, stop) = (stage.clone(), painter.clone(), stop.clone());
            let (interrupted, resumed) = (interrupted.clone(), resumed.clone());
            move || draw_loop(&stage, &painter, &stop, &interrupted, &resumed)
        }));
        threads.push(spawn("rivet-keys", {
            let (stage, stop) = (stage.clone(), stop.clone());
            move || key_loop(&stage, &painter, &stop)
        }));

        Some(Tui {
            stage,
            rows,
            stop,
            interrupted,
            threads: threads.into_iter().flatten().collect(),
            signals,
        })
    }

    /// Stop drawing and give the terminal back, leaving the run's output in the
    /// scrollback and the cursor where the live area was.
    ///
    /// `last` is whatever finished after the final frame, which still belongs
    /// above the display rather than nowhere.
    pub(crate) fn stop(self, last: Vec<Line<'static>>) {
        self.stop.store(true, Ordering::Relaxed);
        for thread in self.threads {
            let _ = thread.join();
        }
        signals::forget(self.signals);
        give_back(&self.stage, last);

        // A run that is interrupted at the moment it ends — killing a tool ends
        // the run that was driving it, so this is the usual way of it rather
        // than a race worth ignoring — must still end as an interrupted one.
        // After this the signals are the shell's again, and it can have them.
        if self.interrupted.load(Ordering::SeqCst) {
            std::process::exit(INTERRUPTED);
        }
    }

    /// Run `f` with the live area taken down and the terminal in its ordinary
    /// mode, then put it back.
    pub(crate) fn suspend<R>(&self, f: impl FnOnce() -> R) -> R {
        let mut stage = self.stage.hold();
        let _ = stage.terminal.clear();
        let _ = stage.terminal.show_cursor();
        let _ = stage.flush();
        let _ = disable_raw_mode();

        let result = f();

        if enable_raw_mode().is_ok() {
            signals::keep_keys();
        }
        // Whatever ran has printed, moved the cursor and scrolled the screen,
        // so where the live area used to be means nothing now. It is taken
        // again from wherever the cursor has ended up, which puts it under what
        // was printed rather than over it.
        stage.retake(self.rows);
        result
    }

    /// Hold the display still while `f` writes to the terminal itself.
    ///
    /// For something that writes no visible output — an escape sequence asking
    /// the terminal to do something — and so needs no redraw afterwards.
    pub(crate) fn hold<R>(&self, f: impl FnOnce() -> R) -> R {
        let _stage = self.stage.hold();
        f()
    }
}

/// The terminal, and whose turn it is with it.
///
/// Holding it is what it means to have the terminal: the drawing thread takes
/// it for the moment a frame takes, the key thread for the moment a key takes,
/// and [`Tui::suspend`] for as long as it needs.
struct Stage {
    terminal: Mutex<Terminal<CrosstermBackend<Stderr>>>,
}

impl Stage {
    fn hold(&self) -> Held<'_> {
        // A panic while the terminal was held must not leave the run unable to
        // give it back.
        Held {
            terminal: self
                .terminal
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
        }
    }
}

struct Held<'a> {
    terminal: MutexGuard<'a, Terminal<CrosstermBackend<Stderr>>>,
}

impl Held<'_> {
    /// Leave `lines` in the scrollback, above the live area.
    ///
    /// Long lines wrap rather than being cut: a failure's message is the one
    /// thing on screen that most wants to be read in full.
    fn print(&mut self, lines: Vec<Line<'static>>) {
        // Asked again every time: a terminal can be resized to nothing while a
        // run is going, and drawing into no rows at all does not return.
        if lines.is_empty() || !has_room() {
            return;
        }
        let width = self
            .terminal
            .size()
            .map(|size| size.width)
            .unwrap_or(80)
            .max(1);
        let height: u16 = lines.iter().map(|line| rows_for(line, width)).sum();
        let text = Text::from(lines);
        let _ = self.terminal.insert_before(height, |buf| {
            Paragraph::new(text)
                .wrap(Wrap { trim: false })
                .render(buf.area, buf);
        });
    }

    /// Draw the live area: the running steps, and the summary under them.
    ///
    /// Bottom-aligned, so the summary stays where it is while the number of
    /// steps above it changes. The steps are a `List` rather than more lines,
    /// which is what keeps the cursor on screen when more steps are running
    /// than there are rows for them, and what scrolls by as little as it can.
    fn draw(&mut self, screen: Screen, list: &mut ListState) {
        let _ = self.terminal.draw(|frame| {
            let height = frame.area().height;
            let footer = (screen.footer.len() as u16).min(height);
            let steps = (screen.steps.len() as u16).min(height - footer);
            let [_blank, steps_area, footer_area] = Layout::vertical([
                Constraint::Fill(1),
                Constraint::Length(steps),
                Constraint::Length(footer),
            ])
            .areas(frame.area());

            list.select(screen.selected);
            let items = screen.steps.into_iter().map(ListItem::new);
            frame.render_stateful_widget(List::new(items), steps_area, list);
            frame.render_widget(Paragraph::new(Text::from(screen.footer)), footer_area);
        });
    }

    /// Take the live area again, wherever the cursor is now.
    ///
    /// A viewport remembers where it was put, which is no use once something
    /// else has had the terminal: a new one is placed by asking.
    fn retake(&mut self, rows: u16) {
        let taken = Terminal::with_options(
            CrosstermBackend::new(io::stderr()),
            TerminalOptions {
                viewport: Viewport::Inline(rows),
            },
        );
        if let Ok(taken) = taken {
            *self.terminal = taken;
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Write::flush(self.terminal.backend_mut())
    }
}

/// Whether the terminal has any room to draw in.
fn has_room() -> bool {
    ratatui::crossterm::terminal::size().is_ok_and(|(columns, rows)| columns > 0 && rows > 0)
}

/// How many rows a line takes once it has wrapped.
fn rows_for(line: &Line, width: u16) -> u16 {
    (line.width() as u16).max(1).div_ceil(width)
}

/// Redraw the live area, and push anything finished into the scrollback.
fn draw_loop(
    stage: &Stage,
    painter: &Weak<dyn Paint>,
    stop: &AtomicBool,
    interrupted: &AtomicBool,
    resumed: &AtomicBool,
) {
    // How far the list has scrolled, which is the one thing about the live area
    // that has to be remembered between frames.
    let mut list = ListState::default();
    loop {
        // Checked ahead of `stop`, so that an interrupt arriving as the run
        // ends is acted on rather than overtaken by it.
        if interrupted.load(Ordering::SeqCst) {
            // The tools this run started have had the same signal from the
            // terminal already; all that is left is to hand the terminal back
            // before the run goes.
            let last = painter.upgrade().map(|paint| paint.scrollback());
            give_back(stage, last.unwrap_or_default());
            std::process::exit(INTERRUPTED);
        }
        if stop.load(Ordering::Relaxed) {
            return;
        }

        // Back from a `^Z`: the shell had the terminal in the meantime, so
        // nothing on screen is ours and nothing is still set up.
        if resumed.swap(false, Ordering::SeqCst) {
            if enable_raw_mode().is_ok() {
                signals::keep_keys();
            }
            let _ = stage.hold().terminal.clear();
        }

        let Some(paint) = painter.upgrade() else {
            return;
        };
        let (scrollback, screen) = (paint.scrollback(), paint.screen());
        drop(paint);

        let mut held = stage.hold();
        held.print(scrollback);
        held.draw(screen, &mut list);
        drop(held);

        thread::sleep(FRAME);
    }
}

/// Read the keys typed at the display.
fn key_loop(stage: &Stage, painter: &Weak<dyn Paint>, stop: &AtomicBool) {
    while !stop.load(Ordering::Relaxed) {
        // `poll` waits without taking anything: the terminal can be given away
        // between a key arriving and this being able to read it.
        match event::poll(FRAME) {
            Ok(true) => {}
            Ok(false) => continue,
            Err(_) => return,
        }

        let key = {
            let _held = stage.hold();
            // Asked again now that the terminal is this thread's: if it was
            // suspended in between, whatever was typed belongs to whatever had
            // it, and has already been read by it.
            match event::poll(Duration::ZERO) {
                Ok(true) => event::read().ok().and_then(key_of),
                _ => None,
            }
        };

        if let Some(key) = key {
            let Some(paint) = painter.upgrade() else {
                return;
            };
            paint.key(key);
        }
    }
}

fn key_of(event: Event) -> Option<Key> {
    let Event::Key(key) = event else { return None };
    // A terminal that reports releases as well as presses would otherwise move
    // the cursor twice per keystroke.
    if key.kind != KeyEventKind::Press {
        return None;
    }
    match key.code {
        KeyCode::Up | KeyCode::Char('k') => Some(Key::Up),
        KeyCode::Down | KeyCode::Char('j') => Some(Key::Down),
        KeyCode::Enter => Some(Key::Enter),
        _ => None,
    }
}

/// Put the live area away and hand the terminal back as it was found.
fn give_back(stage: &Stage, last: Vec<Line<'static>>) {
    let mut held = stage.hold();
    held.print(last);
    // Puts the cursor back at the top of the live area and clears from there
    // down, so that what is printed next carries on where the run left off.
    let _ = held.terminal.clear();
    let _ = held.terminal.show_cursor();
    let _ = held.flush();
    let _ = disable_raw_mode();
}

fn spawn(name: &str, body: impl FnOnce() + Send + 'static) -> Option<JoinHandle<()>> {
    thread::Builder::new().name(name.into()).spawn(body).ok()
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
    /// ends, and notice coming back from a `^Z`.
    ///
    /// The flags are read by the drawing thread rather than acted on where they
    /// are set: redrawing and restoring a terminal are both far more than a
    /// signal handler may do.
    #[cfg(unix)]
    pub(super) fn catch(interrupted: &Arc<AtomicBool>, resumed: &Arc<AtomicBool>) -> Vec<Id> {
        use signal_hook::consts::{SIGCONT, SIGINT, SIGTERM};
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
        ids.extend(flag::register(SIGCONT, resumed.clone()));
        ids
    }

    #[cfg(not(unix))]
    pub(super) fn catch(_: &Arc<AtomicBool>, _: &Arc<AtomicBool>) -> Vec<Id> {
        Vec::new()
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
    use ratatui::crossterm::event::{KeyEvent, KeyEventState, KeyModifiers};

    fn press(code: KeyCode) -> Option<Key> {
        key_of(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)))
    }

    #[test]
    fn letters_and_arrows_are_the_same_keys() {
        assert_eq!(press(KeyCode::Char('j')), Some(Key::Down));
        assert_eq!(press(KeyCode::Down), Some(Key::Down));
        assert_eq!(press(KeyCode::Char('k')), Some(Key::Up));
        assert_eq!(press(KeyCode::Up), Some(Key::Up));
        assert_eq!(press(KeyCode::Enter), Some(Key::Enter));
    }

    #[test]
    fn keys_that_do_nothing_are_dropped() {
        assert_eq!(press(KeyCode::Char('x')), None);
        assert_eq!(press(KeyCode::Esc), None);
        assert_eq!(key_of(Event::FocusGained), None);
    }

    #[test]
    fn a_key_being_released_is_not_a_second_press() {
        let released = KeyEvent::new_with_kind_and_state(
            KeyCode::Char('j'),
            KeyModifiers::NONE,
            KeyEventKind::Release,
            KeyEventState::NONE,
        );
        assert_eq!(key_of(Event::Key(released)), None);
    }

    #[test]
    fn long_lines_wrap_rather_than_being_cut() {
        assert_eq!(rows_for(&Line::from(""), 80), 1);
        assert_eq!(rows_for(&Line::from("x".repeat(80)), 80), 1);
        assert_eq!(rows_for(&Line::from("x".repeat(81)), 80), 2);
        assert_eq!(rows_for(&Line::from("x".repeat(240)), 80), 3);
    }
}
