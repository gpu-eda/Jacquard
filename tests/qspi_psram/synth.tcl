# Yosys synthesis for the QSPI PSRAM cosim DUT → AIGPDK
read_verilog qspi_psram_dut.v
hierarchy -check -top qspi_psram_dut
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
write_verilog qspi_psram_dut_synth.gv
stat
