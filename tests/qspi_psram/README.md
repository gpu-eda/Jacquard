# QSPI PSRAM (RAM-mode flash) cosim test

Validates the writable QSPI-PSRAM extension of Jacquard's SPI-flash GPU
peripheral — the APS6404L-class model used for post-PnR gate-level timing
cosim of a chip whose main RAM is external QSPI PSRAM. See the flash
peripheral in `src/sim/cosim/` (`FlashConfig::writable`, `FlashState::qpi`,
the `enter_qpi` / `quad_write` kernel paths) and ADR 0017 for the cosim
execution model.

## Design (`qspi_psram_dut.v`)

A minimal QSPI PSRAM host that plays a fixed micro-program against the model:

1. **Enter QPI** — `0x35`, single-lane. Latches the model into 4-lane mode.
2. **Quad write** — `0x38`, address `0x000001`, data `0xA5`.
3. **Quad read** — `0xEB`, address `0x000001`, 6 dummy SCK cycles, capture.
4. Assert the captured byte `rdata == 0xA5` via the `match` output.

Wire protocol matches the cocotb reference (`qspi_psram_model.py`) and the
`qspi_psram_ctrl` controller: SPI mode 0, MSB/high-nibble first, host samples
MISO on rising SCK / the model drives MISO on falling SCK, `0xEB` has 6 dummy
cycles (`QRD_DUMMY = 6`). One SPI clock period = two system-clock cycles so
the GPU model (which steps twice per system tick) sees one clean rising/falling
SCK edge per SPI cycle.

The `flash` peripheral is configured in RAM mode:

```json
"flash": {
    "clk_gpio": 2, "csn_gpio": 3, "d0_gpio": 4,
    "writable": true, "enter_qpi_cmd": 53, "quad_write_cmd": 56,
    "read_dummy_cycles": 6, "size_bytes": 65536
}
```

`d0_gpio = 4` indexes both the `sio_o` outputs (MOSI, which the model samples)
and the `sio_i` inputs (MISO, which the model drives) — the same shared-pad
convention the flash config uses.

## Build + run

```bash
# Synthesize to AIGPDK (committed as qspi_psram_dut_synth.gv)
yosys -s synth.tcl                     # run from tests/qspi_psram/

# Cosim → VCD, then assert the write→read round-trip
cargo run -r --features metal --bin jacquard -- cosim \
    tests/qspi_psram/qspi_psram_dut_synth.gv \
    --config tests/qspi_psram/sim_config.json \
    --top-module qspi_psram_dut \
    --max-clock-edges 200 \
    --output-vcd target/test-out/qspi_psram.vcd

python3 tests/qspi_psram/check.py target/test-out/qspi_psram.vcd
```

The committed golden `expected/qspi_psram.vcd` was captured on `CpuBackend`
and is byte-identical to the Metal flash kernel (cross-backend gate in
`scripts/ci/cosim_cpu_check.sh`, scope `qspi`). Backward compatibility: with
`writable` unset the flash peripheral is byte-for-byte the original SPI flash,
so the `mcu_soc` flash golden is unaffected.

## Notes

- `size_bytes` documents the RAM size for address bounds; the backing store is
  the shared 16 MiB flash buffer, zero-filled at power-on in RAM mode (matching
  the cocotb `bytearray(size)` init) instead of `0xFF` erased-flash.
- `--check-with-cpu` reports internal-net mismatches for this design (clocked
  DFFs sampled at opposite phases that reconverge) exactly as the `apb_trace`
  fixture does; the top-level IO VCD is byte-identical across backends, which
  is the committed gate.
