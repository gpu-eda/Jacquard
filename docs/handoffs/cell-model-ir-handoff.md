# Handoff — Cell-model IR (ADR 0019)

**Active thread:** ADR 0019 "Cell-model IR: a complete per-cell-type
library descriptor" is **Proposed** and up as **PR #132** (docs-only,
nothing wired). It is waiting on the maintainer to react and to steer the
open questions before any code is written. Branch:
`docs/cell-model-ir-adr` (worktree `../Jacquard-cellir`).

## Done this session

- **JTAG debug server — MERGED (PR #125 → `main`, CI green).** Implements
  `docs/plans/jtag-debug-server.md` (J1–J4) *plus* the fix that made it
  actually work: a **power-on TRST pulse** (real OpenOCD couldn't examine
  the DTM without a `negedge trst_n`; replay/synthetic gates couldn't
  catch it). Added a real-OpenOCD CI gate (`jtag-minimal-openocd`),
  auto-port (`--jtag-server 0`), `--max-clock-edges` relaxed while
  attached, `jacquard jtag-openocd-config` generator, and
  `--jtag-reconnect`. All verified on Metal. This thread is **closed**.
- **Issue #127** filed — non-deterministic output state-slot assignment
  (mt-kahypar partitioning). Confirmed **no observable effect** (VCD is
  byte-reproducible across slots; JTAG unaffected). Parked, low priority.
- **Issue #130** (maintainer-filed) — GF180 cosim hardcodes the 7T
  stdcell functional models regardless of netlist library. This is the
  **trigger** for ADR 0019.
- **ADR 0019 + plan + ADR 0002 amendment** written and revised (PR #132).

## ADR 0019 — the design, in one breath

Everything Jacquard needs about a cell is a **per-cell-type fact from one
Liberty file**: L1 directions, L2 combinational logic, L3
sequential/classification, **L4 timing characterization**. Put all four in
**one generated JSON descriptor per library**; core consumes it as its
only source of cell semantics. Vendored PDKs demote to *generation
inputs*; proprietary libraries work with zero Jacquard changes.

Decisions (full text in the ADR): D1 one descriptor (timing IR
orthogonal); D2 JSON-first (FlatBuffers escape hatch); D3 L2 = pre-built
AIG (decomp moves to the generator); D4 L3 sequential schema; D5 L4
timing folded in; D6 converter crate (Liberty → IR, reusing `pdk_decomp`
+ `liberty_parser`); D7 bundled descriptors; D8 selection by declared
prefix + `--cell-descriptor`.

**The load-bearing insight** (don't re-derive it): there are **two
different "timing" artifacts** — per-cell-type *characterization*
(`liberty_parser::TimingLibrary`, a library fact → goes **in** the cell
IR) vs the per-design *annotation* timing IR (ADR 0002, `TimingArc {
cell_instance }`, SDF → **orthogonal**, untouched). The cell IR is **not**
the timing IR's "logic sibling"; it's the *"separate IR for cell
characterization"* ADR 0002's scope boundary predicted. ADR 0002 got a
dated cross-reference amendment.

## Next steps (in order)

1. **Maintainer reviews PR #132** and steers the four open questions
   (listed at the bottom of the ADR): L2 source unification
   (`function` vs `functional.v`), L4 multi-corner shape, bundled-descriptor
   provenance (checked-in vs CI-regenerated), migration ordering.
2. On approval, start **plan C1** (`docs/plans/cell-model-ir.md`):
   relocate `pdk_decomp` into a shared lib; define the L1+L2 JSON schema
   (pre-built AIG); write the Liberty→IR converter; redirect GF180's
   stdcell logic to consume it. **Gate:** a 9T netlist (e.g.
   `tests/jtag_minimal`) sims against its *own* 9T descriptor — this also
   closes #130.
3. Optional cheap interim for #130 before the full IR: make `build.rs`
   diff functional *bodies* (not just ports) across 7t/9t and tighten the
   `src/gf180mcu_pdk.rs` doc comment (option 1 in #130) — makes the
   current 7T-canonical assumption honest.

## Key anchors (verified this session)

- Hardcoded 7T path: `src/aig.rs:1895`. Port-only assert: `build.rs:202/206`.
- Per-cell-type timing (to fold in): `src/liberty_parser.rs`
  (`TimingLibrary`), consumed at `src/aig.rs:2793` + `src/flatten.rs`
  (incl. `liberty_fallback`); loaded from a user `.lib` via
  `TimingLibrary::from_file`.
- Per-PDK classifiers to retire: `src/pdk.rs` (`PdkVariant`, prefix
  detection), `src/gf180mcu_pdk.rs`, `src/sky130_pdk.rs`, DFF pin matches
  `src/aig.rs:2080-2260`.
- The sibling pattern to mirror: `crates/timing-ir` + `crates/opensta-to-ir`.

## Loose ends

- **Two stray worktrees** off this repo: `../Jacquard-jtag-server` (merged
  JTAG branch) and `../Jacquard-cellir` (this ADR branch). Maintainer
  asked to leave them; remove with `git worktree remove` when done.
- **#127** parked (non-issue in practice).
- At resolution: fold this handoff's content into the ADR/plan and
  **delete this file** (project handoff discipline — exactly one exists).
