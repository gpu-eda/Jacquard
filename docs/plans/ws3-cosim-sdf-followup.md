# Plan — WS3 follow-up: re-add cosim `--sdf` via `opensta-to-ir`

**Status:** Deferred. Tracked here so future work can pick it up.
**Predecessor:** WS3 phase 3.4 (deletes hand-rolled `src/sdf_parser.rs`).

## Background

Phase 3.4 deleted `src/sdf_parser.rs`. The `jacquard sim` subcommand kept
SDF input working (Phase 3.3 wired `--sdf` through
`setup::load_sdf_via_opensta_to_ir`, an internal subprocess wrapper that
calls the `opensta-to-ir` crate to convert SDF→IR). The `jacquard cosim`
subcommand chose **Option B** of the phase 3.4 handoff: drop `--sdf`
entirely rather than thread `--liberty` through. As a result, cosim now
only accepts pre-converted IR via `--timing-ir`.

## What was removed in 3.4

- `CosimArgs::sdf`, `sdf_corner`, `sdf_debug` CLI fields
  (`src/bin/jacquard.rs`).
- The `config.timing.sdf_file` / `sdf_corner` fallback path in
  `src/sim/cosim_metal.rs::run_cosim`.
- `TimingSimConfig::sdf_file` and `sdf_corner` JSON fields
  (`src/testbench.rs`).

## User-facing migration (current state)

The `tests/mcu_soc/` cosim flow that used to load SDF via the testbench
config now needs an explicit pre-conversion step.

### Feed `6_final.v` directly to `opensta-to-ir`

**Retraction (2026-05-18).** Earlier versions of this section
recommended feeding `tests/mcu_soc/data/top_synth.v` (post-synthesis,
pre-P&R) to `opensta-to-ir` to dodge a parse error on `6_final.v`'s
chipflow integration wrapper. That was wrong: `top_synth.v` is
missing the ~236K cells P&R inserts (`clkbuf_regs_*` CTS buffers,
`ANTENNA_*` diodes, `delaybuf_*`, fillers), so OpenSTA silently drops
every SDF entry referencing a P&R-inserted cell and the resulting IR
is missing the bulk of the design's timing. The "28162 matched /
2090 unmatched" verification log we celebrated at the time measured
jtir records against the cosim-loaded netlist, not SDF coverage
against the jtir — high surface match rate, materially incomplete
IR. See **ADR 0009** (OpenSTA Verilog reader input constraints) for
the broader rule.

`opensta-to-ir` now transparently extracts `module <--top>` from each
input file before invoking OpenSTA (implementation in
`crates/opensta-to-ir/src/verilog_filter.rs`). For the chipflow
mcu_soc case this strips the `openframe_project_wrapper` module
automatically; the same handling kicks in for any LibreLane +
wafer.space user (hazard3 and future tapeouts) whose final netlist
carries an integration wrapper around the structural top.

```sh
# Convert SDF → IR once. Pass 6_final.v directly; the wrapper module
# is dropped automatically.
opensta-to-ir \
    --liberty /path/to/sky130_fd_sc_hd__tt_025C_1v80.lib \
    --verilog tests/mcu_soc/data/6_final.v \
    --sdf tests/mcu_soc/data/6_final.sdf \
    --top top \
    --output tests/mcu_soc/data/6_final.jtir

# Run cosim with the pre-converted IR. Cosim loads 6_final.v (the
# wrapper) because that's what carries GPIO ports. The IR consumer's
# hierarchy-prefix detection strips the `top_inst/` prefix from the
# wrapper's cell paths so they match the IR's instance names.
cargo run -r --features metal --bin jacquard -- cosim \
    tests/mcu_soc/data/6_final.v \
    --config tests/mcu_soc/sim_config_sky130.json \
    --top-module openframe_project_wrapper \
    --timing-ir tests/mcu_soc/data/6_final.jtir
```

`tests/mcu_soc/sim_config_sky130.json` no longer carries `sdf_file` /
`sdf_corner` (the fields would be silently ignored if added back;
cosim does not consume them).

## Events-reference comparison: nuances

`tests/mcu_soc/events_reference.json` was wired into the sky130 cosim
config as part of phase 3.4 verification. End-to-end pipeline result
on a 3M-tick run:

- 67 UART bytes captured; the reference's 155 UART events end at
  timestamp 4,187,182. All 67 captured payloads match the reference's
  leading bytes (decoded UART output: `....: nyaa~!\nSoC type:
  CA7F100F\nFlash ID: CA7CA7FF\nQuad mode`). No payload divergence.
- 15 non-UART entries in the reference (cxxrtl-emitted SPI `deselect`
  events with `payload: ""`) are filtered out at parse time by the
  tolerant deserializer in `cosim_metal.rs::run_cosim`. Without that
  filter the comparison panicked on the first SPI entry.

### chipflow's `num_steps` and `timestamp` are edge-counted

**Retraction.** Earlier drafts of this section claimed Jacquard's
`--max-cycles` counts half-cycles. That was a misdiagnosis based on
reading `MultiClockScheduler::new` (which does emit per-edge raw
entries) without noticing the pairing layer at
`src/sim/cosim_metal.rs:2604-2675` that collapses them into one
paired buffer per cycle. Today, **`--max-cycles N` correctly counts
N full clock cycles**: each cosim tick does one fall-edge dispatch
plus one rise-edge dispatch and DFFs capture once per tick. Verified
via `--stimulus-vcd` trace (5 ticks → simulated time spans 0–200000
ps for a 40 ns period clock, exactly 5 cycles).

The actual unit difference vs chipflow's cxxrtl harness:

- chipflow's `num_steps` is the count of `tick()` calls; each `tick()`
  bumps `++timestamp` twice (once after the negedge dispatch, once
  after the posedge), so the `events_reference.json` `timestamp`
  field counts **clock edges** (a full cycle = posedge-to-posedge =
  2 edges). The harness:
  ```cpp
  auto tick = [&]() {
      {{interface}}.step(timestamp);
      top.clk.set(false); agent.step(); ++timestamp;  // post-negedge (odd)
      top.clk.set(true);  agent.step(); ++timestamp;  // post-posedge (even)
  };
  for (int i = 0; i < num_steps; i++) tick();
  ```
  See `chipflow-lib/chipflow/common/sim/main.cc.jinja:32-74`.
- The half-tick timestamp is an **intentional design**, not a bug:
  parity tags each event with the clock phase it fired on (useful for
  verification of async paths).
- chipflow's `num_steps` therefore doubles as an edge budget: 3 M
  num_steps = 3 M edges = 1.5 M full clock cycles.

To compare a Jacquard cosim run against today's `events_reference.json`,
divide reference timestamps by 2 to convert edges → cycles. Empirical
spot-check on mcu_soc/sky130: byte-0 in Jacquard at `--max-cycles
200000` arrives at tick 28682; reference timestamp 58290 / 2 = 29145
cycles; ratio 0.984× (simulators agree on simulated time within 2%).

The earlier "67 of 155 events captured" gap is **not** a budget
issue — chipflow drives input stimulus via `design/tests/input.json`
and reference events 69+ require those driven inputs. The input-stimulus
dispatcher was added in commit `4a1a989`, and the mcu_soc/sky130 cosim
now matches the cut-down chipflow reference 1:1 (90/90 events).

The earlier "Jacquard ~14% slower per byte than cxxrtl" claim relied
on a phantom half-cycle correction; it is also retracted. There is
no rate gap to explain at this level.

### Done: `--max-cycles` renamed to `--max-clock-edges` (commit `46b5c28`)

Cosim's internal granularity moved from full clock cycles to scheduler
edges, aligning Jacquard's CLI 1:1 with chipflow's `num_steps` and
unlocking per-edge event timestamping. Section retained for context on
the unit conventions captured above.

## Option A — restore cosim `--sdf` ergonomics

When this becomes a priority, mirror the `jacquard sim` surface:

### Changes

1. **Add `--liberty` to `CosimArgs`** (`src/bin/jacquard.rs`). Plumb it
   through `DesignArgs::liberty` (currently hardcoded `None` in
   `cmd_cosim`). Also passthrough `--top-module` if not already.
2. **Add `--sdf`, `--sdf-corner`, `--sdf-debug` back to `CosimArgs`**.
   Make them mutually exclusive with `--timing-ir` (clap
   `conflicts_with = "timing_ir"`).
3. **Re-add `TimingSimConfig::sdf_file` / `sdf_corner` (optional)** —
   plus a new `liberty_file` field for the OpenSTA invocation. Update
   `tests/mcu_soc/sim_config_sky130.json` to use the new shape.
4. **Restore the cosim config-file fallback**: in
   `src/sim/cosim_metal.rs::run_cosim`, when timing is not yet enabled
   and the config provides SDF + Liberty paths, call
   `setup::load_sdf_via_opensta_to_ir`. Match the priority order:
   CLI > config.timing.* > nothing.
5. **Update `--timing-vcd` error message** to mention `--sdf` again.

### Out of scope for Option A

- Rebuilding a hand-rolled SDF parser. (See ADR 0006 — the durable
  replacement is the native Rust SDF→IR converter, tracked separately as
  Phase 3 in the original phasing.)
- Adding cosim-specific corner-selection beyond what `jacquard sim`
  already offers. The IR's `min/typ/max` triple is selected via
  `ir_corner0_max` (currently always `max`); changing that is a separate
  concern that affects both subcommands.

## Verification

After Option A lands:

```sh
cargo build --features metal
cargo test --lib
# Manual smoke test of the previous mcu_soc workflow:
cargo run -r --features metal --bin jacquard -- cosim \
    tests/mcu_soc/data/6_final.v \
    --config tests/mcu_soc/sim_config_sky130.json \
    --liberty <path>/sky130.lib \
    --sdf tests/mcu_soc/data/6_final.sdf
```

Should produce equivalent results to the pre-3.4 hand-rolled-parser
path within the IR's representational bounds (single-value
interconnect delays, `max` corner selection).

## Walk-back

If Option A is never picked up before first release, the existing IR-only
cosim surface is fine — contributors using SDF can pre-convert via
`opensta-to-ir` and pass the resulting `.jtir`. The follow-up exists as a
contributor-ergonomics improvement, not a correctness gap.
