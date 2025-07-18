use std::collections::HashMap;

use genus::Genus;
use indoc::formatdoc;
use rivet::flow::{Flow, FlowNode};

pub fn add(left: u64, right: u64) -> u64 {
    left + right
}

fn set_default_options() -> String {
    r#"#set_db hdl_error_on_blackbox true
set_db max_cpus_per_server 12
set_multi_cpu_usage -local_cpu 12
set_db super_thread_debug_jobs true
set_db super_thread_debug_directory super_thread_debug
set_db lp_clock_gating_infer_enable  true
set_db lp_clock_gating_prefix  {CLKGATE}
set_db lp_insert_clock_gating  true
set_db lp_clock_gating_register_aware true"#
}

fn sky130_cds_mmmc(sky130_cds_path: &str) -> String {
    formatdoc!(
        r#"puts "create_library_set -name ss_100C_1v60.setup_set -timing [list {sky130_cds_path}/sky130_scl_9T_0.0.5/lib/sky130_ss_1.62_125_nldm.lib]"
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

pub fn genus_syn() -> FlowNode {}

pub fn reference_flow() -> Flow {
    Flow {
        workflow: HashMap::from_iter([(
            "syn",
            FlowNode {
                tool: Genus::new(""), // TODO
            },
        )]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_works() {
        let result = add(2, 2);
        assert_eq!(result, 4);
    }
}
