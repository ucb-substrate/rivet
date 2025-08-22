set_db design_process_node 130 
set_multi_cpu_usage -local_cpu 12
set_db timing_analysis_cppr both
set_db timing_analysis_type ocv

init_design
read_physical -lef {/scratch/cs199-cbc/labs/sp25-chipyard/vlsi/build/lab4/tech-sky130-cache/sky130_scl_9T.tlef  /home/ff/eecs251b/sky130/sky130_cds/sky130_scl_9T_0.0.5/lef/sky130_scl_9T.lef }
create_constraint_mode -name my_constraint_mode -sdc_files [list "/scratch/cs199-cbc/rivet/examples/decoder/src/syn-rundir/clock_pin_constraints.sdc"]
create_library_set -name ss_100C_1v60.setup_set -timing [list "/home/ff/eecs251b/sky130/sky130_cds/sky130_scl_9T_0.0.5/lib/sky130_ss_1.62_125_nldm.lib"]
create_timing_condition -name ss_100C_1v60.setup_cond -library_sets [list ss_100C_1v60.setup_set]
create_rc_corner -name ss_100C_1v60.setup_rc -temperature 100.0
create_delay_corner -name ss_100C_1v60.setup_delay -timing_condition ss_100C_1v60.setup_cond -rc_corner ss_100C_1v60.setup_rc
create_analysis_view -name ss_100C_1v60.setup_view -delay_corner ss_100C_1v60.setup_delay -constraint_mode my_constraint_mode
create_library_set -name ff_n40C_1v95.hold_set -timing [list "/home/ff/eecs251b/sky130/sky130_cds/sky130_scl_9T_0.0.5/lib/sky130_ff_1.98_0_nldm.lib"]
create_timing_condition -name ff_n40C_1v95.hold_cond -library_sets [list ff_n40C_1v95.hold_set]
create_rc_corner -name ff_n40C_1v95.hold_rc -temperature -40.0
create_delay_corner -name ff_n40C_1v95.hold_delay -timing_condition ff_n40C_1v95.hold_cond -rc_corner ff_n40C_1v95.hold_rc
create_analysis_view -name ff_n40C_1v95.hold_view -delay_corner ff_n40C_1v95.hold_delay -constraint_mode my_constraint_mode
create_library_set -name tt_025C_1v80.extra_set -timing [list "/home/ff/eecs251b/sky130/sky130_cds/sky130_scl_9T_0.0.5/lib/sky130_tt_1.8_25_nldm.lib"]
create_timing_condition -name tt_025C_1v80.extra_cond -library_sets [list tt_025C_1v80.extra_set]
create_rc_corner -name tt_025C_1v80.extra_rc -temperature 25.0
create_delay_corner -name tt_025C_1v80.extra_delay -timing_condition tt_025C_1v80.extra_cond -rc_corner tt_025C_1v80.extra_rc
create_analysis_view -name tt_025C_1v80.extra_view -delay_corner tt_025C_1v80.extra_delay -constraint_mode my_constraint_mode
set_analysis_view -setup { ss_100C_1v60.setup_view } -hold { ff_n40C_1v95.hold_view tt_025C_1v80.extra_view } -dynamic tt_025C_1v80.extra_view -leakage tt_025C_1v80.extra_view

read_netlist /syn-rundir/{module}.mapped.v -top decoder

set_db design_bottom_routing_layer 2
set_db design_top_routing_layer 6
set_db design_flow_effort standard
set_db design_power_effort low

write_db -to_file /scratch/cs199-cbc/rivet/examples/decoder/src/par-rundir/checkpoints/sky130_innovus_settings.cpf

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

write_db -to_file /scratch/cs199-cbc/rivet/examples/decoder/src/par-rundir/checkpoints/floorplan_design.cpf

source -echo -verbose /scratch/cs199-cbc/rivet/examples/decoder/src/par-rundir/floorplan.tcl 
read_power_intent -cpf /scratch/cs199-cbc/rivet/examples/decoder/src/par-rundir/power_spec.cpf
commit_power_intent


write_db -to_file /scratch/cs199-cbc/rivet/examples/decoder/src/par-rundir/checkpoints/sky130_connect_nets.cpf

connect_global_net VDD -type pg_pin -pin_base_name VPWR -all -auto_tie -netlist_override
connect_global_net VDD -type net    -net_base_name VPWR -all -netlist_override
connect_global_net VDD -type pg_pin -pin_base_name VPB -all -auto_tie -netlist_override
connect_global_net VDD -type net    -net_base_name VPB -all -netlist_override
connect_global_net VDD -type pg_pin -pin_base_name vdd -all -auto_tie -netlist_override
connect_global_net VDD -type net    -net_base_name vdd -all -netlist_override
connect_global_net VSS -type pg_pin -pin_base_name VGND -all -auto_tie -netlist_override
connect_global_net VSS -type net    -net_base_name VGND -all -netlist_override
connect_global_net VSS -type pg_pin -pin_base_name VNB -all -auto_tie -netlist_override
connect_global_net VSS -type net    -net_base_name VNB -all -netlist_override
connect_global_net VSS -type pg_pin -pin_base_name vss -all -auto_tie -netlist_override
connect_global_net VSS -type net    -net_base_name vss -all -netlist_override

write_db -to_file /scratch/cs199-cbc/rivet/examples/decoder/src/par-rundir/checkpoints/power_straps.cpf

#Power strap definition for layer met1:
set_db add_stripes_stacked_via_top_layer met1
set_db add_stripes_stacked_via_bottom_layer met1
set_db add_stripes_spacing_from_block 4.000
add_stripes -nets {VDD VSS} -layer met1 -direction horizontal -start_offset -.2 -width .4 -spacing 3.74 -set_to_set_distance 8.28 -start_from bottom -switch_layer_over_obs false -max_same_layer_jog_length 2 -pad_core_ring_top_layer_limit met5 -pad_core_ring_bottom_layer_limit met1 -block_ring_top_layer_limit met5 -block_ring_bottom_layer_limit met1 -use_wire_group 0 -snap_wire_center_to_grid none
#Power strap definition for layer met4:
set_db add_stripes_stacked_via_top_layer met4
set_db add_stripes_stacked_via_bottom_layer met1
set_db add_stripes_trim_antenna_back_to_shape {stripe}
set_db add_stripes_spacing_from_block 2.000
add_stripes -create_pins 0 -block_ring_bottom_layer_limit met4 -block_ring_top_layer_limit met1 -direction vertical -layer met4 -nets {VSS VDD} -pad_core_ring_bottom_layer_limit met1 -set_to_set_distance 75.90 -spacing 3.66 -switch_layer_over_obs 0 -width 1.86 -area [get_db designs .core_bbox] -start [expr [lindex [lindex [get_db designs .core_bbox] 0] 0] + 7.35]
#Power strap definition for layer met5:
set_db add_stripes_stacked_via_top_layer met5
set_db add_stripes_stacked_via_bottom_layer met4
set_db add_stripes_trim_antenna_back_to_shape {stripe}
set_db add_stripes_spacing_from_block 2.000
add_stripes -create_pins 1 -block_ring_bottom_layer_limit met5 -block_ring_top_layer_limit met4 -direction horizontal -layer met5 -nets {VSS VDD} -pad_core_ring_bottom_layer_limit met4 -set_to_set_distance 225.40 -spacing 17.68 -switch_layer_over_obs 0 -width 1.64 -area [get_db designs .core_bbox] -start [expr [lindex [lindex [get_db designs .core_bbox] 0] 1] + 5.62]

write_db -to_file /scratch/cs199-cbc/rivet/examples/decoder/src/par-rundir/checkpoints/place_pins.cpf

set_db assign_pins_edit_in_batch true
set_db assign_pins_promoted_macro_bottom_layer 1
set_db assign_pins_promoted_macro_top_layer 5
set all_ppins "" 
edit_pin -fixed_pin -pin * -hinst decoder -spread_type range -layer {met4} -side bottom -start {30 0} -end {0 0}   
if {[llength $all_ppins] ne 0} {assign_io_pins -move_fixed_pin -pins [get_db $all_ppins .net.name]}
set_db assign_pins_edit_in_batch false

write_db -to_file /scratch/cs199-cbc/rivet/examples/decoder/src/par-rundir/checkpoints/place_opt_design.cpf

set unplaced_pins [get_db ports -if {.place_status == unplaced}]
if {$unplaced_pins ne ""} {
    print_message -error "Some pins remain unplaced, which will cause invalid placement and routing. These are the unplaced pins: $unplaced_pins"
    exit 2
}
set_db opt_enable_podv2_clock_opt_flow true
place_opt_design

write_db -to_file /scratch/cs199-cbc/rivet/examples/decoder/src/par-rundir/checkpoints/add_fillers.cpf

set_db add_fillers_cells "FILL0 FILL1 FILL4 FILL9 FILL16 FILL25 FILL36"
add_fillers

write_db -to_file /scratch/cs199-cbc/rivet/examples/decoder/src/par-rundir/checkpoints/route_design.cpf

puts "set_db design_express_route true" 
set_db design_express_route true
puts "route_design" 
route_design

write_db -to_file /scratch/cs199-cbc/rivet/examples/decoder/src/par-rundir/checkpoints/opt_design.cpf

set_db opt_post_route_hold_recovery auto
set_db opt_post_route_fix_si_transitions true
set_db opt_verbose true
set_db opt_detail_drv_failure_reason true
set_db opt_sequential_genus_restructure_report_failure_reason true
opt_design -post_route -setup -hold -expanded_views -timing_debug_report

write_db -to_file /scratch/cs199-cbc/rivet/examples/decoder/src/par-rundir/checkpoints/write_regs.cpf

set write_cells_ir "./find_regs_cells.json"
set write_cells_ir [open $write_cells_ir "w"]
puts $write_cells_ir "\["

set refs [get_db [get_db lib_cells -if .is_sequential==true] .base_name]

set len [llength $refs]

for {set i 0} {$i < [llength $refs]} {incr i} {
    if {$i == $len - 1} {
        puts $write_cells_ir "    \"[lindex $refs $i]\""
    } else {
        puts $write_cells_ir "    \"[lindex $refs $i]\","
    }
}

puts $write_cells_ir "\]"
close $write_cells_ir
set write_regs_ir "./find_regs_paths.json"
set write_regs_ir [open $write_regs_ir "w"]
puts $write_regs_ir "\["

set regs [get_db [get_db [all_registers -edge_triggered -output_pins] -if .direction==out] .name]

set len [llength $regs]

for {set i 0} {$i < [llength $regs]} {incr i} {
    #regsub -all {/} [lindex $regs $i] . myreg
    set myreg [lindex $regs $i]
    if {$i == $len - 1} {
        puts $write_regs_ir "    \"$myreg\""
    } else {
        puts $write_regs_ir "    \"$myreg\","
    }
}

puts $write_regs_ir "\]"

close $write_regs_ir

write_db -to_file /scratch/cs199-cbc/rivet/examples/decoder/src/par-rundir/checkpoints/sky130_connect_nets.cpf

connect_global_net VDD -type pg_pin -pin_base_name VPWR -all -auto_tie -netlist_override
connect_global_net VDD -type net    -net_base_name VPWR -all -netlist_override
connect_global_net VDD -type pg_pin -pin_base_name VPB -all -auto_tie -netlist_override
connect_global_net VDD -type net    -net_base_name VPB -all -netlist_override
connect_global_net VDD -type pg_pin -pin_base_name vdd -all -auto_tie -netlist_override
connect_global_net VDD -type net    -net_base_name vdd -all -netlist_override
connect_global_net VSS -type pg_pin -pin_base_name VGND -all -auto_tie -netlist_override
connect_global_net VSS -type net    -net_base_name VGND -all -netlist_override
connect_global_net VSS -type pg_pin -pin_base_name VNB -all -auto_tie -netlist_override
connect_global_net VSS -type net    -net_base_name VNB -all -netlist_override
connect_global_net VSS -type pg_pin -pin_base_name vss -all -auto_tie -netlist_override
connect_global_net VSS -type net    -net_base_name vss -all -netlist_override

write_db -to_file /scratch/cs199-cbc/rivet/examples/decoder/src/par-rundir/checkpoints/write_design.cpf

set_db timing_enable_simultaneous_setup_hold_mode true
write_db decoder_FINAL -def -verilog
set_db write_stream_virtual_connection false
connect_global_net VDD -type net -net_base_name VPWR
connect_global_net VDD -type net -net_base_name VPB
connect_global_net VDD -type net -net_base_name vdd
connect_global_net VSS -type net -net_base_name VGND
connect_global_net VSS -type net -net_base_name VNB
connect_global_net VSS -type net -net_base_name vss
write_netlist /scratch/cs199-cbc/rivet/examples/decoder/src/par-rundir/decoder.lvs.v -top_module_first -top_module decoder -exclude_leaf_cells -phys -flat -exclude_insts_of_cells {}
write_netlist /scratch/cs199-cbc/rivet/examples/decoder/src/par-rundir/decoder.sim.v -top_module_first -top_module decoder -exclude_leaf_cells -exclude_insts_of_cells {}
write_stream -mode ALL -format stream -map_file /scratch/cs199-cbc/labs/sp25-chipyard/vlsi/hammer/hammer/technology/sky130/sky130_lefpin.map -uniquify_cell_names -merge { /home/ff/eecs251b/sky130/sky130_cds/sky130_scl_9T_0.0.5/gds/sky130_scl_9T.gds }  /scratch/cs199-cbc/rivet/examples/decoder/src/par-rundir/decoder.gds
write_sdf -max_view ss_100C_1v60.setup_view -min_view ff_n40C_1v95.hold_view -typical_view tt_025C_1v80.extra_view /scratch/cs199-cbc/rivet/examples/decoder/src/par-rundir/decoder.par.sdf
set_db extract_rc_coupled true
extract_rc
write_parasitics -spef_file /scratch/cs199-cbc/rivet/examples/decoder/src/par-rundir/decoder.ss_100C_1v60.par.spef -rc_corner ss_100C_1v60.setup_rc
write_parasitics -spef_file /scratch/cs199-cbc/rivet/examples/decoder/src/par-rundir/decoder.ff_n40C_1v95.par.spef -rc_corner ff_n40C_1v95.hold_rc
write_parasitics -spef_file /scratch/cs199-cbc/rivet/examples/decoder/src/par-rundir/decoder.tt_025C_1v80.par.spef -rc_corner tt_025C_1v80.extra_rc

exit
