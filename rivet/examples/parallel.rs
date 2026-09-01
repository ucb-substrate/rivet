//! A mock flow showing what the executor and its display do.
//!
//! ```text
//! cargo run -p rivet --example parallel
//! ```
//!
//! Runs twice: once cleanly, then once where LVS fails.
//!
//! ```text
//!   sram compile (pinned)
//!   syn ──▶ par ──┬──▶ drc ────┐
//!                 ├──▶ lvs ────┤
//!                 └──▶ merge ──┴──▶ signoff
//! ```
//!
//! No real tools are involved: each step shells out to a bash script that
//! prints substep banners and chatter the way an EDA tool would.

use std::process::Command;
use std::thread::sleep;
use std::time::Duration;

use rivet::{exec, progress, ExecuteConfig, Executor, Step, StepRef, StepResult};

/// A step that drives a "tool": a bash script printing banners on stdout.
#[derive(Debug)]
struct ToolStep {
    name: String,
    /// Substeps the tool announces as it runs. With none, the step keeps a
    /// plain spinner.
    substeps: Vec<&'static str>,
    /// Seconds of pretend work.
    duration: f64,
    /// Set a status from Rust before the tool starts, so both halves of the
    /// line are populated at once.
    prep: bool,
    /// Make the tool exit non-zero.
    fails: bool,
    pinned: bool,
    deps: Vec<StepRef<dyn Step>>,
}

impl ToolStep {
    fn new(name: &str, duration: f64, deps: Vec<StepRef<dyn Step>>) -> Self {
        Self {
            name: name.to_string(),
            substeps: Vec::new(),
            duration,
            prep: false,
            fails: false,
            pinned: false,
            deps,
        }
    }

    fn substeps(mut self, substeps: Vec<&'static str>) -> Self {
        self.substeps = substeps;
        self
    }

    fn prep(mut self) -> Self {
        self.prep = true;
        self
    }

    fn failing(mut self) -> Self {
        self.fails = true;
        self
    }

    fn pin(mut self) -> Self {
        self.pinned = true;
        self
    }

    fn build(self) -> StepRef<dyn Step> {
        StepRef::new(self).into_dyn()
    }

    /// Behave like a tool: chatter on stdout, with a banner per substep.
    fn script(&self) -> String {
        let ticks = (self.duration / 0.15).round().max(1.0) as u32;
        let exit = if self.fails { 1 } else { 0 };

        if self.substeps.is_empty() {
            return format!(
                r#"for i in $(seq 1 {ticks}); do echo "{} working ($i/{ticks})"; sleep 0.15; done; exit {exit}"#,
                self.name
            );
        }

        let total = self.substeps.len();
        let per_substep = (ticks as usize).div_ceil(total).max(1);
        let mut script = String::new();
        for (index, substep) in self.substeps.iter().enumerate() {
            script.push_str(&format!(
                "echo '{}'\n",
                progress::banner(index + 1, total, substep)
            ));
            script.push_str(&format!(
                r#"for i in $(seq 1 {per_substep}); do echo "{substep}: iteration $i"; sleep 0.15; done"#
            ));
            script.push('\n');
        }
        script.push_str(&format!("exit {exit}\n"));
        script
    }
}

impl Step for ToolStep {
    fn execute(&self) -> StepResult {
        let work_dir = std::env::temp_dir().join("rivet-example");
        std::fs::create_dir_all(&work_dir)?;

        // Rust-side work before the tool starts. Its status stays on the left
        // half of the line while the tool's banners fill the right half.
        if self.prep {
            let corners = ["ss_100C", "ff_n40C", "tt_25C"];
            for (index, corner) in corners.iter().enumerate() {
                progress::status_progress(index + 1, corners.len(), format!("reading {corner}"));
                sleep(Duration::from_millis(200));
            }
        }

        let mut command = Command::new("/bin/bash");
        command.args(["-c", &self.script()]).current_dir(&work_dir);

        let status = exec::run_logged_in(&mut command, &work_dir, &self.name.replace(' ', "_"))?;
        if !status.success() {
            // An expected failure, not a panic.
            return Err(format!(
                "{} did not match (tool exited with {status}); see {}",
                self.name,
                work_dir
                    .join(format!("{}.out", self.name.replace(' ', "_")))
                    .display()
            )
            .into());
        }
        Ok(())
    }

    fn deps(&self) -> Vec<StepRef<dyn Step>> {
        self.deps.clone()
    }

    fn pinned(&self) -> bool {
        self.pinned
    }

    fn label(&self) -> String {
        self.name.clone()
    }
}

/// Work done entirely in Rust, reporting progress with no tool to parse.
#[derive(Debug)]
struct MergeStep {
    files: Vec<&'static str>,
    deps: Vec<StepRef<dyn Step>>,
}

impl Step for MergeStep {
    fn execute(&self) -> StepResult {
        for (index, file) in self.files.iter().enumerate() {
            progress::status_progress(index + 1, self.files.len(), format!("merging {file}"));
            sleep(Duration::from_millis(300));
        }
        Ok(())
    }

    fn deps(&self) -> Vec<StepRef<dyn Step>> {
        self.deps.clone()
    }

    fn pinned(&self) -> bool {
        false
    }

    fn label(&self) -> String {
        "decoder merge".into()
    }
}

/// Build the flow. `lvs_fails` decides whether LVS's tool exits non-zero.
fn flow(lvs_fails: bool) -> StepRef<dyn Step> {
    let sram = ToolStep::new("sram compile", 0.0, vec![]).pin().build();
    let syn = ToolStep::new("decoder syn", 1.2, vec![sram])
        .substeps(vec!["read_design", "elaborate", "syn_generic", "syn_map"])
        .build();

    // `prep` sets a Rust status before the tool runs, so this step shows a
    // status bar on the left and the tool's substep bar on the right.
    let par = ToolStep::new("decoder par", 2.1, vec![syn])
        .substeps(vec![
            "init_design",
            "floorplan_design",
            "place_opt_design",
            "route_design",
            "add_fillers",
        ])
        .prep()
        .build();

    let drc = ToolStep::new("decoder drc", 2.4, vec![par.clone()])
        .substeps(vec!["density", "spacing", "antenna"])
        .build();

    let mut lvs =
        ToolStep::new("decoder lvs", 1.2, vec![par.clone()]).substeps(vec!["extract", "compare"]);
    if lvs_fails {
        lvs = lvs.failing();
    }
    let lvs = lvs.build();

    let merge = StepRef::new(MergeStep {
        files: vec!["decoder.gds", "sram22.gds", "sky130_fd_sc_hd.gds"],
        deps: vec![par],
    })
    .into_dyn();

    ToolStep::new("decoder signoff", 0.4, vec![drc, lvs, merge]).build()
}

fn banner_line(text: &str) {
    progress::note("");
    progress::note(format!("── {text} "));
}

fn main() {
    banner_line("1. a clean run");
    println!(
        "   pinned steps are skipped; drc, lvs and merge run together once par is done.\n\
         \x20  `decoder par` shows a Rust status on the left and tool substeps on the right."
    );
    match ExecuteConfig::new().concurrency(4).run_dyn(flow(false)) {
        Ok(summary) => println!(
            "\n   {} of {} steps ran in {:.1}s",
            summary.executed,
            summary.total,
            summary.elapsed.as_secs_f64()
        ),
        Err(error) => println!("\n   unexpected failure: {error}"),
    }

    banner_line("2. the same flow, with LVS failing");
    println!(
        "   lvs returns an error rather than panicking. drc and merge do not depend\n\
         \x20  on it, so they run to completion; only signoff is dropped."
    );
    match Executor::new().concurrency(4).target_dyn(flow(true)).run() {
        Ok(_) => println!("\n   unexpectedly succeeded"),
        Err(error) => println!("\n   {error}"),
    }

    println!(
        "\n   full tool output for every step is in {}",
        std::env::temp_dir().join("rivet-example").display()
    );
}
