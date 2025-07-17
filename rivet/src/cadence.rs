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
        //
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

pub fn power_spec_commands() -> Vec<String> {
    //power_spec_file = self.create_power_spec()
    //   power_spec_arg = self.map_power_spec_name()
    //
    //   return ["read_power_intent -{arg} {path}".format(arg=power_spec_arg, path=power_spec_file),
    //           "commit_power_intent"]
    //
    //
    //     def create_power_spec(self) -> str:
    //    """
    //    Generate a power specification file for Cadence tools.
    //    """
    //
    //    power_spec_type = str(self.get_setting("vlsi.inputs.power_spec_type"))  # type: str
    //    power_spec_contents = ""  # type: str
    //    power_spec_mode = str(self.get_setting("vlsi.inputs.power_spec_mode"))  # type: str
    //    if power_spec_mode == "empty":
    //        return ""
    //    elif power_spec_mode == "auto":
    //        if power_spec_type == "cpf":
    //            power_spec_contents = self.cpf_power_specification
    //        elif power_spec_type == "upf":
    //            power_spec_contents = self.upf_power_specification
    //    elif power_spec_mode == "manual":
    //        power_spec_contents = str(self.get_setting("vlsi.inputs.power_spec_contents"))
    //    else:
    //        self.logger.error("Invalid power specification mode '{mode}'; using 'empty'.".format(mode=power_spec_mode))
    //        return ""
    //
    //    # Write the power spec contents to file and include it
    //    power_spec_file = os.path.join(self.run_dir, "power_spec.{tpe}".format(tpe=power_spec_type))
    //    self.write_contents_to_path(power_spec_contents, power_spec_file)
    //
    //    return power_spec_file
    //
    //
    //def map_power_spec_name(self) -> str:
    //    """
    //    Return the CPF or UPF flag name for Cadence tools.
    //    """
    //
    //    power_spec_type = str(self.get_setting("vlsi.inputs.power_spec_type"))  # type: str
    //    power_spec_arg = ""  # type: str
    //    if power_spec_type == "cpf":
    //        power_spec_arg = "cpf"
    //    elif power_spec_type == "upf":
    //        power_spec_arg = "1801"
    //    else:
    //        self.logger.error(
    //            "Invalid power specification type '{tpe}'; only 'cpf' or 'upf' supported".format(tpe=power_spec_type))
    //        return ""
    //    return power_spec_arg
    //
}
