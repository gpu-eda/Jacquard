# Decision 0019 — Cell-model IR: a complete per-cell-type library descriptor

**Status:** Proposed — all four design open questions resolved (see below);

**Current architecture:** [PDK enablement](../pdk-enablement.md) and [Timing correctness](../timing-correctness.md).
pending maintainer approval to start implementation (plan C1).

## Context

Everything Jacquard needs to know about a standard cell is a **property
of the cell type** and comes from the **same Liberty library** — its pin
directions, its combinational logic, its sequential/classification
nature, *and* its timing characterization:

| Per-cell-type fact | What Jacquard needs | Where it lives today |
|---|---|---|
| **L1 — pin directions** | input vs output per pin | build-time baked from vendored submodules (`build.rs` → `GF180MCU_PIN_TABLE`/`SKY130_PIN_TABLE`), hand-coded for AIGPDK; partly user-suppliable via `--cell-library <v>` (Decision 0010 Tier 1) |
| **L2 — combinational logic** | the boolean function of each output, to build the AIG | read at runtime from a **hardcoded** `vendor/…/<cell>.functional.v` and decomposed by `pdk_decomp` (`src/aig.rs:1895`); fully hand-coded for AIGPDK |
| **L3 — sequential & classification** | is the cell a DFF/latch/clock-gate/SRAM/tie/filler, and what are its pin roles (clock, D, Q, async set/reset, enable; SRAM ports)? | hand-coded per-PDK Rust (`src/pdk.rs`, `src/gf180mcu_pdk.rs`, `src/sky130_pdk.rs`, pin-name matches in `src/aig.rs:2080-2260`) — *data masquerading as code* (Decision 0010's framing) |
| **L4 — timing characterization** | per-cell-type setup/hold, clock→Q, DFF/SRAM timing | parsed at runtime from a `.lib` into `liberty_parser::TimingLibrary` (`src/liberty_parser.rs`), consumed in `src/aig.rs:2793` / `src/flatten.rs` (incl. as a `liberty_fallback`); already user-suppliable via a `.lib` path |

### Two different "timing" artifacts — only one is per-cell-type

This is the easy thing to conflate. There are two:

- **Per-cell-type characterization** (L4 above) — setup/hold, clock→Q,
  delays — a **library** property, one value per cell type, straight from
  the `.lib`.
- **Per-design annotation** — the **timing IR** (Decision 0002),
  `TimingArc { cell_instance: "<hierarchical instance path>" }` — SDF for
  *a specific netlist's instances*, produced per-design by SDF/OpenSTA.

Only L4 is a cell-library fact. The timing IR is **orthogonal** to a cell
library — it annotates a design, not a library — and stays its own IR.
Decision 0002 drew exactly this line and predicted this ADR:

> "The IR represents *timing annotation data only*. It is **not** … cell
> characterization. Attempts to extend it toward those adjacent formats
> are rejected — **they become separate IRs if needed**."

Cell characterization is that separate IR. It is **not a sibling** of the
timing IR (same scope, different half); it is a **different axis**
entirely — per-cell-type library facts vs per-instance design annotation.

### Where the declarative path got to

Decision 0010 opened a **declarative path** for some of L1–L3 (`--cell-library`
for L1, a `.cells.toml` manifest for cell *kind*), and Decision 0011 added a
real port-mapping schema for `kind = "ram"`. But Decision 0010 explicitly
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
timing IR (Decision 0002) is a portable, versioned, generated, diff-able
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
hand-override manifest from Decision 0010/0011) as its **only** source of cell
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
descriptor. It does **not** touch the per-design timing IR (Decision 0002),
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
analogue of Decision 0011's RAM port schema, which stands as the worked
precedent; RAM keeps its ADR-0011 schema.

### D5 — L4 timing characterization, in the same descriptor

The descriptor carries the per-cell-type timing — setup/hold, clock→Q,
DFF/SRAM timing — keyed by the same cell type as the logic; this is the
data `liberty_parser::TimingLibrary` holds today, extracted once by the
generator instead of parsed from a `.lib` at every run. At runtime this
*replaces* `TimingLibrary::from_file`; the per-design timing IR (Decision 0002)
still layers a specific design's instance arrivals on top, unchanged.

**Multi-corner lives in the one descriptor, keyed by corner** — the same
shape the timing IR already uses (`crates/timing-ir`: an ordered corner-name
set + a `corner_index` per value, with `min/typ/max` as the *within-corner*
derate triple). Corner is the outer key; min/typ/max is the orthogonal inner
derate — not a reinterpretation of the triple. File-per-corner is rejected
because L1–L3 (directions, function, sequential roles, and especially the
D3 AIG — the bulk of the descriptor) are **corner-invariant**: only L4's
numbers vary across `ss`/`tt`/`ff`, so a per-corner file would duplicate the
*expensive* AIG to vary the *cheap* timing. Corner-keyed L4 dedups the AIG,
keeps **one descriptor = one library** (which D8 selection assumes), and
degrades to a one-entry corner map for the common single-corner flow. A
corner-overlay-file split is a *deferred size escape hatch* (parallel to D2),
to be decided from a real descriptor size — not built now.

**The simulation corner is user-selected, not read from the netlist or
SDF.** A `--corner <name>` run flag picks among the descriptor's corner set,
defaulting to a descriptor-declared `default_corner` (typically `tt`). The
netlist is structural and records no corner; synthesis *used* a setup corner
to choose cells but writes it nowhere Jacquard reads. SDF carries PVT header
fields (`VOLTAGE`/`PROCESS`/`TEMPERATURE`), but they are the wrong source on
both counts: when an SDF *is* present its delays already encode the corner
and feed the orthogonal timing IR (Decision 0002), so L4 corner selection does
not apply to annotated instances; and L4 matters precisely in the **no-SDF**
cosim path (today's `liberty_fallback`), where there is no SDF header to
read. There are two different corners — the **setup corner** synthesis
chose for (a flow fact, recorded nowhere) and the **simulation corner** the
user wants to observe — and only the user knows the latter, so user-passed
is the *correct* source, not a fallback. This also makes today's implicit
single-corner choice (whatever `.lib` was passed) explicit. When an SDF is
supplied, its PVT header may be surfaced as an informational cross-check
(`SDF says ss_125C, you selected tt_025C`), never as the selector. This
mirrors `opensta-to-ir`, which already treats corner identity as declared
input, not auto-detected.

### D6 — A separate generator (converter crate)

The IR is produced by a **converter crate** mirroring `opensta-to-ir`:
Liberty → cell-model IR — emitting **all of L1–L4 from a single pass over
the `.lib`**, with `functional.v` consulted only as a fallback for the L2
combinational cases Liberty under-specifies (see the resolved L2 open
question below). It **reuses, not reimplements**, the existing machinery:
`pdk_decomp` relocates into a shared library the generator links, and the
Liberty group-walker (`src/liberty_parser.rs`, which today reads only the
L4 timing groups) is extended to also read `function` / `ff` / `latch` /
cell-class. The generator — not jacquard core — owns all Verilog/Liberty
parsing and AIG decomposition; it is unit-testable in isolation, off the
runtime critical path.

**Cross-check when both sources are present.** For a library that ships
both Liberty and `functional.v` (the open PDKs), the generator validates
the two against each other and **surfaces any disagreement** as a
generation-time diagnostic rather than silently trusting one. Two checks:

- **Logic (L2/L3).** Build the combinational AIG from the Liberty
  `function` *and* from the `.v` gate/UDP netlist and check the two for
  logical equivalence; likewise the sequential next-state/reset from
  Liberty `ff`/`latch` against the `.v` sequential UDP.
- **Timing (L4).** For **standard cells**, the `.v` `specify` block carries
  timing *arc topology* but no values (zero-placeholder SDF scaffold), so
  the check is **arc-set agreement**, not value comparison: every Liberty
  `timing()` group (`related_pin` + `timing_type`) should correspond to a
  `.v` `specify` path or timing check and vice versa, and across a
  multi-corner library every corner `.lib` should carry the **same
  cell/arc set** (only the values differ). For **macros/SRAM**, whose
  behavioural `.v` often embeds *real* hardcoded timing (access time,
  setup/hold) that can diverge from the macro's Liberty, the check extends
  to a **value comparison** and flags the divergence. A missing, extra, or
  mis-typed arc, a cross-corner arc-set difference, or a macro value
  divergence — on either source — is surfaced. Liberty remains the
  authoritative L4 source either way.

This is the same diff discipline Decision 0001/0002 use, applied across the L2
logic sources, the L4 arc topology, and the corner set; it is the
structural check `build.rs`'s port-only `assert_eq!` never had — directly
the silent-substitution class #130 names.

### D7 — Bundled descriptors regenerated in CI from pinned vendored PDKs

The built-in PDKs — AIGPDK, SKY130, GF180MCU, and **IHP SG13G2** (added
through the IR; see D7a) — are bundled as generated cell-model-IR files,
**regenerated in CI from the pinned vendored PDKs rather than checked in as
artifacts**. CI runs the converter (D6) over the pinned submodules and
embeds the resulting descriptors into the binary at build time; the
descriptors are *not* committed blobs. The vendored PDKs therefore become
inputs to *cell-library generation*, not a runtime dependency of jacquard
core — core depends on neither the cell submodules nor `pdk_decomp` at
runtime, and a released binary is self-contained.

This is the deliberate contrast with the timing-IR bindings (which are
checked in): cell descriptors are larger, fully derived, and have a
deterministic generator, so committing them would only add generated blobs
and drift risk. Build-time generation replaces today's `build.rs`
pin-table generation, so source builds keep the same vendored-submodule
requirement they already have. Diff-ability (D2) is preserved as a
*capability* — CI regenerates deterministically and the generator ships a
descriptor diff tool (mirroring `timing-ir-diff`) — rather than via git
history; drift and provenance are enforced by the CI regeneration + the D6
cross-check, not by a committed reference.

### D7a — IHP SG13G2 as a new PDK added with zero per-PDK Rust

IHP's open SG13G2 PDK ([IHP-Open-PDK](https://github.com/IHP-GmbH/IHP-Open-PDK))
is added as a **new** built-in target — Jacquard has no existing per-PDK
Rust for it — so it is the proof of the headline consequence that *adding
a PDK is no longer a Jacquard code change*: it is onboarded purely by
vendoring its submodule and generating a descriptor. It is also the
cleanest **Liberty-first** validation: every SG13G2 combinational cell
carries a `function` string and every sequential cell a complete `ff`
group (with reset polarity), exercising the D6 Liberty-first path end to
end, in contrast to GF180's `functional.v`-primary legacy path.

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
  favour of one IR consumer. Adding a PDK stops being a Jacquard PR — and
  **IHP SG13G2 (D7a) is the worked proof**: a new built-in PDK onboarded
  with zero per-PDK Rust, purely by vendoring + generating a descriptor.
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
  discipline Decision 0002 demands: the cell-model IR is *per-cell-type library
  facts only* — not a netlist, not per-design annotation, not a
  placement/physical model. Creep is rejected.
- **Verification moves to a diff/round-trip discipline** (Decision 0001/0002
  pattern): since descriptors are CI-regenerated rather than committed
  (D7), the gate is that **CI regeneration is deterministic** plus a logic
  round-trip — does the IR's AIG reproduce a reference of the cell? — and
  the D6 cross-check across sources. Together these generalise today's
  `build.rs` port-only `assert_eq!` into an actual logic check, closing the
  gap #130 names.
- **Sequential fidelity is the risk surface.** Mapping Liberty `ff`
  (`clear` / `preset` / `clear_preset_var`) onto Jacquard's DFF +
  async-reset model is where bugs will hide; it gets dedicated generator
  tests against the existing GF180/SKY130 behaviour as the oracle.

## Open questions

- ~~**L2 source of truth**~~ — **Resolved (D6): Liberty-first, `.v` as a
  bounded fallback.** A survey of open and commercial (Liberate-generated)
  standard-cell libraries found the same shape in all of them: Liberty
  carries `function` for every combinational cell and `ff`/`latch` groups
  (`clocked_on`/`next_state`/`clear`/`preset`, with reset polarity) for
  every sequential cell, while the behavioural `.v` models sequential cells
  as a vendor UDP wrapped in `notifier`/`$setuphold` timing-check
  machinery. So **Liberty is the cleaner and sufficient source for L2
  `function` and L3 `ff`/`latch`** — it states the next-state/reset logic
  directly, whereas the `.v` buries it in simulation scaffolding. The
  generator reads Liberty first and consults `.v`/UDP **only** for the L2
  combinational cases Liberty under-specifies (cells with no `function`
  string, or UDP truth tables whose exact X-semantics you want, e.g. some
  muxes). This is the converter's *target*; the existing GF180/SKY130
  `functional.v` path is fine as the first converter input in plan C1.

  Two bounded sub-points:

  - **L4 timing is Liberty-exclusive for standard cells.** A stdcell `.v`
    `specify` block carries timing *arc topology* (which arcs/checks exist)
    but **zero/placeholder values** — it is an SDF back-annotation scaffold,
    redundant with the `.lib`'s own arc set. So the cross-check on stdcell
    timing is arc-set agreement, not value comparison (see D6).
    **Exception — macros/SRAM.** A memory/macro behavioural `.v` often
    **embeds real, hardcoded timing** (access time, setup/hold) that can
    *diverge* from the macro's Liberty — here the two sources disagree on
    *values*, not just topology. (The GF180 SRAM model is a suspected case,
    not yet diffed; SRAM timing is already a special path in Jacquard, with
    a hardcoded conservative read-delay fallback in
    `src/liberty_parser.rs`.) The generator must surface that disagreement
    (D6); L4 still comes from Liberty as the single authoritative source,
    but the divergence is a real correctness signal worth flagging rather
    than hiding.
  - **Violation-response is out of scope.** The `.v`'s `notifier`→UDP-`x`
    corruption-on-violation is a *simulator policy*, not a per-cell-type
    library fact: the constraint *values* it needs are already L4 from
    Liberty, and the *response* is owned by Jacquard's own timing-violation
    subsystem (`docs/timing-violations.md`: report-based, X only under
    `--xprop`). It is not extracted into the IR.
- ~~**L4 multi-corner shape**~~ — **Resolved (D5):** all corners in the one
  descriptor, L4 keyed by corner (mirroring the timing IR's corner-set +
  `corner_index`; min/typ/max stays the orthogonal within-corner derate).
  Simulation corner is user-selected via `--corner` (default
  `default_corner`), *not* inferred from the netlist or SDF — the setup
  corner is recorded nowhere Jacquard reads, and SDF delays already encode
  the corner via the orthogonal timing IR. Corner-overlay-file split deferred
  as a size escape hatch.
- ~~**Bundled-descriptor provenance**~~ — **Resolved (D7): regenerated in
  CI** from pinned vendored PDKs and embedded at build time, *not* checked
  in as artifacts. Build-time generation replaces the `build.rs` pin-table
  step (source builds keep the existing submodule requirement); diff-ability
  and drift control move to deterministic CI regeneration + the generator's
  descriptor diff tool + the D6 cross-check.
- ~~**Migration shape**~~ — **Resolved: per-PDK cutover.** The IR consumer
  runs alongside the per-PDK Rust and each PDK is migrated independently,
  keeping the suite green throughout, rather than a single switch. **IHP
  SG13G2 (D7a) is added in the same per-PDK fashion** — but greenfield (no
  existing Rust to retire), so it doubles as the zero-per-PDK-Rust proof.

## Relationship to other ADRs

- **Orthogonal to [Decision 0002](0002-timing-ir.md)** — the timing IR is
  per-design instance annotation; the cell-model IR is per-cell-type
  library facts (incl. L4 timing characterization). Different axes, not
  siblings — but the cell-model IR *realises* 0002's predicted "separate
  IR for cell characterization", and reuses its versioning + diff
  discipline.
- **Extends [Decision 0010](0010-declarative-cell-metadata.md)** — supplies the
  "future ADR" 0010 deferred for the heavy (L2/L3) schema, folds in the L4
  `.lib` characterization, and recasts the `.cells.toml` path as a
  hand-override over generated IR.
- **Builds on [Decision 0011](0011-ram-port-mapping-schema.md)** — RAM keeps its
  port schema; it is the worked precedent for the D4 sequential schema.
- **Feeds [Decision 0014](0014-aig-as-simulation-ir.md)** — the cell-model IR's
  L2 payload *is* AIG (D3), spliced into the design AIG at load.

Staged delivery: [`docs/plans/cell-model-ir.md`](../../plans/cell-model-ir.md).
Tracks [#130](https://github.com/gpu-eda/Jacquard/issues/130) and
[#67](https://github.com/gpu-eda/Jacquard/issues/67).
