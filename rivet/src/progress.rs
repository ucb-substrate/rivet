//! Live terminal reporting for flow execution.
//!
//! While a flow is running, every step gets a line: a spinner, its elapsed time
//! and whatever progress it reports (see below) while it runs, and afterwards
//! how it ended — `✔` (executed), `⏭` (skipped because it was pinned), `⊘`
//! (never runnable, because something it depended on failed) or `✖` (failed).
//! A summary bar under them tracks the run as a whole.
//!
//! The lines are a list on a screen of their own, which stays up when the run
//! is over until it is dismissed: see `crate::tui` for the screen, its pages
//! and its keys. What this module decides is what the run looks like — what
//! each line says, which step the cursor is on, what a step has to offer to be
//! read — and it says so through the `Paint` trait defined there.
//!
//! # The cursor
//!
//! One of the steps is under a cursor, which `↑`/`↓` (or `j`/`k`) move between
//! them, finished steps included: the log worth reading is usually one
//! belonging to a step that has already stopped — a step that failed most of
//! all. `enter` opens the step, which is its log as it is written.
//!
//! The cursor stays on the step it is on. The one thing that moves it by itself
//! is that step finishing while it was running, when it goes to the newest step
//! still running, so that someone watching the run keeps watching the run.
//!
//! See [`StepHandle::set_output_files`] for what a step offers to read.
//!
//! When stderr is not a terminal (CI, redirected logs) the display degrades to
//! plain, one-line-per-event logging instead of drawing escape sequences. The
//! same happens when the display is dismissed before the run is over: the
//! record so far is left in the terminal, and everything after is reported
//! plainly.
//!
//! # The display is not a log
//!
//! Two things reach a step's line, and nothing else: [`status`], which a step
//! sets from Rust, and the substep [`banner`]s a tool is told to print, picked
//! out of its output by [`parse_banner`]. Raw tool output goes to the step's
//! log files and stops there — it is not summarised, and not distinguished by
//! stream. A tool writing to stderr means nothing in particular; plenty write
//! all their chatter there. The step's page reads those files back, which is
//! how the output is seen without ever going through the display.
//!
//! # Nothing else may write to the terminal
//!
//! The display owns the screen while a flow runs. Anything that reaches the
//! terminal without going through it — a bare `println!` in flow code, a child
//! process with inherited stdio, a panic message from a thread the executor
//! does not know about — is written over what is drawn there, until the next
//! full redraw (`^L`, or the terminal being resized) paints over it in turn.
//!
//! So while a flow is running, print with [`note`], run subprocesses through
//! [`crate::exec`] so their output is captured to file, and wrap anything that
//! insists on the terminal for itself in [`suspend`].
//!
//! Anything worth recording rather than showing goes through `tracing`, which
//! [`crate::log`] writes to files and never to a stream.

use std::cell::RefCell;
use std::io::IsTerminal;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard, RwLock};
use std::time::{Duration, Instant};

use ratatui::style::{Style, Stylize};
use ratatui::text::{Line, Span};

use crate::tui::{Detail, Motion, Paint, Screen, StepLine, Tui};

/// Longest step label rendered before it is truncated.
const MAX_LABEL_WIDTH: usize = 44;

/// Width of each inline progress bar, in characters.
const BAR_WIDTH: usize = 10;

/// Width of the bar on the summary line.
const SUMMARY_BAR_WIDTH: usize = 24;

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
    /// The step never ran, because a step it depended on failed.
    Blocked,
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
    /// How long the run took, set once every step has ended: the display then
    /// has only to wait to be dismissed, and the clock has stopped.
    ended: Mutex<Option<Duration>>,
    ui: Option<Ui>,
}

/// The live display, when there is one.
struct Ui {
    state: Mutex<UiState>,
    /// The terminal, for as long as the run is drawing on it.
    tui: Mutex<Option<Tui>>,
}

/// What the display shows, under one lock with whether it is showing anything
/// at all: an event either goes on screen or into plain output, never neither
/// and never both, however close to the display being dismissed it arrives.
struct UiState {
    /// The steps, and which of them the cursor is on.
    cursor: Cursor,
    /// Lines that are not steps — notes, warnings — and when they were said,
    /// so the record can put them where they happened.
    notes: Vec<(Instant, Line<'static>)>,
    /// The display has been given up: everything from now on is plain output.
    detached: bool,
}

impl Reporter {
    pub(crate) fn new(total: usize, label_width: usize, progress: bool) -> Arc<Self> {
        let ui = (progress && on_a_terminal()).then(|| Ui {
            state: Mutex::new(UiState {
                cursor: Cursor::new(),
                notes: Vec::new(),
                detached: false,
            }),
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
            ended: Mutex::new(None),
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
        let tui = Tui::start(Arc::downgrade(&paint));
        if tui.is_none() {
            ui.state.lock().unwrap().detached = true;
        }
        *ui.tui.lock().unwrap() = tui;
    }

    /// The display's state, if the display is up — and held, so that whatever
    /// the caller does with it happens while it still is.
    ///
    /// `None` means plain output: there is no display, or it has been
    /// dismissed.
    fn display(&self) -> Option<MutexGuard<'_, UiState>> {
        let ui = self.ui.as_ref()?;
        let state = ui.state.lock().unwrap();
        (!state.detached).then_some(state)
    }

    /// Whether the live display is up, as opposed to plain logging.
    fn drawing(&self) -> bool {
        self.display().is_some()
    }

    /// Announce that `label` has started running, returning a handle the step
    /// can use to report its own output.
    ///
    /// `log` is the step's own log file: both where what it logs is written,
    /// and — until it starts a tool of its own — the file the cursor offers to
    /// read.
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

        match self.display() {
            Some(mut display) => display.cursor.insert(Row {
                id,
                label: Arc::clone(&label),
                started,
                state: Arc::clone(&state),
                log: log.as_ref().map(|log| log.path().to_path_buf()),
                ended: None,
            }),
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
    ///
    /// `log` is where the step's own log would be: it was not written this
    /// time, but the run that last ran the step left one there, and that is
    /// the log there is to read for it.
    pub(crate) fn skip(&self, label: &str, reason: &str, log: Option<PathBuf>) {
        self.skipped.fetch_add(1, Ordering::Relaxed);
        self.finished.fetch_add(1, Ordering::Relaxed);
        let record = Line::from(vec![
            span("⏭ ", Style::new().yellow()),
            span(self.pad(label), Style::new()),
            span("  ", Style::new()),
            span(reason.to_string(), Style::new().yellow()),
        ]);
        self.never_ran(label, log, Outcome::Skipped, record);
    }

    /// Record a step that can never run because something it depends on
    /// failed.
    ///
    /// A failure does not stop the rest of the run — independent steps keep
    /// going and new ones still start — so the steps downstream of it are
    /// named as they are dropped, rather than being left to look forgotten.
    pub(crate) fn block(&self, label: &str, blame: &str, log: Option<PathBuf>) {
        self.blocked.fetch_add(1, Ordering::Relaxed);
        self.finished.fetch_add(1, Ordering::Relaxed);
        let record = Line::from(vec![
            span("⊘ ", Style::new().yellow()),
            span(self.pad(label), Style::new()),
            span("  ", Style::new()),
            span(format!("blocked by {blame}"), Style::new().yellow()),
        ]);
        self.never_ran(label, log, Outcome::Blocked, record);
    }

    /// A step that ended without starting gets a row like any other, already
    /// ended, so that it is under the cursor along with the rest.
    fn never_ran(
        &self,
        label: &str,
        log: Option<PathBuf>,
        outcome: Outcome,
        record: Line<'static>,
    ) {
        match self.display() {
            Some(mut display) => {
                let now = Instant::now();
                let id = self.next_step.fetch_add(1, Ordering::Relaxed);
                display.cursor.insert(Row {
                    id,
                    label: Arc::from(truncate(label, MAX_LABEL_WIDTH)),
                    started: now,
                    state: Arc::new(Mutex::new(StepState::default())),
                    log,
                    ended: Some(Ended {
                        outcome,
                        record,
                        at: now,
                    }),
                });
            }
            None => self.print(indent(record)),
        }
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
            Outcome::Skipped | Outcome::Blocked => Line::from(vec![
                span("⏭ ", Style::new().yellow()),
                span(padded, Style::new()),
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

        match self.display() {
            Some(mut display) => display.cursor.end(
                handle.id,
                Ended {
                    outcome,
                    record,
                    at: Instant::now(),
                },
            ),
            None => self.print(indent(record)),
        }
    }

    /// Say a line that is not a step: on screen under the list while the
    /// display is up, and in the record afterwards, where it happened.
    pub(crate) fn print(&self, line: Line<'static>) {
        match self.display() {
            Some(mut display) => display.notes.push((Instant::now(), line)),
            // Nothing is drawing, so the line is just output. Written rather
            // than printed because `eprintln!` panics if the write fails, and
            // this runs on a worker: a run piped into something that stops
            // reading — `| head`, or `| less` quit early — would otherwise take
            // a step down with it.
            None => write_line(&plain(&line)),
        }
    }

    /// The run is over: wait for the display to be dismissed, then print a
    /// closing summary where the terminal is ordinary again.
    ///
    /// The display stays up for as long as it is wanted, with the run on it to
    /// be looked through. Nothing else is drawing by now, so waiting costs the
    /// run nothing but the time someone spends reading.
    pub(crate) fn finish_all(&self, elapsed: Duration) {
        // Taken out of the lock before it is waited on: `suspend` wants this
        // same lock, and must find nothing rather than wait.
        let tui = self
            .ui
            .as_ref()
            .and_then(|ui| ui.tui.lock().unwrap().take());
        // Asked before the run is marked done. An interrupt that arrived while
        // steps were running is what ended them — killing a tool ends the step
        // that was driving it — and the run must end as interrupted, however
        // quickly they then finished. One arriving after this is only a way of
        // dismissing a finished display.
        let interrupted = tui.as_ref().is_some_and(Tui::interrupted);
        *self.ended.lock().unwrap() = Some(elapsed);
        if let Some(tui) = tui {
            tui.wait();
        }
        if interrupted {
            std::process::exit(crate::tui::INTERRUPTED);
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

    /// How long the run took, once it is over.
    fn ended(&self) -> Option<Duration> {
        *self.ended.lock().unwrap()
    }

    /// How long the run has been going — or took, once it is over, when the
    /// clock stops rather than counting time spent looking at the result.
    fn elapsed(&self) -> Duration {
        self.ended().unwrap_or_else(|| self.started.elapsed())
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
                    fmt_duration(self.elapsed())
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
        if self.ended().is_some() {
            spans.push(span(" · done", Style::new().bold()));
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
}

/// What the display draws, asked for afresh every frame.
impl Paint for Reporter {
    fn screen(&self) -> Screen {
        let Some(display) = self.display() else {
            return Screen::default();
        };

        let spinner = self.spinner();
        let selected = display.cursor.position();
        let steps = display
            .cursor
            .rows
            .iter()
            .enumerate()
            .map(|(index, row)| StepLine {
                id: row.id,
                line: row.line(Some(index) == selected, self.label_width, spinner),
                running: row.ended.is_none(),
            })
            .collect();
        let notes = display.notes.iter().map(|(_, line)| line.clone()).collect();
        drop(display);

        Screen {
            steps,
            selected,
            notes,
            summary: self.summary(),
            done: self.ended().is_some(),
        }
    }

    fn detail(&self, id: usize) -> Option<Detail> {
        let display = self.display()?;
        let row = display.cursor.rows.iter().find(|row| row.id == id)?;
        let (files, follow) = row.files();
        Some(Detail {
            label: row.label.to_string(),
            line: row.line(false, self.label_width, self.spinner()),
            files,
            follow,
            running: row.ended.is_none(),
        })
    }

    fn move_cursor(&self, motion: Motion) {
        if let Some(mut display) = self.display() {
            display.cursor.step(motion);
        }
    }

    fn select(&self, id: usize) {
        if let Some(mut display) = self.display() {
            display.cursor.select(id);
        }
    }

    fn done(&self) -> bool {
        self.ended().is_some()
    }

    fn detach(&self) -> Vec<Line<'static>> {
        let Some(ui) = &self.ui else {
            return Vec::new();
        };
        let mut state = ui.state.lock().unwrap();
        if state.detached {
            return Vec::new();
        }
        state.detached = true;

        // The record: every step that has ended, and every note, in the order
        // things happened — which is the order plain output would have had.
        // Steps still running have no line yet; they will be reported plainly
        // as they end.
        let mut record: Vec<(Instant, Line<'static>)> = state
            .cursor
            .rows
            .iter()
            .filter_map(|row| {
                row.ended
                    .as_ref()
                    .map(|ended| (ended.at, indent(ended.record.clone())))
            })
            .collect();
        record.append(&mut state.notes);
        record.sort_by_key(|(at, _)| *at);
        record.into_iter().map(|(_, line)| line).collect()
    }
}

/// Which step the display's cursor is on.
///
/// The rows are every step so far — running, finished, skipped, blocked — in
/// the order they started, which is the order the record is in as well. They
/// stay for as long as the display does: the log worth reading is usually one
/// belonging to a step that has already stopped, and a failed step that
/// vanished the moment it failed would take its log with it.
///
/// # The cursor stays put
///
/// Steps starting never move it. Only the step it is on finishing does, and
/// then only if that step was running: it goes to the newest step still
/// running, so that someone watching the run keeps watching the run. With
/// nothing running it stays where it is and waits, and the next step to start
/// takes it. Put on a step by hand, running or not, it stays there until it is
/// moved again.
struct Cursor {
    rows: Vec<Row>,
    /// The step the cursor is on. `None` only before the first step starts.
    on: Option<usize>,
    /// The step the cursor was on has finished with nothing else running, so
    /// the next step to start takes it.
    parked: bool,
}

/// A step, as the display sees it.
struct Row {
    id: usize,
    label: Arc<str>,
    started: Instant,
    state: Arc<Mutex<StepState>>,
    /// The step's own log file, if it has one — or, for a step that did not
    /// run this time, where the last run that did left it.
    log: Option<PathBuf>,
    /// How it ended, once it has. `None` while it is still running.
    ended: Option<Ended>,
}

/// How a step that has stopped ended.
struct Ended {
    outcome: Outcome,
    /// Its line for the record, which is also its row in the list.
    record: Line<'static>,
    /// When, so the record can be put in order.
    at: Instant,
}

impl Cursor {
    fn new() -> Self {
        Self {
            rows: Vec::new(),
            on: None,
            parked: false,
        }
    }

    /// Take on a step that has just started — or that has just been found
    /// never to be going to — at the end.
    ///
    /// A step starting never moves the cursor off a step it was put on. It
    /// takes the cursor only when the cursor has been waiting for something to
    /// run: before the first step, or after the step it was on finished with
    /// nothing else going.
    fn insert(&mut self, row: Row) {
        if row.ended.is_none() && (self.on.is_none() || self.parked) {
            self.on = Some(row.id);
            self.parked = false;
        }
        self.rows.push(row);
    }

    /// Record how a step ended.
    ///
    /// This is the one thing that moves the cursor on its own: the step it was
    /// on has stopped, so it goes to what is running now — the newest such step
    /// — rather than being left on what the step turned into. That row is still
    /// there, a key or two up. With nothing running it stays and waits for the
    /// next step to start.
    fn end(&mut self, id: usize, ended: Ended) {
        if let Some(row) = self.rows.iter_mut().find(|row| row.id == id) {
            row.ended = Some(ended);
        }
        if self.on == Some(id) {
            match self.newest_running() {
                Some(index) => self.on = Some(self.rows[index].id),
                None => self.parked = true,
            }
        }
    }

    /// Move the cursor through the list, stopping at the ends.
    ///
    /// Deliberately not wrapping: the list is history as well as what is
    /// running, and a cursor that jumped from one end to the other would lose
    /// someone's place in it.
    fn step(&mut self, motion: Motion) {
        let Some(position) = self.position() else {
            return;
        };
        let last = self.rows.len() - 1;
        let moved = match motion {
            Motion::Up(by) => position.saturating_sub(by),
            Motion::Down(by) => position.saturating_add(by).min(last),
            Motion::First => 0,
            Motion::Last => last,
        };
        self.select(self.rows[moved].id);
    }

    /// Put the cursor on a step by hand, where it then stays.
    fn select(&mut self, id: usize) {
        if self.rows.iter().any(|row| row.id == id) {
            self.on = Some(id);
            self.parked = false;
        }
    }

    /// Where the cursor is in the list.
    fn position(&self) -> Option<usize> {
        match self.on {
            Some(id) => self.rows.iter().position(|row| row.id == id),
            None => self.rows.len().checked_sub(1),
        }
    }

    fn newest_running(&self) -> Option<usize> {
        self.rows.iter().rposition(|row| row.ended.is_none())
    }

    #[cfg(test)]
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
            // different things. In full colour, however it ended: a finished
            // step is as much there to be opened as a running one, and greying
            // it out would say otherwise.
            Some(ended) => {
                let mut spans = vec![cursor];
                spans.extend(ended.record.spans.iter().cloned());
                Line::from(spans)
            }
            None => {
                let label = format!("{:<width$}", truncate(&self.label, MAX_LABEL_WIDTH));
                self.running_line(cursor, label, spinner)
            }
        }
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

    /// The files this step has to read, and the ones to follow.
    ///
    /// Everything first, most useful first: what the tool running now is
    /// writing, then what earlier tools wrote, then the step's own log. Then
    /// what someone following the step wants: the output of the tool the step
    /// is driving, which is what the display deliberately never shows, or the
    /// step's own log until a tool has started.
    fn files(&self) -> (Vec<PathBuf>, Vec<PathBuf>) {
        let state = self.state.lock().unwrap();
        let mut files: Vec<PathBuf> = state.outputs.clone();
        for file in state.history.iter().chain(self.log.iter()) {
            if !files.contains(file) {
                files.push(file.clone());
            }
        }
        let follow = if state.outputs.is_empty() {
            self.log.iter().cloned().collect()
        } else {
            state.outputs.clone()
        };
        (files, follow)
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
    /// the step's page reads, and what a copied command follows.
    outputs: Vec<PathBuf>,
    /// Every file the step has ever said it was writing, in the order they
    /// were first named, so that an earlier tool's output can still be read
    /// once a later tool has replaced it as what the step is writing.
    history: Vec<PathBuf>,
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
    /// Not shown on the step's line — raw tool output never is — but offered to
    /// whoever is watching: they are what the step's page reads, and what the
    /// display copies a command to follow. [`crate::exec`] calls this for the
    /// commands it runs; a step driving a tool some other way should call it
    /// itself.
    pub fn set_output_files(&self, files: Vec<PathBuf>) {
        let mut state = self.state.lock().unwrap();
        for file in &files {
            if !state.history.contains(file) {
                state.history.push(file.clone());
            }
        }
        state.outputs = files;
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

/// Say something that is not a step: on screen under the list while the live
/// display is up, in the run's record afterwards, or straight to stderr if no
/// flow is running.
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
/// Only stderr has to be one: that is the stream the display draws on, and the
/// keys and the terminal's size come from the controlling terminal itself. A
/// run whose stdout is redirected still gets the display — and anything flow
/// code prints to stdout lands in the file rather than over the screen.
fn on_a_terminal() -> bool {
    std::io::stderr().is_terminal()
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
pub(crate) fn indent(line: Line<'static>) -> Line<'static> {
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
/// Escape sequences and control characters are dropped, colour included: the
/// display owns its own styling, and a line is drawn a cell at a time, so a
/// kept sequence would be printed rather than obeyed. The step's page runs its
/// log through this too, for the same reason.
pub(crate) fn clean(line: &str) -> String {
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
        Reporter::new(1, 8, false)
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
        let reporter = Reporter::new(7, 8, false);
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

    /// A cursor over `count` running steps.
    fn filled(count: usize) -> Cursor {
        let mut cursor = Cursor::new();
        for id in 1..=count {
            cursor.insert(row(id));
        }
        cursor
    }

    fn end(cursor: &mut Cursor, id: usize, outcome: Outcome) {
        let glyph = match outcome {
            Outcome::Completed => "✔",
            Outcome::Skipped => "⏭",
            Outcome::Blocked => "⊘",
            Outcome::Failed => "✖",
        };
        cursor.end(
            id,
            Ended {
                outcome,
                record: Line::from(format!("{glyph} step {id}")),
                at: Instant::now(),
            },
        );
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
        cursor.step(Motion::Down(3));
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
    fn with_nothing_running_the_cursor_waits_where_it_is_for_the_next_step() {
        // One step at a time — how a flow with one licence runs — must not
        // leave the cursor stranded on every step as it finishes. Nor may it
        // jump about: it stays on the step that finished, so there is something
        // to see and to move from, until something starts.
        let mut cursor = filled(1);
        end(&mut cursor, 1, Outcome::Completed);
        assert_eq!(cursor.on, Some(1));
        assert!(cursor.parked);
        assert_eq!(cursor.position(), Some(0));

        cursor.insert(row(2));
        assert_eq!(cursor.on, Some(2));
        assert!(!cursor.parked);

        // A step that never ran arriving is not something to watch.
        end(&mut cursor, 2, Outcome::Completed);
        let mut skipped = row(3);
        skipped.ended = Some(Ended {
            outcome: Outcome::Skipped,
            record: Line::from("⏭ step 3"),
            at: Instant::now(),
        });
        cursor.insert(skipped);
        assert_eq!(cursor.on, Some(2));
        cursor.insert(row(4));
        assert_eq!(cursor.on, Some(4));
    }

    #[test]
    fn put_on_a_finished_step_the_cursor_stays_there() {
        // The whole point: a step's log outlives the step, and a failed one is
        // the log most worth reading.
        let mut cursor = filled(3);
        end(&mut cursor, 2, Outcome::Failed);
        cursor.step(Motion::Down(1));
        assert_eq!(cursor.on, Some(2));

        // Nothing that happens to the run moves it — not even everything else
        // finishing and something new starting.
        cursor.insert(row(4));
        end(&mut cursor, 1, Outcome::Completed);
        end(&mut cursor, 3, Outcome::Completed);
        end(&mut cursor, 4, Outcome::Completed);
        assert_eq!(cursor.on, Some(2));
        cursor.insert(row(5));
        assert_eq!(cursor.on, Some(2));
        assert_eq!(&*cursor.selected().unwrap().label, "step 2");
    }

    #[test]
    fn put_on_another_running_step_the_cursor_stays_until_that_step_ends() {
        let mut cursor = filled(3);
        cursor.step(Motion::Down(1));
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
    fn the_cursor_moves_through_the_list_and_stops_at_the_ends() {
        let mut cursor = filled(5);
        assert_eq!(cursor.position(), Some(0));

        // The top of the list.
        cursor.step(Motion::Up(1));
        assert_eq!(cursor.position(), Some(0));

        cursor.step(Motion::Down(1));
        assert_eq!(cursor.position(), Some(1));
        // Several rows at once, clamped at the bottom.
        cursor.step(Motion::Down(10));
        assert_eq!(cursor.position(), Some(4));
        cursor.step(Motion::Up(2));
        assert_eq!(cursor.position(), Some(2));
        cursor.step(Motion::First);
        assert_eq!(cursor.position(), Some(0));
        cursor.step(Motion::Last);
        assert_eq!(cursor.position(), Some(4));
    }

    #[test]
    fn selecting_a_step_by_hand_puts_the_cursor_there_for_good() {
        let mut cursor = filled(2);
        end(&mut cursor, 1, Outcome::Completed);
        end(&mut cursor, 2, Outcome::Completed);
        assert!(cursor.parked);

        // Coming back from a step's page puts the cursor on that step, and
        // unparks it: the next step to start must not snatch it away.
        cursor.select(1);
        assert_eq!(cursor.on, Some(1));
        cursor.insert(row(3));
        assert_eq!(cursor.on, Some(1));

        // An id that is not in the list is ignored.
        cursor.select(99);
        assert_eq!(cursor.on, Some(1));
    }

    #[test]
    fn every_step_stays_in_the_list_however_it_ended() {
        let mut cursor = filled(2);
        end(&mut cursor, 1, Outcome::Completed);
        end(&mut cursor, 2, Outcome::Failed);
        for id in 3..=20 {
            cursor.insert(row(id));
            end(&mut cursor, id, Outcome::Completed);
        }
        assert_eq!(cursor.rows.len(), 20);
        assert_eq!(listed(&cursor)[..3], [1, 2, 3]);
    }

    // -- what is drawn ------------------------------------------------------

    #[test]
    fn a_finished_step_is_drawn_as_its_line_for_the_record() {
        let mut row = row(1);
        row.ended = Some(Ended {
            outcome: Outcome::Failed,
            record: Line::from("✖ step 1   1m14s  during compare (2/2)"),
            at: Instant::now(),
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

    #[test]
    fn finished_steps_keep_their_colour() {
        use ratatui::style::Modifier;

        // As each cell ends up: the line's style, with the span's over it.
        let dimmed = |row: &Row| {
            let line = row.line(false, 8, "⠹");
            line.spans.iter().any(|span| {
                line.style
                    .patch(span.style)
                    .add_modifier
                    .contains(Modifier::DIM)
            })
        };
        for outcome in [
            Outcome::Completed,
            Outcome::Skipped,
            Outcome::Blocked,
            Outcome::Failed,
        ] {
            let mut row = row(1);
            row.ended = Some(Ended {
                outcome,
                record: Line::from(vec![
                    Span::styled("✔ ", Style::new().green()),
                    Span::styled("step 1", Style::new().bold()),
                ]),
                at: Instant::now(),
            });
            assert!(!dimmed(&row), "{outcome:?} should keep its colour");
        }
        assert!(!dimmed(&row(3)));
    }

    // -- what a step has to read --------------------------------------------

    #[test]
    fn a_step_offers_its_tools_output_and_falls_back_to_its_own_log() {
        let mut row = row(1);
        // Nothing to read before the step has written anything.
        assert_eq!(row.files(), (vec![], vec![]));

        let own = PathBuf::from("/build/decoder/par/decoder par.rivet.log");
        row.log = Some(own.clone());
        assert_eq!(row.files(), (vec![own.clone()], vec![own.clone()]));

        // Once a tool is running, its output is what someone watching wants,
        // to read and to follow both; the step's own log is still there to read.
        let out = PathBuf::from("/build/decoder/par/decoder.par.out");
        let err = PathBuf::from("/build/decoder/par/decoder.par.err");
        let handle = StepHandle {
            id: 1,
            label: Arc::clone(&row.label),
            reporter: reporter(),
            started: row.started,
            state: Arc::clone(&row.state),
            log: None,
        };
        handle.set_output_files(vec![out.clone(), err.clone()]);
        assert_eq!(
            row.files(),
            (
                vec![out.clone(), err.clone(), own.clone()],
                vec![out.clone(), err.clone()]
            )
        );

        // A second tool: its files first, the first tool's still there to read,
        // and only the second's to follow.
        let lvs = PathBuf::from("/build/decoder/par/decoder.lvs.out");
        handle.set_output_files(vec![lvs.clone()]);
        assert_eq!(row.files(), (vec![lvs.clone(), out, err, own], vec![lvs]));
    }
}
