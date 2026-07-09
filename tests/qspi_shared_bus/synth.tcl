# Yosys synth for the shared-bus QSPI cosim DUT.
# Pure logic → AIGPDK (nomem lib): the DUT has no on-chip memory (the two
# flashes are external cosim models), so no memlib step is needed.
read_verilog shared_bus_dut.v
hierarchy -check -top shared_bus_dut
proc;;
flatten
opt_expr; opt_dff; opt_clean
techmap
opt
dfflibmap -liberty ../../aigpdk/aigpdk_nomem.lib
abc -liberty ../../aigpdk/aigpdk_nomem.lib
opt_clean -purge
techmap
abc -liberty ../../aigpdk/aigpdk_nomem.lib
opt_clean -purge
write_verilog shared_bus_dut_synth.gv
stat
