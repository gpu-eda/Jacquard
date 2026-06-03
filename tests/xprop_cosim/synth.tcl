# Yosys synthesis for the X-propagation cosim demo → AIGPDK
read_verilog xprop_demo.v
hierarchy -check -top xprop_demo
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
write_verilog xprop_demo_synth.gv
stat
