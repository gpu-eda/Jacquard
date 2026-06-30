# Handoff — Cell-model IR (ADR 0019)

**Active thread:** ADR 0019 "Cell-model IR" is **approved by the maintainer**
(all four design open questions resolved). **Plan checkpoints C1, C2, C3.1,
C3.1b, C3.2, C3.3a, C3.3b, and C3.3c are COMPLETE.** The IR + converter
foundation is built (C1, #130 closed); GF180 **simulates AND times**
sequential cells from a generated descriptor with no per-PDK Rust DFF handling
and no runtime `.lib` parse (C2); descriptors are **generated + embedded at
build time** (C3.1, D7); and the converter now handles arbitrary commercial
Liberty — corner from Liberty PVT, ff internal-state-var — proven on the
proprietary GF130 (C3.1b). Work is on branch `docs/cell-model-ir-adr` (worktree
`../Jacquard-cellir`), **PR #132 — MERGEABLE, rebased onto current main, all 7
crates version-aligned at 0.2.4** (main released v0.2.4; see gotcha #4). Schema
is `SCHEMA 0.2` (`MAJOR=0, MINOR=2`). Next: **C3.2/C3.3** (selection + cutover)
and **C3a** (IHP SG13G2, zero-Rust, sparse checkout); the `clear_preset`
set-dominant field and the GF130 sim/doc proof are **C4**.

## C3.2 — COMPLETE (commit 5eb37d0)

Descriptor auto-selection by declared cell-name prefix (ADR 0019 D8). New
`bundled_descriptors::auto_select(cell_types) -> Result<Option<CellModelIr>,
String>`: matches a netlist's distinct cell-type names against each embedded
descriptor's declared D8 `library.prefixes`. `Ok(Some)` on a unique match,
`Ok(None)` on no match (→ legacy fallback), `Err` on ambiguity (netlist mixing
two descriptors' prefixes → actionable "pin with --bundled-descriptor/
--cell-descriptor" message). Precedence chain (extends C3.1's `resolve`):
explicit `--cell-descriptor` file > explicit `--bundled-descriptor` name >
**auto-match (new)** > legacy `None`. Wired into both the functional load path
(`src/sim/setup.rs::build_netlist_and_aig`, via `.or_else`) and the timing path
(`src/bin/jacquard.rs::run_timing_analysis`, now takes `&netlistdb`). The
`.cells.toml` hand-override (ADR 0010) is an orthogonal runtime-library layer
(`cell_lib_ref`), unaffected.

**7t-vs-9t disambiguation (the #130 crux) — declared prefixes ALONE suffice.**
The track designation is *inside* the cell name: 7t cells are
`gf180mcu_fd_sc_mcu7t5v0__*`, 9t are `gf180mcu_fd_sc_mcu9t5v0__*`. The
converter's `derive_prefix` (longest common prefix trimmed to `__`) therefore
yields disjoint declared prefixes — 7t descriptor declares
`gf180mcu_fd_sc_mcu7t5v0__`, 9t declares `gf180mcu_fd_sc_mcu9t5v0__`. So a 9t
netlist matches only the 9t descriptor. This is the whole point of #130: the
legacy `detect_library`/`PdkVariant` collapsed both tracks to one
`CellLibrary::GF180MCU` served by 7t models; prefix-keyed auto-match keys on the
full vendor prefix and picks the right track. A netlist mixing 7t+9t cells
(which `detect_library` does *not* flag as Mixed) is the only ambiguous case and
is rejected with a clear error rather than guessed.

**Equivalence proof (PASS).** `tests/jtag_minimal` (9t-only netlist) cosim over
300k edges with **no descriptor flag** auto-selects `gf180mcu_9t` and produces
output **byte-identical** (`md5 d87c9f75b4f6af415a9df2d3c728e906`) to the
explicit `--bundled-descriptor gf180mcu_9t` run — the C2 oracle. The CI cosim
(`.github/workflows/ci.yml`, no flag, 4M edges) thus now exercises the 9t
descriptor by default and still passes `data0_obs == 0xCAFEBABE` (#130 fix
becomes the default). No 7t end-to-end fixture exists in `tests/`; 7t selection
is covered by `auto_select_picks_7t_for_a_7t_netlist` and the C1/C2 descriptor==
legacy cross-checks. 5 new unit tests in `bundled_descriptors.rs` (9t/7t pick,
AIGPDK no-match, mixed-track ambiguity, prefix-presence guard for
submodule-absent builds). Full lib suite 314/314, version check 7 crates @
0.2.4, `cargo clippy --lib` no new `^error`.

**What C3.3 can now remove** once auto-select is the *only* descriptor source
(today it's the default but the legacy `None` arm still backstops non-GF180):
the GF180 branches of `PdkVariant` per-cell predicates + `gf180mcu_postprocess`
legacy combinational/sequential splice (the `None`-descriptor arms), the 7t
hardcoded `functional.v` default, and `build.rs`'s pin-table gen. **Still
blocking SKY130/AIGPDK auto-selection:** no embedded descriptors exist for them
(`bundled_descriptors::ALL` is GF180 7t/9t only) — SKY130 needs a `.lib.json`
reader (vendor ships no `.lib` text), AIGPDK needs hand-coded models with no
cross-check oracle (both noted in C3.1). Until those descriptors exist,
auto_select returns `Ok(None)` for SKY130/AIGPDK and they ride the legacy path.

## C3.3a/b/c — COMPLETE (branch `feat/cell-model-ir-c3`)

All three built-in PDKs now build **combinational** cells from a generated
cell-model-IR descriptor. The cutover is **additive** — legacy stays as the
fallback; nothing was deleted yet (that is C3.3d).

**C3.3a (AIGPDK descriptor, prior agent, validated).** `liberty-to-cellir
aigpdk/aigpdk.lib` → 11 cells (7 comb, 2 L3, 10 L4), `cross_check_mismatches=0`,
no converter change. Comb cells match `aigpdk.v` truth tables; DFF/DFFSR roles
match the legacy `aig.rs` overlay + `aigpdk.v`. RAM (`$__RAMGEM_SYNC_`), clock
gate (`CKLNQD`), and side-effect (`GEM_*`) cells are correctly excluded.

**C3.3b (embed, commit 4edf7f99).** `build.rs` now generates+embeds two more
descriptors into `$OUT_DIR` alongside GF180 7t/9t: **SKY130** (from the vendored
`.lib.json` per-corner layout, 428 cells, declared prefix `sky130_fd_sc_hd__`)
and **AIGPDK** (from in-repo `aigpdk/aigpdk.lib`, 11 cells, **no** prefix).
`bundled_descriptors::ALL` gains both; AIGPDK carries `is_default_fallback=true`
and a new `default_fallback()` returns it. SKY130 auto-selects by prefix;
AIGPDK is the no-prefix-match default (mirrors `pdk::resolve_library`).
Behaviour-neutral (consumer still GF180-gated). `cargo test --lib` 316 (+2).

**C3.3c (wire, commit 649d9986).** New PDK-neutral `AIG::try_descriptor_comb`
splices a descriptor `logic` block for ANY stdcell; wired into the `Process`
dispatch for SKY130 (before `sky130_postprocess`) and AIGPDK (before the
hardcoded AND2/INV/BUF match). `setup.rs` adds `.or_else(default_fallback)` so
AIGPDK netlists resolve the AIGPDK descriptor. Cells with no `logic` (seq / tie
/ SRAM / multi-output / IO-pad) return false → legacy path UNCHANGED.

**Gate (all green).** SKY130 `mcu_soc` flash cosim == committed golden (165
sky130 types — the descriptor's combinational logic, **never cross-checked at
generation**, is now proven byte-identical to legacy); AIGPDK xprop_cosim ×4 +
dual_uart + apb_trace(±xprop) == goldens; GF180 jtag_minimal 9t MD5
`d87c9f75b4f6af415a9df2d3c728e906` (`--features metal`, 300k edges), GF180
untouched; `cargo test --lib` 316; four pipeline crates green; 7 crates @ 0.2.4.

### What C3.3d (DESTRUCTIVE — not started) still needs

The additive landing means the legacy is all still present and load-bearing:
- **SKY130/AIGPDK *sequential* is still legacy.** Only *combinational* is
  descriptor-driven for them. Wiring seq needs coordinated changes to BOTH the
  input-side (`from_netlistdb_impl` seq branches ~2360 sky130 / ~2332 aigpdk)
  and the output-side overlay (`sky130_postprocess` ~1344, aigpdk `Process`
  ~1100), the way GF180 does both (`wire_sequential_from_ir` + the
  `gf180mcu_postprocess` IR overlay). GF180 seq is already IR-driven.
- **`PdkVariant` is NOT removed** (`grep -rn PdkVariant src/` still ~71 hits) —
  the dispatch classifies (is_sequential / is_tie / is_multi_output / is_io_pad
  / extract_cell_type) via PdkVariant + the build.rs-generated pin/classifier
  tables BEFORE consulting the descriptor.
- **Runtime `vendor/` stdcell dep still load-bearing.** `load_pdk_models`
  (`aig.rs` ~2158 sky130 / ~2179 gf180) still reads the vendored cells for the
  legacy decompose fallback AND the `LeafPinProvider` L1 pin directions. Before
  deleting it, confirm the descriptor carries L1 directions for the leaf-pin
  provider.
- **Residual name-matches still present:** the clock-tracing loop
  (`"CLK"|"CLKN"|"PORT_*_CLK"`, `aig.rs` ~2263), `get_gf180mcu_dependencies`
  `"RN"|"SETN"` (~1635), `sky130_postprocess` `SET_B`/`RESET_B` (~1361). These
  run during dependency/clock ordering before the splice and are intricate to
  make descriptor-driven.
- Then: remove `PdkVariant` + all uses, `build.rs::generate_pin_table` +
  `GF180MCU_PIN_TABLE`/`SKY130_PIN_TABLE` + the `generated` modules, the stdcell
  classifiers, and the `None`-descriptor fallback (only after every built-in PDK
  resolves a descriptor). PRESERVE per the boundary: `$__RAMGEM_SYNC_` + GF180
  SRAM macros, `GEM_ASSERT/DISPLAY`, `CKLNQD`/`icgtp`/`icgtn`, `bi_24t`/`in_c`/
  `in_s` IO pads, `.cells.toml` / `RuntimeCellLibrary`.

## C3.1b — COMPLETE (commit on branch)

Generalized `crates/liberty-to-cellir` for commercial Liberty, driven by the
GF130 findings below. (1) **Corner from Liberty PVT** —
`timing::corner_from_library` reads `operating_conditions` named by
`default_operating_conditions` (+ `nom_*` fallback), filename heuristic only
last; GF130 TT now `l4_timing=615, corners=1` (`{TT_1P50V_25C, tt, 1.5, 25}`),
GF130 SS a distinct corner; GF180 7t/9t L4 still emits (Liberty-derived corner
name, PVT unchanged) with `cross_check_mismatches=0`. (2) **ff internal
state-var** — `next_state` may reference the `ff(IQ,IQN)` state var; the
compiler folds `IQN`→`!IQ` and admits `IQ` as a self-feedback input appended
after real pins; GF130 `l3_next_state_errors` 20→0. **Consumer implication:**
such cells' `next_state.inputs` carry a name that is NOT a cell pin (the state
var) — a consumer must wire it to the flop's own current Q. GF180/SKY130 never
do this, so no green consumer path is affected (arises only for commercial
libs, which aren't on the consumer path until C3.3/C4). (3) cross-check now
**warns** on 0 `.v` models indexed (used `eprintln!` not `clilog` — the
converter deliberately doesn't depend on vendor `eda-infra-rs`).

## C3.1 — COMPLETE (commit 52be692)

Build-time descriptor generation + embedding (ADR 0019 D7). `build.rs` links
`liberty-to-cellir` (clap gated behind a `cli` feature) + `cell-model-ir` as
build-deps and generates `gf180mcu_{7t,9t}.cellir.json` into `$OUT_DIR`;
`src/bundled_descriptors.rs` `include_str!`s them and `resolve()` gives
`--cell-descriptor <file>` precedence over a new `--bundled-descriptor <name>`
flag (threaded through sim/cosim/xsources/timing). Converter loader extracted
to `crates/liberty-to-cellir/src/load.rs` (`generate_descriptor`). Determinism:
no `HashMap` into the descriptor (Vecs in Liberty order; split-lib discovery
sorts); `tests/determinism.rs` asserts byte-identity; build-time artifact ==
CLI output == `cell-model-ir-diff` clean. Cost ~0.55 s, only on vendored-lib
change. Proof: jtag_minimal 9t `--bundled-descriptor gf180mcu_9t` (no file) is
MD5 `d87c9f75…` — identical to the file/oracle runs. **Sits alongside the old
pin-table gen** (retires in C3.3). SKY130 deferred (`vendor/sky130_fd_sc_hd/`
ships only `.lib.json`, 0 `.lib` text — needs a `.lib.json` reader); AIGPDK
deferred (hand-coded models, no cross-check oracle). CI needs no changes
(GF180-less jobs get a valid empty descriptor; release jobs embed real ones).

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

## GF130 proprietary-PDK smoke test (2026-06-26) — converter generalizations it demands

Ran the C2 converter against the **proprietary GF130 (GF013BCD)** stdcell
library `~/Code/ChipFlow/PDK/pdk-gf130/IP005093` — a real commercial,
**monolithic single-`.lib`-per-corner** library (50 MB TT lib; 11 corners
FF/SS/TT; unified `GF013bcd_sc6_1p5_a0.v` with 2544 `module` defs). Generation
succeeded (`cells=636 combinational=443 l3_sequential=158`, 724 KB JSON, 0.7 s)
but exposed that the converter is still GF180/SKY130-shaped. Four findings, in
priority order — these are the **C4 / proprietary-hardening** work:

1. **L4 corner detection is filename-based (→ `corners=0, l4_timing=0` for
   GF130).** `crates/liberty-to-cellir/src/timing.rs` derives the corner from
   the GF180-style library *name* (`__tt_025C_5v00`). GF130's
   `GF013bcd_sc6_1p5_a0_TT_1P50V_25C_max` doesn't match → no corner → **no L4
   emitted at all**. The corner is in the Liberty itself:
   `operating_conditions(TT_1P50V_25C)` { process/temperature/voltage } +
   `default_operating_conditions` + `nom_process/nom_temperature/nom_voltage`.
   **Fix: read the corner from the Liberty PVT groups, not the filename.** This
   is the correct general source and also hardens the D5 multi-corner story for
   the built-ins. **Highest-value generalization.**
2. **`next_state` referencing the ff's internal state var (20 GF130 cells).**
   Commercial flops write next_state over the ff's own state node, e.g.
   `SEDFF_X4` → `((SE&SI)|((!SE)&((E&D)|((!E)&IQ))))` where `IQ` is the ff
   state variable. The converter rejects `IQ` as "not in inputs". Register the
   `ff(IQ,IQN)` variable names as known symbols (self-feedback) in the
   next_state compile.
3. **`.v` cross-check indexer found 0 models.** The GF130 `.v` is 2544 flat
   `module NAME(ports);` defs in one file; the indexer (built for GF180's
   per-cell `.behavioral.v` + UDP tree, single `--functional-v` dir) no-ops
   silently → `arc_no_specify=636`. Low priority (Liberty-first; cross-check is
   optional) but the silent 0 should at least warn.
4. **`clear_preset` set-dominant now seen in TWO foundry libs.** GF130 has
   **28** set-dominant latches (`TLATSR`/`TLATNSR`, `clear_preset_var1=H`),
   GF180 had 3. Recurrence across independent commercial libraries makes the
   case that a schema `clear_preset` tie-break field is genuinely needed for
   proprietary support, not deferrable forever. (Built-in sim is unaffected —
   GEM models no true latches — but a proprietary user simulating these would
   get wrong results.)

Descriptor at `/tmp/claude/gf130_sc6_tt.json` (NOT committed; proprietary —
never vendor GF130). Converter CLI used:
`cargo run --release --manifest-path crates/liberty-to-cellir/Cargo.toml -- <TT.lib> --functional-v <sc6.v> -o /tmp/claude/gf130_sc6_tt.json`.

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
  before expecting CI to run. (4) **Merge-ref version skew when main releases.**
  CI builds the `pull_request` *merge ref* (main ⊕ branch), so when **main cuts
  a release** (e.g. `aaa61b3 chore: release v0.2.4`) the merge ref takes main's
  bumped **root** version while the branch's **new** crates keep the old one —
  `bump_version.py --check` then fails **in CI even though it passes locally**
  (the branch is internally consistent). Fix: rebase onto current main and
  `python3 scripts/bump_version.py <new-version>` to re-align all 7 crates.
  Watch for this whenever `gh run view` shows only Lint red with the four
  cell-model-IR crates one patch behind root. (5) **`gh run watch
  --exit-status` returns 0 on a CANCELLED run** (and superseded runs get
  cancelled when you push again) — always confirm with `gh run view <id> --json
  conclusion,jobs`, never trust the watch exit code alone.

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
