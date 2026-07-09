# qspi_shared_bus — shared SCK/SIO bus, distinct CS

Regression for the CS-gated MISO arbitration on a **shared** QSPI bus (ADR 0013).

Two flash instances share one SCK + SIO bus and are selected by distinct chip
selects. In `sim_config.json` both memories use the same `clk_gpio` (2) and
`d0_gpio` (5) but distinct `csn_gpio` (3 vs 4). The DUT (`shared_bus_dut.v`) is
a single SPI master that reads address 0 from flash A, then from flash B, over
the shared bus.

The point: a **deselected** flash must present high-Z on the shared MISO. If it
drives (the pre-fix behaviour), it clobbers the selected flash's read data —
`gpu_apply_flash_din` / `CpuBackend::apply_flash_din_gated` iterate every
instance, so the last-iterated (often deselected) memory wins. The fix gates the
MISO injection on selection (`prev_csn` low). Flash A holds `0xA1`, flash B
holds `0xB2`; distinct reads prove no clobbering.

## Pass criterion

Byte-exact golden VCD diff (`expected/qspi_shared_bus.vcd`, captured on
CpuBackend, byte-identical to the Metal/CUDA/HIP kernels) plus a content
assertion (`check.py`): `fa == 0xA1`, `fb == 0xB2`, `all_match == 1`.

Runs in the `all` and `qspi` scopes of `scripts/ci/cosim_cpu_check.sh`, so it
gates CpuBackend (Linux) and the CUDA/HIP/Metal GPU suites alike.

## Regenerating

```bash
# Netlist (system Yosys):
cd tests/qspi_shared_bus && yosys -q -s synth.tcl

# Golden VCD (any backend — output is byte-identical across backends):
target/release/jacquard cosim tests/qspi_shared_bus/shared_bus_dut_synth.gv \
    --config tests/qspi_shared_bus/sim_config.json --top-module shared_bus_dut \
    --max-clock-edges 2000 --output-vcd tests/qspi_shared_bus/expected/qspi_shared_bus.vcd
```

Firmware (`fw/flashA.bin` = `0xA1…`, `fw/flashB.bin` = `0xB2…`) is 16 bytes each;
only byte 0 is read.
