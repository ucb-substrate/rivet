set_db super_thread_debug_directory super_thread_debug
set_db hdl_error_on_blackbox true
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

write_db -to_file pre_read_design_files

puts "create_constraint_mode -name my_constraint_mode -sdc_files [list /home/ff/eecs251b/sp25-chipyard/vlsi/build/lab4/syn-rundir/clock_constraints_fragment.sdc /home/ff/eecs251b/sp25-chipyard/vlsi/build/lab4/syn-rundir/pin_constraints_fragment.sdc] "
create_constraint_mode -name my_constraint_mode -sdc_files "clock_pin_constraints.sdc"
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
set_analysis_view -setup { ss_100C_1v60.setup_view } -hold { ff_n40C_1v95.hold_view tt_025C_1v80.extra_view } -dynamic tt_025C_1v80.extra_view -leakage tt_025C_1v80.extra_view
read_physical -lef { /scratch/cs199-cbc/labs/sp25-chipyard/vlsi/build/lab4/tech-sky130-cache/sky130_scl_9T.tlef /home/ff/eecs251b/sky130/sky130_cds/sky130_scl_9T_0.0.5/lef/sky130_scl_9T.lef }
read_hdl -sv { decoder.v }


elaborate decoder
init_design -top decoder
write_db -to_file pre_power_intent

read_power_intent -cpf power_spec.cpf
apply_power_intent -summary
Commit_power_intent

write_db -to_file pre_syn_generic

syn_generic
write_db -to_file pre_syn_map

syn_map
write_db -to_file pre_add_tieoffs

set_db message:WSDF-201 .max_print 20
set_db use_tiehilo_for_const duplicate
set ACTIVE_SET [string map { .setup_view .setup_set .hold_view .hold_set .extra_view .extra_set } [get_db [get_analysis_views] .name]]
set HI_TIEOFF [get_db base_cell:TIEHI .lib_cells -if { .library.library_set.name == $ACTIVE_SET }]
set LO_TIEOFF [get_db base_cell:TIELO .lib_cells -if { .library.library_set.name == $ACTIVE_SET }]
add_tieoffs -high $HI_TIEOFF -low $LO_TIEOFF -max_fanout 1 -verbose

write_db -to_file pre_write_design

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
puts "write_reports -directory reports -tag final" 
write_reports -directory reports -tag final
puts "report_timing -unconstrained -max_paths 50 > reports/final_unconstrained.rpt" 
report_timing -unconstrained -max_paths 50 > reports/final_unconstrained.rpt

puts "write_hdl > decoder.mapped.v" 
write_hdl > decoder.mapped.v
puts "write_template -full -outfile decoder.mapped.scr" 
write_template -full -outfile decoder.mapped.scr
puts "write_sdc -view ss_100C_1v60.setup_view > decoder.mapped.sdc" 
write_sdc -view ss_100C_1v60.setup_view > decoder.mapped.sdc
puts "write_sdf > decoder.mapped.sdf" 
write_sdf > decoder.mapped.sdf
puts "write_design -gzip_files decoder" 
write_design -gzip_files decoder
    
quit
