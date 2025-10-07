use indoc::formatdoc;
use rust_decimal::Decimal;
use std::fmt::Write as FmtWrite;
use std::fs::File;
use std::io::Write;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct Substep {
    //could make this a cadence concept inside the cadence.rs file
    pub name: String,
    pub command: String,
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
    pub libs: Vec<PathBuf>,
    pub temperature: Decimal,
}

/// Contains the parameters for generating the mmmc.tcl
#[derive(Clone)]
pub struct MmmcConfig {
    pub sdc_files: Vec<PathBuf>,
    pub corners: Vec<MmmcCorner>,
    pub setup: Vec<String>,
    pub hold: Vec<String>,
    pub dynamic: String,
    pub leakage: String,
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
            config.corners.iter().any(|c| c.name == *corner),
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
        let library_set_name = format!("{}_set", corner.name);
        let timing_cond_name = format!("{}_cond", corner.name);
        let rc_corner_name = format!("{}_rc", corner.name);
        let delay_corner_name = format!("{}_delay", corner.name);
        let analysis_view_name = format!("{}_view", corner.name);
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
        write!(&mut mmmc, " {corner}_view").unwrap();
    }
    write!(&mut mmmc, " }}").unwrap();
    write!(&mut mmmc, " -hold {{").unwrap();
    for corner in config.hold.iter() {
        write!(&mut mmmc, " {corner}_view").unwrap();
    }
    write!(&mut mmmc, " }}").unwrap();
    writeln!(
        &mut mmmc,
        " -dynamic {}_view -leakage {}_view",
        config.dynamic, config.leakage,
    )
    .unwrap();

    mmmc
}

//fn get_timing_libs(lib_pref: String, corner: &Corner) -> String {
//
//    //prefilter the corner to get the prefilters
//    //
//    //lib_args = self.technology.read_libs([hammer_tech.filters.get_timing_lib_with_preference(lib_pref)],
//    //hammer_tech.HammerTechnologyUtils.to_plain_item,
//    //extra_pre_filters=pre_filters)
//}

//
// pub fn power_spec_commands(
//     run_dir: &PathBuf,
//     power_spec_type: &String,
//     module: String,
// ) -> Vec<String> {
//     let power_spec_file = generate_power_spec(power_spec_type.to_string(), run_dir, module);
//     let mut power_spec_arg = String::new();
//
//     if power_spec_type == "upf" {
//         power_spec_arg = "1801".to_string();
//     } else if power_spec_type == "cpf" {
//         power_spec_arg = "cpf".to_string();
//     } else {
//         power_spec_arg = "".to_string();
//     }
//     let mut power_spec_commands: Vec<String> = Vec::new();
//     power_spec_commands.push(format!(
//         "read_power_intent -{} {}",
//         power_spec_arg,
//         power_spec_file.display()
//     ));
//     power_spec_commands.push("apply_power_intent -summary".to_string());
//     power_spec_commands.push("commit_power_intent".to_string());
//
//     power_spec_commands
// }
//
// fn generate_power_spec(power_spec_type: String, run_dir: &PathBuf, top_module: String) -> PathBuf {
//     let mut power_spec_file_path = run_dir.join("power_spec.{power_spec_type}");
//     let mut power_spec_file = File::create(power_spec_file_path.clone()).unwrap();
//     if power_spec_type == "cpf" {
//         writeln!(power_spec_file, "set_cpf_version 1.0e");
//         writeln!(power_spec_file, "set_hierarchy_separator /");
//         writeln!(power_spec_file, "set_design {}", top_module);
//
//         //create power nets
//         //create ground nets
//         //using a loop
//         writeln!(power_spec_file, "create_power_domain -name AO -default");
//
//         //output.append(f'update_power_domain -name {domain} -primary_power_net {power_nets[0].name} -primary_ground_net {ground_nets[0].name}')
//         writeln!(
//             power_spec_file,
//             "update_power_domain -name AO -primary_power_net VDD -primary_ground_net VSS"
//         );
//
//         //create global connections using a loop
//
//         //output.append(f'create_nominal_condition -name {condition} -voltage {nominal_vdd.value}')
//         //output.append(f'create_power_mode -name {mode} -default -domain_conditions {{{domain}@{condition}}}')
//         //
//         writeln!(power_spec_file, "end_design");
//     }
//     power_spec_file_path
// }
