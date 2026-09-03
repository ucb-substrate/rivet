pub mod genus;
pub mod innovus;
pub mod pegasus;

use indoc::formatdoc;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::fmt::Write as FmtWrite;
use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The script `GDB` is pointed at. Written into the step's work directory,
/// beside the tcl, so it needs no packaging and no path known ahead of time.
const KILL_ON_CRASH: &str = r#"#!/bin/sh
# Stand-in for `gdb`, pointed at by $GDB when a Cadence tool runs. See
# `kill_on_fatal_signal` for why this exists and why here.
#
# The tool's crash handler forks `.pstk`, which runs `/bin/pstack <pid>`, which
# runs us: tool -> .pstk (csh) -> pstack (bash) -> here. So the tool is our
# great-grandparent, and `.pstk`'s live PPid names it.
ppid() { awk '/^PPid:/ { print $2 }' "/proc/$1/status" 2>/dev/null; }
tool=$(ppid "$(ppid "$PPID")")
told=$(tr '\0' ' ' < "/proc/$PPID/cmdline" 2>/dev/null | awk '{ print $NF }')

# Kill only if the live parent of `.pstk` is the very pid pstack was told to
# trace. While the tool lives it is blocked in wait4() on this chain, so it
# cannot exit, so its pid cannot be reused, and the two agree. If it has
# already died -- a second collector run after the first one killed it --
# `.pstk` has been reparented and they do not, whoever holds the pid now.
# Any other shape of chain also fails this test, and we do nothing, which
# leaves the tool to carry on exactly as it would without us.
if [ -n "$tool" ] && [ "$tool" = "$told" ] && [ "$tool" -gt 1 ] 2>/dev/null; then
    kill -9 "$tool"
fi
exit 0
"#;

/// Make a fatal signal actually end a Cadence tool.
///
/// On a fatal signal the tool prints its own stack trace and then forks
/// `etc/innovus/.pstk`, which runs `/bin/pstack <pid>` — on RHEL a shell script
/// that runs whatever `$GDB` names, unchecked. Two things follow.
///
/// The first is that the collector must not be a real debugger. `gstack`'s gdb
/// attaches by ptrace, stopping every one of the tool's threads including the
/// one draining its own stdout pipe, and gdb then blocks writing the backtrace
/// into that pipe — a deadlock leaving the process alive with the run waiting
/// on it.
///
/// The second is that being `$GDB` is a precise signal. `.pstk` is forked from
/// the crash handler and nowhere else, so a script in that position is called
/// exactly once, at the moment the tool has decided it is dying. That matters
/// more than not deadlocking, because the tool does not reliably die on its
/// own: its handler calls `exit()`, and the atexit path runs `seiCleanupLog`
/// into `mm_fre_rare`, the tool's own allocator, which after a crash that
/// damaged the heap can spin on one core indefinitely with every other thread
/// parked behind it. That, not the debugger, is why a crashed step could hang
/// for hours.
///
/// So `GDB` names a script that SIGKILLs the tool. It runs before the handler
/// reaches `exit()`, so that loop is never entered: 192ms to a status of 137,
/// against 5s for a small design that unwinds cleanly and forever for one that
/// does not. What is given up is the tail of the tool's own shutdown — the AAE
/// memory dump and the license summary — which a crash of this kind never
/// reaches anyway. The stack trace is already printed by then.
///
/// `NO_GDB` covers `.pstk`'s second half, which runs only on a machine with no
/// `pstack` at all; there `.pstk` picks its own debugger with a csh `set`,
/// shadowing the environment, so `NO_GDB` is the only lever there.
///
/// Neither variable is documented, and both are read by shell scripts inside
/// the tool's own installation, so a version that stops consulting them stops
/// being covered here. Nothing depends on it having worked: a tool that hangs
/// anyway is still reported by the quiet-output warning in `rivet::exec`.
pub fn kill_on_fatal_signal(command: &mut Command, work_dir: &Path) -> io::Result<()> {
    let script = work_dir.join("kill_on_crash.sh");
    fs::write(&script, KILL_ON_CRASH)?;
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755))?;
    command.env("GDB", &script).env("NO_GDB", "1");
    Ok(())
}

#[derive(Debug, Clone)]
pub struct Substep {
    pub name: String,
    pub command: String,
    pub checkpoint: bool,
}

#[derive(Debug, Clone)]
pub struct Checkpoint {
    pub name: String,
    pub path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubmoduleInfo {
    pub name: String,
    pub verilog_paths: Vec<PathBuf>,
    pub gds: PathBuf,
    pub ilm: PathBuf,
    pub lef: PathBuf,
}

/// Returns the TCL for clock_constraints and pin_constraints
pub fn sdc() -> String {
    formatdoc!(
        r#"
        create_clock clk -name clk -period 2.0
        set_clock_uncertainty 0.01 [get_clocks clk]
        set_clock_groups -asynchronous  -group {{ clk }}
        set_load 1.0 [all_outputs]
        set_input_delay -clock clk 0 [all_inputs]
        set_output_delay -clock clk 0 [all_outputs]
        "#
    )
}

/// Defines the properties of MMMC Corners with the label, library paths, and temperature
#[derive(Clone)]
pub struct MmmcCorner {
    pub name: String,
    pub corner_type: String,
    pub libs: Vec<PathBuf>,
    pub temperature: Decimal,
}

/// Contains the parameters for generating the mmmc.tcl
#[derive(Clone)]
pub struct MmmcConfig {
    pub sdc_files: Vec<PathBuf>,
    pub corners: Vec<MmmcCorner>,
    pub setup: Vec<MmmcCorner>,
    pub hold: Vec<MmmcCorner>,
    pub dynamic: MmmcCorner,
    pub leakage: MmmcCorner,
}

/// Generates the tcl for the MMMC views
pub fn mmmc(config: MmmcConfig) -> String {
    for corner in config
        .setup
        .iter()
        .chain(config.hold.iter())
        .chain([&config.dynamic, &config.leakage])
    {
        assert!(
            config.corners.iter().any(|c| c.name == *corner.name),
            "corner referenced but not defined in the list of MMMC corners"
        );
    }

    let sdc_files_vec: Vec<String> = config
        .sdc_files
        .iter()
        .map(|p| p.display().to_string())
        .collect();
    let sdc_files = sdc_files_vec.join(" ");
    let mut mmmc = String::new();
    let constraint_mode_name = "my_constraint_mode";
    writeln!(
        &mut mmmc,
        "create_constraint_mode -name {constraint_mode_name} -sdc_files [list {sdc_files}]"
    )
    .unwrap();

    for corner in config.corners.iter() {
        let library_set_name = format!("{}.{}_set", corner.name, corner.corner_type);
        let timing_cond_name = format!("{}.{}_cond", corner.name, corner.corner_type);
        let rc_corner_name = format!("{}.{}_rc", corner.name, corner.corner_type);
        let delay_corner_name = format!("{}.{}_delay", corner.name, corner.corner_type);
        let analysis_view_name = format!("{}.{}_view", corner.name, corner.corner_type);
        write!(
            &mut mmmc,
            "create_library_set -name {library_set_name} -timing [list"
        )
        .unwrap();
        for lib in corner.libs.iter() {
            write!(&mut mmmc, " {lib:?}").unwrap();
        }
        writeln!(&mut mmmc, "]").unwrap();

        writeln!(&mut mmmc, "create_timing_condition -name {timing_cond_name} -library_sets [list {library_set_name}]").unwrap();
        writeln!(
            &mut mmmc,
            "create_rc_corner -name {rc_corner_name} -temperature {}",
            corner.temperature
        )
        .unwrap();

        writeln!(
            &mut mmmc,
            "create_delay_corner -name {delay_corner_name} -timing_condition {timing_cond_name} -rc_corner {rc_corner_name}",
        )
        .unwrap();

        writeln!(
            &mut mmmc,
            "create_analysis_view -name {analysis_view_name} -delay_corner {delay_corner_name} -constraint_mode {constraint_mode_name}",
        )
        .unwrap();
    }

    write!(&mut mmmc, "set_analysis_view -setup {{").unwrap();
    for corner in config.setup.iter() {
        write!(&mut mmmc, " {}.{}_view", corner.name, corner.corner_type).unwrap();
    }
    write!(&mut mmmc, " }}").unwrap();
    write!(&mut mmmc, " -hold {{").unwrap();
    for corner in config.hold.iter() {
        write!(&mut mmmc, " {}.{}_view", corner.name, corner.corner_type).unwrap();
    }
    write!(&mut mmmc, " }}").unwrap();
    writeln!(
        &mut mmmc,
        " -dynamic {}.{}_view -leakage {}.{}_view",
        config.dynamic.name,
        config.dynamic.corner_type,
        config.leakage.name,
        config.leakage.corner_type,
    )
    .unwrap();

    mmmc
}
