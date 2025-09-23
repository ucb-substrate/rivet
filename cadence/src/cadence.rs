use indoc::formatdoc;
use rust_decimal::Decimal;
use std::fmt::Write as FmtWrite;
use std::fs::File;
use std::io::Write;
use std::path::PathBuf;

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

// pub struct Corner {
//     pub name: String,
//     pub corner_type: String,
//     pub voltage: f64,
//     pub temperature: i64,
// }
//
//fn get_timing_libs(lib_pref: String, corner: &Corner) -> String {
//
//    //prefilter the corner to get the prefilters
//    //
//    //lib_args = self.technology.read_libs([hammer_tech.filters.get_timing_lib_with_preference(lib_pref)],
//    //hammer_tech.HammerTechnologyUtils.to_plain_item,
//    //extra_pre_filters=pre_filters)
//}

// pub fn generate_mmmc_script(
//     work_dir: &PathBuf,
//     corners: Vec<Corner>,
//     constraint_mode_name: String,
//     sdc_files_arg: &PathBuf,
//     ilm_sdc_files_arg: &PathBuf,
//     timing_libs: &PathBuf,
//     qrc: &PathBuf,
// ) -> String {
//     let mut mmmc_file = String::new();
//
//     //need to work on generated sdc files as well
//     writeln!(
//         &mut mmmc_file,
//         "create_constraint_mode -name {} {} {}",
//         constraint_mode_name,
//         sdc_files_arg.display(),
//         ilm_sdc_files_arg.display(),
//     );
//     let mut setup_views: Vec<String> = Vec::new();
//     let mut hold_views: Vec<String> = Vec::new();
//     let mut extra_views: Vec<String> = Vec::new();
//
//     //for corner in corners
//     for corner in corners.iter() {
//         //create innovus library sets
//         //let list = get timing libs of corner
//
//         //let tempInCelsius = corner.temperature;
//         //let qrc = get_mmmc_qrc(corner);
//         //qrc="-qrc_tech {}".format(self.get_mmmc_qrc(corner)) if self.get_mmmc_qrc(corner) != '' else ''
//         //
//         //let constraint=self.constraint_mode
//         if corner.corner_type == "setup" {
//             setup_views.push(corner.name.clone());
//         } else if corner.corner_type == "hold" {
//             hold_views.push(corner.name.clone());
//         } else {
//             extra_views.push(corner.name.clone());
//         }
//
//         writeln!(
//             mmmc_file,
//             "create_library_set -name {}_set -timing [list {}]",
//             corner.name,
//             timing_libs.display(),
//         );
//         //create Innovus timing conditions
//         writeln!(
//             &mut mmmc_file,
//             "create_timing_condition -name {}_cond -library_sets [list {}_set]",
//             corner.name, corner.name,
//         );
//         //create Innovus rc corners from qrc tech files
//         writeln!(
//             &mut mmmc_file,
//             "create_rc_corner -name {}_rc -temperature {} {}",
//             corner.name,
//             corner.temperature,
//             qrc.display(),
//         );
//         //create innovus delay corner
//         writeln!(
//             &mut mmmc_file,
//             "create_delay_corner -name {}_delay -timing_condition {}_cond -rc_corner {}_rc",
//             corner.name, corner.name, corner.name,
//         );
//         //create the analysis views
//         writeln!(
//             &mut mmmc_file,
//             "create_analysis_view -name {}_view -delay_corner {}_delay -constraint_mode {}",
//             corner.name, corner.name, constraint_mode_name,
//         );
//     }
//
//     let mut power = String::new();
//     if extra_views.len() > 0 {
//         writeln!(
//             &mut power,
//             "-dynamic {} -leakage {}",
//             extra_views[0], extra_views[0]
//         );
//     }
//     writeln!(
//         &mut mmmc_file,
//         "set_analysis_view -setup {{ {} }} -hold {{ {} {} }} {}",
//         setup_views.join(" "),
//         hold_views.join(" "),
//         extra_views.join(" "),
//         power,
//     );
//
//     mmmc_file
// }
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
