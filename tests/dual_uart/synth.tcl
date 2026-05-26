# Yosys synthesis for dual-UART test design → AIGPDK
read_verilog dual_uart.v
hierarchy -check -top dual_uart_top
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
write_verilog dual_uart_synth.gv
stat
