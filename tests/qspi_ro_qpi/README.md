# qspi_ro_qpi — read-only QPI flash serving preloaded firmware

Minimal repro for [#222](https://github.com/gpu-eda/Jacquard/issues/222): QPI-mode
entry is gated on `writable`, so a **read-only** QPI flash can never enter QPI and
its reads return zeros.

Writability and QPI capability are independent properties of a real part. QPI NOR
flash (W25Q-class with QE set) is read-only *and* speaks QPI: `0x38` to enter,
then `0xEB` quad reads. Today that part cannot be modelled — `enter_qpi_cmd` is
silently ignored unless the instance also has a writable store.

## What it does

Reuses the `tests/qspi_psram` DUT **unchanged** (no new Verilog, no new synth).
That DUT enters QPI (`0x35`), quad-writes `0xA5` to address `0x000001`, quad-reads
address `0x000001`, and asserts `rdata == 0xA5`.

The only difference here is the config: `fw/ro_flash.bin` already holds `0xA5` at
address `0x000001`, and the flash is `writable = false`. The DUT's write is
therefore **redundant** — a correct read-only QPI part must ignore the write and
still serve `0xA5` from its preloaded content. The read can only succeed by
entering QPI and serving firmware.

`read_dummy_cycles` is set explicitly (6) because `flash_ram_params`
(`src/sim/cosim/mod.rs`) also defaults it off `writable`
(`.unwrap_or(if cfg.writable { 6 } else { 0 })`). Setting it isolates the
QPI-entry bug from a dummy-cycle mismatch.

## Running

```bash
jacquard cosim --config tests/qspi_ro_qpi/sim_config.json \
    tests/qspi_psram/qspi_psram_dut_synth.gv \
    --top-module qspi_psram_dut --max-clock-edges 200 \
    --output-vcd /tmp/qspi_ro_qpi.vcd
python3 tests/qspi_ro_qpi/check.py /tmp/qspi_ro_qpi.vcd
```

## Pass criterion

`rdata == 0xA5`, `match == 1`, `done == 1` (see `check.py`).

## Current status — this test FAILS on `main`

It is a bug repro, not yet a green regression test. On `main` it reports:

```
[WARN] GPU flash model encountered unknown command: 0x41
GPU Flash model: command=0x41, byte_count=0, addr=0x000000, error_cmd=0x41
FAIL: expected rdata=0xA5, match=1, done=1 (got rdata=0x00, match=0, done=1)
```

`0x41` is the 4-lane `0xEB` sampled as if single-lane: QPI never latched, so
`data_width` stayed 1, the command decode went off the rails, `out_buffer` never
loaded, and MISO stayed 0.

Wire it into `scripts/ci/cosim_cpu_check.sh` alongside `qspi_psram` once the fix
lands — adding it before then would turn CI red.

## Verified fix

Dropping the `ram_writable` conjunct from the enter-QPI branch
(`csrc/kernel_v1.metal:830`) flips this fixture to PASS and leaves the existing
`qspi_psram` fixture passing:

```c
if (enter_qpi_cmd != 0xFFFFFFFFu && (uint)command == enter_qpi_cmd) { qpi = 1u; }
```

`ram_writable` should keep gating the quad-*write* branches (lines 834 and 884) —
that is the property it actually describes. The CpuBackend mirror and the
CUDA/HIP kernels need the same change.

## Coverage gap this closes

| fixture | writable | QPI | firmware |
|---|---|---|---|
| `qspi_psram` | yes | yes | no (writes then reads back) |
| `qspi_shared_bus` | no | no | yes (plain `0x03` reads) |
| **`qspi_ro_qpi`** | **no** | **yes** | **yes** |

## Provenance

Found bringing up the C64 tapeout's post-PnR cosim, which boots from an external
read-only QPI NOR flash sharing a bus with a PSRAM (ADR 0005). Modelling that ROM
currently requires `writable: true` purely to buy QPI entry.
