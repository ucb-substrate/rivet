#################################################################################
#
# Created by Genus(TM) Synthesis Solution 21.17-s066_1 on Thu Jul 24 10:30:33 PDT 2025
#
#################################################################################

## library_sets
create_library_set -name ss_100C_1v60.setup_set \
    -timing { /home/ff/eecs251b/sky130/sky130_cds/sky130_scl_9T_0.0.5/lib/sky130_ss_1.62_125_nldm.lib }
create_library_set -name ff_n40C_1v95.hold_set \
    -timing { /home/ff/eecs251b/sky130/sky130_cds/sky130_scl_9T_0.0.5/lib/sky130_ff_1.98_0_nldm.lib }
create_library_set -name tt_025C_1v80.extra_set \
    -timing { /home/ff/eecs251b/sky130/sky130_cds/sky130_scl_9T_0.0.5/lib/sky130_tt_1.8_25_nldm.lib }

## timing_condition
create_timing_condition -name ss_100C_1v60.setup_cond \
    -library_sets { ss_100C_1v60.setup_set }
create_timing_condition -name ff_n40C_1v95.hold_cond \
    -library_sets { ff_n40C_1v95.hold_set }
create_timing_condition -name tt_025C_1v80.extra_cond \
    -library_sets { tt_025C_1v80.extra_set }

## rc_corner
create_rc_corner -name ss_100C_1v60.setup_rc \
    -temperature 100.0 \
    -pre_route_res 1.0 \
    -pre_route_cap 1.0 \
    -pre_route_clock_res 0.0 \
    -pre_route_clock_cap 0.0 \
    -post_route_res {1.0 1.0 1.0} \
    -post_route_cap {1.0 1.0 1.0} \
    -post_route_cross_cap {1.0 1.0 1.0} \
    -post_route_clock_res {1.0 1.0 1.0} \
    -post_route_clock_cap {1.0 1.0 1.0} \
    -post_route_clock_cross_cap {1.0 1.0 1.0}
create_rc_corner -name ff_n40C_1v95.hold_rc \
    -temperature -40.0 \
    -pre_route_res 1.0 \
    -pre_route_cap 1.0 \
    -pre_route_clock_res 0.0 \
    -pre_route_clock_cap 0.0 \
    -post_route_res {1.0 1.0 1.0} \
    -post_route_cap {1.0 1.0 1.0} \
    -post_route_cross_cap {1.0 1.0 1.0} \
    -post_route_clock_res {1.0 1.0 1.0} \
    -post_route_clock_cap {1.0 1.0 1.0} \
    -post_route_clock_cross_cap {1.0 1.0 1.0}
create_rc_corner -name tt_025C_1v80.extra_rc \
    -temperature 25.0 \
    -pre_route_res 1.0 \
    -pre_route_cap 1.0 \
    -pre_route_clock_res 0.0 \
    -pre_route_clock_cap 0.0 \
    -post_route_res {1.0 1.0 1.0} \
    -post_route_cap {1.0 1.0 1.0} \
    -post_route_cross_cap {1.0 1.0 1.0} \
    -post_route_clock_res {1.0 1.0 1.0} \
    -post_route_clock_cap {1.0 1.0 1.0} \
    -post_route_clock_cross_cap {1.0 1.0 1.0}

## delay_corner
create_delay_corner -name ss_100C_1v60.setup_delay \
    -early_timing_condition { ss_100C_1v60.setup_cond } \
    -late_timing_condition { ss_100C_1v60.setup_cond } \
    -early_rc_corner ss_100C_1v60.setup_rc \
    -late_rc_corner ss_100C_1v60.setup_rc
create_delay_corner -name ff_n40C_1v95.hold_delay \
    -early_timing_condition { ff_n40C_1v95.hold_cond } \
    -late_timing_condition { ff_n40C_1v95.hold_cond } \
    -early_rc_corner ff_n40C_1v95.hold_rc \
    -late_rc_corner ff_n40C_1v95.hold_rc
create_delay_corner -name tt_025C_1v80.extra_delay \
    -early_timing_condition { tt_025C_1v80.extra_cond } \
    -late_timing_condition { tt_025C_1v80.extra_cond } \
    -early_rc_corner tt_025C_1v80.extra_rc \
    -late_rc_corner tt_025C_1v80.extra_rc

## constraint_mode
create_constraint_mode -name my_constraint_mode \
    -sdc_files { genus_invs_des/genus.my_constraint_mode.sdc }

## analysis_view
create_analysis_view -name ss_100C_1v60.setup_view \
    -constraint_mode my_constraint_mode \
    -delay_corner ss_100C_1v60.setup_delay
create_analysis_view -name ff_n40C_1v95.hold_view \
    -constraint_mode my_constraint_mode \
    -delay_corner ff_n40C_1v95.hold_delay
create_analysis_view -name tt_025C_1v80.extra_view \
    -constraint_mode my_constraint_mode \
    -delay_corner tt_025C_1v80.extra_delay

## set_analysis_view
set_analysis_view -setup { ss_100C_1v60.setup_view } \
                  -hold { ff_n40C_1v95.hold_view tt_025C_1v80.extra_view } \
                  -leakage tt_025C_1v80.extra_view \
                  -dynamic tt_025C_1v80.extra_view
