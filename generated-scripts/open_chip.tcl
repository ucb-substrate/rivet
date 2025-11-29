# --------------------------------------------------------------------------------
# This script was written and developed by HAMMER at UC Berkeley; however, the
# underlying commands and reports are copyrighted by Cadence. We thank Cadence for
# granting permission to share our research to help promote and foster the next
# generation of innovators.
# --------------------------------------------------------------------------------

puts "set_db design_process_node 130" 
set_db design_process_node 130
puts "set_multi_cpu_usage -local_cpu 12" 
set_multi_cpu_usage -local_cpu 12
read_db latest