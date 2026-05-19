# ADR 0009 — OpenSTA Verilog reader input constraints

**Status:** Accepted.

## Context

OpenSTA's `read_verilog` Tcl command is structural-only: it accepts cell
instantiations and bare-net `assign` statements but rejects RTL
operators (`~`, `&`, `|`, `^`), bit-selects in assigns, and ranged
concatenations. Violations surface as `Error: <file> line <N>, syntax
error` and exit 1. This is a long-standing OpenSTA limitation, not a
flag.

Two patterns make this surprising in practice — both have already
caught us once:

1. **Final-stage outputs from the LibreLane/OpenROAD flow are
   sometimes wrapped.** LibreLane itself only ever reads structural
   netlists (`<design>.pnl.v` — verified locally on
   `chip_top.pnl.v`: zero RTL operators, single module). The wrapping
   is added by downstream integration tooling — for the SkyWater
   openframe flow, chipflow's harness wraps the LibreLane output in
   `openframe_project_wrapper` to patch active-low OEB pins into the
   pad ring, producing the `assign gpio_oeb[0] = ~( ... );` pattern.
   The combined file (`tests/mcu_soc/data/6_final.v`) contains both
   the readable-by-OpenSTA structural `top` module *and* the
   wrapper's unreadable RTL. The SDF was generated against the inner
   `top`, not the wrapper — matching what LibreLane's own STA saw.

2. **Post-synthesis Verilog has the right form but the wrong cells.**
   Pre-P&R synthesis output (e.g. `top_synth.v`) is fully structural
   and uses the same module name `top` as the post-P&R body, so it
   *looks* like an acceptable substitute. It is not: the SDF
   references hundreds of thousands of P&R-inserted cells
   (`clkbuf_regs_*` CTS buffers, `ANTENNA_*` diodes, `delaybuf_*`,
   fillers) that simply do not exist in synthesis output. OpenSTA
   quietly drops SDF entries whose endpoints are not in the loaded
   design; the resulting IR back-annotates only the surviving subset.
   Concrete numbers from the MCU SoC fixture: `top_synth.v` has
   31,500 cells; `module top` inside `6_final.v` has 266,746. Feeding
   `top_synth.v` would silently drop ~88% of the design's structure.

Past convention (`docs/plans/ws3-cosim-sdf-followup.md`, pre
2026-05-18) recommended substituting `top_synth.v` to dodge the
wrapper-parse error. The contemporaneous verification log
(`28162 matched, 2090 unmatched`) reported the jtir-to-cosim-netlist
match rate, *not* SDF coverage against the jtir — high surface
"working" while the IR was missing most of the design's real
timing. That recommendation is retracted in the same change as this
ADR lands.

## Decision

The "structural-only" constraint is **owned by `opensta-to-ir`**,
not by the caller. Specifically:

1. **`opensta-to-ir` filters Verilog inputs at invocation time.** For
   each `--verilog` file, it extracts the `module <--top> …
   endmodule` block before handing files to OpenSTA. Files that do
   not contain `module <--top>` (sub-module-only files in
   hierarchical designs) are passed through unchanged. The wrapper
   modules that LibreLane + wafer.space integration adds — and any
   future analogues — are simply not seen by OpenSTA. Implementation
   in `crates/opensta-to-ir/src/verilog_filter.rs`; integration test
   coverage in `tests/opensta_integration.rs`.

2. **The cell-set match against the SDF is the caller's
   responsibility.** `opensta-to-ir` cannot determine programmatically
   whether a given Verilog input is the right design stage for a
   given SDF. The CI fixture comment in `prepare-mcu-soc-jtir`
   captures the rule for sky130 mcu_soc; copy the *spirit* (use the
   post-P&R structural body, not synthesis output) when adding new
   fixtures, but don't copy a per-design extraction recipe — there
   no longer is one to copy.

**Architectural alternative (separate concern):** the upstream
chipflow harness could preserve LibreLane's pre-wrap `<top>.pnl.v`
alongside its wrapped `<top>_final.v` output. That would make
`opensta-to-ir`'s in-tool extraction a no-op for the common chipflow
case, but it would not obviate the filter — third-party LibreLane +
wafer.space users (hazard3 and future tapeouts using the vanilla
flow) hit the same wrapper pattern. The filter is the right place
for the fix because it covers both `opensta-to-ir` as a CLI and
`jacquard sim --sdf` (which subprocesses `opensta-to-ir`).

## Consequences

- End-user runs of `jacquard sim --sdf <path>` and the standalone
  `opensta-to-ir` tool both transparently handle the LibreLane +
  wafer.space wrapper pattern. No flags, no preprocessing recipe in
  user-facing docs.
- Match-rate metrics in the IR consumer measure jtir coverage
  against the *consuming* netlist, not against the source SDF. A
  high match rate is necessary but not sufficient — confirm the jtir
  contains the post-P&R cell population separately (e.g. by spot
  checking for `clkbuf_regs_*` / `ANTENNA_*` arcs in the IR JSON
  sidecar) before declaring a flow "working".
- The filter assumes `module <--top> … endmodule` is line-anchored
  in the Verilog source. Machine-generated post-P&R netlists meet
  this; hand-rolled Verilog that opens a module mid-line would not.
  If that ever surfaces, upgrade the filter to use a real Verilog
  tokenizer (`sverilogparse` is already a workspace dependency).
- This ADR retroactively retracts the `top_synth.v` recommendation
  in `docs/plans/ws3-cosim-sdf-followup.md`; that doc is corrected
  in the same change.

## Links

- ADR 0001 — OpenSTA as oracle and sole STA path (the upstream
  tool whose constraints these are).
- ADR 0006 — SDF preprocessing model (the surrounding flow that
  consumes these inputs).
- `docs/plans/ws3-cosim-sdf-followup.md` — the prior workaround
  this ADR corrects.
