use std::fmt::Write as FmtWrite;
use std::fs::File;
use std::io::Write;
use std::path::PathBuf;

pub struct Corner {
    pub name: String,
    pub corner_type: String,
    pub voltage: f64,
    pub temperature: i64,
}

//fn get_timing_libs(lib_pref: String, corner: &Corner) -> String {
//
//    //prefilter the corner to get the prefilters
//    //
//    //lib_args = self.technology.read_libs([hammer_tech.filters.get_timing_lib_with_preference(lib_pref)],
//    //hammer_tech.HammerTechnologyUtils.to_plain_item,
//    //extra_pre_filters=pre_filters)
//}

pub fn generate_mmmc_script(
    path: &PathBuf,
    corners: Vec<Corner>,
    constraint_mode_name: String,
    sdc_files_arg: &PathBuf,
    ilm_sdc_files_arg: &PathBuf,
    timing_libs: &PathBuf,
    qrc: &PathBuf,
) -> bool {
    let mut mmmc_file = File::create(&path).unwrap();

    //need to work on generated sdc files as well
    writeln!(
        mmmc_file,
        "create_constraint_mode -name {} {} {}",
        constraint_mode_name,
        sdc_files_arg.display(),
        ilm_sdc_files_arg.display(),
    );
    let mut setup_views: Vec<String> = Vec::new();
    let mut hold_views: Vec<String> = Vec::new();
    let mut extra_views: Vec<String> = Vec::new();

    //for corner in corners
    for corner in corners.iter() {
        //create innovus library sets
        //let list = get timing libs of corner

        //let tempInCelsius = corner.temperature;
        //let qrc = get_mmmc_qrc(corner);
        //qrc="-qrc_tech {}".format(self.get_mmmc_qrc(corner)) if self.get_mmmc_qrc(corner) != '' else ''
        //
        //let constraint=self.constraint_mode
        if corner.corner_type == "setup" {
            setup_views.push(corner.name.clone());
        } else if corner.corner_type == "hold" {
            hold_views.push(corner.name.clone());
        } else {
            extra_views.push(corner.name.clone());
        }

        writeln!(
            mmmc_file,
            "create_library_set -name {}_set -timing [list {}]",
            corner.name,
            timing_libs.display(),
        );
        //create Innovus timing conditions
        writeln!(
            mmmc_file,
            "create_timing_condition -name {}_cond -library_sets [list {}_set]",
            corner.name, corner.name,
        );
        //create Innovus rc corners from qrc tech files
        writeln!(
            mmmc_file,
            "create_rc_corner -name {}_rc -temperature {} {}",
            corner.name,
            corner.temperature,
            qrc.display(),
        );
        //create innovus delay corner
        writeln!(
            mmmc_file,
            "create_delay_corner -name {}_delay -timing_condition {}_cond -rc_corner {}_rc",
            corner.name, corner.name, corner.name,
        );
        //create the analysis views
        writeln!(
            mmmc_file,
            "create_analysis_view -name {}_view -delay_corner {}_delay -constraint_mode {}",
            corner.name, corner.name, constraint_mode_name,
        );
    }

    let mut power = String::new();
    if extra_views.len() > 0 {
        writeln!(
            &mut power,
            "-dynamic {} -leakage {}",
            extra_views[0], extra_views[0]
        );
    }
    writeln!(
        mmmc_file,
        "set_analysis_view -setup {{ {} }} -hold {{ {} {} }} {}",
        setup_views.join(" "),
        hold_views.join(" "),
        extra_views.join(" "),
        power,
    );

    true
}

pub fn power_spec_commands(run_dir: &PathBuf, power_spec_type: &String) -> Vec<String> {
    let power_spec_file = generate_power_spec(power_spec_type.to_string(), run_dir);
    let mut power_spec_arg = String::new();

    if power_spec_type == "upf" {
        power_spec_arg = "1801".to_string();
    } else if power_spec_type == "cpf" {
        power_spec_arg = "cpf".to_string();
    } else {
        power_spec_arg = "".to_string();
    }
    let mut power_spec_commands: Vec<String> = Vec::new();
    power_spec_commands.push(format!(
        "read_power_intent -{} {}",
        power_spec_arg,
        power_spec_file.display()
    ));
    power_spec_commands.push("apply_power_intent -summary".to_string());
    power_spec_commands.push("commit_power_intent".to_string());

    power_spec_commands
}

fn generate_power_spec(power_spec_type: String, run_dir: &PathBuf, top_module: String) -> PathBuf {
    let mut power_spec_file_path = run_dir.join("power_spec.{power_spec_type}");
    let mut power_spec_file = File::create(power_spec_file_path.clone()).unwrap();
    if power_spec_type == "cpf" {
        writeln!(power_spec_file, "set_cpf_version 1.0e");
        writeln!(power_spec_file, "set_hierarchy_separator /");
        writeln!(power_spec_file, "set_design {}", top_module);

        //create power nets
        //create ground nets
        //using a loop
        writeln!(power_spec_file, "create_power_domain -name AO -default");

        //output.append(f'update_power_domain -name {domain} -primary_power_net {power_nets[0].name} -primary_ground_net {ground_nets[0].name}')
        writeln!(
            power_spec_file,
            "update_power_domain -name AO -primary_power_net VDD -primary_ground_net VSS"
        );

        //create global connections using a loop

        //output.append(f'create_nominal_condition -name {condition} -voltage {nominal_vdd.value}')
        //output.append(f'create_power_mode -name {mode} -default -domain_conditions {{{domain}@{condition}}}')
        //
        writeln!(power_spec_file, "end_design");
    }
    power_spec_file_path
}
