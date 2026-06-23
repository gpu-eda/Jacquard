# Cell-model IR — staged delivery plan

**Status:** Proposed — not started. Realises
[ADR 0019](../adr/0019-cell-model-ir.md). Tracks
[#130](https://github.com/gpu-eda/Jacquard/issues/130) and
[#67](https://github.com/gpu-eda/Jacquard/issues/67).

**Goal:** make Jacquard core consume a single, generated, JSON cell-model
IR for cell logic (L2 combinational AIG + L3 sequential/classification),
so any library — including proprietary ones the authors can't vendor — is
selectable at runtime, and the per-PDK Rust + hardcoded `vendor/` paths
retire.

## Why staged

Each step is independently useful and de-risks the next: step 1 ships the
#130 fix and *validates the format on real cells* before the heavier L3
schema is committed; step 2 is the schema-fidelity work; step 3 is the
cleanup that the first two earn.

## Staged checkpoints (each ≈ one reviewable PR)

| # | Scope | Gate |
|---|---|---|
| **C1 — foundation + #130** | Relocate `pdk_decomp` into a shared lib. Define the cell-model-IR JSON schema for **L2 only** (combinational pre-built AIG, D3) + the shared identifier fragment with the timing IR (D1). Write the converter crate (Liberty `function`/`functional.v` → IR) for it. Redirect *one* PDK's (GF180MCU) stdcell logic to consume the IR. | A 9T netlist (e.g. `tests/jtag_minimal`) simulates against its **own** generated 9T descriptor; result byte-identical to the current 7T-substituted run where they truly agree, and the round-trip logic check passes. |
| **C2 — L3 sequential schema** | Add the D4 sequential pin-role schema (clock+edge, D/next-state, Q, async set/reset+polarity, enable) + classification kinds. Extend the converter to emit them from Liberty `ff`/`latch`. Wire the consumer to replace the hardcoded DFF pin-name matches (`src/aig.rs:2080-2260`). | A design with sequential GF180/SKY130 cells simulates from the IR with **no per-PDK Rust DFF handling**; equivalence vs the current hardcoded path (the oracle). |
| **C3 — bundle + cut over + selection** | Generate bundled descriptors for all built-in PDKs (AIGPDK/SKY130/GF180), check them in. Selection by descriptor-declared prefix + `--cell-descriptor` (D7). Drop the runtime `vendor/` cell dependency and `pdk_decomp` from core; retire `gf180mcu*.rs`/`sky130*.rs` classifiers, `PdkVariant`, and the `build.rs` pin-table generators. | The existing PDK regression suite passes consuming **only** bundled descriptors; vendored cell submodules are no longer a build/runtime dependency of jacquard core. |
| **C4 (later)** | A documented proprietary-library workflow: user runs the generator on their own Liberty, gets a descriptor, simulates — no Jacquard build. Honesty fix: the round-trip logic check replaces `build.rs`'s port-only `assert_eq!`. | `docs/adding-a-pdk.md` recipe; a synthetic "private" library exercised end-to-end in a test. |

## Risks / open questions

- **Sequential fidelity (C2).** Liberty `ff` `clear`/`preset`/
  `clear_preset_var` → Jacquard async-reset DFF is the bug-prone mapping;
  gate it on equivalence against the current hardcoded behaviour.
- **L2 cells Liberty under-specifies.** Some structural/complex cells need
  the `functional.v` fallback; the converter takes both inputs (ADR 0019
  open question), so C1 must exercise a cell that *only* has `functional.v`.
- **AIG payload size (D2/D3).** If the JSON AIG is unwieldy for a full
  library, switch that payload to the FlatBuffers escape hatch — decide at
  C1 from the real GF180 descriptor size.
- **Migration ordering (C3).** Per-PDK cutover keeps the suite green
  throughout; a single switch is riskier. Default to per-PDK.
- **Identifier alignment (D1).** The shared cell/pin-name fragment must be
  fixed in C1 before two IRs exist that would diverge.

## References

- [ADR 0019 — Cell-model IR](../adr/0019-cell-model-ir.md).
- [ADR 0002 — Timing IR](../adr/0002-timing-ir.md) (the pattern + scope
  boundary this realises), [ADR 0010](../adr/0010-declarative-cell-metadata.md)
  / [ADR 0011](../adr/0011-ram-port-mapping-schema.md) (the declarative path
  this extends).
- Current state: `src/aig.rs:1895` (hardcoded 7T path), `build.rs`
  (port-only pin gen), `src/pdk.rs` / `src/gf180mcu_pdk.rs` (hardcoded
  classifiers), `src/liberty_parser.rs` (Liberty group-walker to extend),
  `crates/opensta-to-ir` + `crates/timing-ir` (the sibling pattern).
