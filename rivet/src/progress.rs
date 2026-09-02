//! Live terminal reporting for flow execution.
//!
//! While a flow is running, every step that is currently executing gets a line
//! with a spinner, its elapsed time, and where the step says it has got to.
//! Finished steps scroll off into normal terminal output as `✔` (executed),
//! `⏭` (skipped because it was pinned), `⊘` (never runnable, because something
//! it depended on failed) or `✖` (failed), and a summary bar at the bottom
//! tracks overall progress.
//!
//! # The cursor
//!
//! One of the steps is under a cursor, which `↑`/`↓` (or `j`/`k`) move between
//! them. `enter` copies a `tail` command for the log that step is writing, to
//! be pasted into another terminal: the display shows where a step has got to
//! and never what its tool is saying, so this is how to go and read that
//! without disturbing the run.
//!
//! The list is the tail of the record: the steps that are running, and behind
//! them the ones that finished most recently, which stay reachable because the
//! log worth reading is usually one belonging to a step that has already
//! stopped — a step that failed most of all. The cursor stays on the step it is
//! on; the one thing that moves it by itself is that step finishing, when it
//! goes to the newest step still running. Nothing is shown twice: a finished
//! step moves up into the scrollback only when it leaves the list.
//!
//! See [`StepHandle::set_output_files`] for what a step offers to follow, and
//! [`crate::tui`] for the terminal itself.
//!
//! When stderr is not a terminal (CI, redirected logs) the display degrades to
//! plain, one-line-per-event logging instead of drawing escape sequences.
//!
//! # The display is not a log
//!
//! Two things reach it, and nothing else: [`status`], which a step sets from
//! Rust, and the substep [`banner`]s a tool is told to print, picked out of its
//! output by [`parse_banner`]. Raw tool output goes to the step's log files and
//! stops there — it is not shown, not summarised, and not distinguished by
//! stream. A tool writing to stderr means nothing in particular; plenty write
//! all their chatter there.
//!
//! # Nothing else may write to the terminal
//!
//! The display keeps a block of rows at the bottom of the terminal and puts
//! finished lines above it. Anything that reaches the terminal without going
//! through it — a bare `println!` in flow code, a child process with inherited
//! stdio, a panic message from a thread the executor does not know about —
//! scrolls the screen out from under it, and what it thinks it drew and what is
//! there stop matching.
//!
//! So while a flow is running, print with [`note`], run subprocesses through
//! [`crate::exec`] so their output is captured to file, and wrap anything that
//! insists on the terminal for itself in [`suspend`].
//!
//! Anything worth recording rather than showing goes through `tracing`, which
//! [`crate::log`] writes to files and never to a stream.

use std::cell::RefCell;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock, Mutex, RwLock};
use std::time::{Duration, Instant};

use ratatui::style::{Style, Stylize};
use ratatui::text::{Line, Span};

use crate::clipboard;
use crate::tui::{Key, Paint, Screen, Tui};

/// Longest step label rendered before it is truncated.
const MAX_LABEL_WIDTH: usize = 44;

/// Most steps the live area shows at once.
const MAX_VISIBLE: usize = 8;

/// Fewest steps the live area shows, however few run at a time.
const MIN_VISIBLE: usize = 4;

/// Rows kept for steps that have finished, over and above the ones that can be
/// running. Finished steps stay in the list this long before moving up into the
/// scrollback, so there is recent history to move the cursor onto.
const HISTORY: usize = 2;

/// Rows the live area has besides the steps: the summary, and the hint.
const FIXED_ROWS: usize = 2;

/// How long the hint line holds a message before going back to the keys.
const FLASH_FOR: Duration = Duration::from_secs(4);

/// How much of a step's log a copied command shows before it starts following.
const TAIL_LINES: usize = 100;

/// Width of each inline progress bar, in characters.
const BAR_WIDTH: usize = 10;

/// Width of the bar on the summary line.
const SUMMARY_BAR_WIDTH: usize = 24;

/// What the keys do, on the line under the summary.
const HINT: &str = "  ↑/↓ or j/k select a step · enter copies a command to follow its log";

/// The spinner, one frame per [`SPIN_EVERY`].
const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// How long each spinner frame lasts.
const SPIN_EVERY: u128 = 100;

/// How long a tool may say nothing before its line admits it.
///
/// A spinner means the step is still there, which is not the same as making
/// progress: a tool can wedge with its process alive and its output stopped
/// dead. Long enough that a slow-but-working stage does not trip it, short
/// enough that nobody watches a stalled run for an afternoon.
pub(crate) const QUIET_AFTER: Duration = Duration::from_secs(600);

/// Separates the two halves of a step's line, and the two halves of the
/// location reported when a step fails.
pub(crate) const REGION_SEP: &str = " │ ";

/// How a step ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// The step ran to completion.
    Completed,
    /// The step was pinned, so it and its dependencies were not run.
    Skipped,
    /// The step panicked.
    Failed,
}

/// How many steps ended each way.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Counts {
    /// Steps that will not be started again, whatever the reason.
    pub finished: usize,
    /// Steps skipped because they were pinned.
    pub skipped: usize,
    /// Steps that never became runnable because a step upstream of them failed.
    pub blocked: usize,
    /// Steps that failed.
    pub failed: usize,
}

impl Counts {
    /// Steps that actually ran and succeeded.
    pub fn executed(&self) -> usize {
        self.finished
            .saturating_sub(self.skipped + self.blocked + self.failed)
    }
}

/// Renders the state of a run to the terminal.
pub(crate) struct Reporter {
    label_width: usize,
    total: usize,
    started: Instant,
    finished: AtomicUsize,
    running: AtomicUsize,
    skipped: AtomicUsize,
    blocked: AtomicUsize,
    failed: AtomicUsize,
    /// Hands out the ids the cursor tracks steps by.
    next_step: AtomicUsize,
    ui: Option<Ui>,
}

struct Ui {
    /// Lines waiting to be left in the terminal's scrollback.
    scrollback: Mutex<Vec<Line<'static>>>,
    /// The running steps, and which of them the cursor is on.
    cursor: Mutex<Cursor>,
    /// Something to say on the hint line, until the moment it expires.
    flash: Mutex<Option<(Line<'static>, Instant)>>,
    /// How many steps the live area has room for.
    visible: usize,
    /// The terminal, for as long as the run is drawing on it.
    tui: Mutex<Option<Tui>>,
}

impl Reporter {
    pub(crate) fn new(
        total: usize,
        label_width: usize,
        concurrency: usize,
        progress: bool,
    ) -> Arc<Self> {
        // Room for everything that can run at once and a little of what has
        // just finished, within reason either way.
        let visible = (concurrency + HISTORY)
            .clamp(MIN_VISIBLE, MAX_VISIBLE)
            .min(total)
            .max(1);
        let ui = (progress && on_a_terminal()).then(|| Ui {
            scrollback: Mutex::new(Vec::new()),
            cursor: Mutex::new(Cursor::new(visible)),
            flash: Mutex::new(None),
            visible,
            tui: Mutex::new(None),
        });

        let reporter = Arc::new(Self {
            label_width: label_width.min(MAX_LABEL_WIDTH),
            total,
            started: Instant::now(),
            finished: AtomicUsize::new(0),
            running: AtomicUsize::new(0),
            skipped: AtomicUsize::new(0),
            blocked: AtomicUsize::new(0),
            failed: AtomicUsize::new(0),
            next_step: AtomicUsize::new(0),
            ui,
        });
        reporter.take_terminal();
        reporter
    }

    /// Start drawing.
    ///
    /// The display holds the reporter weakly: it must not be the thing keeping
    /// a finished run alive, because a run that has ended has to give the
    /// terminal back. If the terminal will not have it, the run falls back to
    /// plain line-by-line logging.
    fn take_terminal(self: &Arc<Self>) {
        let Some(ui) = &self.ui else { return };
        let paint: Arc<dyn Paint> = Arc::clone(self) as Arc<dyn Paint>;
        let painter = Arc::downgrade(&paint);
        let rows = (ui.visible + FIXED_ROWS) as u16;
        *ui.tui.lock().unwrap() = Tui::start(painter, rows);
    }

    /// Whether the live display is up, as opposed to plain logging.
    fn drawing(&self) -> bool {
        self.ui
            .as_ref()
            .is_some_and(|ui| ui.tui.lock().unwrap().is_some())
    }

    /// Announce that `label` has started running, returning a handle the step
    /// can use to report its own output.
    ///
    /// `log` is the step's own log file: both where what it logs is written,
    /// and — until it starts a tool of its own — the file the cursor offers a
    /// command to follow.
    pub(crate) fn start(
        self: &Arc<Self>,
        label: &str,
        log: Option<Arc<crate::log::LogFile>>,
    ) -> StepHandle {
        self.running.fetch_add(1, Ordering::Relaxed);
        let id = self.next_step.fetch_add(1, Ordering::Relaxed);
        let label: Arc<str> = Arc::from(truncate(label, MAX_LABEL_WIDTH));
        let state = Arc::new(Mutex::new(StepState::default()));
        let started = Instant::now();

        match self.ui.as_ref().filter(|_| self.drawing()) {
            Some(ui) => {
                let retired = ui.cursor.lock().unwrap().insert(Row {
                    id,
                    label: Arc::clone(&label),
                    started,
                    state: Arc::clone(&state),
                    log: log.as_ref().map(|log| log.path().to_path_buf()),
                    ended: None,
                });
                ui.leave(retired);
            }
            None => self.print(Line::from(vec![
                span("  ▶ ", Style::new().cyan()),
                span(label.to_string(), Style::new()),
            ])),
        }

        StepHandle {
            id,
            label,
            reporter: Arc::clone(self),
            started,
            state,
            log,
        }
    }

    /// Record a step that was skipped without ever starting.
    pub(crate) fn skip(&self, label: &str, reason: &str) {
        self.skipped.fetch_add(1, Ordering::Relaxed);
        self.finished.fetch_add(1, Ordering::Relaxed);
        self.print(Line::from(vec![
            span("  ⏭ ", Style::new().yellow()),
            span(self.pad(label), Style::new().dim()),
            span("  ", Style::new()),
            span(reason.to_string(), Style::new().yellow()),
        ]));
    }

    /// Record a step that can never run because something it depends on
    /// failed.
    ///
    /// A failure does not stop the rest of the run — independent steps keep
    /// going and new ones still start — so the steps downstream of it are
    /// named as they are dropped, rather than being left to look forgotten.
    pub(crate) fn block(&self, label: &str, blame: &str) {
        self.blocked.fetch_add(1, Ordering::Relaxed);
        self.finished.fetch_add(1, Ordering::Relaxed);
        self.print(Line::from(vec![
            span("  ⊘ ", Style::new().yellow()),
            span(self.pad(label), Style::new().dim()),
            span("  ", Style::new()),
            span(format!("blocked by {blame}"), Style::new().yellow()),
        ]));
    }

    /// Record the end of a step started with [`Reporter::start`].
    pub(crate) fn finish(&self, handle: &StepHandle, outcome: Outcome, detail: Option<&str>) {
        self.running.fetch_sub(1, Ordering::Relaxed);
        self.finished.fetch_add(1, Ordering::Relaxed);

        // The step's line for the record, which is also its row in the list
        // once it has stopped. Built without the record's indent, so that the
        // cursor can go where the indent goes.
        let elapsed = fmt_duration(handle.started.elapsed());
        let padded = self.pad(&handle.label);
        let record = match outcome {
            Outcome::Completed => Line::from(vec![
                span("✔ ", Style::new().green()),
                span(padded, Style::new().bold()),
                span(format!("  {elapsed}"), Style::new().dim()),
            ]),
            Outcome::Skipped => Line::from(vec![
                span("⏭ ", Style::new().yellow()),
                span(padded, Style::new().dim()),
                span("  ", Style::new()),
                span(
                    detail.unwrap_or("skipped").to_string(),
                    Style::new().yellow(),
                ),
            ]),
            Outcome::Failed => {
                self.failed.fetch_add(1, Ordering::Relaxed);
                let mut spans = vec![
                    span("✖ ", Style::new().red()),
                    span(padded, Style::new().red().bold()),
                    span(format!("  {elapsed}"), Style::new().dim()),
                ];
                // Say where it died, not just that it did. Both halves are
                // reported: which of them caused the failure is exactly what is
                // not known here.
                if let Some(location) = handle.location() {
                    spans.push(span(format!("  during {location}"), Style::new().yellow()));
                }
                if let Some(detail) = detail {
                    spans.push(span(
                        format!("  {}", truncate(&clean(detail), 160)),
                        Style::new().red(),
                    ));
                }
                Line::from(spans)
            }
        };

        match self.ui.as_ref().filter(|_| self.drawing()) {
            Some(ui) => {
                // A failure goes into the record at once: it is the one line
                // nobody should have to wait for. Everything else reaches the
                // record when its row retires from the list, so that nothing is
                // on screen twice.
                if outcome == Outcome::Failed {
                    self.print(indent(record.clone()));
                }
                let retired = ui
                    .cursor
                    .lock()
                    .unwrap()
                    .end(handle.id, Ended { outcome, record });
                ui.leave(retired);
            }
            None => self.print(indent(record)),
        }
    }

    /// Leave a line in the terminal above the live display.
    pub(crate) fn print(&self, line: Line<'static>) {
        match self.ui.as_ref().filter(|_| self.drawing()) {
            Some(ui) => {
                // Steps that finished before this are still in the list; they
                // go into the record ahead of it, so the record reads in the
                // order things happened.
                let held = ui.cursor.lock().unwrap().flush();
                let mut scrollback = ui.scrollback.lock().unwrap();
                scrollback.extend(held);
                scrollback.push(line);
            }
            // Nothing is drawing, so the line is just output. Written rather
            // than printed because `eprintln!` panics if the write fails, and
            // this runs on a worker: a run piped into something that stops
            // reading — `| head`, or `| less` quit early — would otherwise take
            // a step down with it.
            None => write_line(&plain(&line)),
        }
    }

    /// Tear down the live display and print a closing summary.
    pub(crate) fn finish_all(&self, elapsed: Duration) {
        if let Some(ui) = &self.ui {
            // Taken out of the lock before it is stopped: stopping waits for
            // the threads, and a key being handled wants this same lock.
            let tui = ui.tui.lock().unwrap().take();
            if let Some(tui) = tui {
                tui.stop(self.remaining());
            }
        }

        let counts = self.counts();
        let mut spans = vec![
            if counts.failed == 0 {
                span("  ✔ ", Style::new().green())
            } else {
                span("  ✖ ", Style::new().red())
            },
            span(format!("{} executed", counts.executed()), Style::new()),
        ];
        if counts.skipped > 0 {
            spans.push(span(format!(" · {} skipped", counts.skipped), Style::new()));
        }
        if counts.blocked > 0 {
            spans.push(span(
                format!(" · {} blocked", counts.blocked),
                Style::new().yellow(),
            ));
        }
        if counts.failed > 0 {
            spans.push(span(
                format!(" · {} failed", counts.failed),
                Style::new().red(),
            ));
        }
        spans.push(span(
            format!(" · {}", fmt_duration(elapsed)),
            Style::new().dim(),
        ));
        self.print(Line::from(spans));
    }

    /// How the run has gone so far.
    pub(crate) fn counts(&self) -> Counts {
        Counts {
            finished: self.finished.load(Ordering::Relaxed),
            skipped: self.skipped.load(Ordering::Relaxed),
            blocked: self.blocked.load(Ordering::Relaxed),
            failed: self.failed.load(Ordering::Relaxed),
        }
    }

    fn pad(&self, label: &str) -> String {
        let label = truncate(label, MAX_LABEL_WIDTH);
        let width = self.label_width;
        format!("{label:<width$}")
    }

    /// The bar and counts under the steps.
    fn summary(&self) -> Line<'static> {
        let counts = self.counts();
        let (done, todo) = bar_parts(counts.finished, self.total, SUMMARY_BAR_WIDTH);
        let mut spans = vec![
            span("  ", Style::new()),
            // What is done and what is left are different colours, as they were
            // when this bar was indicatif's `{bar:24.green/blue}`.
            span(done, Style::new().green()),
            span(todo, Style::new().blue()),
            span(
                format!(
                    " {}/{} steps · {}",
                    counts.finished,
                    self.total,
                    fmt_duration(self.started.elapsed())
                ),
                Style::new(),
            ),
        ];
        let running = self.running.load(Ordering::Relaxed);
        if running > 0 {
            spans.push(span(format!(" · {running} running"), Style::new().dim()));
        }
        if counts.blocked > 0 {
            spans.push(span(
                format!(" · {} blocked", counts.blocked),
                Style::new().yellow(),
            ));
        }
        if counts.failed > 0 {
            spans.push(span(
                format!(" · {} failed", counts.failed),
                Style::new().red(),
            ));
        }
        Line::from(spans)
    }

    /// Which spinner frame everything running is on.
    ///
    /// One frame for the whole display rather than one per step: they started
    /// at different moments, and spinners that are not in step with each other
    /// read as noise.
    fn spinner(&self) -> &'static str {
        let frame = self.started.elapsed().as_millis() / SPIN_EVERY;
        SPINNER[frame as usize % SPINNER.len()]
    }

    // -- the cursor ---------------------------------------------------------

    fn move_cursor(&self, delta: isize) {
        let Some(ui) = &self.ui else { return };
        ui.cursor.lock().unwrap().step(delta);
    }

    /// Copy a command for reading the selected step's log as it is written.
    ///
    /// What someone watching a step actually wants is the tool's own output,
    /// which the display deliberately never shows — it is in a file, and this
    /// hands over the command to follow that file somewhere else.
    fn copy_follow_command(&self) {
        let Some(ui) = &self.ui else { return };

        // Read out from under the lock: nothing that draws or copies may be
        // holding the cursor while it does so.
        let selected = {
            let cursor = ui.cursor.lock().unwrap();
            cursor
                .selected()
                .map(|row| (row.label.to_string(), row.follow_command()))
        };
        let Some((label, command)) = selected else {
            return;
        };

        let Some(command) = command else {
            self.flash(Line::from(span(
                format!("  no log yet for {label}"),
                Style::new().yellow(),
            )));
            return;
        };

        tracing::info!(step = %label, %command, "copied a command to follow the log");
        let tui = ui.tui.lock().unwrap();
        let copied = match tui.as_ref() {
            // Held still while it writes: asking the terminal to copy is an
            // escape sequence on the same stream the display draws on.
            Some(tui) => tui.hold(|| clipboard::copy(&command)),
            None => return,
        };
        drop(tui);

        if copied {
            self.flash(Line::from(vec![
                span("  ✔ copied ", Style::new().green().bold()),
                span(command, Style::new().dim()),
            ]));
        } else {
            // Nowhere to copy it to, so put it somewhere it can be read.
            self.print(Line::from(vec![
                span("  ⧉ ", Style::new().cyan()),
                span(command, Style::new()),
            ]));
            self.flash(Line::from(span(
                "  no clipboard to copy to; the command is above",
                Style::new().yellow(),
            )));
        }
    }

    /// Show something on the hint line for a few seconds.
    fn flash(&self, line: Line<'static>) {
        let Some(ui) = &self.ui else { return };
        *ui.flash.lock().unwrap() = Some((line, Instant::now() + FLASH_FOR));
    }
}

/// What the display draws, asked for afresh every frame.
impl Paint for Reporter {
    fn screen(&self) -> Screen {
        let Some(ui) = &self.ui else {
            return Screen::default();
        };

        let spinner = self.spinner();
        let cursor = ui.cursor.lock().unwrap();
        // Every running step is handed over, however many there are: keeping
        // the selected one on screen is the list's job, and it does that by
        // scrolling as little as it has to.
        let selected = cursor.position();
        let steps: Vec<Line<'static>> = cursor
            .rows
            .iter()
            .enumerate()
            .map(|(index, row)| row.line(Some(index) == selected, self.label_width, spinner))
            .collect();
        drop(cursor);

        Screen {
            steps,
            selected,
            footer: vec![self.summary(), ui.hint()],
        }
    }

    fn scrollback(&self) -> Vec<Line<'static>> {
        match &self.ui {
            Some(ui) => std::mem::take(&mut *ui.scrollback.lock().unwrap()),
            None => Vec::new(),
        }
    }

    fn remaining(&self) -> Vec<Line<'static>> {
        let Some(ui) = &self.ui else {
            return Vec::new();
        };
        // Everything still in the list that belongs in the record goes there
        // now, in order, ahead of whatever else was waiting.
        let drained = ui.cursor.lock().unwrap().drain();
        ui.leave(drained);
        self.scrollback()
    }

    fn key(&self, key: Key) {
        match key {
            Key::Up => self.move_cursor(-1),
            Key::Down => self.move_cursor(1),
            Key::Enter => self.copy_follow_command(),
        }
    }
}

impl Ui {
    /// Leave lines in the scrollback, above the live area.
    fn leave(&self, lines: Vec<Line<'static>>) {
        if !lines.is_empty() {
            self.scrollback.lock().unwrap().extend(lines);
        }
    }

    /// The bottom line: what was just done, or what the keys do.
    fn hint(&self) -> Line<'static> {
        let mut flash = self.flash.lock().unwrap();
        if let Some((line, until)) = flash.as_ref() {
            if Instant::now() < *until {
                return line.clone();
            }
            *flash = None;
        }
        Line::from(span(HINT, Style::new().dim()))
    }
}

/// Which step the display's cursor is on.
///
/// The rows are the steps that are running, and behind them the steps that
/// finished most recently: the log worth reading is usually one belonging to a
/// step that has already stopped, and a failed step that vanished the moment
/// it failed would take its log with it. They are in the order the steps
/// started, oldest first, which is the order the record above the display is
/// in as well.
///
/// # The cursor stays put
///
/// Steps starting never move it. Only the step it is on finishing does, and
/// then only if that step was running: it goes to the newest step still
/// running, so that someone watching the run keeps watching the run. Put on a
/// step that has already finished, it stays there until it is moved.
///
/// # Nothing is on screen twice
///
/// A finished step is either in the list or in the record above it, never
/// both. It stays in the list until enough newer steps have come along to push
/// it out, or until something else needs to go into the record after it, and
/// moves up into the scrollback then — so the record still grows as the run
/// goes, and still reads in the order things happened. Failures are the
/// exception both ways: they go into the record at once, because that line
/// should not wait, and they never retire from the list, because reaching them
/// is what the list is for.
struct Cursor {
    rows: Vec<Row>,
    /// The step the cursor is on.
    ///
    /// `None` only while there is nothing running for it to be on: before the
    /// first step starts, and after the step it was on finished with nothing
    /// else going. The next step to start takes it.
    on: Option<usize>,
    /// How many rows the list keeps before finished steps start to retire.
    keep: usize,
}

/// A step that has run, as the display sees it.
struct Row {
    id: usize,
    label: Arc<str>,
    started: Instant,
    state: Arc<Mutex<StepState>>,
    /// The step's own log file, if it has one.
    log: Option<PathBuf>,
    /// How it ended, once it has. `None` while it is still running.
    ended: Option<Ended>,
}

/// How a step that has stopped ended.
struct Ended {
    outcome: Outcome,
    /// Its line for the record, which is also its row in the list.
    record: Line<'static>,
}

impl Cursor {
    fn new(keep: usize) -> Self {
        Self {
            rows: Vec::new(),
            on: None,
            keep,
        }
    }

    /// Take on a step that has just started, at the end, handing back the lines
    /// of any steps that retire to make room.
    ///
    /// A step starting never moves the cursor off a step — only off nothing,
    /// when it has been waiting for something to run.
    fn insert(&mut self, row: Row) -> Vec<Line<'static>> {
        if self.on.is_none() {
            self.on = Some(row.id);
        }
        self.rows.push(row);
        self.retire()
    }

    /// Record how a step ended, handing back the lines of any steps that retire
    /// as a result. Its row stays, so its log can still be reached.
    ///
    /// This is the one thing that moves the cursor on its own: the step it was
    /// on has stopped, so it goes to what is running now — the newest such step
    /// — rather than being left on what the step turned into. That row is still
    /// there, a key or two up. With nothing running it waits for the next step
    /// to start.
    fn end(&mut self, id: usize, ended: Ended) -> Vec<Line<'static>> {
        if let Some(row) = self.rows.iter_mut().find(|row| row.id == id) {
            row.ended = Some(ended);
        }
        if self.on == Some(id) {
            self.on = self.newest_running().map(|index| self.rows[index].id);
        }
        self.retire()
    }

    /// Move finished steps out of the list, oldest first, while it is fuller
    /// than it keeps, and hand back their lines for the record.
    fn retire(&mut self) -> Vec<Line<'static>> {
        let mut retired = Vec::new();
        while self.rows.len() > self.keep {
            let Some(index) = self.rows.iter().position(|row| self.retires(row)) else {
                break;
            };
            retired.push(indent(self.rows.remove(index).record()));
        }
        retired
    }

    /// Move every finished step that can go into the record now, whatever the
    /// list's size, so that something else can go in after them in order.
    fn flush(&mut self) -> Vec<Line<'static>> {
        let mut flushed = Vec::new();
        let mut index = 0;
        while index < self.rows.len() {
            if self.retires(&self.rows[index]) {
                flushed.push(indent(self.rows.remove(index).record()));
            } else {
                index += 1;
            }
        }
        flushed
    }

    /// Whether a row is done with: finished, not a failure, and not what the
    /// cursor is on.
    fn retires(&self, row: &Row) -> bool {
        let finished = row
            .ended
            .as_ref()
            .is_some_and(|ended| ended.outcome != Outcome::Failed);
        finished && self.on != Some(row.id)
    }

    /// Everything in the list that still belongs in the record, in order, for
    /// when the run is over.
    fn drain(&mut self) -> Vec<Line<'static>> {
        let mut rows = std::mem::take(&mut self.rows);
        rows.retain(|row| {
            row.ended
                .as_ref()
                .is_some_and(|ended| ended.outcome != Outcome::Failed)
        });
        rows.into_iter().map(|row| indent(row.record())).collect()
    }

    /// Move the cursor `delta` rows down the list, stopping at the ends.
    ///
    /// Deliberately not wrapping: the list is history as well as what is
    /// running, and a cursor that jumped from one end to the other would lose
    /// someone's place in it.
    fn step(&mut self, delta: isize) {
        let Some(position) = self.position() else {
            return;
        };
        let moved = (position as isize + delta).clamp(0, self.rows.len() as isize - 1) as usize;
        self.on = Some(self.rows[moved].id);
    }

    /// Where the cursor is in the list.
    ///
    /// While it waits for something to run it rests on the last step, so that
    /// there is still something to move from.
    fn position(&self) -> Option<usize> {
        match self.on {
            Some(id) => self.rows.iter().position(|row| row.id == id),
            None => self.rows.len().checked_sub(1),
        }
    }

    fn newest_running(&self) -> Option<usize> {
        self.rows.iter().rposition(|row| row.ended.is_none())
    }

    fn selected(&self) -> Option<&Row> {
        self.position().map(|position| &self.rows[position])
    }
}

impl Row {
    /// This step's line in the live area.
    fn line(&self, selected: bool, width: usize, spinner: &str) -> Line<'static> {
        let cursor = span(
            if selected { "❯ " } else { "  " },
            Style::new().cyan().bold(),
        );
        match &self.ended {
            // The very line the record will get, so the two can never say
            // different things. Dimmed while it is history in the list — except
            // a failure, which is what the list is for reaching.
            Some(ended) => {
                let mut spans = vec![cursor];
                spans.extend(ended.record.spans.iter().cloned());
                let line = Line::from(spans);
                match ended.outcome {
                    Outcome::Failed => line,
                    _ => line.dim(),
                }
            }
            None => {
                let label = format!("{:<width$}", truncate(&self.label, MAX_LABEL_WIDTH));
                self.running_line(cursor, label, spinner)
            }
        }
    }

    /// The step's line for the record. Only a finished step has one.
    fn record(self) -> Line<'static> {
        self.ended.map(|ended| ended.record).unwrap_or_default()
    }

    fn running_line(&self, cursor: Span<'static>, label: String, spinner: &str) -> Line<'static> {
        let mut spans = vec![
            cursor,
            span(format!("{spinner} "), Style::new().cyan()),
            span(label, Style::new().bold()),
            span(
                format!(" {:>5}  ", fmt_duration(self.started.elapsed())),
                Style::new().dim(),
            ),
        ];

        // Left: what the step says it is doing. Right: what its tool says.
        let state = self.state.lock().unwrap();
        let regions = [state.status.as_ref(), state.banner.as_ref()]
            .into_iter()
            .flatten();
        for (index, region) in regions.enumerate() {
            if index > 0 {
                spans.push(span(REGION_SEP, Style::new().dim()));
            }
            spans.extend(region.spans());
        }

        // Last, after whatever the step and its tool had to say, so it reads as
        // a remark on the line rather than displacing it.
        if let Some(quiet) = state.quiet_for(self.started) {
            spans.push(span(
                format!("  (quiet for {})", fmt_duration(quiet)),
                Style::new().yellow(),
            ));
        }
        Line::from(spans)
    }

    /// A command to watch this step's log as it is written, if it has one yet.
    ///
    /// The output of the tool the step is driving is what someone watching it
    /// wants, so that wins; the step's own log is what there is to offer until
    /// a tool has started.
    fn follow_command(&self) -> Option<String> {
        let outputs = self.state.lock().unwrap().outputs.clone();
        let files = if outputs.is_empty() {
            self.log.clone().into_iter().collect()
        } else {
            outputs
        };
        (!files.is_empty()).then(|| follow_command(&files))
    }
}

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
    std::fs::canonicalize(path)
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

/// A label with optional `current/total` progress.
///
/// Used for both halves of a step's progress line: the status the step sets
/// itself, and the substep banner parsed out of its tool's output.
///
/// The halves are filled from different places — [`status`] writes the left,
/// and only [`parse_banner`] writes the right.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Progress {
    /// What is happening, e.g. `place_opt_design`.
    pub name: String,
    /// 1-based index and total, when they are known. Drives the bar.
    pub position: Option<(usize, usize)>,
}

impl Progress {
    /// Progress with no counts.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            position: None,
        }
    }

    /// Progress at `current` of `total`.
    pub fn at(current: usize, total: usize, name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            position: (total > 0).then_some((current, total)),
        }
    }

    /// Render as `place_opt_design (4/9)`.
    pub fn describe(&self) -> String {
        match self.position {
            Some((current, total)) => format!("{} ({current}/{total})", self.name),
            None => self.name.clone(),
        }
    }

    /// Render as a bar followed by the name, for the live display.
    fn spans(&self) -> Vec<Span<'static>> {
        match self.position {
            Some((current, total)) => vec![
                span(bar_glyphs(current, total, BAR_WIDTH), Style::new().cyan()),
                span(format!(" {current}/{total} {}", self.name), Style::new()),
            ],
            None => vec![span(self.name.clone(), Style::new())],
        }
    }
}

/// Draw a bar as text, so a step can show two independent ones on one line.
///
/// Ratatui has `Gauge` and `LineGauge`, but both take a line to themselves: a
/// step's line carries two of these, its own and its tool's, with a label after
/// each.
fn bar_glyphs(current: usize, total: usize, width: usize) -> String {
    let (done, todo) = bar_parts(current, total, width);
    format!("{done}{todo}")
}

/// The two halves of a bar, so that they can be styled apart.
fn bar_parts(current: usize, total: usize, width: usize) -> (String, String) {
    let ratio = (current.min(total) as f64) / (total.max(1) as f64);
    let filled = ((ratio * width as f64).round() as usize).min(width);
    if filled == 0 {
        (String::new(), "─".repeat(width))
    } else if filled >= width {
        ("━".repeat(width), String::new())
    } else {
        (
            format!("{}╸", "━".repeat(filled - 1)),
            "─".repeat(width - filled),
        )
    }
}

const BANNER_PREFIX: &str = "<<rivet:substep ";
const BANNER_SUFFIX: &str = ">>";

/// Build a banner line for a tool to print, e.g. from a generated TCL script:
///
/// ```
/// # let (index, total, name) = (4, 9, "place_opt_design");
/// let tcl = format!("puts {{{}}}", rivet::progress::banner(index, total, name));
/// assert_eq!(tcl, "puts {<<rivet:substep 4/9 place_opt_design>>}");
/// ```
///
/// `current` is 1-based. The marker is recognised anywhere in a line, so tools
/// that prefix their output with a severity or timestamp still work.
pub fn banner(current: usize, total: usize, name: &str) -> String {
    format!("{BANNER_PREFIX}{current}/{total} {name}{BANNER_SUFFIX}")
}

/// A banner for a tool that does not know how many substeps it will run.
pub fn banner_named(name: &str) -> String {
    format!("{BANNER_PREFIX}{name}{BANNER_SUFFIX}")
}

/// Pick a banner out of a line of tool output.
pub fn parse_banner(line: &str) -> Option<Progress> {
    let start = line.find(BANNER_PREFIX)? + BANNER_PREFIX.len();
    let rest = &line[start..];
    let body = rest[..rest.find(BANNER_SUFFIX)?].trim();
    if body.is_empty() {
        return None;
    }

    let (position, name) = match body.split_once(' ') {
        Some((head, tail)) => match parse_position(head) {
            Some(position) => (Some(position), tail.trim()),
            None => (None, body),
        },
        None => match parse_position(body) {
            Some(position) => (Some(position), ""),
            None => (None, body),
        },
    };

    Some(Progress {
        name: clean(name),
        position,
    })
}

fn parse_position(text: &str) -> Option<(usize, usize)> {
    let (current, total) = text.split_once('/')?;
    let current: usize = current.parse().ok()?;
    let total: usize = total.parse().ok()?;
    (total > 0).then_some((current, total))
}

/// A handle to the step running on the current thread.
///
/// Steps use this (usually indirectly, via [`crate::exec`]) to surface tool
/// output without corrupting the live display.
///
/// Nothing here draws. A step says where it has got to, and the display picks
/// that up the next time it paints.
#[derive(Clone)]
pub struct StepHandle {
    /// Which step this is, to the display's cursor.
    id: usize,
    label: Arc<str>,
    reporter: Arc<Reporter>,
    started: Instant,
    state: Arc<Mutex<StepState>>,
    /// Where this step's events are logged, if it has a file of its own.
    ///
    /// The handle is already the answer to "which step is running here", so it
    /// carries both of the step's channels: the line on screen and the file on
    /// disk. See [`crate::log`].
    log: Option<Arc<crate::log::LogFile>>,
}

/// What the step's progress line is currently showing.
///
/// The two halves are independent: nothing written to one ever disturbs the
/// other.
#[derive(Default)]
struct StepState {
    /// Left half: set by the step itself.
    status: Option<Progress>,
    /// Right half: parsed out of the tool's output.
    banner: Option<Progress>,
    /// Files the step's tool is writing. Not part of the line: these are what
    /// the cursor copies a command to follow.
    outputs: Vec<PathBuf>,
    /// When the tool last wrote a line. `None` until it writes its first.
    last_output: Option<Instant>,
}

impl StepState {
    /// How long it has been since the tool last wrote a line, once that is long
    /// enough to be worth saying. `None` while it is still writing.
    ///
    /// A tool that has written nothing at all is timed from `started` instead:
    /// a stage that hangs before its first line — waiting on a license, say —
    /// is exactly the case worth hearing about, and has no last line to go on.
    fn quiet_for(&self, started: Instant) -> Option<Duration> {
        let quiet = self.last_output.unwrap_or(started).elapsed();
        (quiet >= QUIET_AFTER).then_some(quiet)
    }
}

impl StepHandle {
    /// The label this step is displayed under.
    pub fn label(&self) -> &str {
        &self.label
    }

    /// How long the step has been running.
    pub fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    /// How long it has been since the step's tool wrote a line, once that is
    /// long enough to be worth saying. `None` while it is still writing.
    ///
    /// See [`QUIET_AFTER`]. The display shows this on the step's line; it is
    /// public so that whatever is waiting on the tool can say it out loud,
    /// which is the only way a run with no live display hears about it.
    pub fn quiet_for(&self) -> Option<Duration> {
        self.state.lock().unwrap().quiet_for(self.started)
    }

    /// Offer one line of the step's output to the display.
    ///
    /// A line carrying a substep banner moves the step on; see [`banner`].
    /// Everything else is ignored — raw output belongs in the step's log files,
    /// not on screen, whichever stream it arrived on.
    pub fn output_line(&self, line: &str) {
        // Before the banner check, and whatever the line said: any output at
        // all is the tool proving it is still working. The guard is dropped at
        // the end of the statement, because `enter_substep` takes the lock too.
        self.state.lock().unwrap().last_output = Some(Instant::now());

        if let Some(banner) = parse_banner(line) {
            self.enter_substep(banner);
        }
    }

    /// Record that the step has moved on to a new substep.
    ///
    /// Private on purpose: the right half of the line belongs to the step's
    /// tool. It is reached only by parsing a banner out of its output, so what
    /// it shows always reflects what the tool actually said.
    fn enter_substep(&self, banner: Progress) {
        // The status is left alone: the step owns that half of the line.
        self.state.lock().unwrap().banner = Some(banner.clone());

        // With no live display there is nowhere to put the substep, so log it.
        if !self.reporter.drawing() {
            self.reporter.print(Line::from(vec![
                span("  → ", Style::new().cyan()),
                span(self.label.to_string(), Style::new().dim()),
                span(format!("  {}", banner.describe()), Style::new()),
            ]));
        }
    }

    /// The substep last parsed out of this step's output.
    pub fn substep(&self) -> Option<Progress> {
        self.state.lock().unwrap().banner.clone()
    }

    /// Say which files the step's tool is writing its output to.
    ///
    /// Not shown — raw tool output never is — but offered to whoever is
    /// watching: the display's cursor turns these into a command for following
    /// the step's log somewhere else. [`crate::exec`] calls this for the
    /// commands it runs; a step driving a tool some other way should call it
    /// itself.
    pub fn set_output_files(&self, files: Vec<PathBuf>) {
        self.state.lock().unwrap().outputs = files;
    }

    /// The files last given to [`StepHandle::set_output_files`].
    pub fn output_files(&self) -> Vec<PathBuf> {
        self.state.lock().unwrap().outputs.clone()
    }

    /// Clear the substep, for when the tool reporting it has exited.
    ///
    /// Clearing is allowed from Rust even though writing is not: a finished
    /// tool's last substep is not where the step is any more, and leaving it up
    /// would misattribute anything that happens next.
    pub fn clear_substep(&self) {
        self.state.lock().unwrap().banner = None;
    }

    /// Where the step is: its status, its substep, or both when both are known.
    ///
    /// Used to say where a failure happened.
    pub fn location(&self) -> Option<String> {
        let state = self.state.lock().unwrap();
        let parts: Vec<String> = [state.status.as_ref(), state.banner.as_ref()]
            .into_iter()
            .flatten()
            .map(Progress::describe)
            .collect();
        (!parts.is_empty()).then(|| parts.join(REGION_SEP))
    }

    /// The status this step last set.
    pub fn status(&self) -> Option<Progress> {
        self.state.lock().unwrap().status.clone()
    }

    /// Set the status shown in the left half of the line.
    ///
    /// Independent of anything parsed from the step's output: neither a banner
    /// nor a line of tool output can overwrite it.
    pub fn set_status(&self, status: impl Into<String>) {
        self.set_progress(Progress::new(clean(&status.into())));
    }

    /// [`StepHandle::set_status`] with a progress bar over `current`/`total`.
    pub fn set_status_progress(&self, current: usize, total: usize, status: impl Into<String>) {
        self.set_progress(Progress::at(current, total, clean(&status.into())));
    }

    /// Set the status directly.
    pub fn set_progress(&self, status: Progress) {
        self.state.lock().unwrap().status = Some(status);
    }

    /// Clear the status, leaving the rest of the line intact.
    pub fn clear_status(&self) {
        self.state.lock().unwrap().status = None;
    }
}

thread_local! {
    static CURRENT: RefCell<Option<StepHandle>> = const { RefCell::new(None) };
}

static ACTIVE: LazyLock<RwLock<Option<Arc<Reporter>>>> = LazyLock::new(|| RwLock::new(None));

/// The step running on this thread, if any.
pub fn current_step() -> Option<StepHandle> {
    CURRENT.with(|current| current.borrow().clone())
}

/// Make `handle` the step running on this thread until the guard is dropped.
///
/// This is the one place the answer lives. The display reads it to know whose
/// line to draw on, [`crate::exec`] reads it to offer tool output to, and
/// [`crate::log`] reads it to know which file a log line belongs in.
pub(crate) fn enter_step(handle: StepHandle) -> CurrentStep {
    let _ = CURRENT.try_with(|current| *current.borrow_mut() = Some(handle));
    CurrentStep
}

pub(crate) struct CurrentStep;

impl Drop for CurrentStep {
    fn drop(&mut self) {
        let _ = CURRENT.try_with(|current| *current.borrow_mut() = None);
    }
}

/// The log file of the step running on this thread, if it has one.
pub(crate) fn current_step_log() -> Option<Arc<crate::log::LogFile>> {
    CURRENT
        .try_with(|current| {
            current
                .borrow()
                .as_ref()
                .and_then(|handle| handle.log.clone())
        })
        .ok()
        .flatten()
}

pub(crate) fn set_active_reporter(reporter: Option<Arc<Reporter>>) {
    *ACTIVE.write().unwrap() = reporter;
}

/// Print a line above the live display, or to stdout if no flow is running.
///
/// Use this instead of `println!` anywhere that might run inside a flow: see
/// [the module docs](self#nothing-else-may-write-to-the-terminal).
///
/// The line is logged as well as shown, so a run's log holds everything the
/// person watching it was told.
pub fn note(message: impl AsRef<str>) {
    let message = message.as_ref();
    tracing::info!(target: "rivet::note", "{message}");

    // Cloned out so the lock is not held while drawing, which would block the
    // end of the run.
    let active = ACTIVE.read().unwrap().clone();
    match active {
        // Cleaned, and drawn in the display's own styling: the line is put on
        // screen a cell at a time, so an escape sequence in it would be printed
        // rather than obeyed.
        Some(reporter) => reporter.print(Line::from(clean(message))),
        // Outside a run there is no display to protect, but the same applies:
        // a note is not worth panicking over.
        None => write_line(message),
    }
}

/// Run `f` with the live display taken down, then redraw it.
///
/// For the rare thing that has to have the terminal to itself: a subprocess
/// with inherited stdio, an interactive prompt, a library that prints its own
/// progress. Writing to the terminal any other way while a flow is running
/// corrupts the display; see
/// [the module docs](self#nothing-else-may-write-to-the-terminal).
///
/// Prefer [`note`] for plain output. `f` must not call back into this module —
/// the display's lock is held while it runs.
pub fn suspend<R>(f: impl FnOnce() -> R) -> R {
    let active = ACTIVE.read().unwrap().clone();
    let Some(reporter) = active else { return f() };
    let Some(ui) = &reporter.ui else { return f() };
    // The live area comes down and the terminal goes back to collecting and
    // echoing whole lines: whatever has to have the terminal to itself usually
    // wants both.
    let tui = ui.tui.lock().unwrap();
    match tui.as_ref() {
        Some(tui) => tui.suspend(f),
        None => f(),
    }
}

/// Offer one line of the current step's output to the display.
///
/// Only a substep [`banner`] does anything; see [`StepHandle::output_line`].
/// Use [`note`] for something you want on screen.
pub fn log_line(line: impl AsRef<str>) {
    if let Some(handle) = current_step() {
        handle.output_line(line.as_ref());
    }
}

/// Set the status shown in the left half of the current step's line.
///
/// Independent of the step's tool output: a substep banner parsed from that
/// output fills the right half and never disturbs this.
pub fn status(message: impl Into<String>) {
    if let Some(handle) = current_step() {
        handle.set_status(message);
    }
}

/// [`status`] with a progress bar over `current`/`total`.
///
/// For work a step does itself, where there is no tool output to parse:
///
/// ```no_run
/// # let gds_files: Vec<String> = vec![];
/// for (index, file) in gds_files.iter().enumerate() {
///     rivet::progress::status_progress(index + 1, gds_files.len(), format!("merging {file}"));
///     // ...
/// }
/// ```
pub fn status_progress(current: usize, total: usize, message: impl Into<String>) {
    if let Some(handle) = current_step() {
        handle.set_status_progress(current, total, message);
    }
}

/// Clear the current step's status.
pub fn clear_status() {
    if let Some(handle) = current_step() {
        handle.clear_status();
    }
}

/// Clear the current step's substep. See [`StepHandle::clear_substep`].
pub fn clear_substep() {
    if let Some(handle) = current_step() {
        handle.clear_substep();
    }
}

/// Whether there is a terminal to draw the live display on.
///
/// Both streams have to be one, though only stderr is ever drawn on. Placing
/// the live area means asking the terminal where its cursor is, and crossterm
/// asks by writing to stdout: with stdout redirected the question goes into
/// whatever it was redirected to and no answer ever comes. Rather than stall
/// for the two seconds crossterm waits for one, a run that is not talking to a
/// terminal on both streams falls back to plain line-by-line logging.
fn on_a_terminal() -> bool {
    std::io::stderr().is_terminal() && std::io::stdout().is_terminal()
}

/// Write one line to stderr, or give up on it.
///
/// Nothing rivet prints is worth failing a step for, and the display is not the
/// place to discover that a pipe has closed.
fn write_line(line: &str) {
    use std::io::Write as _;
    let mut stderr = std::io::stderr().lock();
    let _ = writeln!(stderr, "{line}");
}

/// A line as it goes into the record: indented to where the cursor would be.
fn indent(line: Line<'static>) -> Line<'static> {
    let mut spans = vec![span("  ", Style::new())];
    spans.extend(line.spans);
    Line::from(spans)
}

/// One piece of a line, in the display's own styling.
fn span(text: impl Into<String>, style: Style) -> Span<'static> {
    Span::styled(text.into(), style)
}

/// A line as plain text, for a terminal that is not being drawn on and for the
/// log.
fn plain(line: &Line) -> String {
    line.spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect()
}

/// Collapse text into something safe to draw on one row.
///
/// A banner name is tool output, and a step's status is often built from one, so
/// neither is plain text: EDA tools animate their own progress with carriage
/// returns and cursor-movement escapes, ring the bell, and colour what they
/// print. Drawn as-is, a single `ESC[1A` moves the cursor out from under the
/// live display and every redraw after it lands a row lower.
///
/// Escape sequences and control characters are dropped, colour included. The
/// display owns its own styling, and indicatif truncates a bar message to the
/// terminal width, which would cut a kept sequence in half.
fn clean(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.trim_end().chars().peekable();

    while let Some(c) = chars.next() {
        match c {
            '\u{1b}' => match chars.peek() {
                // A CSI sequence runs until its final byte: colour, cursor
                // movement, erasing, scrolling, screen switching.
                Some('[') => {
                    chars.next();
                    for c in chars.by_ref() {
                        if ('\u{40}'..='\u{7e}').contains(&c) {
                            break;
                        }
                    }
                }
                // A string escape (window title and friends) runs until a
                // bell or a string terminator.
                Some(']' | 'P' | 'X' | '^' | '_') => {
                    chars.next();
                    while let Some(c) = chars.next() {
                        if c == '\u{7}' {
                            break;
                        }
                        if c == '\u{1b}' && chars.peek() == Some(&'\\') {
                            chars.next();
                            break;
                        }
                    }
                }
                // Anything else is intermediate bytes followed by one final
                // byte: `ESC ( B` to pick a charset, `ESC 7` to save the
                // cursor, and so on.
                _ => {
                    while let Some(&c) = chars.peek() {
                        chars.next();
                        if !('\u{20}'..='\u{2f}').contains(&c) {
                            break;
                        }
                    }
                }
            },
            '\t' => out.push_str("    "),
            c if c.is_control() => out.push(' '),
            c => out.push(c),
        }
    }
    out
}

fn truncate(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_string();
    }
    text.chars()
        .take(width.saturating_sub(1))
        .collect::<String>()
        + "…"
}

pub(crate) fn fmt_duration(duration: Duration) -> String {
    let secs = duration.as_secs();
    match secs {
        0..=59 if secs < 10 => format!("{:.1}s", duration.as_secs_f64()),
        0..=59 => format!("{secs}s"),
        60..=3599 => format!("{}m{:02}s", secs / 60, secs % 60),
        _ => format!("{}h{:02}m", secs / 3600, (secs % 3600) / 60),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A reporter with no terminal, which is every reporter under `cargo test`.
    fn reporter() -> Arc<Reporter> {
        Reporter::new(1, 8, 1, false)
    }

    #[test]
    fn durations_are_human_readable() {
        assert_eq!(fmt_duration(Duration::from_millis(1234)), "1.2s");
        assert_eq!(fmt_duration(Duration::from_secs(42)), "42s");
        assert_eq!(fmt_duration(Duration::from_secs(64)), "1m04s");
        assert_eq!(fmt_duration(Duration::from_secs(3725)), "1h02m");
    }

    #[test]
    fn banners_round_trip() {
        let line = banner(4, 9, "place_opt_design");
        assert_eq!(line, "<<rivet:substep 4/9 place_opt_design>>");
        assert_eq!(
            parse_banner(&line),
            Some(Progress {
                name: "place_opt_design".into(),
                position: Some((4, 9)),
            })
        );

        assert_eq!(
            parse_banner(&banner_named("route_design")),
            Some(Progress {
                name: "route_design".into(),
                position: None,
            })
        );
    }

    #[test]
    fn banners_are_found_inside_decorated_tool_output() {
        // Tools prefix their output with all sorts of things.
        let banner = parse_banner("INFO [12:04:11] <<rivet:substep 2/9 floorplan design>> ok");
        assert_eq!(
            banner,
            Some(Progress {
                name: "floorplan design".into(),
                position: Some((2, 9)),
            })
        );
    }

    #[test]
    fn non_banner_output_is_left_alone() {
        assert_eq!(parse_banner("routing layer 4/6"), None);
        assert_eq!(parse_banner("<<rivet:substep >>"), None);
        // An unterminated marker is not a banner.
        assert_eq!(parse_banner("<<rivet:substep 1/2 place"), None);
    }

    #[test]
    fn a_bare_name_is_a_banner_without_a_position() {
        assert_eq!(
            parse_banner("<<rivet:substep 3 of 9>>"),
            Some(Progress {
                name: "3 of 9".into(),
                position: None,
            })
        );
    }

    #[test]
    fn only_banners_reach_the_display() {
        let reporter = reporter();
        let handle = reporter.start("decoder par", None);

        handle.output_line("starting up");
        assert_eq!(handle.substep(), None);

        handle.output_line(&format!("INFO: {}", banner(4, 9, "place_opt_design")));
        let substep = handle.substep().expect("banner was not picked up");
        assert_eq!(substep.name, "place_opt_design");
        assert_eq!(substep.position, Some((4, 9)));
        assert_eq!(substep.describe(), "place_opt_design (4/9)");
    }

    #[test]
    fn status_and_banners_do_not_disturb_each_other() {
        let reporter = reporter();
        let handle = reporter.start("decoder par", None);

        handle.set_status_progress(3, 12, "merging gds");
        handle.output_line(&banner(2, 5, "route_design"));
        handle.output_line("Info: track 12");

        // A banner, and the tool output after it, leave the status alone.
        let status = handle.status().expect("status was cleared by the tool");
        assert_eq!(status.name, "merging gds");
        assert_eq!(status.position, Some((3, 12)));

        // And setting the status again leaves the substep alone.
        handle.set_status("merging gds (done)");
        let substep = handle.substep().expect("substep was cleared by the status");
        assert_eq!(substep.name, "route_design");
        assert_eq!(substep.position, Some((2, 5)));
    }

    #[test]
    fn location_reports_whichever_halves_are_set() {
        let reporter = reporter();

        let handle = reporter.start("a", None);
        assert_eq!(handle.location(), None);

        let handle = reporter.start("b", None);
        handle.set_status("merging");
        assert_eq!(handle.location().as_deref(), Some("merging"));

        let handle = reporter.start("c", None);
        handle.output_line(&banner(2, 5, "route"));
        assert_eq!(handle.location().as_deref(), Some("route (2/5)"));

        let handle = reporter.start("d", None);
        handle.set_status_progress(7, 12, "merging");
        handle.output_line(&banner(2, 5, "route"));
        assert_eq!(
            handle.location().as_deref(),
            Some("merging (7/12) │ route (2/5)")
        );

        handle.clear_substep();
        assert_eq!(handle.location().as_deref(), Some("merging (7/12)"));
    }

    #[test]
    fn status_can_be_cleared_on_its_own() {
        let reporter = reporter();
        let handle = reporter.start("decoder par", None);

        handle.output_line(&banner(1, 2, "place"));
        handle.set_status("linking");
        handle.clear_status();

        assert!(handle.status().is_none());
        assert_eq!(handle.substep().unwrap().name, "place");
    }

    #[test]
    fn bars_fill_in_proportion() {
        assert_eq!(bar_glyphs(0, 4, 10), "──────────");
        assert_eq!(bar_glyphs(1, 4, 10), "━━╸───────");
        assert_eq!(bar_glyphs(4, 4, 10), "━━━━━━━━━━");
        // Overshooting a total clamps rather than panicking.
        assert_eq!(bar_glyphs(9, 4, 10), "━━━━━━━━━━");
    }

    #[test]
    fn cursor_movement_is_stripped_from_tool_text() {
        // A tool animating its own progress. Drawn as-is, the `ESC[1A` walks
        // the cursor out from under the live display. A stray control character
        // becomes a space rather than vanishing, so it cannot glue two words
        // together.
        let line = "route\u{1b}[1A\u{1b}[2K 42%\u{7}done\r";
        assert_eq!(clean(line), "route 42% done");

        // Colour goes too: the display owns its own styling, and a line is
        // drawn a cell at a time, so a kept sequence would be printed rather
        // than obeyed.
        assert_eq!(
            clean("\u{1b}[1;31m**ERROR: net VDD has no driver\u{1b}[0m"),
            "**ERROR: net VDD has no driver"
        );

        // Window-title and charset escapes go too.
        assert_eq!(clean("a\u{1b}]0;innovus\u{7}b"), "ab");
        assert_eq!(clean("a\u{1b}(Bb"), "ab");
        assert_eq!(clean("a\tb"), "a    b");
    }

    #[test]
    fn banner_names_cannot_move_the_cursor() {
        // The name is tool output, and it is drawn on the step's line.
        let banner = parse_banner("<<rivet:substep 2/9 \u{1b}[2Jfloorplan\u{1b}[1A>>").unwrap();
        assert_eq!(banner.name, "floorplan");
        assert_eq!(banner.position, Some((2, 9)));
    }

    #[test]
    fn long_labels_are_truncated() {
        assert_eq!(truncate("abcdef", 4), "abc…");
        assert_eq!(truncate("abc", 4), "abc");
    }

    // -- what is drawn ------------------------------------------------------

    #[test]
    fn a_step_draws_its_label_status_and_substep() {
        let row = row(1);
        row.state.lock().unwrap().status = Some(Progress::at(3, 12, "merging gds"));
        row.state.lock().unwrap().banner = Some(Progress::new("route_design"));

        let line = plain(&row.line(true, 11, "⠹"));
        assert!(line.starts_with("❯ ⠹ step 1     "), "{line:?}");
        assert!(
            line.ends_with("━━╸─────── 3/12 merging gds │ route_design"),
            "{line:?}"
        );
    }

    #[test]
    fn the_summary_says_what_is_left() {
        let reporter = Reporter::new(7, 8, 4, false);
        reporter.finished.store(3, Ordering::Relaxed);
        reporter.running.store(2, Ordering::Relaxed);
        reporter.failed.store(1, Ordering::Relaxed);

        let summary = plain(&reporter.summary());
        assert!(summary.contains("3/7 steps"), "{summary}");
        assert!(summary.contains("2 running"), "{summary}");
        assert!(summary.contains("1 failed"), "{summary}");
    }

    // -- the cursor ---------------------------------------------------------

    fn row(id: usize) -> Row {
        Row {
            id,
            label: Arc::from(format!("step {id}")),
            started: Instant::now(),
            state: Arc::new(Mutex::new(StepState::default())),
            log: None,
            ended: None,
        }
    }

    /// The steps in the order they are drawn, oldest first.
    fn listed(cursor: &Cursor) -> Vec<usize> {
        cursor.rows.iter().map(|row| row.id).collect()
    }

    /// A cursor over `count` running steps, with room for plenty more.
    fn filled(count: usize) -> Cursor {
        let mut cursor = Cursor::new(100);
        for id in 1..=count {
            cursor.insert(row(id));
        }
        cursor
    }

    /// End step `id`, handing back what retired as plain text.
    fn end(cursor: &mut Cursor, id: usize, outcome: Outcome) -> Vec<String> {
        let glyph = match outcome {
            Outcome::Completed => "✔",
            Outcome::Skipped => "⏭",
            Outcome::Failed => "✖",
        };
        let record = Line::from(format!("{glyph} step {id}"));
        cursor
            .end(id, Ended { outcome, record })
            .iter()
            .map(plain)
            .collect()
    }

    #[test]
    fn steps_starting_never_move_the_cursor() {
        let mut cursor = filled(3);
        assert_eq!(listed(&cursor), [1, 2, 3]);
        // On the first step to start, and still there however many follow.
        assert_eq!(cursor.on, Some(1));
        cursor.insert(row(4));
        assert_eq!(cursor.on, Some(1));

        // Moved onto the newest running step, it stays on that one too.
        cursor.step(3);
        assert_eq!(cursor.on, Some(4));
        cursor.insert(row(5));
        assert_eq!(cursor.on, Some(4));
        assert_eq!(cursor.position(), Some(3));
    }

    #[test]
    fn the_step_under_the_cursor_finishing_moves_it_to_what_is_running() {
        let mut cursor = filled(3);
        assert_eq!(cursor.on, Some(1));

        // To the newest step still running, not the next one along.
        end(&mut cursor, 1, Outcome::Completed);
        assert_eq!(cursor.on, Some(3));

        // A step finishing elsewhere is none of its business.
        end(&mut cursor, 2, Outcome::Completed);
        assert_eq!(cursor.on, Some(3));
    }

    #[test]
    fn with_nothing_running_the_cursor_waits_for_the_next_step() {
        // One step at a time — how a flow with one licence runs — must not
        // leave the cursor stranded on every step as it finishes.
        let mut cursor = filled(1);
        end(&mut cursor, 1, Outcome::Completed);
        assert_eq!(cursor.on, None);
        // Resting on the last step meanwhile, so there is something to see and
        // to move from.
        assert_eq!(cursor.position(), Some(0));

        cursor.insert(row(2));
        assert_eq!(cursor.on, Some(2));
    }

    #[test]
    fn put_on_a_finished_step_the_cursor_stays_there() {
        // The whole point: a step's log outlives the step, and a failed one is
        // the log most worth reading.
        let mut cursor = filled(3);
        end(&mut cursor, 2, Outcome::Failed);
        cursor.step(1);
        assert_eq!(cursor.on, Some(2));

        // Nothing that happens to the run moves it.
        cursor.insert(row(4));
        end(&mut cursor, 1, Outcome::Completed);
        end(&mut cursor, 3, Outcome::Completed);
        end(&mut cursor, 4, Outcome::Completed);
        assert_eq!(cursor.on, Some(2));
        assert_eq!(&*cursor.selected().unwrap().label, "step 2");
    }

    #[test]
    fn put_on_another_running_step_the_cursor_stays_until_that_step_ends() {
        let mut cursor = filled(3);
        cursor.step(1);
        assert_eq!(cursor.on, Some(2));

        // Something newer starting does not pull it away from a step that is
        // still going.
        cursor.insert(row(4));
        assert_eq!(cursor.on, Some(2));

        // That step ending does: on to the newest step still running, rather
        // than being left behind on what the step turned into.
        end(&mut cursor, 2, Outcome::Failed);
        assert_eq!(cursor.on, Some(4));
    }

    #[test]
    fn the_cursor_moves_down_the_list_and_stops_at_the_ends() {
        let mut cursor = filled(3);
        assert_eq!(cursor.position(), Some(0));

        // The top of the list.
        cursor.step(-1);
        assert_eq!(cursor.position(), Some(0));

        cursor.step(1);
        assert_eq!(cursor.position(), Some(1));
        cursor.step(1);
        assert_eq!(cursor.position(), Some(2));
        // Several rows at once, clamped at the bottom.
        cursor.step(5);
        assert_eq!(cursor.position(), Some(2));
        cursor.step(-1);
        assert_eq!(cursor.position(), Some(1));
    }

    // -- retiring to the record ---------------------------------------------

    #[test]
    fn finished_steps_retire_oldest_first_once_the_list_is_full() {
        let mut cursor = Cursor::new(3);
        for id in 1..=3 {
            assert!(cursor.insert(row(id)).is_empty());
        }
        // Full, but nothing has finished, so nothing can go.
        assert!(cursor.insert(row(4)).is_empty());
        assert_eq!(listed(&cursor), [1, 2, 3, 4]);

        // The first to finish is the first to go, with its line for the record.
        assert_eq!(end(&mut cursor, 2, Outcome::Completed), ["  ✔ step 2"]);
        assert_eq!(listed(&cursor), [1, 3, 4]);

        // Under the limit again, so the next to finish stays as history…
        assert!(end(&mut cursor, 1, Outcome::Completed).is_empty());
        assert_eq!(listed(&cursor), [1, 3, 4]);
        // …until something new pushes it out.
        assert_eq!(
            cursor.insert(row(5)).iter().map(plain).collect::<Vec<_>>(),
            ["  ✔ step 1"]
        );
        assert_eq!(listed(&cursor), [3, 4, 5]);
    }

    #[test]
    fn running_steps_failures_and_the_step_under_the_cursor_never_retire() {
        let mut cursor = Cursor::new(5);
        for id in 1..=3 {
            cursor.insert(row(id));
        }
        // A failure stays however full the list gets: reaching it is what the
        // list is for.
        assert!(end(&mut cursor, 1, Outcome::Failed).is_empty());
        assert!(cursor.insert(row(4)).is_empty());

        // A step finishes while there is room, and the cursor is put on it.
        // (The cursor left step 1 when it failed, for step 3, the newest
        // running; step 2 is one up from there.)
        assert!(end(&mut cursor, 2, Outcome::Completed).is_empty());
        assert_eq!(cursor.on, Some(3));
        cursor.step(-1);
        assert_eq!(cursor.on, Some(2));

        // The list overflows, but only running steps, a failure and the step
        // under the cursor are left in it: nothing can go.
        cursor.insert(row(5));
        assert!(cursor.insert(row(6)).is_empty());
        assert_eq!(listed(&cursor), [1, 2, 3, 4, 5, 6]);

        // Once the cursor has moved on, the step it was on can go — and the
        // failure still cannot.
        cursor.step(4);
        assert_eq!(cursor.on, Some(6));
        let retired: Vec<String> = cursor.insert(row(7)).iter().map(plain).collect();
        assert_eq!(retired, ["  ✔ step 2"]);
        assert_eq!(listed(&cursor), [1, 3, 4, 5, 6, 7]);
    }

    #[test]
    fn anything_else_entering_the_record_lets_finished_steps_in_ahead_of_it() {
        // The record must read in the order things happened, so a note or a
        // failure arriving flushes the steps that finished before it — but not
        // a failure, which is already there, nor the step under the cursor.
        let mut cursor = Cursor::new(10);
        for id in 1..=5 {
            cursor.insert(row(id));
        }
        end(&mut cursor, 1, Outcome::Completed);
        end(&mut cursor, 2, Outcome::Failed);
        end(&mut cursor, 3, Outcome::Completed);
        end(&mut cursor, 4, Outcome::Completed);
        cursor.step(-1);
        assert_eq!(cursor.on, Some(4));

        let flushed: Vec<String> = cursor.flush().iter().map(plain).collect();
        assert_eq!(flushed, ["  ✔ step 1", "  ✔ step 3"]);
        assert_eq!(listed(&cursor), [2, 4, 5]);
    }

    #[test]
    fn what_is_left_in_the_list_at_the_end_goes_to_the_record_in_order() {
        let mut cursor = Cursor::new(10);
        for id in 1..=4 {
            cursor.insert(row(id));
        }
        end(&mut cursor, 3, Outcome::Completed);
        end(&mut cursor, 1, Outcome::Failed);
        end(&mut cursor, 2, Outcome::Completed);

        // In list order, not the order they finished, and without the failure,
        // which went into the record when it happened. Step 4 is still running
        // and has no line yet.
        let drained: Vec<String> = cursor.drain().iter().map(plain).collect();
        assert_eq!(drained, ["  ✔ step 2", "  ✔ step 3"]);
        assert!(cursor.rows.is_empty());
    }

    #[test]
    fn a_finished_step_is_drawn_as_its_line_for_the_record() {
        let mut row = row(1);
        row.ended = Some(Ended {
            outcome: Outcome::Failed,
            record: Line::from("✖ step 1   1m14s  during compare (2/2)"),
        });
        assert_eq!(
            plain(&row.line(true, 8, "⠹")),
            "❯ ✖ step 1   1m14s  during compare (2/2)"
        );
        assert_eq!(
            plain(&row.line(false, 8, "⠹")),
            "  ✖ step 1   1m14s  during compare (2/2)"
        );
    }

    // -- what enter copies --------------------------------------------------

    #[test]
    fn a_step_offers_its_tools_output_and_falls_back_to_its_own_log() {
        let mut row = row(1);
        // Nothing to follow before the step has written anything.
        assert_eq!(row.follow_command(), None);

        row.log = Some(PathBuf::from("/build/decoder/par/decoder par.rivet.log"));
        assert_eq!(
            row.follow_command().as_deref(),
            Some(r"tail -n 100 -F '/build/decoder/par/decoder par.rivet.log'")
        );

        // Once a tool is running, its output is what someone watching wants.
        row.state.lock().unwrap().outputs = vec![
            PathBuf::from("/build/decoder/par/decoder.par.out"),
            PathBuf::from("/build/decoder/par/decoder.par.err"),
        ];
        assert_eq!(
            row.follow_command().as_deref(),
            Some(concat!(
                "tail -n 100 -F /build/decoder/par/decoder.par.out",
                " /build/decoder/par/decoder.par.err"
            ))
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
}
