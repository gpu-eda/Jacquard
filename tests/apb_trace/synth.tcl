# Yosys synthesis for APB3 bus-trace test design → AIGPDK
read_verilog apb_trace.v
hierarchy -check -top apb_trace
proc;;
opt_expr; opt_dff; opt_clean
synth -flatten
dfflibmap -liberty ../../aigpdk/aigpdk_nomem.lib
opt_clean -purge
abc -liberty ../../aigpdk/aigpdk_nomem.lib
opt_clean -purge
techmap
abc -liberty ../../aigpdk/aigpdk_nomem.lib
opt_clean -purge
write_verilog apb_trace_synth.gv
stat
