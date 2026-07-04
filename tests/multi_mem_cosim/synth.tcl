# Yosys synth for the multi-memory cosim DUT.
# Logic → AIGPDK (nomem lib, no `$`-named cells); inferred 8192x32 memories →
# $__RAMGEM_SYNC_ via memlib. The RAMGEM cells pass through as blackbox
# instances (Jacquard cosim models them natively).
read_verilog multi_mem_dut.v
hierarchy -check -top multi_mem_dut
proc;;
flatten
opt_expr; opt_dff; opt_clean
memory_dff
memory_libmap -lib ../../aigpdk/memlib_yosys.txt
opt_clean
techmap
opt
dfflibmap -liberty ../../aigpdk/aigpdk_nomem.lib
abc -liberty ../../aigpdk/aigpdk_nomem.lib
opt_clean -purge
techmap
abc -liberty ../../aigpdk/aigpdk_nomem.lib
opt_clean -purge
write_verilog multi_mem_dut_synth.gv
stat
