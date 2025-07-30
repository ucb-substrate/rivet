use std::fmt::Write as FmtWrite;
use std::{
    collections::HashMap,
    fs,
    fs::File,
    io::Write,
    path::{Path, PathBuf},
    sync::Arc,
};

use genus::{dont_avoid_lib_cells, set_default_options, Genus};
use indoc::formatdoc;
use rivet::flow::{Flow, FlowNode, Step};

pub fn sky130_innovus_settings() -> Step {
    Step {
        checkpoint: true,
        command: formatdoc!(
            r#"
            ln -sfn pre_sky130_innovus_settings latest
            ##########################################################
            # Placement attributes  [get_db -category place]
            ##########################################################
            #-------------------------------------------------------------------------------
            set_db place_global_place_io_pins  true

            set_db opt_honor_fences true
            set_db place_detail_dpt_flow true
            set_db place_detail_color_aware_legal true
            set_db place_global_solver_effort high
            set_db place_detail_check_cut_spacing true
            set_db place_global_cong_effort high
            set_db add_fillers_with_drc false

            ##########################################################
            # Optimization attributes  [get_db -category opt]
            ##########################################################
            #-------------------------------------------------------------------------------

            set_db opt_fix_fanout_load true
            set_db opt_clock_gate_aware false
            set_db opt_area_recovery true
            set_db opt_post_route_area_reclaim setup_aware
            set_db opt_fix_hold_verbose true

            ##########################################################
            # Clock attributes  [get_db -category cts]
            ##########################################################
            #-------------------------------------------------------------------------------
            set_db cts_target_skew 0.03
            set_db cts_max_fanout 10
            #set_db cts_target_max_transition_time .3
            set_db opt_setup_target_slack 0.10
            set_db opt_hold_target_slack 0.10

            ##########################################################
            # Routing attributes  [get_db -category route]
            ##########################################################
            #-------------------------------------------------------------------------------
            set_db route_design_antenna_diode_insertion 1
            set_db route_design_antenna_cell_name "ANTENNA"

            set_db route_design_high_freq_search_repair true
            set_db route_design_detail_post_route_spread_wire true
            set_db route_design_with_si_driven true
            set_db route_design_with_timing_driven true
            set_db route_design_concurrent_minimize_via_count_effort high
            set_db opt_consider_routing_congestion true
            set_db route_design_detail_use_multi_cut_via_effort medium
                

            # For top module: snap die to manufacturing grid, not placement grid
            set_db floorplan_snap_die_grid manufacturing
                    

            # note this is required for sky130_fd_sc_hd, the design has a ton of drcs if bottom layer is 1
                            # TODO: why is setting routing_layer not enough?
            set_db design_bottom_routing_layer 2
            set_db design_top_routing_layer 6
            # deprected syntax, but this used to always work
            set_db route_design_bottom_routing_layer 2
            "#
        ),
        name: "sky130_innovus_settings".into(),
    }
}

fn sky130_cds_mmmc(sdc_file: impl AsRef<Path>) -> String {
    let sdc_file = sdc_file.as_ref();
    //the sdc files need their paths not hardcoded to the chipyard directory
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
puts "set_analysis_view -setup {{ ss_100C_1v60.setup_view }} -hold {{ ff_n40C_1v95.hold_view tt_025C_1v80.extra_view }} -dynamic tt_025C_1v80.extra_view -leakage tt_025C_1v80.extra_view"
set_analysis_view -setup {{ ss_100C_1v60.setup_view }} -hold {{ ff_n40C_1v95.hold_view tt_025C_1v80.extra_view }} -dynamic tt_025C_1v80.extra_view -leakage tt_025C_1v80.extra_view"#
    )
}

pub fn reference_flow(work_dir: impl AsRef<Path>) -> Flow {
    let work_dir = work_dir.as_ref().to_path_buf();
    let syn_work_dir = work_dir.join("syn_rundir");
    fs::create_dir(&syn_work_dir);
    //print!("{}", syn_work_dir.join("checkpoints").into_os_string().into_string().expect("print fail"));
    Flow {
        nodes: HashMap::from_iter([(
            "syn".into(),
            FlowNode {
                tool: Arc::new(Genus::new(&syn_work_dir)),
                work_dir: syn_work_dir.clone(),
                checkpoint_dir: syn_work_dir.join("checkpoints"),
                steps: vec![
                    set_default_options(),
                    dont_avoid_lib_cells("ICGX1"),
                    read_design_files(&syn_work_dir, &work_dir),
                    elaborate("decoder"),
                    init_design("decoder"),
                    power_intent(&syn_work_dir),
                    syn_generic(),
                    syn_map(),
                    add_tieoffs(),
                    write_design("decoder"),
                ],
                deps: Vec::new(),
            },
        )]),
    }
}
