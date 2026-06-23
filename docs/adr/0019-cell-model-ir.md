# ADR 0019 — Cell-model IR: the logic sibling of the timing IR

**Status:** Proposed.

## Context

A standard-cell library must tell Jacquard three things about each cell.
Call them the **three layers**:

| Layer | What Jacquard needs | Where it lives today |
|---|---|---|
| **L1 — pin directions** | input vs output per pin | build-time baked from vendored submodules (`build.rs` → `GF180MCU_PIN_TABLE`/`SKY130_PIN_TABLE`), hand-coded for AIGPDK; partly user-suppliable via `--cell-library <v>` (ADR 0010 Tier 1) |
| **L2 — combinational logic** | the boolean function of each output, to build the AIG | read at runtime from a **hardcoded** `vendor/…/<cell>.functional.v` and decomposed by `pdk_decomp` (`src/aig.rs:1895`); fully hand-coded for AIGPDK |
| **L3 — sequential & classification** | is the cell a DFF/latch/clock-gate/SRAM/tie/filler, and what are its pin roles (clock, D, Q, async set/reset, enable; SRAM ports)? | hand-coded per-PDK Rust (`src/pdk.rs`, `src/gf180mcu_pdk.rs`, `src/sky130_pdk.rs`, pin-name matches in `src/aig.rs:2080-2260`) — *data masquerading as code* (ADR 0010's framing) |

ADR 0010 opened a **declarative path** for some of this (`--cell-library`
for L1, a `.cells.toml` manifest for cell *kind*), and ADR 0011 added a
real port-mapping schema for `kind = "ram"`. But ADR 0010 explicitly
**deferred the larger schema** — sequential pin-roles, full L3 — "to a
future ADR after real adoption data", and L2 was never addressed: the
functional-model source is still a hardcoded `vendor/` path.

Two concrete gaps make this acute, both surfaced by
issue [#130](https://github.com/gpu-eda/Jacquard/issues/130):

- **Selection.** A post-P&R netlist built against the 9-track library
  (`gf180mcu_fd_sc_mcu9t5v0`) is simulated against the **7-track**
  functional models, because the path is hardcoded
  (`src/aig.rs:1895`). There is no way to point cosim at the library the
  netlist actually instantiates. (`build.rs` only `assert_eq!`s *ports*,
  not functional bodies, so "7t == 9t" is unenforced for logic.)
- **Libraries we cannot vendor.** A proprietary/NDA foundry library can
  never live in `jacquard/vendor/`. Today L2 and L3 are *only* satisfiable
  by baking the library into the binary, so such libraries simply cannot
  be simulated — there is no all-runtime path.

The project has **already solved the analogous problem for the other half
of a cell.** The **timing IR** (ADR 0002) is a portable, versioned,
generated, diff-able structured description of a cell library's *timing*,
produced from Liberty by a focused converter crate (`opensta-to-ir`) and
consumed by Jacquard core — with the vendored sources demoted to a
generation-time input. ADR 0002 even anticipated this ADR in its scope
boundary:

> "The IR represents *timing annotation data only*. It is **not** … cell
> characterization. Attempts to extend it toward those adjacent formats
> are rejected — **they become separate IRs if needed**."

A cell's *logic* is exactly that adjacent format. This ADR makes it a
separate IR.

## Decision

Introduce a **cell-model IR**: a portable, versioned, **generated**
structured description of a cell library's **logic** — the L2/L3 sibling
of the timing IR's L1-timing description. Jacquard core consumes the
cell-model IR (and the hand-override manifest from ADR 0010/0011) as its
**only** source of cell semantics: no per-PDK Rust classifiers, no
`build.rs` pin-table generation, no runtime `functional.v` parsing, no
hardcoded `vendor/` paths.

The sub-decisions:

### D1 — A separate IR, with aligned identifiers

The cell-model IR is **distinct from** the timing IR; we do not broaden
ADR 0002 (its scope discipline is load-bearing, and the two have
different provenances — see D5). But the two IRs describe the **same
cells**, so they **share a canonical identifier convention** — a small
shared schema fragment (or documented normalization) for cell-type names
(same drive-strength handling the timing IR uses) and pin names — so that
"for cell *X*, here is its logic *and* its timing" is a clean join. The
shared-identifier fragment is the one piece deliberately co-designed
across the two crates.

### D2 — JSON-first

The cell-model IR is **machine-generated, not hand-written**, so its
primary on-disk form is **JSON** — diff-able, inspectable, no `flatc`
dependency to read. (This differs from the timing IR, which went
FlatBuffers-first because timing data is per-instance and large; cell
models are per-cell-*type* and small.) A FlatBuffers encoding is a
**deferred escape hatch** if size or startup cost ever demands it (see
D3). Schema is explicitly versioned, same evolution discipline as ADR
0002.

### D3 — L2 as a pre-built AIG

Each combinational cell's logic is stored as a **pre-decomposed AIG**
(and-inverter nodes, with input-pin → node and output-pin → node maps),
**not** as boolean expressions or truth tables. Rationale: the runtime
splices the cell's AIG directly into the design AIG with no
decomposition work — minimal startup time and memory, and it removes
`pdk_decomp` / functional-Verilog parsing from jacquard core entirely.
Decomposition moves into the generator (D5). If the aggregate AIG payload
grows unwieldy in JSON, encode it with FlatBuffers (the D2 escape hatch).

### D4 — L3 schema: sequential pin-roles + classification

Sequential cells carry **pin-role metadata** — clock pin + edge, D /
next-state, Q/QN, async set/reset pin + polarity, enable — plus their
combinational next-state function as the same pre-built AIG (D3),
consumed by the existing `DriverType::DFF` path (replacing the hardcoded
pin-name matches in `src/aig.rs:2080-2260`). Cell **classification**
(`std`, `dff`, `latch`, `clock_gate`, `ram`, `tie_high/low`, `filler`,
`endcap`, `tap`, `io_pad_*`) is a declared kind. This is the sequential
analogue of ADR 0011's RAM port schema, which stands as the worked
precedent; RAM keeps its ADR-0011 schema.

### D5 — A separate generator (converter crate)

The IR is produced by a **converter crate** mirroring `opensta-to-ir`:
Liberty (with `functional.v` as a fallback for cells Liberty
under-specifies) → cell-model IR. It **reuses, not reimplements**, the
existing decomposition: `pdk_decomp` relocates into a shared library the
generator links, and the Liberty group-walker (`src/liberty_parser.rs`,
which today reads only timing groups) is extended to read `function` /
`ff` / `latch` / cell-class. The generator — not jacquard core — owns all
Verilog/Liberty parsing and AIG decomposition; it is unit-testable in
isolation, off the runtime critical path.

### D6 — Bundled descriptors; vendored PDKs demote to generation inputs

The built-in PDKs (AIGPDK, SKY130, GF180MCU) ship as **generated
cell-model-IR files checked into core** — the same posture as the timing
IR's checked-in bindings. They are generated **from the vendored PDKs**,
which therefore become inputs to *cell-library generation* rather than a
dependency of jacquard core: core depends on neither the cell submodules
nor `pdk_decomp` at runtime. (Initial bootstrap still uses the vendored
PDKs to produce the first built-in descriptors.)

### D7 — Selection by declared prefix + explicit override

Each descriptor **declares the cell-name prefix(es) it covers**. Jacquard
auto-matches a netlist against the bundled descriptors (replacing the
hardcoded prefix detection in `src/pdk.rs`); `--cell-descriptor
<foo.json>` is the explicit override and the path for proprietary
libraries. The ADR-0010 `.cells.toml` becomes the **hand-authored
override layer** over the generated IR — same data model, different
provenance.

## Consequences

- **Library-agnostic binary.** A proprietary library works with **zero
  Jacquard changes**: the user generates a descriptor on their own machine
  from their Liberty (which never leaves it) and points jacquard at it. The
  generator is the only tool that touches raw foundry files.
- **Large core simplification.** `src/gf180mcu.rs`, `src/gf180mcu_pdk.rs`,
  `src/sky130*.rs` special-cases, the `PdkVariant` enum, the `build.rs`
  pin-table generators, and the hardcoded vendor paths all retire in
  favour of one IR consumer. Adding a PDK stops being a Jacquard PR.
- **#130 dissolves.** "Which functional models?" becomes "which descriptor
  matches this netlist" — derived or `--cell-descriptor`-overridden — and
  the 7t/9t silent-substitution risk goes away because each library has
  its own generated descriptor.
- **New schema + converter crate to maintain,** under the same scope
  discipline ADR 0002 demands: the cell-model IR is *cell logic only* — not
  a netlist, not timing, not a placement/physical model. Creep is rejected.
- **Verification moves to a diff/round-trip discipline** (ADR 0001/0002
  pattern): a regenerated descriptor must equal the checked-in one, and a
  logic round-trip — does the IR's AIG reproduce a reference of the cell? —
  generalises today's `build.rs` port-only `assert_eq!` into an actual
  logic check, closing the gap #130 names.
- **Sequential fidelity is the risk surface.** Mapping Liberty `ff`
  (`clear` / `preset` / `clear_preset_var`) onto Jacquard's DFF +
  async-reset model is where bugs will hide; it gets dedicated generator
  tests against the existing GF180/SKY130 behaviour as the oracle.

## Open questions

- **L2 source of truth** — Liberty `function` strings (clean for most
  cells) vs `functional.v` (needed for some): the generator accepts both;
  unifying them is explicitly *not* a goal now.
- **Exact shared-identifier fragment** (D1) co-designed with the timing IR.
- **Bundled-descriptor provenance** — checked-in artifacts (like the
  timing-ir bindings) vs regenerated in CI from pinned vendored PDKs.
- **Migration shape** — run the IR consumer alongside the per-PDK Rust
  during transition (per-PDK cutover) vs a single switch.

## Relationship to other ADRs

- **Complements [ADR 0002](0002-timing-ir.md)** — the logic sibling of the
  timing IR; realises 0002's "separate IRs if needed" boundary; shares its
  versioning + diff discipline and its identifier convention (D1).
- **Extends [ADR 0010](0010-declarative-cell-metadata.md)** — supplies the
  "future ADR" 0010 deferred for the heavy (L2/L3) schema, and recasts the
  `.cells.toml` path as a hand-override over generated IR.
- **Builds on [ADR 0011](0011-ram-port-mapping-schema.md)** — RAM keeps its
  port schema; it is the worked precedent for the D4 sequential schema.
- **Feeds [ADR 0014](0014-aig-as-simulation-ir.md)** — the cell-model IR's
  L2 payload *is* AIG (D3), spliced into the design AIG at load.

Staged delivery: [`docs/plans/cell-model-ir.md`](../plans/cell-model-ir.md).
Tracks [#130](https://github.com/gpu-eda/Jacquard/issues/130) and
[#67](https://github.com/gpu-eda/Jacquard/issues/67).
