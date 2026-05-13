# Handoff — GF180MCU Phase 4b + residual pre-release items

**Created:** 2026-05-13
**Working tree:** dirty (pre-existing user work — see § Uncommitted state)
**Branch:** main
**Supersedes:** the prior pre-release-runners-handoff (2026-05-02), which is
mostly resolved — Apple Silicon Metal runner is back online, Phase 1 sky130
multi-corner test landed, and GF180MCU enablement is in flight.

## Goal & next-up

**Goal of last session:** Land GF180MCU support through Phase 4 combinational
end-to-end (the release-blocker named by user 2026-05-12). Shipped.

**Next session should pick up:** GF180MCU **Phase 4b — sequential cells**.
The combinational sim path works today; DFFs, latches, and clock-gating
cells (`icgtp`/`icgtn`) currently panic in `AIG::gf180mcu_postprocess` at
`src/aig.rs` with a Phase 4b pointer. The work is well-bounded:

1. Read `src/sky130_pdk.rs::build_udp_aig` (the SOP-from-truth-table
   converter) — it's PDK-neutral grammar so most of it should reuse.
2. Extend `gf180mcu_pdk::Gf180PdkModels` to load UDP models alongside
   behavioural models in `load_pdk_models`. The UDP `.v` files live
   adjacent to cell `.functional.v` under the vendored submodule (find
   the exact path by inspecting one DFF cell's `.functional.v` which
   references `UDP_GF018hv5v_mcu_sc7_TT_1P8V_25C_verilog_nonpg_*_FF_UDP`
   — those UDPs are defined somewhere in the PDK).
3. Generalise `decompose_combinational` → `decompose_with_pdk` that
   accepts a UDP map and routes `UDP_GF018*` gate types to
   `build_udp_aig`. Mirror sky130_pdk's `decompose_with_pdk`.
4. In `AIG::gf180mcu_postprocess`, replace the `is_sequential_cell` panic
   with the same shape sky130 uses: pre-create DFF Q pin in
   `gf180mcu_preprocess`, then apply reset/set logic in postprocess.
   Note **reset polarity is active-low** for GF180 (`RN`, `SETN`) — same
   semantics as SKY130's `RESET_B`/`SET_B`. The AIG formula
   `OR(Q, NOT pin)` / `AND(result, pin)` works unchanged.
5. Handle negative-edge clock cells (`dffnq`, `dffnrnq`, etc.) — they use
   `CLKN` instead of `CLK`. AIG construction may need a clock-pin-name
   parameter on the DFF entry.
6. Tests: extend the boolean-equivalence suite in `gf180mcu_pdk::tests`
   to cover DFFs. Use a multi-step truth-table (one clock cycle) approach
   like sky130_pdk::tests::test_all_cells_vs_pdk does.

Estimated scope: ~300–500 LOC.

**After Phase 4b:** Phase 5 (CLI wiring — small, no real surface; the
detection in `AIG::from_netlistdb` already routes via cell library) and
Phase 6 (validation fixture + multi-corner integration test mirroring the
sky130 one in `crates/opensta-to-ir/tests/opensta_integration.rs`).

**Verification command:**

```sh
git log --oneline -14
cargo test --lib 2>&1 | tail -3
# Expect: 202 passed; 0 failed
cargo test --lib gf180mcu 2>&1 | tail -3
# Expect: 33 passed (combinational equivalence + classification + detection + e2e AIG)
```

## Done this session (2026-05-12, 13 commits on main, all CI green)

| Commit | Subject |
|---|---|
| `50f4bf5` | WS2.4 follow-up: real sky130 multi-corner Liberty test |
| `499269b` | sky130: use sky130A consistently across local helpers |
| `c00566d` | docs: plan for GF180MCU PDK full sim enablement (7-phase) |
| `6ae3e54` | GF180MCU Phase 0: foundations (volare pin + vendored cells + skeletons) |
| `858dd70` | GF180MCU Phase 1: library detection + cell-type extraction |
| `e97e2d2` | GF180MCU Phase 2: build.rs-generated pin direction provider |
| `6969b90` | GF180MCU Phase 3: cell classification (sequential/tie/filler/delay) |
| `12e98df` | ci: re-enable Metal jobs on macos-runner-1 |
| `92bb665` | GF180MCU Phase 4 recon: SKY130 behavioral parser is PDK-neutral |
| `02da077` | GF180MCU Phase 4 prep: PDK-neutral decomp module |
| `32fb3b9` | GF180MCU Phase 4 (combinational): AIG decomposition + equivalence tests |
| `d898343` | GF180MCU Phase 4 (aig.rs integration): combinational sim path end-to-end |

Test count: 166 → 202.

## Open follow-ups (priority-ordered)

### 1. GF180MCU Phase 4b — sequential cells (release-gating)

See § Goal & next-up. ~300–500 LOC, fully unblocked.

### 2. GF180MCU Phase 5 + 6 (release-gating)

Phase 5 (CLI map arms) is largely already covered by `AIG::from_netlistdb`
auto-detection; verify nothing else hard-codes a per-PDK branch. Phase 6
adds a synthetic `tests/timing_test/gf180mcu_timing/` fixture (DFF +
inverter chain, mirroring `sky130_timing/`) plus a multi-corner
`opensta_integration.rs` test against the volare-installed GF180 Liberty.

### 3. HIP/CUDA runtime violation routing (blocked on AMD/NVIDIA runners)

Carried forward from the prior handoff. `sim_cuda`/`sim_hip` accept
`timing_constraints` but ignore them — they call the simple-scan path
instead of the timed-batched path. Implementation guide in the previous
handoff (still in `git log -- docs/handoffs/`). AMD runner appears not
yet online; NVIDIA blocked on RAM.

### 4. End-to-end `--timing-report` test on the Metal runner (unblocked)

`macos-runner-1` is live and the existing sky130 multi-corner test
passes locally on Apple Silicon. The CI half of WS2.4 follow-up §2 can
now land — wire a small `--timing-report` end-to-end test against
`benchmarks/dataset/nvdla*` (or any small corpus design) in the `metal`
CI job. Validates the JSON shape against
`tests/timing_ir/sample_reports/two_violations.json`.

### 5. eda-infra-rs submodule bump (still blocked upstream)

Maintainer confirmed the `sverilogparse/Cargo.toml::license = "AGPL-3.0-only"`
typo, but hasn't pushed the fix. Verify with
`git -C vendor/eda-infra-rs fetch && git log origin/master --oneline`
and check whether the latest commit touches `sverilogparse/Cargo.toml`.
Once they do, the NOTICE footnote and `docs/release-process.md` § License
posture both close.

### 6. CUDA / HIP CI jobs (user-driven)

Both still disabled in `.github/workflows/ci.yml` (lines 268 and 357,
`runs-on: nvidia-runner-1`, `if: ${{ false }}`). Re-enable when
`nvidia-runner-1` hardware lands.

## Critical context

- **PDK refactor strategy.** The user explicitly chose "refactor first,
  then implement" for sharing infrastructure between sky130_pdk and
  gf180mcu_pdk. Phase 4 satisfied this in spirit via `src/pdk_decomp.rs`
  (a re-export module) and by exposing `WireVal` / `GATE_MARKER` /
  `build_chain_gate` / `build_xor_chain` / `finalize_decomp_result` as
  `pub(crate)`. The **physical relocation** of these from sky130_pdk.rs
  into pdk_decomp.rs is deferred; do it once Phase 4b confirms the API
  surface is stable enough. Plan Open Q2 ("CellLibrary enum location")
  is also still deferred.

- **build.rs pin-table generator** (`build.rs::generate_gf180mcu_pin_table`)
  is a new precedent — scans `vendor/gf180mcu_fd_sc_mcu{7,9}t5v0/cells/`,
  parses each `.functional.v`, asserts pin layouts match between 7t and
  9t (cross-check at build time), emits `$OUT_DIR/gf180mcu_pins.rs`. SKY130
  currently uses hand-rolled match arms in `src/sky130.rs`; the plan
  flags this as a follow-on cleanup. Don't do it during Phase 4b — keep
  scope tight.

- **Reset polarity convention** (resolves plan Open Q3): GF180MCU uses
  active-low resets/sets (port names `RN`, `SETN`). The "n" *prefix* in
  cell names like `dffnq`/`dffnrnq`/`icgtn` indicates a negative-edge
  clock (port `CLKN`), not reset polarity. Recorded in
  `src/gf180mcu_pdk.rs` module docs.

- **7t vs 9t pin layouts are byte-identical per cell type.** Verified at
  build time by `build.rs`'s dedup-and-assert pass. PDK model loading
  (`gf180mcu_pdk::load_pdk_models`) only reads from the 7t submodule.
  If 9t-only cells ever appear (or the libraries diverge upstream), the
  build will fail loud with a pin-mismatch panic from `build.rs`.

- **Mixed-library detection** is upstream of AIG construction
  (`src/sky130.rs::detect_library` reports `CellLibrary::Mixed` and
  `src/sim/setup.rs:62` panics on it). So `AIG::from_netlistdb_impl`'s
  two `Option<&PdkModels>` parameters are guaranteed to be at most one
  Some at a time. Don't worry about handling Sky130+GF180 mixed AIGs.

- **macos-runner-1 is live.** `.github/workflows/ci.yml::metal` and
  `mcu-soc-metal` both target it; both went green on first try. Custom
  label registered in `.github/actionlint.yaml`. Treat it as a stable
  resource for any new Metal-side CI work.

- **wafer.space integration is deferred.** Plan §Phase 7 placeholder for
  importing one of their test-run-1 designs as a real GF180 fixture.
  User indicated `https://github.com/wafer-space/gf180mcu` exists but
  to start with the upstream `google/gf180mcu-pdk` — which is what the
  vendored submodules at `vendor/gf180mcu_fd_sc_mcu{7,9}t5v0/` do
  (the per-library Google repos are
  `google/globalfoundries-pdk-libs-gf180mcu_fd_sc_mcu{7,9}t5v0`).

## Uncommitted state

Two files have edits that **were not made this session**:

- `CLAUDE.md` — one-line refinement of the "Key limitation" paragraph
  distinguishing `sim` (static VCD) from `cosim` (reactive peripheral
  models). Pre-existed at session start (2026-05-02 17:39).
- `docs/plans/multi-clock-and-stimulus-architecture.md` — 291-line
  design-space doc explicitly marked "Captured architectural thinking,
  not committed". Pre-existed at session start.

Both are the user's in-progress work; this handoff and every session
commit deliberately avoided touching them. Don't commit them as part of
GF180MCU follow-up work — that's outside this handoff's scope.

## Migration note

When this handoff resolves (Phase 4b + 5 + 6 all land):

- Fold the Phase 4b implementation notes into
  `docs/plans/gf180mcu-enablement.md` as the actual Phase 4b commit
  recap. Move the deferred refactor + Open Qs into a follow-on cleanup
  ticket.
- Delete this handoff file (`git rm docs/handoffs/gf180mcu-phase4b-handoff.md`)
  in the same commit as the Phase 6 closure.
- The remaining residual items (HIP/CUDA routing, eda-infra-rs bump,
  CUDA/HIP CI) all migrate to a fresh "release-blockers" handoff or
  fold into `docs/release-process.md` § Pre-release checklist as
  appropriate.
