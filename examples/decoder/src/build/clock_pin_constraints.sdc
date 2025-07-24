create_clock clk -name clk -period 2.0
set_clock_uncertainty 0.01 [get_clocks clk]
set_clock_groups -asynchronous  -group { clk }
set_load 1.0 [all_outputs]
set_input_delay -clock clk 0 [all_inputs]
set_output_delay -clock clk 0 [all_outputs]
