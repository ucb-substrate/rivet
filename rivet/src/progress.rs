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
//! # The list
//!
//! Every step in the run has a line from the start, in the order the run is
//! expected to take them: a step below everything it waits for, and beside the
//! steps that will be running when it is. A step still to come is greyed and
//! names what it is waiting for; one that has started has a spinner; one that
//! has stopped says how it went.
//!
//! The order never changes. A step becomes what it becomes without moving, and
//! without moving anything else, so that a run can be watched by looking at
//! the same row — or left, and come back to. See [`plan_order`].
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
//! When stderr is not a terminal (CI, redirected logs), or the display is
//! turned off ([`ExecuteConfig::progress`](crate::ExecuteConfig::progress)),
//! the run reports plainly instead, one line per event.
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
use std::thread;
use std::time::{Duration, Instant};

use ratatui::style::{Style, Stylize};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthChar;

use crate::tui::{About, Detail, Motion, Paint, Screen, StepLine, Tui};

/// Longest step label rendered before it is truncated.
const MAX_LABEL_WIDTH: usize = 44;

/// Width of each inline progress bar, in characters.
const BAR_WIDTH: usize = 10;

/// Width of the bar on the summary line.
const SUMMARY_BAR_WIDTH: usize = 24;

/// Narrowest the summary's bar is squeezed to before the counts after it are
/// cut instead.
const MIN_SUMMARY_BAR_WIDTH: usize = 8;

/// Narrowest the label column is squeezed to, on a terminal too narrow for the
/// longest label and a step's progress side by side.
const MIN_LABEL_WIDTH: usize = 12;

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

/// How long a tool told to stop is given before it is made to.
///
/// `SIGTERM` first, because a tool asked to stop may have something to close
/// or write out. `SIGKILL` after this, because it may equally sit in a signal
/// handler and never stop at all — see `cadence::kill_on_fatal_signal` for
/// what Cadence tools do with a fatal signal — and a step that was killed and
/// did not die would be worse than one that was never killed.
const KILL_AFTER: Duration = Duration::from_secs(5);

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

/// What came of asking for a step to be killed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kill {
    /// The tool has been told to stop, and will be made to if it does not.
    Sent,
    /// The step is running, but not a tool of its own: there is nothing to
    /// signal, and its own thread cannot be stopped from outside.
    NoTool,
    /// The step is not running: it is finished, or has not started.
    NotRunning,
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

/// A step as the executor plans it, before anything has happened to it.
///
/// The display is told about every step up front, so that the ones still to
/// come can be shown waiting — and so that a step is the same row from the
/// moment the run starts to the moment the display is dismissed, whatever
/// happens to it in between. Steps are referred to by their index in the plan.
pub(crate) struct Planned {
    pub label: String,
    pub pinned: bool,
    /// The steps this one waits for, by index.
    pub deps: Vec<usize>,
    /// Where the step's own log file is, or would be: for a step that does not
    /// run this time, where the last run that did left it.
    pub log: Option<PathBuf>,
}

/// One step, for as long as the run lasts.
struct Entry {
    label: Arc<str>,
    /// What the step's line is showing, shared with the handle the step
    /// reports through once it is running.
    state: Arc<Mutex<StepState>>,
}

/// Renders the state of a run to the terminal.
pub(crate) struct Reporter {
    steps: Vec<Entry>,
    /// The run, for the banner.
    about: About,
    label_width: usize,
    started: Instant,
    finished: AtomicUsize,
    running: AtomicUsize,
    skipped: AtomicUsize,
    blocked: AtomicUsize,
    failed: AtomicUsize,
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
    /// The display has been given up: everything from now on is plain output.
    detached: bool,
}

impl Reporter {
    /// `workers` and `log_dir` are for the banner: how many steps can run at
    /// once, and where `rivet.log` is going, if anywhere.
    pub(crate) fn new(
        plan: Vec<Planned>,
        workers: usize,
        log_dir: Option<PathBuf>,
        progress: bool,
    ) -> Arc<Self> {
        // The targets are the steps nothing else waits on: what the run was
        // asked for, in the order it was asked.
        let mut waited_for = vec![false; plan.len()];
        for step in &plan {
            for &dep in &step.deps {
                if let Some(waited) = waited_for.get_mut(dep) {
                    *waited = true;
                }
            }
        }
        let about = About {
            targets: plan
                .iter()
                .zip(&waited_for)
                .filter(|(_, &waited)| !waited)
                .map(|(step, _)| truncate(&step.label, MAX_LABEL_WIDTH))
                .collect(),
            steps: plan.len(),
            workers,
            log_dir,
        };
        let label_width = plan
            .iter()
            .map(|step| step.label.chars().count())
            .max()
            .unwrap_or(0)
            .min(MAX_LABEL_WIDTH);
        let steps: Vec<Entry> = plan
            .iter()
            .map(|step| Entry {
                label: Arc::from(truncate(&step.label, MAX_LABEL_WIDTH)),
                state: Arc::new(Mutex::new(StepState::default())),
            })
            .collect();

        let ui = (progress && on_a_terminal()).then(|| {
            // Every step gets its row now. A pinned step is already over: it is
            // being taken as up to date, and its line says so from the start.
            let now = Instant::now();
            let ranks = plan_order(&plan);
            let mut cursor = Cursor::new();
            for (id, (step, entry)) in plan.iter().zip(&steps).enumerate() {
                cursor.insert(Row {
                    id,
                    label: Arc::clone(&entry.label),
                    rank: ranks[id],
                    pinned: step.pinned,
                    deps: step.deps.clone(),
                    log: step.log.clone(),
                    started: None,
                    state: Arc::clone(&entry.state),
                    ended: step.pinned.then(|| Ended {
                        record: pinned_record(&step.label, label_width),
                        at: now,
                    }),
                });
            }
            Ui {
                state: Mutex::new(UiState {
                    cursor,
                    detached: false,
                }),
                tui: Mutex::new(None),
            }
        });

        let reporter = Arc::new(Self {
            steps,
            about,
            label_width,
            started: Instant::now(),
            finished: AtomicUsize::new(0),
            running: AtomicUsize::new(0),
            skipped: AtomicUsize::new(0),
            blocked: AtomicUsize::new(0),
            failed: AtomicUsize::new(0),
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

    /// Stop the tool step `id` is running, leaving the rest of the run going.
    ///
    /// What dies is the process the step spawned, not the step: its own thread
    /// goes on, sees the tool exit, and ends the step however it ends a tool
    /// that failed — which is what makes this safe to do from the display. The
    /// steps waiting on it are blocked from then on, as they would be by any
    /// other failure.
    ///
    /// Nothing here waits: the tool is signalled, and the step ends when it
    /// notices.
    pub(crate) fn kill(&self, id: usize) -> Kill {
        let Some(entry) = self.steps.get(id) else {
            return Kill::NotRunning;
        };
        let running = self
            .display()
            .map(|display| {
                display
                    .cursor
                    .rows
                    .iter()
                    .any(|row| row.id == id && row.group() == Group::Running)
            })
            .unwrap_or(true);
        if !running {
            return Kill::NotRunning;
        }

        let child = {
            let mut state = entry.state.lock().unwrap();
            let Some(child) = state.child else {
                return Kill::NoTool;
            };
            state.killed = true;
            child
        };

        tracing::warn!(step = %entry.label, pid = child.pid, "killing the step's tool");
        crate::tui::signals::signal_process(child.pid, false);

        // Asked, then made to: on a worker of its own, because the display's
        // thread has a screen to keep drawing and the run has steps to get on
        // with.
        let state = Arc::clone(&entry.state);
        let label = Arc::clone(&entry.label);
        thread::spawn(move || {
            thread::sleep(KILL_AFTER);
            // Only if it is still the same process. The step may have ended by
            // now, and may have started another tool since.
            if state.lock().unwrap().child != Some(child) {
                return;
            }
            tracing::warn!(step = %label, pid = child.pid, "the tool did not stop; killing it");
            crate::tui::signals::signal_process(child.pid, true);
        });
        Kill::Sent
    }

    /// Announce that step `id` has started running, returning a handle the
    /// step can use to report its own output.
    ///
    /// `log` is the step's own log file: both where what it logs is written,
    /// and — until it starts a tool of its own — the file the cursor offers to
    /// read.
    pub(crate) fn start(
        self: &Arc<Self>,
        id: usize,
        log: Option<Arc<crate::log::LogFile>>,
    ) -> StepHandle {
        self.running.fetch_add(1, Ordering::Relaxed);
        let entry = &self.steps[id];
        let started = Instant::now();

        match self.display() {
            Some(mut display) => display.cursor.start(
                id,
                started,
                log.as_ref().map(|log| log.path().to_path_buf()),
            ),
            None => self.print(Line::from(vec![
                span("  ▶ ", Style::new().cyan()),
                span(entry.label.to_string(), Style::new()),
            ])),
        }

        StepHandle {
            id,
            label: Arc::clone(&entry.label),
            reporter: Arc::clone(self),
            started,
            state: Arc::clone(&entry.state),
            log,
        }
    }

    /// Record that step `id`, which is pinned, is not being run.
    ///
    /// Its row has said as much since the run started; this is the moment the
    /// run gets to it, when it counts.
    pub(crate) fn skip(&self, id: usize) {
        self.skipped.fetch_add(1, Ordering::Relaxed);
        self.finished.fetch_add(1, Ordering::Relaxed);
        let record = pinned_record(&self.steps[id].label, self.label_width);
        match self.display() {
            Some(mut display) => {
                if !display.cursor.has_ended(id) {
                    display.cursor.end(
                        id,
                        Ended {
                            record,
                            at: Instant::now(),
                        },
                    );
                }
            }
            None => self.print(indent(record)),
        }
    }

    /// Record that step `id` can never run because something it depends on
    /// failed.
    ///
    /// A failure does not stop the rest of the run — independent steps keep
    /// going and new ones still start — so the steps downstream of it are
    /// named as they are dropped, rather than being left to look forgotten.
    pub(crate) fn block(&self, id: usize, blame: &str) {
        self.blocked.fetch_add(1, Ordering::Relaxed);
        self.finished.fetch_add(1, Ordering::Relaxed);
        let record = Line::from(vec![
            span("⊘ ", Style::new().yellow()),
            span(pad(&self.steps[id].label, self.label_width), Style::new()),
            span("  ", Style::new()),
            span(format!("blocked by {blame}"), Style::new().yellow()),
        ]);
        match self.display() {
            Some(mut display) => display.cursor.end(
                id,
                Ended {
                    record,
                    at: Instant::now(),
                },
            ),
            None => self.print(indent(record)),
        }
    }

    /// Record the end of a step started with [`Reporter::start`].
    pub(crate) fn finish(&self, handle: &StepHandle, outcome: Outcome, detail: Option<&str>) {
        self.running.fetch_sub(1, Ordering::Relaxed);
        self.finished.fetch_add(1, Ordering::Relaxed);
        if outcome == Outcome::Failed {
            self.failed.fetch_add(1, Ordering::Relaxed);
        }
        let record = self.record(handle, outcome, detail);

        match self.display() {
            Some(mut display) => display.cursor.end(
                handle.id,
                Ended {
                    record,
                    at: Instant::now(),
                },
            ),
            None => self.print(indent(record)),
        }
    }

    /// The step's line for the record, which is also its row in the list once
    /// it has stopped.
    ///
    /// Built without the record's indent, so that the cursor can go where the
    /// indent goes.
    fn record(&self, handle: &StepHandle, outcome: Outcome, detail: Option<&str>) -> Line<'static> {
        let elapsed = fmt_duration(handle.started.elapsed());
        let padded = pad(&handle.label, self.label_width);
        match outcome {
            Outcome::Completed => Line::from(vec![
                span("✔ ", Style::new().green()),
                span(padded, Style::new().bold()),
                span(format!("  {elapsed}"), Style::new().dim()),
            ]),
            Outcome::Skipped => Line::from(vec![
                span("⏭ ", Style::new().yellow()),
                span(padded, Style::new()),
                span("  ", Style::new()),
                span(
                    detail.unwrap_or("skipped").to_string(),
                    Style::new().yellow(),
                ),
            ]),
            Outcome::Failed => {
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
                // A step someone killed did not fail of its own accord, and
                // whatever the tool said on its way out is not the reason it
                // stopped. That is in the log; the line says who to blame.
                if handle.was_killed() {
                    spans.push(span("  killed", Style::new().yellow()));
                } else if let Some(detail) = detail {
                    spans.push(span(
                        format!("  {}", truncate(&clean(detail), 160)),
                        Style::new().red(),
                    ));
                }
                Line::from(spans)
            }
        }
    }

    /// Say a line that is not a step, when there is anywhere to say it.
    ///
    /// Plain output only. While the display is up it owns the terminal and the
    /// line is dropped: everything said this way is logged as well, and the
    /// log is a file the display can be asked to show, which a line printed
    /// once and left on screen could never keep up with.
    pub(crate) fn print(&self, line: Line<'static>) {
        if self.drawing() {
            return;
        }
        // Written rather than printed because `eprintln!` panics if the write
        // fails, and this runs on a worker: a run piped into something that
        // stops reading — `| head`, or `| less` quit early — would otherwise
        // take a step down with it.
        write_line(&plain(&line));
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
        let warnings = crate::log::warnings();
        if warnings > 0 {
            spans.push(span(
                format!(
                    " · {warnings} warning{}",
                    if warnings == 1 { "" } else { "s" }
                ),
                Style::new().yellow(),
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

    /// The bar and counts under the steps, for a terminal `width` columns wide.
    ///
    /// The bar gives way before the counts do: on a terminal too narrow for
    /// both it is squeezed, down to a limit, and only then are the counts cut.
    /// A terminal wide enough for everything gets the full bar.
    fn summary(&self, width: usize) -> Line<'static> {
        self.summary_of(width, crate::log::warnings())
    }

    /// The summary, given how many warnings there are to mention: taken apart
    /// from [`Reporter::summary`] because the count is process-wide and a test
    /// should not have to arrange one.
    fn summary_of(&self, width: usize, warnings: usize) -> Line<'static> {
        let counts = self.counts();
        let mut spans = vec![span(
            format!(
                " {}/{} steps · {}",
                counts.finished,
                self.steps.len(),
                fmt_duration(self.elapsed())
            ),
            Style::new(),
        )];
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
        // A count rather than the warnings themselves: what they said is in
        // the log, which `L` opens, and a count cannot go stale the way a line
        // of it repeated here would.
        if warnings > 0 {
            spans.push(span(format!(" · ⚠ {warnings}"), Style::new().yellow()));
        }
        if self.ended().is_some() {
            spans.push(span(" · done", Style::new().bold()));
        }

        let text: usize = spans.iter().map(Span::width).sum();
        let bar = width
            .saturating_sub(2 + text)
            .clamp(MIN_SUMMARY_BAR_WIDTH, SUMMARY_BAR_WIDTH);
        let (done, todo) = bar_parts(counts.finished, self.steps.len(), bar);
        let mut line = vec![
            span("  ", Style::new()),
            // What is done and what is left are different colours, as they were
            // when this bar was indicatif's `{bar:24.green/blue}`.
            span(done, Style::new().green()),
            span(todo, Style::new().blue()),
        ];
        line.append(&mut spans);
        Line::from(line)
    }

    /// How wide the label column is on a terminal `width` columns wide.
    ///
    /// As wide as the longest label, so that the columns after it line up —
    /// unless that leaves a running step's line no room even with its bars
    /// gone, when the column is squeezed by however much is missing, down to a
    /// limit. Every label is cut to the column, so the columns still line up.
    fn label_width_for(&self, rows: &[Row], width: usize, spinner: &str) -> usize {
        let full = self.label_width;
        let overflow = rows
            .iter()
            .filter(|row| row.group() == Group::Running)
            .map(|row| row.bare_width(full, spinner))
            .max()
            .unwrap_or(0)
            .saturating_sub(width);
        if overflow == 0 {
            full
        } else {
            full.saturating_sub(overflow).max(MIN_LABEL_WIDTH.min(full))
        }
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
    fn screen(&self, width: usize) -> Screen {
        let Some(display) = self.display() else {
            return Screen::default();
        };

        let spinner = self.spinner();
        let selected = display.cursor.position();
        let label_width = self.label_width_for(&display.cursor.rows, width, spinner);
        let steps = display
            .cursor
            .rows
            .iter()
            .enumerate()
            .map(|(index, row)| StepLine {
                id: row.id,
                line: row.fit(
                    Some(index) == selected,
                    label_width,
                    spinner,
                    width,
                    &display.cursor.waiting_on(row),
                ),
                running: row.ended.is_none(),
            })
            .collect();
        drop(display);

        Screen {
            about: self.about.clone(),
            steps,
            selected,
            summary: self.summary(width),
            done: self.ended().is_some(),
        }
    }

    fn detail(&self, id: usize, width: usize) -> Option<Detail> {
        let display = self.display()?;
        let row = display.cursor.rows.iter().find(|row| row.id == id)?;
        let (files, follow) = row.files();
        let spinner = self.spinner();
        let label_width = self.label_width_for(&display.cursor.rows, width, spinner);
        Some(Detail {
            label: row.label.to_string(),
            line: row.fit(
                false,
                label_width,
                spinner,
                width,
                &display.cursor.waiting_on(row),
            ),
            files,
            follow,
            running: row.group() == Group::Running,
            pending: row.group() == Group::Pending,
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

    fn kill(&self, id: usize) -> Kill {
        Reporter::kill(self, id)
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

        // The record: how each step that has ended went, in the order they
        // ended, which is the order plain output would have had. A summary of
        // the run and not a replay of it — what was logged along the way is in
        // `rivet.log`, and putting it all back on the terminal would bury the
        // one thing someone wants from a display that has just come down.
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
        record.sort_by_key(|(at, _)| *at);
        record.into_iter().map(|(_, line)| line).collect()
    }
}

/// The record's line for a pinned step.
fn pinned_record(label: &str, width: usize) -> Line<'static> {
    Line::from(vec![
        span("⏭ ", Style::new().yellow()),
        span(pad(label, width), Style::new()),
        span("  ", Style::new()),
        span("pinned", Style::new().yellow()),
    ])
}

/// A label padded to the label column.
fn pad(label: &str, width: usize) -> String {
    let label = truncate(label, MAX_LABEL_WIDTH);
    format!("{label:<width$}")
}

/// Where each step comes in the order the run is expected to take, as a rank
/// per step.
///
/// By how deep into the run a step can start: a step is one deeper than the
/// deepest thing it waits for, and everything at a depth could run at once. A
/// walk of the graph from each target would put a step above things it waits
/// for — never above its own, but above the ones another target waits for, so
/// a hierarchical flow lists the whole of the top block before any of the
/// child block whose output it is waiting on. Depth interleaves them the way
/// the run will.
///
/// Steps at the same depth keep the order the walk found them in, which keeps
/// each target's own steps together.
///
/// Worked out once, from the plan. A list that is watched for hours should not
/// rearrange itself while it is being watched, so nothing here depends on how
/// the run is going.
fn plan_order(plan: &[Planned]) -> Vec<usize> {
    let found = found_order(plan);
    let depths = depths(plan);
    let mut order: Vec<usize> = (0..plan.len()).collect();
    order.sort_by_key(|&id| (depths[id], found[id]));

    let mut ranks = vec![0; plan.len()];
    for (rank, id) in order.into_iter().enumerate() {
        ranks[id] = rank;
    }
    ranks
}

/// How deep into the run each step is: one more than the deepest step it waits
/// for, and zero for a step that waits for nothing.
///
/// A step in a cycle waits, however indirectly, for itself, and has no depth to
/// find. The walk stops rather than recurring, so every step still gets one.
fn depths(plan: &[Planned]) -> Vec<usize> {
    #[derive(Clone, Copy, PartialEq)]
    enum State {
        Unseen,
        Waiting,
        Placed,
    }

    fn depth(plan: &[Planned], id: usize, state: &mut [State], depths: &mut [usize]) -> usize {
        match state[id] {
            State::Placed => return depths[id],
            State::Waiting => return 0,
            State::Unseen => {}
        }
        state[id] = State::Waiting;
        let mut deepest = 0;
        for index in 0..plan[id].deps.len() {
            let dep = plan[id].deps[index];
            if dep < plan.len() && dep != id {
                deepest = deepest.max(depth(plan, dep, state, depths) + 1);
            }
        }
        depths[id] = deepest;
        state[id] = State::Placed;
        deepest
    }

    let mut state = vec![State::Unseen; plan.len()];
    let mut depths = vec![0; plan.len()];
    for id in 0..plan.len() {
        depth(plan, id, &mut state, &mut depths);
    }
    depths
}

/// Where a walk of the graph from the targets reaches each step, which is what
/// tells apart two steps that could start at the same moment.
fn found_order(plan: &[Planned]) -> Vec<usize> {
    fn visit(plan: &[Planned], id: usize, seen: &mut [bool], order: &mut Vec<usize>) {
        if std::mem::replace(&mut seen[id], true) {
            return;
        }
        for &dep in &plan[id].deps {
            if dep < plan.len() {
                visit(plan, dep, seen, order);
            }
        }
        order.push(id);
    }

    let mut waited_for = vec![false; plan.len()];
    for step in plan {
        for &dep in &step.deps {
            if let Some(waited) = waited_for.get_mut(dep) {
                *waited = true;
            }
        }
    }
    let mut seen = vec![false; plan.len()];
    let mut order = Vec::with_capacity(plan.len());
    // The targets first, so that each target's own steps are found together;
    // then anything a cycle kept out of reach.
    for id in (0..plan.len()).filter(|&id| !waited_for[id]) {
        visit(plan, id, &mut seen, &mut order);
    }
    for id in 0..plan.len() {
        visit(plan, id, &mut seen, &mut order);
    }

    let mut found = vec![0; plan.len()];
    for (at, id) in order.into_iter().enumerate() {
        found[id] = at;
    }
    found
}

/// Which step the display's cursor is on.
///
/// The rows are every step in the run, in the order the run is expected to
/// take them — see [`plan_order`] — and they stay in it. A step changes as it
/// goes, from waiting to running to however it ended, but it changes where it
/// is: nothing is moved to a different part of the list for having started or
/// stopped, and nothing else shifts to make room.
///
/// That is worth more than collecting the finished work in one place. A run
/// watched for hours is watched by looking at the same row, or by leaving the
/// cursor on a step and coming back to it; a list that rearranged itself under
/// that every time a step ended would be a list nobody could keep their place
/// in. Steps that run at the same time are neighbours anyway, being at the same
/// depth in the plan, so what is running is not scattered.
///
/// Every step stays for as long as the display does: the log worth reading is
/// usually one belonging to a step that has already stopped, and a failed step
/// that vanished the moment it failed would take its log with it.
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
    /// Where the step comes in the plan; see [`plan_order`].
    rank: usize,
    pinned: bool,
    /// The steps this one waits for, by id.
    deps: Vec<usize>,
    /// The step's own log file, if it has one — or, for a step that has not
    /// run this time, where the last run that did left it.
    log: Option<PathBuf>,
    /// When it started running. `None` while it waits.
    started: Option<Instant>,
    state: Arc<Mutex<StepState>>,
    /// How it ended, once it has. `None` until then.
    ended: Option<Ended>,
}

/// How a step that has stopped ended.
struct Ended {
    /// Its line for the record, which is also its row in the list.
    record: Line<'static>,
    /// When, so the record can be put in order.
    at: Instant,
}

/// Where a step is in the run: what its line says, not where its line is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Group {
    Pinned,
    Finished,
    Running,
    Pending,
}

impl Cursor {
    fn new() -> Self {
        Self {
            rows: Vec::new(),
            on: None,
            parked: false,
        }
    }

    /// Take on a step from the plan. Nothing about the cursor changes: a step
    /// that has not started is not something to watch.
    fn insert(&mut self, row: Row) {
        self.rows.push(row);
        self.sort();
    }

    /// Record that a step has started, and where it is writing its log — which
    /// replaces wherever the last run wrote it, whether or not this run writes
    /// one at all.
    ///
    /// A step starting never moves the cursor off a step it was put on. It
    /// takes the cursor only when the cursor has been waiting for something to
    /// run: before the first step, or after the step it was on finished with
    /// nothing else going.
    fn start(&mut self, id: usize, at: Instant, log: Option<PathBuf>) {
        if let Some(row) = self.rows.iter_mut().find(|row| row.id == id) {
            row.started = Some(at);
            row.log = log;
        }
        if self.on.is_none() || self.parked {
            self.on = Some(id);
            self.parked = false;
        }
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

    /// Put the rows in the order the run is expected to take them, which is
    /// the order they keep: see [`Cursor`] and [`plan_order`].
    ///
    /// Nothing here reads how the run is going, so this says the same thing
    /// every time it is asked. It is done as the rows arrive and never needs
    /// doing again.
    fn sort(&mut self) {
        self.rows.sort_by_key(|row| row.rank);
    }

    fn has_ended(&self, id: usize) -> bool {
        self.rows
            .iter()
            .any(|row| row.id == id && row.ended.is_some())
    }

    /// The steps `row` is still waiting for, by label.
    fn waiting_on(&self, row: &Row) -> Vec<Arc<str>> {
        if row.group() != Group::Pending {
            return Vec::new();
        }
        row.deps
            .iter()
            .filter_map(|&dep| self.rows.iter().find(|row| row.id == dep))
            .filter(|dep| dep.ended.is_none())
            .map(|dep| Arc::clone(&dep.label))
            .collect()
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

    /// The step that started most recently and is still going.
    fn newest_running(&self) -> Option<usize> {
        self.rows
            .iter()
            .enumerate()
            .filter(|(_, row)| row.group() == Group::Running)
            .max_by_key(|(_, row)| row.started)
            .map(|(index, _)| index)
    }

    #[cfg(test)]
    fn selected(&self) -> Option<&Row> {
        self.position().map(|position| &self.rows[position])
    }
}

impl Row {
    fn group(&self) -> Group {
        if self.pinned {
            Group::Pinned
        } else if self.ended.is_some() {
            Group::Finished
        } else if self.started.is_some() {
            Group::Running
        } else {
            Group::Pending
        }
    }

    /// This step's line in the live area. `waiting` names the steps it is
    /// waiting for, if it has not started.
    fn line(
        &self,
        selected: bool,
        width: usize,
        spinner: &str,
        waiting: &[Arc<str>],
    ) -> Line<'static> {
        match &self.ended {
            // The very line the record will get, so the two can never say
            // different things. In full colour, however it ended: a finished
            // step is as much there to be opened as a running one, and greying
            // it out would say otherwise.
            Some(ended) => {
                let mut spans = vec![cursor_span(selected)];
                spans.extend(ended.record.spans.iter().cloned());
                Line::from(spans)
            }
            None if self.started.is_some() => {
                let mut line = self.running_line(selected, width, spinner, true);
                // Last, after whatever the step and its tool had to say, so it
                // reads as a remark on the line rather than displacing it.
                line.spans.extend(self.quiet_remark());
                line
            }
            None => self.pending_line(selected, width, waiting),
        }
    }

    /// This step's line, made to fit a terminal `columns` wide.
    ///
    /// A line that fits is drawn as it is. A running step's line that does not
    /// gives up its bars first — the `3/12` beside each says as much — and is
    /// then cut, with an ellipsis to say so; a waiting step's line is cut the
    /// same way. A finished step's line is left whole: it never changes again,
    /// so the list can wrap it instead.
    fn fit(
        &self,
        selected: bool,
        width: usize,
        spinner: &str,
        columns: usize,
        waiting: &[Arc<str>],
    ) -> Line<'static> {
        let full = self.line(selected, width, spinner, waiting);
        if self.ended.is_some() || full.width() <= columns {
            return full;
        }
        if self.started.is_none() {
            return fit_line(full, columns);
        }
        let bare = self.running_line(selected, width, spinner, false);
        let Some(remark) = self.quiet_remark() else {
            return fit_line(bare, columns);
        };
        // A quiet tool is the one thing on the line worth interrupting someone
        // over, and it sits at the end, where a plain cut would take it first.
        // So it is what the line is fitted around: room is kept for it, and
        // what the step and its tool are saying gives way instead.
        let mut spans = fit_line(bare, columns.saturating_sub(remark.width())).spans;
        spans.push(remark);
        Line::from(spans)
    }

    /// The remark at the end of a running step's line: that it has been told
    /// to stop, or that its tool has gone quiet.
    ///
    /// Not both. A step that has been killed and has not stopped yet is quiet
    /// for the most ordinary of reasons, and the two together would be saying
    /// the same thing twice.
    fn quiet_remark(&self) -> Option<Span<'static>> {
        let started = self.started?;
        let state = self.state.lock().unwrap();
        if state.killed {
            return Some(span("  (stopping)", Style::new().yellow()));
        }
        let quiet = state.quiet_for(started)?;
        Some(span(
            format!("  (quiet for {})", fmt_duration(quiet)),
            Style::new().yellow(),
        ))
    }

    /// How wide this step's line is with its bars given up: the width the label
    /// column is squeezed against. The quiet remark counts towards it, because
    /// [`Row::fit`] keeps that whatever else it has to cut.
    fn bare_width(&self, width: usize, spinner: &str) -> usize {
        self.running_line(false, width, spinner, false).width()
            + self.quiet_remark().map_or(0, |remark| remark.width())
    }

    /// The line of a step that is running, with or without its bars.
    fn running_line(
        &self,
        selected: bool,
        width: usize,
        spinner: &str,
        bars: bool,
    ) -> Line<'static> {
        let label = format!("{:<width$}", truncate(&self.label, width));
        let started = self.started.unwrap_or_else(Instant::now);
        let mut spans = vec![
            cursor_span(selected),
            span(format!("{spinner} "), Style::new().cyan()),
            span(label, Style::new().bold()),
            span(
                format!(" {:>5}  ", fmt_duration(started.elapsed())),
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
            spans.extend(region.spans(bars));
        }

        Line::from(spans)
    }

    /// The line of a step that has not started: greyed, with what it is
    /// waiting for.
    fn pending_line(&self, selected: bool, width: usize, waiting: &[Arc<str>]) -> Line<'static> {
        let label = format!("{:<width$}", truncate(&self.label, width));
        let mut spans = vec![
            cursor_span(selected),
            span("○ ", Style::new().dim()),
            span(label, Style::new().dim()),
        ];
        if !waiting.is_empty() {
            let names: Vec<&str> = waiting.iter().map(|label| &**label).collect();
            spans.push(span(
                format!("  waits for {}", names.join(", ")),
                Style::new().dim(),
            ));
        }
        Line::from(spans)
    }

    /// The files this step has to read, and the ones to open elsewhere.
    ///
    /// Everything first, most useful first: what the tool running now is
    /// writing, then what earlier tools wrote, then the step's own log. Then
    /// what someone reading the step's log wants: the output of the tool the
    /// step is driving, which is what the display deliberately never shows, or
    /// the step's own log until a tool has started.
    ///
    /// A step that has not started has nothing: the log at its path is the last
    /// run's, about to be replaced, and would only look like this run's.
    fn files(&self) -> (Vec<PathBuf>, Vec<PathBuf>) {
        if self.group() == Group::Pending {
            return (Vec::new(), Vec::new());
        }
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

/// The cursor's column: the marker on the step it is on, blank elsewhere.
fn cursor_span(selected: bool) -> Span<'static> {
    span(
        if selected { "❯ " } else { "  " },
        Style::new().cyan().bold(),
    )
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

    /// Render as a bar followed by the name, for the live display — or, with
    /// `bars` off, as the counts and the name alone, for a line with no room.
    fn spans(&self, bars: bool) -> Vec<Span<'static>> {
        match self.position {
            Some((current, total)) if bars => vec![
                span(bar_glyphs(current, total, BAR_WIDTH), Style::new().cyan()),
                span(format!(" {current}/{total} {}", self.name), Style::new()),
            ],
            Some((current, total)) => {
                vec![span(
                    format!("{current}/{total} {}", self.name),
                    Style::new(),
                )]
            }
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

/// Says that a step is running a process, for as long as it is held: see
/// [`StepHandle::watch_child`].
pub struct ChildGuard {
    state: Arc<Mutex<StepState>>,
    child: Child,
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let mut state = self.state.lock().unwrap();
        // Only if it is still the process this guard was for. A step that runs
        // one tool after another has a guard for each, and the one going out
        // of scope must not forget the one that took its place.
        if state.child == Some(self.child) {
            state.child = None;
        }
    }
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
    /// The tool the step is running, while it is running one: what a kill
    /// from the display has to reach. See [`StepHandle::watch_child`].
    child: Option<Child>,
    /// That the step has been told to stop, so its line can say so and its
    /// record can say it was killed rather than however the tool died.
    killed: bool,
}

/// A process a step is running, and which one it is.
///
/// The pid alone would not do: a pid read here and signalled a moment later
/// could by then belong to something else entirely. The token says whether the
/// process signalled is the one that was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Child {
    pid: u32,
    token: u64,
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
    /// See [`QUIET_AFTER`]. The display shows this on the step's line, in
    /// yellow, for as long as it lasts. It is public so that whatever is
    /// waiting on the tool can say it out loud as well — through [`note`] or
    /// `tracing`, which is how a run with no live display hears about it.
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

    /// Whether the step was killed from the display.
    pub fn was_killed(&self) -> bool {
        self.state.lock().unwrap().killed
    }

    /// Say that the step is running the process `pid`, so that a kill from the
    /// display can reach it, for as long as the returned guard is held.
    ///
    /// Dropping the guard says the process is gone. Signalling a step that is
    /// not running one does nothing, which is the right answer for a step
    /// doing its work in Rust: there is nothing to kill but the step itself,
    /// and a thread cannot be stopped from outside.
    ///
    /// [`crate::exec`] does this for the commands it runs. A step driving a
    /// tool some other way should do it itself, or be unkillable.
    pub fn watch_child(&self, pid: u32) -> ChildGuard {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let child = Child {
            pid,
            token: NEXT.fetch_add(1, Ordering::Relaxed) as u64,
        };
        let mut state = self.state.lock().unwrap();
        state.child = Some(child);
        ChildGuard {
            state: Arc::clone(&self.state),
            child,
        }
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

/// Say something that is not a step: to `rivet.log` always, and to stderr as
/// well when nothing is drawing on it.
///
/// Use this instead of `println!` anywhere that might run inside a flow: see
/// [the module docs](self#nothing-else-may-write-to-the-terminal). A flow with
/// the display up is not interrupted — the line goes to the log, which the
/// display can be asked to show, and which is still there to read when the run
/// is over.
pub fn note(message: impl AsRef<str>) {
    let message = message.as_ref();
    tracing::info!(target: "rivet::note", "{message}");
    say(message);
}

/// A [`note`] about something worth noticing, logged at `WARN`.
///
/// The level is the whole difference, and it is what tells a stalled tool from
/// the chatter around it when the log is read back, searched, or narrowed with
/// `RIVET_LOG`.
pub fn warn(message: impl AsRef<str>) {
    let message = message.as_ref();
    tracing::warn!(target: "rivet::note", "{message}");
    say(message);
}

/// The terminal half of [`note`] and [`warn`], which is nothing at all while
/// the display is up: see [`Reporter::print`].
fn say(message: &str) {
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

/// A line cut to `width` columns, ending in an ellipsis where it was cut, in
/// the style of what was cut. A line that fits is returned as it is.
fn fit_line(line: Line<'static>, width: usize) -> Line<'static> {
    if line.width() <= width {
        return line;
    }
    let room = width.saturating_sub(1);
    let style = line.style;
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut used = 0;
    let mut cut_style = Style::new();
    for span in line.spans {
        let mut kept = String::new();
        let mut cut = false;
        for c in span.content.chars() {
            let w = c.width().unwrap_or(0);
            if used + w > room {
                cut = true;
                break;
            }
            kept.push(c);
            used += w;
        }
        cut_style = span.style;
        if !kept.is_empty() {
            spans.push(Span::styled(kept, span.style));
        }
        if cut {
            break;
        }
    }
    spans.push(Span::styled("…", cut_style));
    Line::from(spans).style(style)
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

    /// A plan of steps with these labels, none pinned and none waiting.
    fn planned(labels: &[&str]) -> Vec<Planned> {
        labels
            .iter()
            .map(|label| Planned {
                label: label.to_string(),
                pinned: false,
                deps: Vec::new(),
                log: None,
            })
            .collect()
    }

    /// A reporter with no terminal, which is every reporter under `cargo test`.
    fn reporter() -> Arc<Reporter> {
        reporter_of(&["decoder par"])
    }

    fn reporter_of(labels: &[&str]) -> Arc<Reporter> {
        Reporter::new(planned(labels), 1, None, false)
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
        let handle = reporter.start(0, None);

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
        let handle = reporter.start(0, None);

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
        let reporter = reporter_of(&["a", "b", "c", "d"]);

        let handle = reporter.start(0, None);
        assert_eq!(handle.location(), None);

        let handle = reporter.start(1, None);
        handle.set_status("merging");
        assert_eq!(handle.location().as_deref(), Some("merging"));

        let handle = reporter.start(2, None);
        handle.output_line(&banner(2, 5, "route"));
        assert_eq!(handle.location().as_deref(), Some("route (2/5)"));

        let handle = reporter.start(3, None);
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
        let handle = reporter.start(0, None);

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

        let line = plain(&row.line(true, 11, "⠹", &[]));
        assert!(line.starts_with("❯ ⠹ step 1     "), "{line:?}");
        assert!(
            line.ends_with("━━╸─────── 3/12 merging gds │ route_design"),
            "{line:?}"
        );
    }

    #[test]
    fn the_summary_says_what_is_left() {
        let reporter = reporter_of(&["a", "b", "c", "d", "e", "f", "g"]);
        reporter.finished.store(3, Ordering::Relaxed);
        reporter.running.store(2, Ordering::Relaxed);
        reporter.failed.store(1, Ordering::Relaxed);

        let summary = plain(&reporter.summary(200));
        assert!(summary.contains("3/7 steps"), "{summary}");
        assert!(summary.contains("2 running"), "{summary}");
        assert!(summary.contains("1 failed"), "{summary}");
    }

    #[test]
    fn warnings_are_counted_on_the_summary_for_the_log_to_be_read_for() {
        let reporter = reporter_of(&["one", "two"]);
        // None to mention, so nothing is said.
        assert!(!plain(&reporter.summary_of(80, 0)).contains('⚠'));

        let text = plain(&reporter.summary_of(80, 3));
        assert!(text.contains("⚠ 3"), "{text}");
        // After what the run did, which is what the row is mostly about.
        assert!(text.find("steps") < text.find('⚠'), "{text}");
    }

    #[test]
    fn the_summary_bar_gives_way_before_the_counts_do() {
        let reporter = reporter_of(&["a", "b", "c", "d", "e", "f", "g"]);
        reporter.finished.store(3, Ordering::Relaxed);
        reporter.running.store(2, Ordering::Relaxed);

        // Wide enough: the full bar.
        let wide = plain(&reporter.summary(120));
        let bar = |summary: &str| summary.chars().filter(|c| "━╸─".contains(*c)).count();
        assert_eq!(bar(&wide), SUMMARY_BAR_WIDTH);
        assert_eq!(
            plain(&reporter.summary(1000)),
            wide,
            "wider changes nothing"
        );

        // Exactly as wide as the line: still the full bar.
        let width = reporter.summary(120).width();
        assert_eq!(plain(&reporter.summary(width)), wide);

        // A column short: the bar loses a column, the text nothing.
        let squeezed = reporter.summary(width - 1);
        assert_eq!(squeezed.width(), width - 1);
        assert_eq!(bar(&plain(&squeezed)), SUMMARY_BAR_WIDTH - 1);
        assert!(
            plain(&squeezed).ends_with("2 running"),
            "{}",
            plain(&squeezed)
        );

        // Far too narrow: the bar stops at its minimum and the text is what
        // overflows, for the terminal to cut.
        let narrow = plain(&reporter.summary(10));
        assert_eq!(bar(&narrow), MIN_SUMMARY_BAR_WIDTH);
        assert!(narrow.contains("3/7 steps"), "{narrow}");
    }

    // -- killing a step -----------------------------------------------------

    /// A step running a process that will not stop on its own.
    fn sleeping(reporter: &Arc<Reporter>) -> (StepHandle, std::process::Child, ChildGuard) {
        let child = std::process::Command::new("sleep")
            .arg("60")
            .spawn()
            .expect("spawn");
        let handle = reporter.start(0, None);
        let guard = handle.watch_child(child.id());
        (handle, child, guard)
    }

    #[test]
    fn killing_a_step_kills_the_tool_it_is_running() {
        let reporter = reporter_of(&["one", "two"]);
        let (handle, mut child, _guard) = sleeping(&reporter);

        let started = Instant::now();
        assert_eq!(reporter.kill(0), Kill::Sent);
        // Nothing waits for the tool to die: the step's own thread is already
        // waiting for that, and is what ends the step when it does.
        assert!(started.elapsed() < Duration::from_secs(1), "waited for it");

        let status = child.wait().expect("wait");
        assert!(!status.success(), "{status}");
        assert!(handle.was_killed());
    }

    #[test]
    fn a_killed_steps_line_says_it_was_killed_rather_than_how_the_tool_died() {
        let reporter = reporter_of(&["one", "two"]);
        let (handle, mut child, _guard) = sleeping(&reporter);
        reporter.kill(0);
        let _ = child.wait();

        // What the tool said on its way out is in the log. The line says the
        // step did not fail of its own accord.
        let killed = plain(&reporter.record(&handle, Outcome::Failed, Some("signal 15")));
        assert!(killed.contains("killed"), "{killed}");
        assert!(!killed.contains("signal 15"), "{killed}");

        // A step that failed on its own still says why.
        let other = reporter.start(1, None);
        let failed = plain(&reporter.record(&other, Outcome::Failed, Some("lvs did not match")));
        assert!(failed.contains("lvs did not match"), "{failed}");
        assert!(!failed.contains("killed"), "{failed}");
    }

    #[test]
    fn a_step_running_no_tool_of_its_own_says_there_is_nothing_to_kill() {
        let reporter = reporter_of(&["one", "two"]);
        let handle = reporter.start(0, None);
        // Nothing spawned: a step doing its work in Rust. Its own thread
        // cannot be stopped from outside, and saying so is better than
        // pretending it was.
        assert_eq!(reporter.kill(0), Kill::NoTool);
        assert!(!handle.was_killed());

        // Not a step at all.
        assert_eq!(reporter.kill(9), Kill::NotRunning);
    }

    #[test]
    fn a_step_being_killed_says_so_on_its_line_until_it_stops() {
        let reporter = reporter_of(&["one", "two"]);
        let (_handle, mut child, _guard) = sleeping(&reporter);

        // The row the display would draw for it, sharing the step's state.
        let mut row = row(0);
        row.state = Arc::clone(&reporter.steps[0].state);
        let line = |row: &Row| plain(&row.fit(false, 8, "⠹", 200, &[]));
        assert!(!line(&row).contains("stopping"), "{}", line(&row));

        reporter.kill(0);
        assert!(line(&row).contains("(stopping)"), "{}", line(&row));
        let _ = child.wait();
    }

    #[test]
    fn a_tool_that_has_gone_is_not_signalled_again() {
        let reporter = reporter_of(&["one", "two"]);
        let handle = reporter.start(0, None);
        let child = std::process::Command::new("sleep")
            .arg("60")
            .spawn()
            .expect("spawn");
        let pid = child.id();

        // The guard is what says the step is running it, and dropping it says
        // the process is gone — as `exec` does once it has been waited for.
        // A pid is reused sooner or later, and killing whatever holds it next
        // would be a bug worth avoiding.
        {
            let _guard = handle.watch_child(pid);
        }
        assert_eq!(reporter.kill(0), Kill::NoTool);

        let mut child = child;
        let _ = child.kill();
        let _ = child.wait();
    }

    // -- fitting the width --------------------------------------------------

    /// A running step with both halves of its line filled in.
    fn busy_row() -> Row {
        let row = row(1);
        row.state.lock().unwrap().status = Some(Progress::at(3, 12, "merging gds"));
        row.state.lock().unwrap().banner = Some(Progress::at(2, 5, "route_design"));
        row
    }

    #[test]
    fn a_line_that_fits_is_drawn_as_it_is() {
        let row = busy_row();
        let full = row.line(true, 11, "⠹", &[]);
        let width = full.width();
        assert_eq!(plain(&row.fit(true, 11, "⠹", width, &[])), plain(&full));
        assert_eq!(plain(&row.fit(true, 11, "⠹", 500, &[])), plain(&full));
        assert!(plain(&full).contains("━━╸─────── 3/12 merging gds │ ━━━╸────── 2/5 route_design"));
    }

    #[test]
    fn a_running_line_with_no_room_loses_its_bars_and_then_its_end() {
        let row = busy_row();
        let full = row.line(true, 11, "⠹", &[]).width();

        // A column short: the bars go, the counts and names stay.
        let bare = row.fit(true, 11, "⠹", full - 1, &[]);
        let text = plain(&bare);
        assert!(
            text.ends_with("3/12 merging gds │ 2/5 route_design"),
            "{text}"
        );
        assert!(!text.contains('━'), "{text}");
        assert!(bare.width() < full);

        // Narrower than even that: cut, and marked as cut.
        let cut = row.fit(true, 11, "⠹", bare.width() - 4, &[]);
        let text = plain(&cut);
        assert_eq!(cut.width(), bare.width() - 4);
        assert!(text.ends_with("2/5 route_d…"), "{text}");
        assert!(text.starts_with("❯ ⠹ step 1"), "{text}");
    }

    /// A step whose tool has said nothing for long enough to be worth saying.
    fn quiet_row() -> Row {
        let row = busy_row();
        row.state.lock().unwrap().last_output = Some(Instant::now() - QUIET_AFTER);
        row
    }

    #[test]
    fn a_quiet_tool_is_said_on_the_step_line_and_kept_when_the_line_is_cut() {
        let row = quiet_row();
        let full = row.line(true, 11, "⠹", &[]);
        assert!(
            plain(&full).ends_with("(quiet for 10m00s)"),
            "{}",
            plain(&full)
        );

        // Everything else on the line gives way to it first: the bars, then
        // what the step and its tool are saying, cut back from the right.
        for columns in [full.width() - 1, 40, 30] {
            let cut = row.fit(true, 11, "⠹", columns, &[]);
            let text = plain(&cut);
            assert!(text.ends_with("(quiet for 10m00s)"), "{columns}: {text}");
            assert!(cut.width() <= columns, "{columns}: {text}");
        }

        // And it is yellow, wherever the cut fell.
        let cut = row.fit(true, 11, "⠹", 30, &[]);
        assert_eq!(
            cut.spans.last().unwrap().style.fg,
            Some(ratatui::style::Color::Yellow)
        );
    }

    #[test]
    fn a_tool_that_is_still_writing_says_nothing_about_being_quiet() {
        let row = busy_row();
        assert!(!plain(&row.line(true, 11, "⠹", &[])).contains("quiet"));
        assert!(!plain(&row.fit(true, 11, "⠹", 30, &[])).contains("quiet"));
    }

    #[test]
    fn a_finished_line_is_never_cut_because_it_wraps_instead() {
        let mut row = row(1);
        row.ended = Some(Ended {
            record: Line::from("✖ step 1  a message far too long for the width given here"),
            at: Instant::now(),
        });
        let line = row.fit(false, 8, "⠹", 20, &[]);
        assert_eq!(plain(&line), plain(&row.line(false, 8, "⠹", &[])));
    }

    #[test]
    fn the_label_column_is_squeezed_only_when_a_running_line_needs_it() {
        let reporter = reporter_of(&["a step with a rather long name", "x"]);
        let mut long = busy_row();
        long.label = Arc::from("a step with a rather long name");
        let rows = vec![long, row(2)];

        // Wide enough: the column is as wide as the longest label.
        assert_eq!(reporter.label_width_for(&rows, 200, "⠹"), 30);
        let bare = rows[0].running_line(false, 30, "⠹", false).width();
        assert_eq!(reporter.label_width_for(&rows, bare, "⠹"), 30);

        // Too narrow even without bars: squeezed by what is missing…
        assert_eq!(reporter.label_width_for(&rows, bare - 5, "⠹"), 25);
        // …but no further than the minimum.
        assert_eq!(reporter.label_width_for(&rows, 10, "⠹"), MIN_LABEL_WIDTH);
        // And a squeezed column cuts the label to fit it, so the columns after
        // it still line up.
        let line = plain(&rows[0].fit(false, 25, "⠹", bare - 5, &[]));
        assert!(
            line.starts_with("  ⠹ a step with a rather lon…  "),
            "{line}"
        );

        // Finished steps do not count: they wrap.
        let mut done = busy_row();
        done.label = Arc::from("a step with a rather long name");
        done.ended = Some(Ended {
            record: Line::from("✔ whatever"),
            at: Instant::now(),
        });
        assert_eq!(reporter.label_width_for(&[done], 10, "⠹"), 30);
    }

    #[test]
    fn cutting_a_line_keeps_the_style_of_what_was_cut() {
        let line = Line::from(vec![
            Span::styled("abc", Style::new().bold()),
            Span::styled("defgh", Style::new().red()),
        ]);
        let cut = fit_line(line.clone(), 6);
        assert_eq!(plain(&cut), "abcde…");
        assert_eq!(
            cut.spans.last().unwrap().style.fg,
            Some(ratatui::style::Color::Red)
        );
        assert_eq!(plain(&fit_line(line.clone(), 8)), "abcdefgh");
        assert_eq!(plain(&fit_line(line.clone(), 3)), "ab…");
        // Wide characters are not split.
        assert_eq!(plain(&fit_line(Line::from("漢字表"), 4)), "漢…");
    }

    // -- the cursor ---------------------------------------------------------

    /// A step that has not started, ranked by its id.
    fn pending(id: usize) -> Row {
        Row {
            id,
            label: Arc::from(format!("step {id}")),
            rank: id,
            pinned: false,
            deps: Vec::new(),
            log: None,
            started: None,
            state: Arc::new(Mutex::new(StepState::default())),
            ended: None,
        }
    }

    /// A step that is running.
    fn row(id: usize) -> Row {
        let mut row = pending(id);
        row.started = Some(Instant::now());
        row
    }

    /// The steps in the order they are drawn.
    fn listed(cursor: &Cursor) -> Vec<usize> {
        cursor.rows.iter().map(|row| row.id).collect()
    }

    /// Plan step `id` and start it.
    fn add(cursor: &mut Cursor, id: usize) {
        cursor.insert(pending(id));
        cursor.start(id, Instant::now(), None);
    }

    /// A cursor over `count` running steps.
    fn filled(count: usize) -> Cursor {
        let mut cursor = Cursor::new();
        for id in 1..=count {
            add(&mut cursor, id);
        }
        cursor
    }

    fn end(cursor: &mut Cursor, id: usize, outcome: Outcome) {
        let glyph = match outcome {
            Outcome::Completed => "✔",
            Outcome::Skipped => "⏭",
            Outcome::Failed => "✖",
        };
        cursor.end(
            id,
            Ended {
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
        add(&mut cursor, 4);
        assert_eq!(cursor.on, Some(1));

        // Moved onto the newest running step, it stays on that one too.
        cursor.step(Motion::Down(3));
        assert_eq!(cursor.on, Some(4));
        add(&mut cursor, 5);
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

        // Newest by the clock, not by the list: the list is in the plan's
        // order, so the step that started last can be one further up it.
        let mut cursor = Cursor::new();
        for id in 1..=3 {
            cursor.insert(pending(id));
        }
        let at = Instant::now();
        cursor.start(3, at, None);
        cursor.start(1, at + Duration::from_secs(1), None);
        assert_eq!(cursor.on, Some(3), "the first to start took the cursor");
        end(&mut cursor, 3, Outcome::Completed);
        assert_eq!(cursor.on, Some(1), "not the newest thing running");
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

        add(&mut cursor, 2);
        assert_eq!(cursor.on, Some(2));
        assert!(!cursor.parked);

        // A step that never ran arriving is not something to watch.
        end(&mut cursor, 2, Outcome::Completed);
        let mut skipped = pending(3);
        skipped.pinned = true;
        skipped.ended = Some(Ended {
            record: Line::from("⏭ step 3"),
            at: Instant::now(),
        });
        cursor.insert(skipped);
        assert_eq!(cursor.on, Some(2));
        add(&mut cursor, 4);
        assert_eq!(cursor.on, Some(4));
    }

    #[test]
    fn put_on_a_finished_step_the_cursor_stays_there() {
        // The whole point: a step's log outlives the step, and a failed one is
        // the log most worth reading.
        let mut cursor = filled(3);
        end(&mut cursor, 2, Outcome::Failed);
        // Where it was: the list is the plan's order, and a step failing does
        // not take it out of it.
        assert_eq!(listed(&cursor), [1, 2, 3]);
        cursor.step(Motion::First);
        cursor.step(Motion::Down(1));
        assert_eq!(cursor.on, Some(2));

        // Nothing that happens to the run moves it — not even everything else
        // finishing and something new starting.
        add(&mut cursor, 4);
        end(&mut cursor, 1, Outcome::Completed);
        end(&mut cursor, 3, Outcome::Completed);
        end(&mut cursor, 4, Outcome::Completed);
        assert_eq!(cursor.on, Some(2));
        add(&mut cursor, 5);
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
        add(&mut cursor, 4);
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
        add(&mut cursor, 3);
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
            record: Line::from("✖ step 1   1m14s  during compare (2/2)"),
            at: Instant::now(),
        });
        assert_eq!(
            plain(&row.line(true, 8, "⠹", &[])),
            "❯ ✖ step 1   1m14s  during compare (2/2)"
        );
        assert_eq!(
            plain(&row.line(false, 8, "⠹", &[])),
            "  ✖ step 1   1m14s  during compare (2/2)"
        );
    }

    #[test]
    fn finished_steps_keep_their_colour() {
        use ratatui::style::{Color, Modifier};

        // A finished step is drawn as its record, span for span, with nothing
        // laid over the line: a grey line would say "not for you", and every
        // step is there to be opened.
        let record = Line::from(vec![
            Span::styled("⏭ ", Style::new().yellow()),
            Span::styled("step 1", Style::new().bold()),
            Span::styled("  pinned", Style::new().yellow()),
        ]);
        let mut row = row(1);
        row.ended = Some(Ended {
            record: record.clone(),
            at: Instant::now(),
        });
        let line = row.line(true, 8, "⠹", &[]);
        assert!(!line.style.add_modifier.contains(Modifier::DIM));
        assert_eq!(&line.spans[1..], &record.spans[..]);
        assert_eq!(line.spans[2].style.fg, None);
        assert_eq!(line.spans[0].style.fg, Some(Color::Cyan), "the cursor");
    }

    // -- the order of the list ----------------------------------------------

    #[test]
    fn the_list_is_in_the_plans_order_and_stays_in_it() {
        let mut cursor = Cursor::new();
        // A plan: a pinned step, and four to run in rank order 1, 2, 3, 4 —
        // planned in another order, to show it is the rank that counts.
        let mut pinned_step = pending(0);
        pinned_step.pinned = true;
        pinned_step.ended = Some(Ended {
            record: Line::from("⏭ step 0"),
            at: Instant::now(),
        });
        for id in [3, 1, 4, 2] {
            cursor.insert(pending(id));
        }
        cursor.insert(pinned_step);
        assert_eq!(listed(&cursor), [0, 1, 2, 3, 4]);
        // Nothing has started, so nothing is watched yet.
        assert_eq!(cursor.on, None);

        // Steps starting, in an order of their own, moves none of them.
        cursor.start(2, Instant::now(), None);
        cursor.start(1, Instant::now(), None);
        assert_eq!(listed(&cursor), [0, 1, 2, 3, 4]);
        assert_eq!(cursor.on, Some(2), "the first to start takes the cursor");

        // Nor does finishing, in any order, or failing, or being skipped
        // without ever having started.
        end(&mut cursor, 1, Outcome::Completed);
        cursor.start(3, Instant::now(), None);
        end(&mut cursor, 3, Outcome::Failed);
        end(&mut cursor, 2, Outcome::Completed);
        end(&mut cursor, 4, Outcome::Skipped);
        assert_eq!(listed(&cursor), [0, 1, 2, 3, 4]);
        assert!(cursor.rows.iter().all(|row| row.group() != Group::Pending));
    }

    #[test]
    fn a_waiting_step_names_what_it_waits_for() {
        let mut cursor = Cursor::new();
        cursor.insert(pending(1));
        cursor.insert(pending(2));
        let mut signoff = pending(3);
        signoff.deps = vec![1, 2];
        cursor.insert(signoff);

        let waiting = |cursor: &Cursor| -> Vec<String> {
            let row = cursor.rows.iter().find(|row| row.id == 3).unwrap();
            cursor
                .waiting_on(row)
                .iter()
                .map(|s| s.to_string())
                .collect()
        };
        assert_eq!(waiting(&cursor), ["step 1", "step 2"]);
        let line = cursor.rows.iter().find(|row| row.id == 3).unwrap();
        assert_eq!(
            plain(&line.line(false, 6, "⠹", &cursor.waiting_on(line))),
            "  ○ step 3  waits for step 1, step 2"
        );

        // Still waiting on a step that is running; not on one that has ended.
        cursor.start(1, Instant::now(), None);
        assert_eq!(waiting(&cursor), ["step 1", "step 2"]);
        end(&mut cursor, 1, Outcome::Completed);
        assert_eq!(waiting(&cursor), ["step 2"]);
        end(&mut cursor, 2, Outcome::Completed);
        assert_eq!(waiting(&cursor), Vec::<String>::new());
        let line = cursor.rows.iter().find(|row| row.id == 3).unwrap();
        assert_eq!(plain(&line.line(false, 6, "⠹", &[])), "  ○ step 3");

        // Once it runs there is nothing to wait for.
        cursor.start(3, Instant::now(), None);
        assert!(waiting(&cursor).is_empty());
    }

    #[test]
    fn a_waiting_line_that_does_not_fit_is_cut() {
        let row = pending(1);
        let waiting: Vec<Arc<str>> = vec![Arc::from("a long dependency name")];
        let full = row.fit(false, 6, "⠹", 200, &waiting);
        assert_eq!(plain(&full), "  ○ step 1  waits for a long dependency name");
        let cut = row.fit(false, 6, "⠹", 30, &waiting);
        assert_eq!(cut.width(), 30);
        assert!(plain(&cut).ends_with('…'), "{}", plain(&cut));
    }

    #[test]
    fn steps_are_ranked_after_what_they_wait_for_with_targets_last() {
        // The graph as the executor flattens it: the target first, its
        // dependencies after, shared ones once.
        let mut plan = planned(&["signoff", "drc", "lvs", "par", "syn", "sram"]);
        plan[0].deps = vec![1, 2];
        plan[1].deps = vec![3];
        plan[2].deps = vec![3];
        plan[3].deps = vec![4];
        plan[4].deps = vec![5];
        let ranks = plan_order(&plan);
        let mut by_rank: Vec<(usize, &str)> = ranks
            .iter()
            .zip(&plan)
            .map(|(&rank, step)| (rank, step.label.as_str()))
            .collect();
        by_rank.sort();
        let order: Vec<&str> = by_rank.into_iter().map(|(_, label)| label).collect();
        assert_eq!(order, ["sram", "syn", "par", "drc", "lvs", "signoff"]);

        // A cycle still gives every step a rank.
        let mut plan = planned(&["a", "b"]);
        plan[0].deps = vec![1];
        plan[1].deps = vec![0];
        let ranks = plan_order(&plan);
        assert_eq!(ranks.len(), 2);
        assert_ne!(ranks[0], ranks[1]);
    }

    /// The case the depth is for: a hierarchical flow, where the top block's
    /// steps wait on the child block's and the child's own signoff does not
    /// wait on the top's at all.
    #[test]
    fn a_blocks_steps_are_listed_among_the_other_blocks_not_after_them() {
        // As the executor interns it, from the targets down: the top block's
        // signoff first, then what it waits for, then the child's signoff.
        let labels = [
            "top drc",      // 0
            "top fill",     // 1
            "top synpar",   // 2
            "child synpar", // 3
            "gen rtl",      // 4
            "top lvs",      // 5
            "top v2lvs",    // 6
            "child v2lvs",  // 7
            "child drc",    // 8
            "child fill",   // 9
            "child lvs",    // 10
        ];
        let mut plan = planned(&labels);
        plan[0].deps = vec![1];
        plan[1].deps = vec![2];
        plan[2].deps = vec![3, 4];
        plan[3].deps = vec![4];
        plan[5].deps = vec![6, 1];
        plan[6].deps = vec![2, 7];
        plan[7].deps = vec![3];
        plan[8].deps = vec![9];
        plan[9].deps = vec![3];
        plan[10].deps = vec![7, 9];

        let ranks = plan_order(&plan);
        let mut by_rank: Vec<(usize, &str)> = ranks.iter().copied().zip(labels).collect();
        by_rank.sort();
        let order: Vec<&str> = by_rank.into_iter().map(|(_, label)| label).collect();

        // The child's fill and v2lvs run while the top is still in synpar, and
        // are listed there rather than after everything the top does.
        assert_eq!(
            order,
            [
                "gen rtl",
                "child synpar",
                "top synpar",
                "child v2lvs",
                "child fill",
                "top fill",
                "top v2lvs",
                "child drc",
                "child lvs",
                "top drc",
                "top lvs",
            ]
        );

        // Whatever else it does, nothing is ever listed above something it is
        // waiting for.
        for (id, step) in plan.iter().enumerate() {
            for &dep in &step.deps {
                assert!(
                    ranks[dep] < ranks[id],
                    "{} before {}",
                    labels[id],
                    labels[dep]
                );
            }
        }
    }

    #[test]
    fn the_banner_names_the_targets() {
        let mut plan = planned(&["signoff", "drc", "lvs", "par"]);
        plan[0].deps = vec![1, 2];
        plan[1].deps = vec![3];
        plan[2].deps = vec![3];
        let reporter = Reporter::new(plan, 4, Some(PathBuf::from("/build")), false);
        assert_eq!(
            reporter.about,
            About {
                targets: vec!["signoff".into()],
                steps: 4,
                workers: 4,
                log_dir: Some(PathBuf::from("/build")),
            }
        );

        // Several targets are named in the order they were asked for.
        let reporter = Reporter::new(planned(&["drc", "lvs"]), 2, None, false);
        assert_eq!(reporter.about.targets, ["drc", "lvs"]);
    }

    #[test]
    fn skipping_a_pinned_step_counts_it_once() {
        let mut plan = planned(&["sram", "syn"]);
        plan[0].pinned = true;
        let reporter = Reporter::new(plan, 1, None, false);
        reporter.skip(0);
        assert_eq!(reporter.counts().skipped, 1);
        assert_eq!(reporter.counts().finished, 1);
        assert_eq!(reporter.counts().executed(), 0);
    }

    // -- what a step has to read --------------------------------------------

    #[test]
    fn a_step_that_has_not_started_offers_nothing_to_read() {
        // The plan knows where the step's log goes, but what is there now is
        // the last run's, and would only look like this run's.
        let mut cursor = Cursor::new();
        let mut row = pending(1);
        let old = PathBuf::from("/build/decoder par.rivet.log");
        row.log = Some(old.clone());
        cursor.insert(row);
        assert_eq!(cursor.rows[0].files(), (vec![], vec![]));

        // Started, it offers the log this run opened for it — and, with
        // logging off, nothing rather than the old one.
        let new = PathBuf::from("/build/decoder par.rivet.log");
        cursor.start(1, Instant::now(), Some(new.clone()));
        assert_eq!(cursor.rows[0].files(), (vec![new.clone()], vec![new]));

        let mut cursor = Cursor::new();
        let mut row = pending(2);
        row.log = Some(old);
        cursor.insert(row);
        cursor.start(2, Instant::now(), None);
        assert_eq!(cursor.rows[0].files(), (vec![], vec![]));
    }

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
            started: Instant::now(),
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
