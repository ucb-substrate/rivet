################################################################################
#
# Init setup file
# Created by Genus(TM) Synthesis Solution on 08/22/2025 11:46:09
#
################################################################################
if { ![is_common_ui_mode] } { error "ERROR: This script requires common_ui to be active."}
::legacy::set_attribute -quiet init_mmmc_version 2 /

read_mmmc genus_invs_des/genus.mmmc.tcl

read_physical -lef {/scratch/cs199-cbc/labs/sp25-chipyard/vlsi/build/lab4/tech-sky130-cache/sky130_scl_9T.tlef /home/ff/eecs251b/sky130/sky130_cds/sky130_scl_9T_0.0.5/lef/sky130_scl_9T.lef}

read_netlist genus_invs_des/genus.v.gz

read_power_intent  -cpf genus_invs_des/genus.cpf

init_design -skip_sdc_read
