use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use genus::Genus;
use indoc::formatdoc;
use rivet::flow::{Flow, FlowNode, Step};

fn set_default_options() -> Step {
    Step {
        name: "set_default_options".into(),
        command: r#"set_db hdl_error_on_blackbox true
set_db max_cpus_per_server 12
set_multi_cpu_usage -local_cpu 12
set_db super_thread_debug_jobs true
set_db super_thread_debug_directory super_thread_debug
set_db lp_clock_gating_infer_enable  true
set_db lp_clock_gating_prefix  {CLKGATE}
set_db lp_insert_clock_gating  true
set_db lp_clock_gating_register_aware true
set_db root: .auto_ungroup none
set_db [get_db lib_cells -if {.base_name == ICGX1}] .avoid false
"#
        .into(),
        checkpoint: false,
    }
}

pub fn sdc() -> String {
    // Combine contents of clock_constraints_fragment.sdc and pin_constraints_fragment.sdc
    todo!()
}

fn sky130_cds_mmmc(sdc_file: impl AsRef<Path>) -> String {
    let sdc_file = sdc_file.as_ref();
    formatdoc!(
        r#"puts "create_constraint_mode -name my_constraint_mode -sdc_files [list /home/ff/eecs251b/sp25-chipyard/vlsi/build/lab4/syn-rundir/clock_constraints_fragment.sdc /home/ff/eecs251b/sp25-chipyard/vlsi/build/lab4/syn-rundir/pin_constraints_fragment.sdc] "
create_constraint_mode -name my_constraint_mode -sdc_files {sdc_file:?}
puts "create_library_set -name ss_100C_1v60.setup_set -timing [list /home/ff/eecs251b/sky130/sky130_cds/sky130_scl_9T_0.0.5/lib/sky130_ss_1.62_125_nldm.lib]"
create_library_set -name ss_100C_1v60.setup_set -timing [list /home/ff/eecs251b/sky130/sky130_cds/sky130_scl_9T_0.0.5/lib/sky130_ss_1.62_125_nldm.lib]
puts "create_timing_condition -name ss_100C_1v60.setup_cond -library_sets [list ss_100C_1v60.setup_set]"
create_timing_condition -name ss_100C_1v60.setup_cond -library_sets [list ss_100C_1v60.setup_set]
puts "create_rc_corner -name ss_100C_1v60.setup_rc -temperature 100.0 "
create_rc_corner -name ss_100C_1v60.setup_rc -temperature 100.0
puts "create_delay_corner -name ss_100C_1v60.setup_delay -timing_condition ss_100C_1v60.setup_cond -rc_corner ss_100C_1v60.setup_rc"
create_delay_corner -name ss_100C_1v60.setup_delay -timing_condition ss_100C_1v60.setup_cond -rc_corner ss_100C_1v60.setup_rc
puts "create_analysis_view -name ss_100C_1v60.setup_view -delay_corner ss_100C_1v60.setup_delay -constraint_mode my_constraint_mode"
create_analysis_view -name ss_100C_1v60.setup_view -delay_corner ss_100C_1v60.setup_delay -constraint_mode my_constraint_mode
puts "create_library_set -name ff_n40C_1v95.hold_set -timing [list /home/ff/eecs251b/sky130/sky130_cds/sky130_scl_9T_0.0.5/lib/sky130_ff_1.98_0_nldm.lib]"
create_library_set -name ff_n40C_1v95.hold_set -timing [list /home/ff/eecs251b/sky130/sky130_cds/sky130_scl_9T_0.0.5/lib/sky130_ff_1.98_0_nldm.lib]
puts "create_timing_condition -name ff_n40C_1v95.hold_cond -library_sets [list ff_n40C_1v95.hold_set]"
create_timing_condition -name ff_n40C_1v95.hold_cond -library_sets [list ff_n40C_1v95.hold_set]
puts "create_rc_corner -name ff_n40C_1v95.hold_rc -temperature -40.0 "
create_rc_corner -name ff_n40C_1v95.hold_rc -temperature -40.0
puts "create_delay_corner -name ff_n40C_1v95.hold_delay -timing_condition ff_n40C_1v95.hold_cond -rc_corner ff_n40C_1v95.hold_rc"
create_delay_corner -name ff_n40C_1v95.hold_delay -timing_condition ff_n40C_1v95.hold_cond -rc_corner ff_n40C_1v95.hold_rc
puts "create_analysis_view -name ff_n40C_1v95.hold_view -delay_corner ff_n40C_1v95.hold_delay -constraint_mode my_constraint_mode"
create_analysis_view -name ff_n40C_1v95.hold_view -delay_corner ff_n40C_1v95.hold_delay -constraint_mode my_constraint_mode
puts "create_library_set -name tt_025C_1v80.extra_set -timing [list /home/ff/eecs251b/sky130/sky130_cds/sky130_scl_9T_0.0.5/lib/sky130_tt_1.8_25_nldm.lib]"
create_library_set -name tt_025C_1v80.extra_set -timing [list /home/ff/eecs251b/sky130/sky130_cds/sky130_scl_9T_0.0.5/lib/sky130_tt_1.8_25_nldm.lib]
puts "create_timing_condition -name tt_025C_1v80.extra_cond -library_sets [list tt_025C_1v80.extra_set]"
create_timing_condition -name tt_025C_1v80.extra_cond -library_sets [list tt_025C_1v80.extra_set]
puts "create_rc_corner -name tt_025C_1v80.extra_rc -temperature 25.0 "
create_rc_corner -name tt_025C_1v80.extra_rc -temperature 25.0
puts "create_delay_corner -name tt_025C_1v80.extra_delay -timing_condition tt_025C_1v80.extra_cond -rc_corner tt_025C_1v80.extra_rc"
create_delay_corner -name tt_025C_1v80.extra_delay -timing_condition tt_025C_1v80.extra_cond -rc_corner tt_025C_1v80.extra_rc
puts "create_analysis_view -name tt_025C_1v80.extra_view -delay_corner tt_025C_1v80.extra_delay -constraint_mode my_constraint_mode"
create_analysis_view -name tt_025C_1v80.extra_view -delay_corner tt_025C_1v80.extra_delay -constraint_mode my_constraint_mode
puts "set_analysis_view -setup { ss_100C_1v60.setup_view } -hold { ff_n40C_1v95.hold_view tt_025C_1v80.extra_view } -dynamic tt_025C_1v80.extra_view -leakage tt_025C_1v80.extra_view"
set_analysis_view -setup { ss_100C_1v60.setup_view } -hold { ff_n40C_1v95.hold_view tt_025C_1v80.extra_view } -dynamic tt_025C_1v80.extra_view -leakage tt_025C_1v80.extra_view"#
    )
}

pub fn read_design_files() -> Step {
    // Write SDC and mmmc.tcl, run commands up to read_hdl.
    todo!()
}

pub fn elaborate(module: &str) -> Step {
    Step {
        checkpoint: false,
        command: format!("elaborate {module}"),
        name: "elaborate".to_string(),
    }
}

pub fn init_design(module: &str) -> Step {
    Step {
        checkpoint: false,
        command: format!("init_design -top {module}"),
        name: "init_design".to_string(),
    }
}

pub fn power_intent() -> Step {
    // Write power_spec.cpf and run power_intent TCL commands.
    todo!()
}

pub fn syn_generic() -> Step {
    Step {
        checkpoint: true,
        command: "syn_generic".to_string(),
        name: "syn_generic".to_string(),
    }
}

pub fn syn_map() -> Step {
    Step {
        checkpoint: true,
        command: "syn_map".to_string(),
        name: "syn_map".to_string(),
    }
}

pub fn add_tieoffs() -> Step {
    todo!()
}

pub fn write_design() -> Step {
    // All write TCL commands
    todo!()
}

pub fn genus_syn() -> FlowNode {}

pub fn reference_flow(work_dir: impl AsRef<Path>) -> Flow {
    let work_dir = work_dir.as_ref().to_path_buf();
    let syn_work_dir = work_dir.join("syn");
    Flow {
        nodes: HashMap::from_iter([(
            "syn",
            FlowNode {
                tool: Arc::new(Genus::new(&syn_work_dir)),
                work_dir: syn_work_dir,
                checkpoint_dir: syn_work_dir.join("checkpoints"),
                steps: vec![
                    set_default_options(),
                    read_design_files(),
                    elaborate("decoder"),
                    init_design("decoder"),
                    power_intent(),
                    syn_generic(),
                    syn_map(),
                    add_tieoffs(),
                    write_design(),
                ],
                deps: Vec::new(),
            },
        )]),
    }
}
