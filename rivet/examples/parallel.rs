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
use std::sync::Arc;

use rivet::{exec, ExecuteConfig, Step};

#[derive(Debug)]
struct DemoStep {
    name: String,
    /// Seconds of pretend work.
    duration: f64,
    pinned: bool,
    deps: Vec<Arc<dyn Step>>,
}

impl DemoStep {
    fn step(name: &str, duration: f64, deps: Vec<Arc<dyn Step>>) -> Arc<dyn Step> {
        Arc::new(Self {
            name: name.to_string(),
            duration,
            pinned: false,
            deps,
        }) as Arc<dyn Step>
    }

    fn pinned(name: &str, deps: Vec<Arc<dyn Step>>) -> Arc<dyn Step> {
        Arc::new(Self {
            name: name.to_string(),
            duration: 0.0,
            pinned: true,
            deps,
        }) as Arc<dyn Step>
    }
}

impl Step for DemoStep {
    fn execute(&self) {
        let work_dir = std::env::temp_dir().join("rivet-example");
        std::fs::create_dir_all(&work_dir).expect("failed to create work dir");

        // Stand in for a real tool: emit progress on stdout for a while.
        let ticks = (self.duration / 0.15).round().max(1.0) as u32;
        let script = format!(
            r#"for i in $(seq 1 {ticks}); do echo "{} phase $i/{ticks}"; sleep 0.15; done"#,
            self.name
        );

        let mut command = Command::new("/bin/bash");
        command.args(["-c", &script]).current_dir(&work_dir);

        let status = exec::run_logged_in(&mut command, &work_dir, &self.name.replace(' ', "_"))
            .expect("failed to run demo step");
        assert!(status.success(), "{} failed", self.name);
    }

    fn deps(&self) -> Vec<Arc<dyn Step>> {
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
    let syn = DemoStep::step("decoder syn", 1.2, vec![sram]);
    let par = DemoStep::step("decoder par", 1.5, vec![syn]);
    let drc = DemoStep::step("decoder drc", 2.4, vec![Arc::clone(&par)]);
    let lvs = DemoStep::step("decoder lvs", 1.8, vec![Arc::clone(&par)]);
    let signoff = DemoStep::step("decoder signoff", 0.6, vec![drc, lvs]);

    match ExecuteConfig::new().concurrency(4).run_arc(signoff) {
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
