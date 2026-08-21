//! Live terminal reporting for flow execution.
//!
//! While a flow is running, every step that is currently executing gets a line
//! with a spinner, its elapsed time, and the most recent line of output from
//! the tool it is driving. Finished steps scroll off into normal terminal
//! output as `✔` (executed), `⏭` (skipped because it was pinned) or `✖`
//! (failed), and a summary bar at the bottom tracks overall progress.
//!
//! When stderr is not a terminal (CI, redirected logs) the display degrades to
//! plain, one-line-per-event logging instead of drawing escape sequences.

use std::cell::RefCell;
use std::fmt::Write as _;
use std::io::IsTerminal;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock, Mutex, RwLock};
use std::time::{Duration, Instant};

use colored::Colorize;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};

/// Longest step label rendered before it is truncated.
const MAX_LABEL_WIDTH: usize = 44;

/// How output produced by a running step is surfaced in the terminal.
///
/// In every mode the full output is still written to the step's log files.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OutputMode {
    /// Show only the most recent line, inline on the step's spinner.
    #[default]
    Tail,
    /// Print every line above the live display, prefixed with the step label.
    Stream,
    /// Show substep banners only. Raw tool output goes to the log files.
    Quiet,
}

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

/// Renders the state of a run to the terminal.
pub(crate) struct Reporter {
    mode: OutputMode,
    label_width: usize,
    finished: AtomicUsize,
    running: AtomicUsize,
    skipped: AtomicUsize,
    failed: AtomicUsize,
    ui: Option<Ui>,
}

struct Ui {
    multi: MultiProgress,
    overall: ProgressBar,
}

impl Reporter {
    pub(crate) fn new(
        total: usize,
        label_width: usize,
        mode: OutputMode,
        progress: bool,
    ) -> Arc<Self> {
        let ui = if progress && live_display_available() {
            let multi = MultiProgress::new();
            let overall = multi.add(ProgressBar::new(total as u64));
            overall.set_style(overall_style());
            overall.enable_steady_tick(Duration::from_millis(120));
            Some(Ui { multi, overall })
        } else {
            None
        };

        let reporter = Arc::new(Self {
            mode,
            label_width: label_width.min(MAX_LABEL_WIDTH),
            finished: AtomicUsize::new(0),
            running: AtomicUsize::new(0),
            skipped: AtomicUsize::new(0),
            failed: AtomicUsize::new(0),
            ui,
        });
        reporter.refresh();
        reporter
    }

    pub(crate) fn mode(&self) -> OutputMode {
        self.mode
    }

    /// Announce that `label` has started running, returning a handle the step
    /// can use to report its own output.
    pub(crate) fn start(self: &Arc<Self>, label: &str) -> StepHandle {
        self.running.fetch_add(1, Ordering::Relaxed);

        let bar = self.ui.as_ref().map(|ui| {
            let bar = ui.multi.insert(0, ProgressBar::new_spinner());
            bar.set_style(step_style());
            bar.set_prefix(self.pad(label));
            bar.enable_steady_tick(Duration::from_millis(120));
            bar
        });

        if self.ui.is_none() {
            self.print_above(&format!("  {} {}", "▶".cyan(), label));
        }
        self.refresh();

        StepHandle {
            label: Arc::from(truncate(label, MAX_LABEL_WIDTH)),
            bar,
            reporter: Arc::clone(self),
            started: Instant::now(),
            state: Arc::new(Mutex::new(StepState::default())),
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
                if let Some(detail) = detail {
                    let _ = write!(line, "  {}", truncate(&one_line(detail), 160).red());
                }
                line
            }
        };

        if let (Some(ui), Some(bar)) = (&self.ui, &handle.bar) {
            bar.finish_and_clear();
            ui.multi.remove(bar);
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
            ui.overall.finish_and_clear();
        }

        let (finished, skipped, failed) = self.counts();
        let executed = finished.saturating_sub(skipped + failed);
        let mut line = format!(
            "  {} {} executed",
            if failed == 0 {
                "✔".green()
            } else {
                "✖".red()
            },
            executed
        );
        if skipped > 0 {
            let _ = write!(line, " · {skipped} skipped");
        }
        if failed > 0 {
            let _ = write!(line, " · {}", format!("{failed} failed").red());
        }
        let _ = write!(line, " · {}", fmt_duration(elapsed).dimmed());
        self.print_above(&line);
    }

    /// `(finished, skipped, failed)` step counts.
    pub(crate) fn counts(&self) -> (usize, usize, usize) {
        (
            self.finished.load(Ordering::Relaxed),
            self.skipped.load(Ordering::Relaxed),
            self.failed.load(Ordering::Relaxed),
        )
    }

    fn refresh(&self) {
        let Some(ui) = &self.ui else { return };
        let (finished, _, failed) = self.counts();
        ui.overall.set_position(finished as u64);

        let running = self.running.load(Ordering::Relaxed);
        let mut msg = match running {
            0 => String::new(),
            1 => "1 running".to_string(),
            n => format!("{n} running"),
        };
        if failed > 0 {
            if !msg.is_empty() {
                msg.push_str(" · ");
            }
            let _ = write!(msg, "{}", format!("{failed} failed").red());
        }
        ui.overall.set_message(msg);
    }

    fn pad(&self, label: &str) -> String {
        let label = truncate(label, MAX_LABEL_WIDTH);
        let width = self.label_width;
        format!("{label:<width$}")
    }
}

/// A substep banner emitted by a tool part-way through a step.
///
/// A step such as P&R is one node to the scheduler but a long sequence of
/// substeps to the tool driving it. A tool can announce which substep it is on
/// by printing a marker line; rivet picks it out of the output stream and shows
/// it on the step's progress line instead of the raw tail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Banner {
    /// Name of the substep, e.g. `place_opt_design`.
    pub name: String,
    /// 1-based index and total, when the tool knows how many substeps there are.
    pub position: Option<(usize, usize)>,
}

impl Banner {
    /// Render as `place_opt_design (4/9)`.
    pub fn describe(&self) -> String {
        match self.position {
            Some((current, total)) => format!("{} ({current}/{total})", self.name),
            None => self.name.clone(),
        }
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
pub fn parse_banner(line: &str) -> Option<Banner> {
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

    Some(Banner {
        name: name.to_string(),
        position,
    })
}

fn parse_position(text: &str) -> Option<(usize, usize)> {
    let (current, total) = text.split_once('/')?;
    let current: usize = current.parse().ok()?;
    let total: usize = total.parse().ok()?;
    (total > 0).then_some((current, total))
}

/// Report a substep from Rust, for steps that do their own work rather than
/// shelling out to a tool.
pub fn substep(current: usize, total: usize, name: impl Into<String>) {
    if let Some(handle) = current_step() {
        handle.enter_substep(Banner {
            name: name.into(),
            position: Some((current, total)),
        });
    }
}

/// A handle to the step running on the current thread.
///
/// Steps use this (usually indirectly, via [`crate::exec`]) to surface tool
/// output without corrupting the live display.
#[derive(Clone)]
pub struct StepHandle {
    label: Arc<str>,
    bar: Option<ProgressBar>,
    reporter: Arc<Reporter>,
    started: Instant,
    state: Arc<Mutex<StepState>>,
}

/// What the step's progress line is currently showing.
#[derive(Default)]
struct StepState {
    banner: Option<Banner>,
    tail: Option<String>,
    /// Set once the spinner has been swapped for a substep progress bar.
    counted: bool,
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

    /// Report one line of output produced by the step.
    ///
    /// Lines carrying a substep banner are consumed as progress rather than
    /// shown as output; see [`banner`].
    pub fn output_line(&self, line: &str) {
        if let Some(banner) = parse_banner(line) {
            self.enter_substep(banner);
            return;
        }

        let line = clean(line);
        match self.reporter.mode() {
            OutputMode::Quiet => {}
            OutputMode::Tail => {
                if !line.trim().is_empty() {
                    self.set_status(line);
                }
            }
            OutputMode::Stream => self
                .reporter
                .print_above(&format!("  {} {line}", format!("{}:", self.label).dimmed())),
        }
    }

    /// Record that the step has moved on to a new substep.
    ///
    /// Once a step reports a substep with a position, its spinner is replaced
    /// by a progress bar over the substeps.
    pub fn enter_substep(&self, banner: Banner) {
        {
            let mut state = self.state.lock().unwrap();
            // Whatever the previous substep last printed is stale now.
            state.tail = None;
            if let (Some(bar), Some((current, total))) = (&self.bar, banner.position) {
                // Position first: the spinner template ignores it, so swapping
                // the style last avoids drawing one empty frame at 0/n.
                bar.set_length(total as u64);
                bar.set_position(current.min(total) as u64);
                if !state.counted {
                    bar.set_style(substep_style());
                    state.counted = true;
                }
            }
            state.banner = Some(banner.clone());
        }
        self.render();

        // With no live bar there is nowhere to put the substep, so log it.
        if self.bar.is_none() || self.reporter.mode() == OutputMode::Stream {
            self.reporter.print_above(&format!(
                "  {} {}  {}",
                "→".cyan(),
                self.label.dimmed(),
                banner.describe()
            ));
        }
    }

    /// The substep this step last reported.
    pub fn substep(&self) -> Option<Banner> {
        self.state.lock().unwrap().banner.clone()
    }

    /// Replace the status shown next to this step's spinner.
    ///
    /// Shown after the current substep name, if there is one.
    pub fn set_status(&self, status: impl Into<String>) {
        self.state.lock().unwrap().tail = Some(clean(&status.into()));
        self.render();
    }

    fn render(&self) {
        let Some(bar) = &self.bar else { return };
        let state = self.state.lock().unwrap();

        let mut message = String::new();
        if let Some(banner) = &state.banner {
            message.push_str(&banner.name);
        }
        if let Some(tail) = &state.tail {
            if !message.is_empty() {
                message.push_str(" · ");
            }
            message.push_str(tail);
        }
        bar.set_message(message);
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

pub(crate) fn set_current_step(handle: Option<StepHandle>) {
    CURRENT.with(|current| *current.borrow_mut() = handle);
}

pub(crate) fn set_active_reporter(reporter: Option<Arc<Reporter>>) {
    *ACTIVE.write().unwrap() = reporter;
}

/// Print a line above the live display, or to stdout if no flow is running.
pub fn note(message: impl AsRef<str>) {
    let active = ACTIVE.read().unwrap().clone();
    match active {
        Some(reporter) => reporter.print_above(message.as_ref()),
        None => println!("{}", message.as_ref()),
    }
}

/// Report one line of output from the step running on this thread.
pub fn log_line(line: impl AsRef<str>) {
    match current_step() {
        Some(handle) => handle.output_line(line.as_ref()),
        None => note(line),
    }
}

/// Replace the status shown next to the current step's spinner.
pub fn status(message: impl Into<String>) {
    if let Some(handle) = current_step() {
        handle.set_status(message);
    }
}

fn live_display_available() -> bool {
    // The forcing hook exists so the display can be exercised when stderr is a
    // pipe (tests, recorded demos).
    std::io::stderr().is_terminal() || std::env::var_os("RIVET_FORCE_PROGRESS").is_some()
}

fn step_style() -> ProgressStyle {
    ProgressStyle::with_template("  {spinner:.cyan} {prefix:.bold} {elapsed:>5} {wide_msg}")
        .expect("valid step template")
        .tick_chars("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏ ")
}

/// Used once a step reports how many substeps it has: the spinner keeps its
/// place, and a bar over the substeps is added.
fn substep_style() -> ProgressStyle {
    ProgressStyle::with_template(
        "  {spinner:.cyan} {prefix:.bold} {elapsed:>5} {bar:10.cyan/blue} {pos}/{len} {wide_msg}",
    )
    .expect("valid substep template")
    .tick_chars("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏ ")
    .progress_chars("━╸─")
}

fn overall_style() -> ProgressStyle {
    ProgressStyle::with_template(
        "  {bar:24.green/blue} {pos}/{len} steps · {elapsed_precise} {msg}",
    )
    .expect("valid overall template")
    .progress_chars("━╸─")
}

/// Collapse a line of tool output into something safe to draw on one row.
fn clean(line: &str) -> String {
    line.trim_end()
        .replace(['\r', '\n'], " ")
        .replace('\t', "    ")
}

fn one_line(text: &str) -> String {
    text.replace(['\r', '\n'], " ")
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
        step_style();
        substep_style();
        overall_style();
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
            Some(Banner {
                name: "place_opt_design".into(),
                position: Some((4, 9)),
            })
        );

        assert_eq!(
            parse_banner(&banner_named("route_design")),
            Some(Banner {
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
            Some(Banner {
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
            Some(Banner {
                name: "3 of 9".into(),
                position: None,
            })
        );
    }

    #[test]
    fn banner_lines_become_substeps_not_output() {
        let reporter = Reporter::new(1, 8, OutputMode::Quiet, false);
        let handle = reporter.start("decoder par");

        handle.output_line("starting up");
        assert_eq!(handle.substep(), None);

        handle.output_line(&format!("INFO: {}", banner(4, 9, "place_opt_design")));
        let substep = handle.substep().expect("banner was not picked up");
        assert_eq!(substep.name, "place_opt_design");
        assert_eq!(substep.position, Some((4, 9)));
        assert_eq!(substep.describe(), "place_opt_design (4/9)");
    }

    #[test]
    fn a_new_substep_drops_the_previous_tail() {
        let reporter = Reporter::new(1, 8, OutputMode::Tail, false);
        let handle = reporter.start("decoder par");

        handle.output_line(&banner(1, 2, "place"));
        handle.set_status("placing row 40");
        assert_eq!(
            handle.state.lock().unwrap().tail.as_deref(),
            Some("placing row 40")
        );

        handle.output_line(&banner(2, 2, "route"));
        assert_eq!(handle.state.lock().unwrap().tail, None);
        assert_eq!(handle.substep().unwrap().name, "route");
    }

    #[test]
    fn long_labels_are_truncated() {
        assert_eq!(truncate("abcdef", 4), "abc…");
        assert_eq!(truncate("abc", 4), "abc");
    }
}
