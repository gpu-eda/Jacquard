# Cell-model IR — staged delivery plan

**Status:** Largely delivered. Realises
[ADR 0019](../adr/0019-cell-model-ir.md). Tracks
[#130](https://github.com/gpu-eda/Jacquard/issues/130) and
[#67](https://github.com/gpu-eda/Jacquard/issues/67).

- **C1 (foundation + #130)** — ✅ landed (#132). #130 is fixed by default: a 9T
  netlist auto-selects its own 9T descriptor.
- **C1b (converter generalization)** — ✅ landed (#155): corner from Liberty PVT,
  ff internal-state-var (`IQ`). Surfaced by running the converter against the
  proprietary GF130 (GF013BCD) library.
- **C2 (L3 sequential + L4 timing)** — ✅ landed (#132).
- **C3.1/C3.2 (build-time generation + prefix selection)** — ✅ landed (#132).
- **SKY130 `.lib.json` reader** — ✅ landed (#155): SKY130 ships only
  `.lib.json`, so `liberty-parse` reads it.
- **C3a (IHP SG13G2, zero-Rust new PDK)** — ✅ landed (#155): vendored as a
  sparse/shallow submodule + a generated descriptor, no `ihp_*.rs`.
- **C3.3 (cut over + drop runtime `vendor/` dep)** — ✅ landed (#160): the
  runtime binary is self-contained for standard cells (`load_pdk_models` has no
  runtime callers), and `PdkVariant` is deleted (`grep -rn PdkVariant src/` = 0).
- **C4 (proprietary workflow + `clear_preset`)** — 🟡 partial: `clear_preset`
  set-dominant field landed (#155); `docs/adding-a-pdk.md` reframed around the
  descriptor workflow (#155). The proprietary *sequential* end-to-end test and
  the round-trip honesty fix remain (see Remaining follow-ups).

**Goal:** make Jacquard core consume a single, generated, JSON cell-model
IR carrying *all* per-cell-type facts of a library — L1 directions, L2
combinational AIG, L3 sequential/classification, **and L4 timing
characterization** — so any library, including proprietary ones the
authors can't vendor, is selectable at runtime, and the per-PDK Rust, the
hardcoded `vendor/` paths, *and* the runtime `.lib` parse all retire.

## Why staged

Each step is independently useful and de-risks the next: step 1 ships the
#130 fix and *validates the format on real cells* before the heavier L3
schema is committed; step 2 is the schema-fidelity work; step 3 is the
cleanup that the first two earn.

## Staged checkpoints (each ≈ one reviewable PR)

| # | Scope | Gate |
|---|---|---|
| **C1 — foundation + #130** | Relocate `pdk_decomp` into a shared lib. Define the cell-model-IR JSON schema for **L1 directions + L2 combinational AIG** (D3) — the corner of the schema needed to build the AIG. Write the converter crate (Liberty `function`/`functional.v` → IR). Redirect *one* PDK's (GF180MCU) stdcell logic to consume the IR. | A 9T netlist (e.g. `tests/jtag_minimal`) simulates against its **own** generated 9T descriptor; result byte-identical to the current 7T-substituted run where they truly agree, and the round-trip logic check passes. |
| **C2 — L3 sequential + L4 timing schema** | Add the D4 sequential pin-role schema (clock+edge, D/next-state, Q, async set/reset+polarity, enable) + classification kinds, **and the D5 L4 timing block** (setup/hold, clock→Q, DFF/SRAM timing). Extend the converter to emit both from Liberty `ff`/`latch` and the timing groups it already parses, with **L4 keyed by corner** (one descriptor, all corners; mirrors the timing IR). Wire the consumer to replace the hardcoded DFF pin-name matches (`src/aig.rs:2080-2260`) and to read L4 from the IR instead of `TimingLibrary::from_file`, selecting the corner via `--corner` (default `default_corner`). | A design with sequential GF180/SKY130 cells simulates **and times** from the IR with **no per-PDK Rust DFF handling and no runtime `.lib` parse**; equivalence vs the current hardcoded + `liberty_parser` path (the oracle). |
| **C3 — bundle + cut over + selection** | **Regenerate** bundled descriptors for all built-in PDKs (AIGPDK/SKY130/GF180) **in CI from the pinned vendored submodules and embed at build time — not checked in** (D7); build-time generation replaces the `build.rs` pin-table step. Selection by descriptor-declared prefix + `--cell-descriptor` (D8). Drop the runtime `vendor/` cell dependency, `pdk_decomp`, and `liberty_parser::TimingLibrary` from core; retire `gf180mcu*.rs`/`sky130*.rs` classifiers, `PdkVariant`, and the `build.rs` pin-table generators. | The existing PDK regression suite (incl. timed runs) passes consuming **only** build-time-generated descriptors; vendored cell submodules are no longer a *runtime* dependency of jacquard core, and CI regeneration is deterministic. |
| **C3a — IHP SG13G2 (new PDK, zero Rust)** | Vendor the [IHP-Open-PDK](https://github.com/IHP-GmbH/IHP-Open-PDK) SG13G2 stdcells as a submodule; add it to the bundle by **generating a descriptor only — no per-PDK Rust** (D7a). Exercises the Liberty-first path cleanly (every SG13G2 cell has `function`; every flop a full `ff` group with reset polarity). | An SG13G2 gate-level design simulates **and times** purely from its generated descriptor, with no `ihp_*.rs` in core — the worked proof that adding a PDK is no longer a Jacquard code change. |
| **C4 (later)** | A documented proprietary-library workflow: user runs the generator on their own Liberty, gets a descriptor, simulates — no Jacquard build. Honesty fix: the round-trip logic check replaces `build.rs`'s port-only `assert_eq!`. | `docs/adding-a-pdk.md` recipe; a synthetic "private" library exercised end-to-end in a test. |

## Remaining follow-ups (post-cutover)

Discovered during implementation; folded here from the working handoff so they
survive its deletion. None reopen the runtime `vendor/` stdcell dependency or
reintroduce `PdkVariant`.

1. **Descriptor-drive AIGPDK `DFF`/`DFFSR`.** AIGPDK's two internal flops are
   still on a literal-match path. Their descriptor L3 is well-formed
   (clock=CLK/rising, next_state=D, async S/R active-low, reset-dominant); the
   conversion is gate-covered (every AIGPDK/IHP fixture exercises them). Do this
   first — it is bounded and safe.
2. **Descriptor-drive the preserved `edfxtp`/`icgtp`/`icgtn` seq branches.** The
   legacy `SET_B`/`RESET_B`/`RN`/`SETN` sequential-wiring branches remain because
   they still serve *preserved* cells: SKY130 `edfxtp` (its data-enable folds
   into `next_state` via the `ff` state var `IQ`, so `ir_seq_input_wireable`
   returns false) and GF180 `icgtp`/`icgtn` clock gates (emit no L3). **No gated
   design exercises these**, so add a small `edfxtp`/`icgtp` fixture *first*, then
   descriptor-drive them and delete the residual literals + the per-PDK
   classifiers.
3. **Gate IHP sequential.** IHP `ff` cells are code-supported (previously
   panicked) but ungated — no flop-bearing SG13G2 fixture exists (needs synthesis
   infra to produce an IHP gate-level netlist). Add one to lock in IHP sequential.
4. **Flat-module `.v` cross-check indexer.** Commercial libraries (GF130, IHP)
   ship one flat-module `.v`, so the D6 logic cross-check reports 0 models indexed
   and does not run at generation (the descriptor is still emitted; correctness is
   validated via simulation, and the converter now *warns*). Teach the indexer
   flat-module `.v` so commercial descriptors are logic-cross-checked at
   generation.
5. **Descriptor-backed `LeafPinProvider` + delete `build.rs` pin-table gen.**
   Confirmed feasible (descriptor L1 covers every stdcell's scalar pins; chain:
   power-pin → filler → descriptor L1 → preserved pad/IP/SRAM tables). Not yet
   wired; the per-PDK `LeafPinProvider`s + `build.rs::generate_pin_table` +
   `generated` modules remain (inert for stdcell *logic*, but still the parse-time
   pin-direction source). This is the last mechanical cleanup.
6. **C4 — proprietary sequential end-to-end test + round-trip honesty fix.** GF130
   descriptor *generation* is proven (the C4 proof), but no proprietary
   *sequential* simulation test exists; add a synthetic "private" library
   exercised end-to-end. Separately, the round-trip logic check should replace
   `build.rs`'s port-only `assert_eq!` (the plan's C4 honesty fix).
7. **`clear_preset` for true set-dominant sequential sim.** The field is emitted
   and the consumer honors it, but a proprietary user simulating set-dominant
   flops still needs the non-GF180 sequential path fully descriptor-driven
   (items 1–2) to observe the correct behaviour end-to-end.

## Risks / open questions

- **Sequential fidelity (C2).** Liberty `ff` `clear`/`preset`/
  `clear_preset_var` → Jacquard async-reset DFF is the bug-prone mapping;
  gate it on equivalence against the current hardcoded behaviour.
- **L2 source (Liberty-first; resolved in ADR 0019 D6).** The converter
  reads Liberty `function`/`ff`/`latch` first and falls back to
  `functional.v`/UDP only where Liberty under-specifies. C1 must therefore
  exercise both ends: a clean `function` cell *and* a cell that needs the
  `.v`/UDP fallback (a UDP-modelled mux). Where both sources exist, the
  converter cross-checks them (logic equivalence; timing arc-set agreement;
  macro/SRAM timing-value divergence) and **surfaces disagreement** — the
  structural check `build.rs`'s port-only `assert_eq!` never had (#130).
- **AIG payload size (D2/D3).** If the JSON AIG is unwieldy for a full
  library, switch that payload to the FlatBuffers escape hatch — decide at
  C1 from the real GF180 descriptor size.
- **Migration ordering (C3) — resolved: per-PDK cutover.** Each PDK
  migrates independently with the IR consumer running alongside the per-PDK
  Rust, keeping the suite green; a single switch is riskier. IHP (C3a) is
  added the same way but greenfield (no Rust to retire).
- **Bundled-descriptor provenance — resolved: CI-regenerated (D7).** Not
  checked in; embedded at build time from pinned submodules. C3 must wire
  the generation into the build/CI and prove it deterministic.
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
