# ADR 0019 — Cell-model IR: a complete per-cell-type library descriptor

**Status:** Proposed.

## Context

Everything Jacquard needs to know about a standard cell is a **property
of the cell type** and comes from the **same Liberty library** — its pin
directions, its combinational logic, its sequential/classification
nature, *and* its timing characterization:

| Per-cell-type fact | What Jacquard needs | Where it lives today |
|---|---|---|
| **L1 — pin directions** | input vs output per pin | build-time baked from vendored submodules (`build.rs` → `GF180MCU_PIN_TABLE`/`SKY130_PIN_TABLE`), hand-coded for AIGPDK; partly user-suppliable via `--cell-library <v>` (ADR 0010 Tier 1) |
| **L2 — combinational logic** | the boolean function of each output, to build the AIG | read at runtime from a **hardcoded** `vendor/…/<cell>.functional.v` and decomposed by `pdk_decomp` (`src/aig.rs:1895`); fully hand-coded for AIGPDK |
| **L3 — sequential & classification** | is the cell a DFF/latch/clock-gate/SRAM/tie/filler, and what are its pin roles (clock, D, Q, async set/reset, enable; SRAM ports)? | hand-coded per-PDK Rust (`src/pdk.rs`, `src/gf180mcu_pdk.rs`, `src/sky130_pdk.rs`, pin-name matches in `src/aig.rs:2080-2260`) — *data masquerading as code* (ADR 0010's framing) |
| **L4 — timing characterization** | per-cell-type setup/hold, clock→Q, DFF/SRAM timing | parsed at runtime from a `.lib` into `liberty_parser::TimingLibrary` (`src/liberty_parser.rs`), consumed in `src/aig.rs:2793` / `src/flatten.rs` (incl. as a `liberty_fallback`); already user-suppliable via a `.lib` path |

### Two different "timing" artifacts — only one is per-cell-type

This is the easy thing to conflate. There are two:

- **Per-cell-type characterization** (L4 above) — setup/hold, clock→Q,
  delays — a **library** property, one value per cell type, straight from
  the `.lib`.
- **Per-design annotation** — the **timing IR** (ADR 0002),
  `TimingArc { cell_instance: "<hierarchical instance path>" }` — SDF for
  *a specific netlist's instances*, produced per-design by SDF/OpenSTA.

Only L4 is a cell-library fact. The timing IR is **orthogonal** to a cell
library — it annotates a design, not a library — and stays its own IR.
ADR 0002 drew exactly this line and predicted this ADR:

> "The IR represents *timing annotation data only*. It is **not** … cell
> characterization. Attempts to extend it toward those adjacent formats
> are rejected — **they become separate IRs if needed**."

Cell characterization is that separate IR. It is **not a sibling** of the
timing IR (same scope, different half); it is a **different axis**
entirely — per-cell-type library facts vs per-instance design annotation.

### Where the declarative path got to

ADR 0010 opened a **declarative path** for some of L1–L3 (`--cell-library`
for L1, a `.cells.toml` manifest for cell *kind*), and ADR 0011 added a
real port-mapping schema for `kind = "ram"`. But ADR 0010 explicitly
**deferred the larger schema** — sequential pin-roles, full L3 — "to a
future ADR after real adoption data", L2 was never addressed (functional
models still come from a hardcoded `vendor/` path), and L4 lives in a
parallel runtime `.lib` parser. Four facts about one cell type, from one
Liberty file, arrive through four different mechanisms.

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

The project already proved the *shape* of the fix on the timing side: the
timing IR (ADR 0002) is a portable, versioned, generated, diff-able
structured form produced from Liberty by a focused converter crate, with
the vendored sources demoted to a generation-time input. This ADR applies
the same shape to the cell library — but to **all** of its per-cell-type
facts at once, in **one** descriptor, because they share one source.

## Decision

Introduce a **cell-model IR**: a portable, versioned, **generated**,
JSON-first structured descriptor that carries **everything per-cell-type
about a library** — L1 directions, L2 combinational logic, L3
sequential/classification, **and L4 timing characterization** — in one
file per library. Jacquard core consumes the cell-model IR (and the
hand-override manifest from ADR 0010/0011) as its **only** source of cell
semantics: no per-PDK Rust classifiers, no `build.rs` pin-table
generation, no runtime `functional.v` parsing, and no runtime `.lib`
parsing.

The ADR-0002 timing IR is **unchanged and orthogonal** — it annotates a
specific design's instances; nothing in it is per-library.

The sub-decisions:

### D1 — One descriptor for all per-cell-type facts; the timing IR stays orthogonal

All five facts (L1–L4 + classification) live in **one** cell-model IR per
library, because they all come from one Liberty file and are all keyed by
cell type. This *folds in* `liberty_parser::TimingLibrary` (L4): the
runtime stops parsing `.lib` and reads pre-extracted timing from the
descriptor. It does **not** touch the per-design timing IR (ADR 0002),
which is a different axis (per-instance design annotation) and keeps its
own crate/format. The only cross-reference the two need is the netlist
itself — an instance knows its cell type — so there is no shared-schema
join to co-design between them.

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
Decomposition moves into the generator (D6). If the aggregate AIG payload
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

### D5 — L4 timing characterization, in the same descriptor

The descriptor carries the per-cell-type timing — setup/hold, clock→Q,
DFF/SRAM timing — keyed by the same cell type as the logic; this is the
data `liberty_parser::TimingLibrary` holds today, extracted once by the
generator instead of parsed from a `.lib` at every run. Multi-corner is
representable as a value set per arc (reusing the timing IR's min/typ/max
precedent), though most flows ship one corner. At runtime this *replaces*
`TimingLibrary::from_file`; the per-design timing IR (ADR 0002) still
layers a specific design's instance arrivals on top, unchanged.

### D6 — A separate generator (converter crate)

The IR is produced by a **converter crate** mirroring `opensta-to-ir`:
Liberty (with `functional.v` as a fallback for cells Liberty
under-specifies) → cell-model IR — emitting L1–L4 from a single pass over
the `.lib`. It **reuses, not reimplements**, the existing machinery:
`pdk_decomp` relocates into a shared library the generator links, and the
Liberty group-walker (`src/liberty_parser.rs`, which today reads only the
L4 timing groups) is extended to also read `function` / `ff` / `latch` /
cell-class. The generator — not jacquard core — owns all Verilog/Liberty
parsing and AIG decomposition; it is unit-testable in isolation, off the
runtime critical path.

### D7 — Bundled descriptors; vendored PDKs demote to generation inputs

The built-in PDKs (AIGPDK, SKY130, GF180MCU) ship as **generated
cell-model-IR files checked into core** — the same posture as the timing
IR's checked-in bindings. They are generated **from the vendored PDKs**,
which therefore become inputs to *cell-library generation* rather than a
dependency of jacquard core: core depends on neither the cell submodules
nor `pdk_decomp` at runtime. (Initial bootstrap still uses the vendored
PDKs to produce the first built-in descriptors.)

### D8 — Selection by declared prefix + explicit override

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
- **Retires the runtime `.lib` parse and unifies "bring your own
  library."** L4 folds in, so `liberty_parser::TimingLibrary` leaves the
  runtime for the generator. Today you already point at your own `.lib`
  for timing while the logic is baked from `vendor/`; now both come from
  one descriptor — exactly the proprietary-library goal.
- **New schema + converter crate to maintain,** under the same scope
  discipline ADR 0002 demands: the cell-model IR is *per-cell-type library
  facts only* — not a netlist, not per-design annotation, not a
  placement/physical model. Creep is rejected.
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
- **L4 multi-corner shape** — single value vs the timing IR's min/typ/max
  value set per arc; default to one corner until a multi-corner cell flow
  demands more.
- **Bundled-descriptor provenance** — checked-in artifacts (like the
  timing-ir bindings) vs regenerated in CI from pinned vendored PDKs.
- **Migration shape** — run the IR consumer alongside the per-PDK Rust
  during transition (per-PDK cutover) vs a single switch.

## Relationship to other ADRs

- **Orthogonal to [ADR 0002](0002-timing-ir.md)** — the timing IR is
  per-design instance annotation; the cell-model IR is per-cell-type
  library facts (incl. L4 timing characterization). Different axes, not
  siblings — but the cell-model IR *realises* 0002's predicted "separate
  IR for cell characterization", and reuses its versioning + diff
  discipline.
- **Extends [ADR 0010](0010-declarative-cell-metadata.md)** — supplies the
  "future ADR" 0010 deferred for the heavy (L2/L3) schema, folds in the L4
  `.lib` characterization, and recasts the `.cells.toml` path as a
  hand-override over generated IR.
- **Builds on [ADR 0011](0011-ram-port-mapping-schema.md)** — RAM keeps its
  port schema; it is the worked precedent for the D4 sequential schema.
- **Feeds [ADR 0014](0014-aig-as-simulation-ir.md)** — the cell-model IR's
  L2 payload *is* AIG (D3), spliced into the design AIG at load.

Staged delivery: [`docs/plans/cell-model-ir.md`](../plans/cell-model-ir.md).
Tracks [#130](https://github.com/gpu-eda/Jacquard/issues/130) and
[#67](https://github.com/gpu-eda/Jacquard/issues/67).
