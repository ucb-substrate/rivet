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
//! One of the running steps is under a cursor, which `↑`/`↓` (or `j`/`k`) move
//! between them. `enter` copies a `tail` command for the log the step is
//! writing, to be pasted into another terminal: the display shows where a step
//! has got to and never what its tool is saying, so this is how to go and read
//! that without disturbing the run. See [`crate::keys`] for how the keys are
//! read, and [`StepHandle::set_output_files`] for what a step offers to follow.
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
//! The display works by counting the rows it drew and moving the cursor back up
//! over them. Anything that reaches the terminal without going through it — a
//! bare `println!` in flow code, a child process with inherited stdio, a panic
//! message from a thread the executor does not know about — scrolls the screen
//! out from under that count, and from then on every redraw lands a row lower
//! than the last: the bars march down the screen leaving copies behind.
//!
//! So while a flow is running, print with [`note`], run subprocesses through
//! [`crate::exec`] so their output is captured to file, and wrap anything that
//! insists on the terminal for itself in [`suspend`].
//!
//! Anything worth recording rather than showing goes through `tracing`, which
//! [`crate::log`] writes to files and never to a stream.

use std::cell::RefCell;
use std::fmt::Write as _;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard, RwLock};
use std::time::{Duration, Instant};

use colored::Colorize;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};

use crate::clipboard;
use crate::keys::{Event, Flow, Key, Keyboard};

/// Longest step label rendered before it is truncated.
const MAX_LABEL_WIDTH: usize = 44;

/// How long the hint line holds a message before going back to the keys.
const FLASH_FOR: Duration = Duration::from_secs(4);

/// How much of a step's log a copied command shows before it starts following.
const TAIL_LINES: usize = 100;

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
    multi: MultiProgress,
    overall: ProgressBar,
    /// The bottom line: what the keys do, or what one of them just did.
    hint: ProgressBar,
    /// The running steps, and which of them the cursor is on.
    cursor: Mutex<Cursor>,
    /// When the hint line goes back to showing the keys.
    flash_until: Mutex<Option<Instant>>,
    /// Reads the keys, for exactly as long as the run lasts.
    keyboard: Mutex<Option<Keyboard>>,
}

impl Ui {
    /// The keys being read, if any are.
    ///
    /// A panic while the terminal was borrowed — inside [`suspend`] — must not
    /// leave the run unable to give it back, so a poisoned lock is taken
    /// anyway.
    fn keyboard(&self) -> MutexGuard<'_, Option<Keyboard>> {
        self.keyboard
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Run `f` with the terminal in the mode it was in before the run started.
    fn cooked<R>(&self, f: impl FnOnce() -> R) -> R {
        match self.keyboard().as_ref() {
            Some(keyboard) => keyboard.cooked(f),
            None => f(),
        }
    }
}

impl Reporter {
    pub(crate) fn new(total: usize, label_width: usize, progress: bool) -> Arc<Self> {
        let ui = if progress && std::io::stderr().is_terminal() {
            // `colored` and `console` (indicatif's template styles, e.g. the
            // spinner's cyan) both key their tty checks to stdout, but the
            // display draws on stderr: with stdout redirected (`cargo test ...
            // > /dev/null`) they would strip every color. stderr is what
            // matters here, and it was just checked.
            colored::control::set_override(true);
            console::set_colors_enabled(true);
            let multi = MultiProgress::new();
            let overall = multi.add(ProgressBar::new(total as u64));
            overall.set_style(overall_style());
            overall.enable_steady_tick(Duration::from_millis(120));
            // Under the summary, so the steps and their total stay together.
            let hint = multi.add(ProgressBar::new_spinner());
            hint.set_style(hint_style());
            Some(Ui {
                multi,
                overall,
                hint,
                cursor: Mutex::new(Cursor::default()),
                flash_until: Mutex::new(None),
                keyboard: Mutex::new(None),
            })
        } else {
            None
        };

        let reporter = Arc::new(Self {
            label_width: label_width.min(MAX_LABEL_WIDTH),
            finished: AtomicUsize::new(0),
            running: AtomicUsize::new(0),
            skipped: AtomicUsize::new(0),
            blocked: AtomicUsize::new(0),
            failed: AtomicUsize::new(0),
            next_step: AtomicUsize::new(0),
            ui,
        });
        reporter.listen();
        reporter.refresh();
        reporter
    }

    /// Start reading keys, giving the display a cursor.
    ///
    /// The reader holds the reporter weakly: it must not be the thing keeping a
    /// finished run alive, because a run that has ended has to give the
    /// terminal back.
    fn listen(self: &Arc<Self>) {
        let Some(ui) = &self.ui else { return };

        let reporter = Arc::downgrade(self);
        let keyboard = Keyboard::start(move |event| match reporter.upgrade() {
            Some(reporter) => {
                reporter.on_event(event);
                Flow::Continue
            }
            // The run is over and nothing stopped the reader; stop anyway.
            None => Flow::Stop,
        });

        match keyboard {
            Some(keyboard) => {
                // The lock is not held past here: drawing the hint takes the
                // display's own lock, and `suspend` takes the two the other way
                // round.
                *ui.keyboard() = Some(keyboard);
                ui.hint.set_message(hint_line());
                ui.hint.tick();
            }
            // No keys to read, so nothing to tell anyone about.
            None => ui.multi.remove(&ui.hint),
        }
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

        let bar = self.ui.as_ref().map(|ui| {
            let bar = ui.multi.insert(0, ProgressBar::new_spinner());
            bar.set_style(step_style(false));
            bar.set_prefix(self.pad(&label));
            bar.enable_steady_tick(Duration::from_millis(120));

            let mut cursor = ui.cursor.lock().unwrap();
            cursor.insert(Row {
                id,
                label: Arc::clone(&label),
                bar: bar.clone(),
                state: Arc::clone(&state),
                log: log.as_ref().map(|log| log.path().to_path_buf()),
            });
            cursor.draw();
            bar
        });

        if self.ui.is_none() {
            self.print_above(&format!("  {} {}", "▶".cyan(), label));
        }
        self.refresh();

        StepHandle {
            id,
            label,
            bar,
            reporter: Arc::clone(self),
            started: Instant::now(),
            state,
            log,
        }
    }

    /// Record a step that was skipped without ever starting.
    pub(crate) fn skip(&self, label: &str, reason: &str) {
        self.skipped.fetch_add(1, Ordering::Relaxed);
        self.finished.fetch_add(1, Ordering::Relaxed);
        self.print_above(&format!(
            "  {} {}  {}",
            "⏭".yellow(),
            self.pad(label).dimmed(),
            reason.yellow()
        ));
        self.refresh();
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
        self.print_above(&format!(
            "  {} {}  {}",
            "⊘".yellow(),
            self.pad(label).dimmed(),
            format!("blocked by {blame}").yellow()
        ));
        self.refresh();
    }

    /// Record the end of a step started with [`Reporter::start`].
    pub(crate) fn finish(&self, handle: &StepHandle, outcome: Outcome, detail: Option<&str>) {
        self.running.fetch_sub(1, Ordering::Relaxed);
        self.finished.fetch_add(1, Ordering::Relaxed);

        let elapsed = fmt_duration(handle.started.elapsed());
        let padded = self.pad(&handle.label);
        let line = match outcome {
            Outcome::Completed => {
                format!("  {} {}  {}", "✔".green(), padded.bold(), elapsed.dimmed())
            }
            Outcome::Skipped => format!(
                "  {} {}  {}",
                "⏭".yellow(),
                padded.dimmed(),
                detail.unwrap_or("skipped").yellow()
            ),
            Outcome::Failed => {
                self.failed.fetch_add(1, Ordering::Relaxed);
                let mut line = format!(
                    "  {} {}  {}",
                    "✖".red(),
                    padded.red().bold(),
                    elapsed.dimmed()
                );
                // Say where it died, not just that it did. Both halves are
                // reported: which of them caused the failure is exactly what is
                // not known here.
                if let Some(location) = handle.location() {
                    let _ = write!(line, "  {}", format!("during {location}").yellow());
                }
                if let Some(detail) = detail {
                    let _ = write!(line, "  {}", truncate(&clean(detail), 160).red());
                }
                line
            }
        };

        if let (Some(ui), Some(bar)) = (&self.ui, &handle.bar) {
            bar.finish_and_clear();
            ui.multi.remove(bar);
            let mut cursor = ui.cursor.lock().unwrap();
            cursor.remove(handle.id);
            cursor.draw();
        }
        self.print_above(&line);
        self.refresh();
    }

    /// Print a line above the live display without disturbing it.
    pub(crate) fn print_above(&self, line: &str) {
        match &self.ui {
            Some(ui) => {
                let _ = ui.multi.println(line);
            }
            None => eprintln!("{line}"),
        }
    }

    /// Tear down the live display and print a closing summary.
    pub(crate) fn finish_all(&self, elapsed: Duration) {
        if let Some(ui) = &self.ui {
            // First: the terminal has to be the shell's own again before the
            // summary is printed and the run hands back control. Taken out of
            // the lock before it is stopped, because stopping waits for the
            // reader and the reader may be part-way through drawing.
            let keyboard = ui.keyboard().take();
            if let Some(keyboard) = keyboard {
                keyboard.stop();
            }
            ui.hint.finish_and_clear();
            ui.multi.remove(&ui.hint);
            ui.overall.finish_and_clear();
        }

        let counts = self.counts();
        let mut line = format!(
            "  {} {} executed",
            if counts.failed == 0 {
                "✔".green()
            } else {
                "✖".red()
            },
            counts.executed()
        );
        if counts.skipped > 0 {
            let _ = write!(line, " · {} skipped", counts.skipped);
        }
        if counts.blocked > 0 {
            let _ = write!(
                line,
                " · {}",
                format!("{} blocked", counts.blocked).yellow()
            );
        }
        if counts.failed > 0 {
            let _ = write!(line, " · {}", format!("{} failed", counts.failed).red());
        }
        let _ = write!(line, " · {}", fmt_duration(elapsed).dimmed());
        self.print_above(&line);
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

    fn refresh(&self) {
        let Some(ui) = &self.ui else { return };
        let counts = self.counts();
        ui.overall.set_position(counts.finished as u64);

        let running = self.running.load(Ordering::Relaxed);
        let mut msg = match running {
            0 => String::new(),
            1 => "1 running".to_string(),
            n => format!("{n} running"),
        };
        if counts.blocked > 0 {
            if !msg.is_empty() {
                msg.push_str(" · ");
            }
            let _ = write!(msg, "{}", format!("{} blocked", counts.blocked).yellow());
        }
        if counts.failed > 0 {
            if !msg.is_empty() {
                msg.push_str(" · ");
            }
            let _ = write!(msg, "{}", format!("{} failed", counts.failed).red());
        }
        ui.overall.set_message(msg);
    }

    fn pad(&self, label: &str) -> String {
        let label = truncate(label, MAX_LABEL_WIDTH);
        let width = self.label_width;
        format!("{label:<width$}")
    }

    // -- the cursor ---------------------------------------------------------

    /// Act on a key, or on a moment having passed without one.
    ///
    /// Called from the reader thread, so it does its own locking and holds
    /// nothing while it draws.
    fn on_event(&self, event: Event) {
        match event {
            Event::Key(Key::Up) => self.move_cursor(-1),
            Event::Key(Key::Down) => self.move_cursor(1),
            Event::Key(Key::Enter) => self.copy_follow_command(),
            Event::Tick => self.expire_flash(),
        }
    }

    fn move_cursor(&self, delta: isize) {
        let Some(ui) = &self.ui else { return };
        let mut cursor = ui.cursor.lock().unwrap();
        cursor.step(delta);
        cursor.draw();
    }

    /// Copy a command for reading the selected step's log as it is written.
    ///
    /// What someone watching a step actually wants is the tool's own output,
    /// which the display deliberately never shows — it is in a file, and this
    /// hands over the command to follow that file somewhere else.
    fn copy_follow_command(&self) {
        let Some(ui) = &self.ui else { return };

        // Read out from under the lock: the reader thread must not be holding
        // the cursor while it holds the display still to copy.
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
            self.flash(format!("{} {}", "no log yet for".yellow(), label.yellow()));
            return;
        };

        tracing::info!(step = %label, %command, "copied a command to follow the log");
        let copied = ui.multi.suspend(|| clipboard::copy(&command));
        if copied {
            self.flash(format!(
                "{} {}",
                "✔ copied".green().bold(),
                command.dimmed()
            ));
        } else {
            // Nowhere to copy it to, so put it somewhere it can be read.
            self.print_above(&format!("  {} {command}", "⧉".cyan()));
            self.flash(
                "no clipboard to copy to; the command is above"
                    .yellow()
                    .to_string(),
            );
        }
    }

    /// Show something on the hint line for a few seconds.
    fn flash(&self, message: String) {
        let Some(ui) = &self.ui else { return };
        *ui.flash_until.lock().unwrap() = Some(Instant::now() + FLASH_FOR);
        ui.hint.set_message(message);
        ui.hint.tick();
    }

    /// Put the keys back on the hint line once a message has had its moment.
    fn expire_flash(&self) {
        let Some(ui) = &self.ui else { return };
        let mut flash = ui.flash_until.lock().unwrap();
        if flash.is_some_and(|until| Instant::now() >= until) {
            *flash = None;
            ui.hint.set_message(hint_line());
            ui.hint.tick();
        }
    }
}

/// Which running step the display's cursor is on.
///
/// The rows are in the order they are drawn, newest first: a step's line is put
/// at the top when it starts, so every other row moves down under the cursor.
/// That is why the selection is kept as a step and not as a position.
#[derive(Default)]
struct Cursor {
    rows: Vec<Row>,
    selected: Option<usize>,
}

/// A running step, as the cursor sees it.
struct Row {
    id: usize,
    label: Arc<str>,
    bar: ProgressBar,
    state: Arc<Mutex<StepState>>,
    /// The step's own log file, if it has one.
    log: Option<PathBuf>,
}

impl Cursor {
    /// Take on a step that has just started, at the top.
    fn insert(&mut self, row: Row) {
        // With nothing running the cursor had nowhere to be, so it lands on
        // whatever starts first; after that it only moves when it is moved.
        if self.selected.is_none() {
            self.selected = Some(row.id);
        }
        self.rows.insert(0, row);
    }

    /// Drop a step that has finished, keeping the cursor on screen.
    fn remove(&mut self, id: usize) {
        let Some(position) = self.rows.iter().position(|row| row.id == id) else {
            return;
        };
        self.rows.remove(position);
        if self.selected == Some(id) {
            // Stay where it was: the row that moved up into this slot, or the
            // last row when it was the bottom one that finished.
            self.selected = self
                .rows
                .get(position)
                .or_else(|| self.rows.last())
                .map(|row| row.id);
        }
    }

    /// Move the cursor `delta` rows down the screen, stopping at the ends.
    ///
    /// Deliberately not wrapping: rows appear and disappear under the cursor on
    /// their own as steps start and finish, and a cursor that also jumped from
    /// one end of the list to the other would be one more thing moving.
    fn step(&mut self, delta: isize) {
        let Some(position) = self.position() else {
            return;
        };
        let moved = (position as isize + delta).clamp(0, self.rows.len() as isize - 1);
        self.selected = self.rows.get(moved as usize).map(|row| row.id);
    }

    fn position(&self) -> Option<usize> {
        let selected = self.selected?;
        self.rows.iter().position(|row| row.id == selected)
    }

    fn selected(&self) -> Option<&Row> {
        self.position().map(|position| &self.rows[position])
    }

    /// Draw the cursor where it now is.
    fn draw(&self) {
        for row in &self.rows {
            row.bar.set_style(step_style(self.selected == Some(row.id)));
        }
    }
}

impl Row {
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
    fn render(&self) -> String {
        match self.position {
            Some((current, total)) => format!(
                "{} {current}/{total} {}",
                bar_glyphs(current, total, BAR_WIDTH),
                self.name
            ),
            None => self.name.clone(),
        }
    }
}

/// Width of each inline progress bar, in characters.
const BAR_WIDTH: usize = 10;

/// Separates the two halves of a step's line, and the two halves of the
/// location reported when a step fails.
pub(crate) const REGION_SEP: &str = " │ ";

/// Draw a bar without indicatif, so a step can show two independent ones.
fn bar_glyphs(current: usize, total: usize, width: usize) -> String {
    let ratio = (current.min(total) as f64) / (total.max(1) as f64);
    let filled = ((ratio * width as f64).round() as usize).min(width);
    let bar = if filled == 0 {
        "─".repeat(width)
    } else if filled >= width {
        "━".repeat(width)
    } else {
        format!("{}╸{}", "━".repeat(filled - 1), "─".repeat(width - filled))
    };
    format!("{}", bar.cyan())
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
#[derive(Clone)]
pub struct StepHandle {
    /// Which step this is, to the display's cursor.
    id: usize,
    label: Arc<str>,
    bar: Option<ProgressBar>,
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

    /// Offer one line of the step's output to the display.
    ///
    /// A line carrying a substep banner moves the step on; see [`banner`].
    /// Everything else is ignored — raw output belongs in the step's log files,
    /// not on screen, whichever stream it arrived on.
    pub fn output_line(&self, line: &str) {
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
        self.render();

        // With no live bar there is nowhere to put the substep, so log it.
        if self.bar.is_none() {
            self.reporter.print_above(&format!(
                "  {} {}  {}",
                "→".cyan(),
                self.label.dimmed(),
                banner.describe()
            ));
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
        self.render();
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
        self.render();
    }

    /// Clear the status, leaving the rest of the line intact.
    pub fn clear_status(&self) {
        self.state.lock().unwrap().status = None;
        self.render();
    }

    fn render(&self) {
        let Some(bar) = &self.bar else { return };
        let state = self.state.lock().unwrap();

        // Left: what the step says it is doing. Right: what its tool says.
        let regions: Vec<String> = [state.status.as_ref(), state.banner.as_ref()]
            .into_iter()
            .flatten()
            .map(Progress::render)
            .collect();

        // `.to_string()` matters: `join` takes `&str`, and a bare
        // `&ColoredString` deref-coerces to the unstyled inner string.
        bar.set_message(regions.join(&REGION_SEP.dimmed().to_string()));
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
        Some(reporter) => reporter.print_above(message),
        None => println!("{message}"),
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
    match active.as_ref().and_then(|reporter| reporter.ui.as_ref()) {
        // The terminal goes back to collecting and echoing whole lines as well
        // as going still: whatever has to have it to itself usually wants both.
        Some(ui) => ui.multi.suspend(|| ui.cooked(f)),
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

fn step_style(selected: bool) -> ProgressStyle {
    // The cursor goes in the indent every line already has, so that a step
    // moving under it does not shift the column the labels line up in.
    let cursor = if selected {
        "❯".cyan().bold().to_string()
    } else {
        " ".to_string()
    };
    ProgressStyle::with_template(&format!(
        "{cursor} {{spinner:.cyan}} {{prefix:.bold}} {{elapsed:>5}} {{wide_msg}}"
    ))
    .expect("valid step template")
    .tick_chars("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏ ")
}

fn hint_style() -> ProgressStyle {
    ProgressStyle::with_template("  {wide_msg}").expect("valid hint template")
}

/// What the keys do, on the line under the summary.
fn hint_line() -> String {
    "↑/↓ or j/k select a step · enter copies a command to follow its log"
        .dimmed()
        .to_string()
}

fn overall_style() -> ProgressStyle {
    ProgressStyle::with_template(
        "  {bar:24.green/blue} {pos}/{len} steps · {elapsed_precise} {msg}",
    )
    .expect("valid overall template")
    .progress_chars("━╸─")
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

fn fmt_duration(duration: Duration) -> String {
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

    #[test]
    fn progress_templates_are_valid() {
        // These templates are only built when a terminal is attached, so parse
        // them here to keep the unwraps honest.
        step_style(false);
        step_style(true);
        overall_style();
        hint_style();
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
        let reporter = Reporter::new(1, 8, false);
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
        let reporter = Reporter::new(1, 8, false);
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
        let reporter = Reporter::new(1, 8, false);

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
        let reporter = Reporter::new(1, 8, false);
        let handle = reporter.start("decoder par", None);

        handle.output_line(&banner(1, 2, "place"));
        handle.set_status("linking");
        handle.clear_status();

        assert!(handle.status().is_none());
        assert_eq!(handle.substep().unwrap().name, "place");
    }

    #[test]
    fn bars_fill_in_proportion() {
        assert_eq!(strip(&bar_glyphs(0, 4, 10)), "──────────");
        assert_eq!(strip(&bar_glyphs(1, 4, 10)), "━━╸───────");
        assert_eq!(strip(&bar_glyphs(4, 4, 10)), "━━━━━━━━━━");
        // Overshooting a total clamps rather than panicking.
        assert_eq!(strip(&bar_glyphs(9, 4, 10)), "━━━━━━━━━━");
    }

    /// Drop ANSI colour so bar output can be compared.
    fn strip(text: &str) -> String {
        let mut out = String::new();
        let mut chars = text.chars();
        while let Some(c) = chars.next() {
            if c == '\u{1b}' {
                for c in chars.by_ref() {
                    if c == 'm' {
                        break;
                    }
                }
            } else {
                out.push(c);
            }
        }
        out
    }

    #[test]
    fn cursor_movement_is_stripped_from_tool_text() {
        // A tool animating its own progress. Drawn as-is, the `ESC[1A` walks
        // the cursor out from under the live display. A stray control character
        // becomes a space rather than vanishing, so it cannot glue two words
        // together.
        let line = "route\u{1b}[1A\u{1b}[2K 42%\u{7}done\r";
        assert_eq!(clean(line), "route 42% done");

        // Colour goes too: the display owns its own styling, and indicatif
        // truncates a bar message to the terminal width, which would cut a kept
        // sequence in half.
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
        // The name is tool output, and it is drawn on the step's bar.
        let banner = parse_banner("<<rivet:substep 2/9 \u{1b}[2Jfloorplan\u{1b}[1A>>").unwrap();
        assert_eq!(banner.name, "floorplan");
        assert_eq!(banner.position, Some((2, 9)));
    }

    #[test]
    fn long_labels_are_truncated() {
        assert_eq!(truncate("abcdef", 4), "abc…");
        assert_eq!(truncate("abc", 4), "abc");
    }

    // -- the cursor ---------------------------------------------------------

    /// A row with a bar that draws nowhere, so the cursor can be moved around
    /// without a terminal.
    fn row(id: usize) -> Row {
        Row {
            id,
            label: Arc::from(format!("step {id}")),
            bar: ProgressBar::hidden(),
            state: Arc::new(Mutex::new(StepState::default())),
            log: None,
        }
    }

    /// The rows in the order they are drawn, newest first.
    fn running(cursor: &Cursor) -> Vec<usize> {
        cursor.rows.iter().map(|row| row.id).collect()
    }

    #[test]
    fn the_cursor_lands_on_the_first_step_and_then_stays_put() {
        let mut cursor = Cursor::default();
        assert_eq!(cursor.selected, None);

        cursor.insert(row(1));
        assert_eq!(cursor.selected, Some(1));

        // A step starting goes above it and does not take the cursor with it.
        cursor.insert(row(2));
        cursor.insert(row(3));
        assert_eq!(running(&cursor), [3, 2, 1]);
        assert_eq!(cursor.selected, Some(1));
    }

    #[test]
    fn the_cursor_moves_down_the_screen_and_stops_at_the_ends() {
        let mut cursor = Cursor::default();
        for id in 1..=3 {
            cursor.insert(row(id));
        }
        // Drawn 3, 2, 1 from the top, and the cursor starts on the first step
        // to have started, which is at the bottom.
        assert_eq!(cursor.position(), Some(2));

        cursor.step(-1);
        assert_eq!(cursor.selected, Some(2));
        cursor.step(-1);
        assert_eq!(cursor.selected, Some(3));
        // The top row.
        cursor.step(-1);
        assert_eq!(cursor.selected, Some(3));

        cursor.step(1);
        assert_eq!(cursor.selected, Some(2));
        // Several rows at once, clamped at the bottom.
        cursor.step(5);
        assert_eq!(cursor.selected, Some(1));
    }

    #[test]
    fn a_step_finishing_under_the_cursor_leaves_it_where_it_was() {
        let mut cursor = Cursor::default();
        for id in 1..=3 {
            cursor.insert(row(id));
        }

        // The middle row finishes while the cursor is on it: the row that moves
        // up into that slot takes the cursor.
        cursor.selected = Some(2);
        cursor.remove(2);
        assert_eq!(running(&cursor), [3, 1]);
        assert_eq!(cursor.selected, Some(1));

        // The bottom row goes, and there is nothing below to move up.
        cursor.remove(1);
        assert_eq!(cursor.selected, Some(3));

        // Nothing left is running, so there is nothing to select.
        cursor.remove(3);
        assert_eq!(cursor.selected, None);

        // And the next step to start takes the cursor again.
        cursor.insert(row(4));
        assert_eq!(cursor.selected, Some(4));
    }

    #[test]
    fn a_step_finishing_elsewhere_does_not_move_the_cursor() {
        let mut cursor = Cursor::default();
        for id in 1..=3 {
            cursor.insert(row(id));
        }
        cursor.selected = Some(1);

        cursor.remove(3);
        assert_eq!(running(&cursor), [2, 1]);
        assert_eq!(cursor.selected, Some(1));
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
