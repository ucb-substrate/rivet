use std::{
    collections::HashMap,
    fs::File,
    io::Write,
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

    formatdoc!(
        r#"create_clock clk -name clk -period 2.0
        set_clock_uncertainty 0.01 [get_clocks clk]
        set_clock_groups -asynchronous  -group {{ clk }}
        set_load 1.0 [all_outputs]
        set_input_delay -clock clk 0 [all_inputs]
        set_output_delay -clock clk 0 [all_outputs]"#
    )
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
    //read mmmc.tcl
    //read physical -lef
    //read_hdl -sv {}
    //
    let mut sdc_file = File::create("clock_pin_constraints.sdc");
    let sdc_path = writeln!(sdc_file, sdc());
    let mmmc_tcl = sky130_cds_mmmc(sdc_path);

    Step {
        checkpoint: true,
        command: r#"


            "#
        .into(),
        name: "read_design_files".into(),
    }
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
    Step {
        checkpoint: true,
        command: r#"set_db message:WSDF-201 .max_print 20
        set_db use_tiehilo_for_const duplicate
        set ACTIVE_SET [string map { .setup_view .setup_set .hold_view .hold_set .extra_view .extra_set } [get_db [get_analysis_views] .name]]
        set HI_TIEOFF [get_db base_cell:TIEHI .lib_cells -if { .library.library_set.name == $ACTIVE_SET }]
        set LO_TIEOFF [get_db base_cell:TIELO .lib_cells -if { .library.library_set.name == $ACTIVE_SET }]
        add_tieoffs -high $HI_TIEOFF -low $LO_TIEOFF -max_fanout 1 -verbose
"#.into(),
        name:"add_tieoffs".into(),
    }
}

pub fn write_design() -> Step {
    // All write TCL commands
    // this includes write regs, write reports, write outputs
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
