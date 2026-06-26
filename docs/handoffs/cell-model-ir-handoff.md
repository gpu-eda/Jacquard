# Handoff — Cell-model IR (ADR 0019)

**Active thread:** ADR 0019 "Cell-model IR" is **approved by the maintainer**
(all four design open questions resolved). **Plan checkpoints C1 and C2 are
both COMPLETE.** The IR + converter foundation is built (C1, #130 closed) and
GF180 now **simulates AND times** sequential cells from a generated descriptor
with no per-PDK Rust DFF handling and no runtime `.lib` parse (C2). Work is on
branch `docs/cell-model-ir-adr` (worktree `../Jacquard-cellir`), **PR #132 —
MERGEABLE, 18 commits, rebased onto current main (2026-06-26), all crates
version-aligned at 0.2.3**. Schema is now `SCHEMA 0.2` (`MAJOR=0, MINOR=2`).
Next up is **C3** (bundle + cut over + CI-regen provenance) and **C3a** (IHP
SG13G2, zero-Rust new PDK).

## Design decisions resolved (ADR 0019, all four open questions closed)

- **L2 source — Liberty-first, `.v` fallback (D6).** Survey of GF180/SKY130
  + commercial Liberate libs (Helvellyn, GF130, IHP) showed Liberty carries
  `function` (combinational) + `ff`/`latch` (sequential) for every cell,
  while the behavioural `.v` buries sequential logic in vendor-UDP +
  `notifier`/`$setuphold` simulation scaffolding. So the converter reads
  Liberty first; `.v`/UDP is a fallback for cells Liberty under-specifies.
  The `.v` `specify` block carries timing *topology* but **zero-placeholder
  values** (SDF scaffold) → L4 is Liberty-exclusive for stdcells;
  macro/SRAM `.v` can embed real timing that diverges (surface it).
  Violation-response (`notifier`→X) is a runtime policy, out of the IR.
- **L4 multi-corner (D5).** One descriptor, L4 keyed by corner (mirrors the
  timing IR's corner-set + `corner_index`; min/typ/max = within-corner
  derate). Simulation corner is **user-selected** (`--corner`, default
  `default_corner`), not inferred from netlist/SDF.
- **Provenance (D7).** Descriptors are **CI-regenerated** from pinned
  vendored PDKs and embedded at build time — NOT checked-in artifacts.
- **Migration (per-PDK).** Per-PDK cutover; **IHP SG13G2** added as a new
  built-in PDK with zero per-PDK Rust (D7a) — the proof that adding a PDK
  is no longer a code change.

## C1 — COMPLETE (7 commits on the branch, each CI-gated + /simplify'd)

Four small crates mirroring the `timing-ir`/`opensta-to-ir` pattern, then
the runtime wire-in:

1. **`crates/cell-decomp`** — PDK-neutral Verilog parse + AIG-decomp
   primitives, lifted out of `src/pdk_decomp.rs` + `src/sky130_pdk.rs`
   (made vendor-agnostic: UDP dispatch by map membership, not sky130 prefix).
2. **`crates/cell-model-ir`** — the JSON schema (D2): L1 pin directions + L2
   combinational AIG as a flat AIGER-like node list (`CombLogic`: const-0 /
   inputs / and-nodes, `Ref{node,inverted}`), `CombLogic::eval`, a structural
   diff + `cell-model-ir-diff` bin. Cells keyed by full netlist name, pins by
   netlistdb pin-name (D1). `SCHEMA_MAJOR/MINOR = 0.1`.
3. **`crates/liberty-parse`** — ONE generic Liberty parser (group tree +
   `Value`); `TimingLibrary` refactored to walk it (was a separate hand-rolled
   parser). `liberty_parser.rs` 1183 → 797 lines.
4. **`crates/liberty-to-cellir`** — standalone converter (like opensta-to-ir):
   `function.rs` compiles Liberty boolean exprs → AIG (precedence
   invert>AND>XOR>OR; TDD'd against an independent reference evaluator, 22
   tests); `convert.rs` walks the lib tree → `CellModelIr`; `crosscheck.rs`
   is the D6 Liberty-vs-`.v` eval cross-check.
5. **GF180 descriptor consumer** (`src/aig.rs` `splice_comb_logic` + a
   `--cell-descriptor` flag threaded through `from_netlistdb_with_cells_and_descriptor`):
   GF180 combinational cells build from the descriptor's pre-decomposed AIG
   instead of the hardcoded 7t `functional.v`. **Closes #130.**

**Proof:** GF180 7t and 9t descriptors both generate with
`cross_check_mismatches=0` (143 combinational cells agree with `.v` on every
input vector), 212 KB JSON (→ JSON-first holds, no FlatBuffers needed). The
`tests/jtag_minimal` 9t cosim is **byte-identical** between the legacy 7t
path and the `--cell-descriptor` 9t path (matching MD5), and the full
4M-edge descriptor run passes `data0_obs == 0xCAFEBABE`.

## C2 — COMPLETE (4 commits on the branch: C2.1 schema, C2.2 ×2 converter, C2.3 consumer)

1. **Schema (`cell-model-ir`, `SCHEMA_MINOR` 1→2, additive).** D4 L3
   `Sequential { clock: Option<ClockPin{pin,edge}>, enable, next_state:
   CombLogic, outputs: Vec<SeqOutput>, async_set, async_reset }` (scan-mux
   `D_eff=SE?SI:D` and gated-clock `E|TE` fold into the `next_state` AIG — no
   special-casing); `CellKind` enum; D5 L4 `CellTiming { delays, constraints,
   sram }` with descriptor-level `corners: Vec<Corner>` + `default_corner` and
   corner-indexed `TimingValue{corner_index,min,typ,max}` (f64 ps), mirroring
   `crates/timing-ir`. Diff tool extended.
2. **Converter (`liberty-to-cellir`): `sequential.rs` + `timing.rs` +
   `specify.rs`.** Emits L3 from Liberty `ff`/`latch`, L4 (corner-keyed) from
   `timing()` groups. D6 cross-check extended to L4 **arc-set agreement**
   (parses `.v` `specify` paths). GF180 7t/9t regen: combinational
   `cross_check_mismatches=0`, 48 cells emit L3, 211 emit L4, arc_disagree=0.
3. **Consumer (`src/aig.rs`, `src/liberty_parser.rs`, `src/bin/jacquard.rs`).**
   `wire_sequential_from_ir` drives DFF roles from the IR when
   `--cell-descriptor` is set (legacy path is the untouched fallback —
   per-PDK cutover). `TimingLibrary::from_cell_model_ir(ir, corner_index)`
   projects L4 onto the views the runtime already consumes; new `--corner`
   flag (default `default_corner`).

**Proof (C2.4 gate, PASS):** `tests/jtag_minimal` 9t cosim **byte-identical
MD5 `d87c9f75…`** legacy vs `--cell-descriptor` over 300k edges; full 4M-edge
IR run passes `data0_obs == 0xCAFEBABE`; flipping a descriptor async polarity
diverges the MD5 (roles are load-bearing). Timing equivalence vs the
`TimingLibrary::from_file` oracle is structural-match with **two intended
divergences, both fixes**: (a) the IR honours GF180 `time_unit : 1ns` (×1000
to true ps) which the legacy `parse_float_to_ps` **ignored** (legacy GF180
runtime timing was degenerate ≈0 ps); (b) negedge-DFF clk→Q arcs the oracle
silently dropped are now surfaced. No green CI path consumes GF180 L4 from a
descriptor yet, so neither regresses anything today.

### C2 findings carried into C3

- **GF180 set-dominant latches.** `latrsnq_1/2/4` are SET-dominant in Liberty
  (`clear_preset_var1=H`) but Jacquard's `wire_dff_reset_set_overlay` is
  reset-dominant. GEM simulates no true latches — the oracle treats them as
  reset-dominant level-enabled DFFs, so the IR path matches bit-for-bit and
  **no schema `clear_preset` field was needed**. The converter flags these as
  `clear_preset_divergent=3` generation diagnostics. If a future PDK needs a
  real set-dominant tie-break, add a `clear_preset` field then.
- **Residual name-based matches NOT yet retired** (fine for GF180, generalise
  in C3): `get_gf180mcu_dependencies` (`src/aig.rs` ~1524) still hardcodes
  `RN`/`SETN` as Q deps; the upfront clock-domain registration loop
  (`src/aig.rs` ~2090) still matches `"CLK"|"CLKN"|"PORT_*_CLK"` literally.
  SKY130/AIGPDK sequential branches still on the legacy path (per-PDK cutover;
  the IR helper is PDK-neutral — wiring SKY130 is a ~one-line dispatch add).
- **Clock gates** (`icgtp`/`icgtn`) emit no L3 (GF180 ICGs use Liberty
  `statetable`) → fall through to the unchanged legacy clock-gate path. No
  GF180 ICG fixture exists in `tests/` — **add one in C3** to gate this.
- **SKY130 end-to-end blocked in-worktree:** `vendor/sky130_fd_sc_hd/` ships
  only `.lib.json`, but `liberty-parse` reads `.lib` text → SKY130 covered by
  synthetic unit fixtures only this checkpoint.

## Next steps (in order)

1. **C3 — bundle + cut over + selection** (incl. CI-regen provenance + drop
   the runtime `vendor/` dep) and **C3a — IHP SG13G2** (new PDK, zero Rust).
   Wire descriptor generation into the build/CI (build-time, not committed —
   D7); add a clock-gate fixture; consider a committed end-to-end test
   asserting `from_cell_model_ir == 1000× from_file` over GF180 views (needs a
   crate that has both the descriptor and `TimingLibrary` — not co-located
   today). Generalise the residual name-based matches; wire SKY130/AIGPDK onto
   the IR path.
2. **C4 — proprietary-library workflow** doc + test.

## Key anchors (verified this session)

- Descriptor splice: `src/aig.rs` `splice_comb_logic` + dispatch in
  `gf180mcu_postprocess` (the `decompose_with_pdk` block); legacy path
  preserved in the `None` arm.
- Sequential pin-name matches to retire in C2: `src/aig.rs:2080-2260`.
- Per-cell-type timing to fold in (C2): `src/liberty_parser.rs` (`TimingLibrary`,
  now tree-walking `liberty-parse`), consumed at `src/aig.rs` + `src/flatten.rs`.
- Converter CLI: `cargo run -p`-style via
  `--manifest-path crates/liberty-to-cellir/Cargo.toml -- <lib> --functional-v <cells> -o <json>`.
  GF180 ships per-cell split libs; the converter merges them.
- CI runs the four pipeline crates' tests standalone in the Unit Tests job.
- **CI gotchas (cost real time this session, avoid next time):** (1) the
  **Lint** job runs `python3 scripts/bump_version.py --check` — the repo uses
  **lockstep crate versions**; every new crate must match the root version or
  Lint fails. (2) Verify CI at the **job level** (`gh run view <id> --json
  jobs`), not a blind `gh run watch --limit 1` (it latches onto whatever
  workflow registered last and can report a passing sub-run while the real CI
  Lint job is red). (3) When the PR shows `mergeable=CONFLICTING`, GitHub
  **won't dispatch `pull_request` CI at all** (can't build the merge ref) —
  rebase onto *current* `origin/main` (re-fetch first; main moves) and resolve
  before expecting CI to run.

## Loose ends

- The generated descriptors live in `/tmp/claude/gf180_{7t,9t}.json` (NOT
  committed — D7 says CI-regenerated). C3 wires generation into the build/CI.
- `vendor/gf180mcu_fd_sc_mcu9t5v0` submodule was initialised this session
  (1.5 GB) to generate + gate the 9t descriptor.
- C1 follow-ups noted in the C1.3c commit: no `.v`→IR fallback for
  no-`function` cells yet; `liberty-to-cellir` split-library discovery is
  GF180-shaped (a `--cells-dir` flag would generalise it); making
  `cell_decomp::eval_behavioral_model` fallible would drop the crosscheck's
  `catch_unwind`.
- **Two stray worktrees** off this repo (`../Jacquard-jtag-server`,
  `../Jacquard-cellir`) — maintainer asked to leave them; remove with
  `git worktree remove` when done.
- At resolution (C-series complete): fold this handoff into the ADR/plan and
  **delete this file** (one handoff at a time).
