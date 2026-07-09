# Yosys synth for the ÷2 derived-clock DUT (issue #185 minimal repro).
read_verilog div2_clock_dut.v
hierarchy -check -top div2_clock_dut
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
write_verilog div2_clock_dut_synth.gv
stat
