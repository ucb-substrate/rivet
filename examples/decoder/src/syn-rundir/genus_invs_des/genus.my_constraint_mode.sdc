# ####################################################################

#  Created by Genus(TM) Synthesis Solution 21.17-s066_1 on Fri Aug 22 11:46:09 PDT 2025

# ####################################################################

set sdc_version 2.0

set_units -capacitance 1000fF
set_units -time 1000ps

# Set the current design
current_design decoder

create_clock -name "clk" -period 2.0 -waveform {0.0 1.0} [get_ports clk]
set_load -pin_load 1.0 [get_ports {Z[15]}]
set_load -pin_load 1.0 [get_ports {Z[14]}]
set_load -pin_load 1.0 [get_ports {Z[13]}]
set_load -pin_load 1.0 [get_ports {Z[12]}]
set_load -pin_load 1.0 [get_ports {Z[11]}]
set_load -pin_load 1.0 [get_ports {Z[10]}]
set_load -pin_load 1.0 [get_ports {Z[9]}]
set_load -pin_load 1.0 [get_ports {Z[8]}]
set_load -pin_load 1.0 [get_ports {Z[7]}]
set_load -pin_load 1.0 [get_ports {Z[6]}]
set_load -pin_load 1.0 [get_ports {Z[5]}]
set_load -pin_load 1.0 [get_ports {Z[4]}]
set_load -pin_load 1.0 [get_ports {Z[3]}]
set_load -pin_load 1.0 [get_ports {Z[2]}]
set_load -pin_load 1.0 [get_ports {Z[1]}]
set_load -pin_load 1.0 [get_ports {Z[0]}]
set_clock_groups -name "clock_groups_clk_to_others" -asynchronous -group [get_clocks clk]
set_clock_gating_check -setup 0.0 
set_input_delay -clock [get_clocks clk] -add_delay 0.0 [get_ports {A[3]}]
set_input_delay -clock [get_clocks clk] -add_delay 0.0 [get_ports {A[2]}]
set_input_delay -clock [get_clocks clk] -add_delay 0.0 [get_ports {A[1]}]
set_input_delay -clock [get_clocks clk] -add_delay 0.0 [get_ports {A[0]}]
set_output_delay -clock [get_clocks clk] -add_delay 0.0 [get_ports {Z[15]}]
set_output_delay -clock [get_clocks clk] -add_delay 0.0 [get_ports {Z[14]}]
set_output_delay -clock [get_clocks clk] -add_delay 0.0 [get_ports {Z[13]}]
set_output_delay -clock [get_clocks clk] -add_delay 0.0 [get_ports {Z[12]}]
set_output_delay -clock [get_clocks clk] -add_delay 0.0 [get_ports {Z[11]}]
set_output_delay -clock [get_clocks clk] -add_delay 0.0 [get_ports {Z[10]}]
set_output_delay -clock [get_clocks clk] -add_delay 0.0 [get_ports {Z[9]}]
set_output_delay -clock [get_clocks clk] -add_delay 0.0 [get_ports {Z[8]}]
set_output_delay -clock [get_clocks clk] -add_delay 0.0 [get_ports {Z[7]}]
set_output_delay -clock [get_clocks clk] -add_delay 0.0 [get_ports {Z[6]}]
set_output_delay -clock [get_clocks clk] -add_delay 0.0 [get_ports {Z[5]}]
set_output_delay -clock [get_clocks clk] -add_delay 0.0 [get_ports {Z[4]}]
set_output_delay -clock [get_clocks clk] -add_delay 0.0 [get_ports {Z[3]}]
set_output_delay -clock [get_clocks clk] -add_delay 0.0 [get_ports {Z[2]}]
set_output_delay -clock [get_clocks clk] -add_delay 0.0 [get_ports {Z[1]}]
set_output_delay -clock [get_clocks clk] -add_delay 0.0 [get_ports {Z[0]}]
set_clock_uncertainty -setup 0.01 [get_clocks clk]
set_clock_uncertainty -hold 0.01 [get_clocks clk]
