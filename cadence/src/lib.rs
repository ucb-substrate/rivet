pub mod genus;
pub mod innovus;
pub mod pegasus;

use indoc::formatdoc;
use rust_decimal::Decimal;
use std::fmt::Write as FmtWrite;
use std::path::PathBuf;

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

#[derive(Debug, Clone)]
pub struct SubmoduleInfo {
    pub name: String,
    pub ilm: PathBuf,
    pub lef: PathBuf,
}

/// Returns the TCL for clock_constraints and pin_constraints
pub fn sdc() -> String {
    formatdoc!(
        r#"create_clock clk -name clk -period 2.0
            set_clock_uncertainty 0.01 [get_clocks clk]
            set_clock_groups -asynchronous  -group {{ clk }}
            set_load 1.0 [all_outputs]
            set_input_delay -clock clk 0 [all_inputs]
            set_output_delay -clock clk 0 [all_outputs]"#
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
