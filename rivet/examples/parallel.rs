//! A mock flow that shows off parallel execution and the live display.
//!
//! ```text
//! cargo run -p rivet --example parallel
//! ```
//!
//! The graph is the usual signoff diamond: once P&R finishes, DRC and LVS run
//! at the same time, and signoff waits for both.
//!
//! ```text
//!   sram compile (pinned)
//!   syn ──▶ par ──┬──▶ drc ──┐
//!                 └──▶ lvs ──┴──▶ signoff
//! ```

use std::process::Command;

use rivet::{exec, progress, Executor, Step, StepRef};

#[derive(Debug)]
struct DemoStep {
    name: String,
    /// Substeps the pretend tool announces as it runs. A step with none of
    /// these keeps a plain spinner and shows the tail of its output instead.
    substeps: Vec<&'static str>,
    /// Seconds of pretend work.
    duration: f64,
    pinned: bool,
    deps: Vec<StepRef<dyn Step>>,
}

impl DemoStep {
    fn step(name: &str, duration: f64, deps: Vec<StepRef<dyn Step>>) -> StepRef<dyn Step> {
        StepRef::new(Self {
            name: name.to_string(),
            substeps: Vec::new(),
            duration,
            pinned: false,
            deps,
        })
        .into_dyn()
    }

    fn with_substeps(
        name: &str,
        duration: f64,
        substeps: Vec<&'static str>,
        deps: Vec<StepRef<dyn Step>>,
    ) -> StepRef<dyn Step> {
        StepRef::new(Self {
            name: name.to_string(),
            substeps,
            duration,
            pinned: false,
            deps,
        })
        .into_dyn()
    }

    fn pinned(name: &str, deps: Vec<StepRef<dyn Step>>) -> StepRef<dyn Step> {
        StepRef::new(Self {
            name: name.to_string(),
            substeps: Vec::new(),
            duration: 0.0,
            pinned: true,
            deps,
        })
        .into_dyn()
    }

    /// A shell script that behaves like a tool: chatter on stdout, and a
    /// banner whenever it starts a new substep.
    fn script(&self) -> String {
        let ticks = (self.duration / 0.15).round().max(1.0) as u32;
        if self.substeps.is_empty() {
            return format!(
                r#"for i in $(seq 1 {ticks}); do echo "{} working ($i/{ticks})"; sleep 0.15; done"#,
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
        script
    }
}

impl Step for DemoStep {
    fn execute(&self) {
        let work_dir = std::env::temp_dir().join("rivet-example");
        std::fs::create_dir_all(&work_dir).expect("failed to create work dir");

        let mut command = Command::new("/bin/bash");
        command.args(["-c", &self.script()]).current_dir(&work_dir);

        let status = exec::run_logged_in(&mut command, &work_dir, &self.name.replace(' ', "_"))
            .expect("failed to run demo step");
        assert!(status.success(), "{} failed", self.name);
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

fn main() {
    let sram = DemoStep::pinned("sram compile", vec![]);
    let syn = DemoStep::with_substeps(
        "decoder syn",
        1.5,
        vec!["read_design", "elaborate", "syn_generic", "syn_map"],
        vec![sram],
    );
    let par = DemoStep::with_substeps(
        "decoder par",
        2.4,
        vec![
            "init_design",
            "floorplan_design",
            "place_opt_design",
            "route_design",
            "add_fillers",
        ],
        vec![syn],
    );
    // No substeps: these keep a spinner and show the tail of their output.
    let drc = DemoStep::step("decoder drc", 2.4, vec![par.clone()]);
    let lvs = DemoStep::step("decoder lvs", 1.8, vec![par.clone()]);
    let signoff = DemoStep::step("decoder signoff", 0.6, vec![drc, lvs]);

    // Several targets can be queued; they are run as one graph, so shared work
    // happens once and independent branches still overlap.
    match Executor::new().concurrency(4).target_dyn(signoff).run() {
        Ok(summary) => println!(
            "\n{} of {} steps ran in {:.1}s",
            summary.executed,
            summary.total,
            summary.elapsed.as_secs_f64()
        ),
        Err(error) => {
            eprintln!("\nflow failed: {error}");
            std::process::exit(1);
        }
    }
}
