# `jtag_minimal` — JTAG cosim regression

End-to-end regression for `jacquard cosim --jtag-replay`: a minimal
RISC-V Debug Module driven by a recorded `remote_bitbang` byte stream.
Catches TAP-traversal / DMI-decode / CDC regressions that the
synthetic JtagReplayModel unit tests don't cover, without needing
a CPU or memory backing.

## Design

`rtl/top.sv` instantiates:

- `hazard3_jtag_dtm`  (vendored from Wren6991/Hazard3, Apache-2.0) — TCK-domain TAP
  state machine + DMI APB master. 5-bit IR; IDCODE = `0xdeadbeef`.
- `hazard3_dm` (also vendored, Apache-2.0) with `N_HARTS=1` — chip-clock-domain DMI
  APB slave. Decodes DMCONTROL, DMSTATUS, ABSTRACTCS, DATA0..2.
- A **fake hart stub**: `hart_halted = hart_req_halt`, so DM thinks a
  hart is present and responds to halt requests instantly. No CPU.
  This is enough for OpenOCD's `riscv` driver to progress past
  examination and write DMI registers.

Observable outputs (registered via `--trace-signals` to land in the
output VCD):

| Pin              | Source              | Verifies                              |
|------------------|---------------------|---------------------------------------|
| `dmactive_obs`   | `u_dm.dmactive`     | First DMI write completed             |
| `haltreq_obs`    | `hart_req_halt[0]`  | DMCONTROL.haltreq sequence            |
| `data0_obs[31:0]`| `hart_data0_rdata`  | DATA0 register written via DMI        |

## Expected bitbang sequence

`scripts/capture_bitbang.py` records the stream produced by a real
OpenOCD `init; reset halt; write DATA0=0xCAFEBABE; shutdown` against
the RTL through Icarus. The captured stream lives at
`data/bitbang.rec` and is committed to the repo (regenerate when the
RTL changes).

Pass criterion at end of cosim: `data0_obs == 32'hCAFEBABE`.

## Flow

```
rtl/top.sv  →  Yosys synth (gf180mcu_fd_sc_mcu9t5v0)  →  data/top.pnl.v
                                                              │
data/bitbang.rec  ─────────────────────────────────────────────┤
                                                              ▼
                                                  jacquard cosim
                                                              │
                                              data0_obs in VCD
                                                              │
                                              CI assertion: == 0xCAFEBABE
```

## Files

| File                                  | Purpose                                       |
|---------------------------------------|-----------------------------------------------|
| `rtl/top.sv`                          | Test design wrapper                           |
| `rtl/hazard3_compat.vh`               | Empty-define shim for `HAZARD3_REG_KEEP_ATTRIBUTE` |
| `rtl/hazard3_jtag_dtm.v`              | Vendored DTM (Wren6991/Hazard3, Apache-2.0)   |
| `rtl/hazard3_jtag_dtm_core.v`         | Vendored DTM core (Apache-2.0)                       |
| `rtl/hazard3_apb_async_bridge.v`      | Vendored APB CDC bridge (Apache-2.0) — unused here but kept for variant tests |
| `rtl/hazard3_sync_1bit.v`             | Vendored 2FF sync (Apache-2.0)                       |
| `rtl/hazard3_dm.v`                    | Vendored DM (Apache-2.0)                             |
| `scripts/capture_bitbang.py`          | Generates `data/bitbang.rec` from a cocotb RTL run |
| `data/top.pnl.v`                      | Yosys post-synth flat netlist (committed; regen on RTL change) |
| `data/bitbang.rec`                    | Captured OpenOCD `remote_bitbang` byte stream |
| `sim_config.json`                     | Jacquard cosim config (clock, reset, JTAG, observables) |
| `openocd.cfg`                         | OpenOCD config used by `capture_bitbang.py`   |

## What this catches

The regression test is sensitive to:

- **JtagReplayModel TCK/TMS pacing bugs** (e.g. #84) — TAP would get
  stuck and DATA0 would stay at 0.
- **`extra_observable_names` resolution for `<top>.<dm>.dmactive`** —
  `--trace-signals` must surface internal hierarchical nets.
- **APB-side DMI handling in `cosim_metal.rs`** — DMI psel/penable
  must be driven from the DTM through to the DM and prdata returned
  in time.
- **Reset-domain handling**: TAP exits TLR only after a clean TRST
  pulse; DM only responds when chip rst_n is deasserted.

## What it does *not* catch

- Memory abstract commands (no SRAM backing the hart)
- Multi-hart hartsel logic
- System Bus Access (HAVE_SBA=0)
- Anything CPU-instruction-execution-related

Those would belong in a separate `tests/jtag_with_memory/` regression
in the future, layered on top of this one.
