# Handoff — backend alignment (CUDA/HIP/Metal) + cosim portability

**Created:** 2026-06-07
**Branch:** `feat/cosim-seam-phase0` (PR #115 open). `main` @ `d0e25a8`+ (all
session work below merged).
**Working tree:** clean (after this handoff commit).

## Goal & next-up

**Goal:** bring CUDA/HIP up to Metal parity. Two tracks: (a) cross-backend
*equivalence* of the `sim` kernel (done — guard in CI), (b) **cosim backend
portability (#105)** — currently executing **Phase 0** (the seam extraction).

**Now (pick up here):** PR **#115** (Phase 0 step 1 — CPU `simulate_block_v1`
consolidation) is open and CI-watching (run 27087166445). When green, merge
it, then continue Phase 0 **step 0b** (de-Metal `ScheduleBuffers`). Decision
point left with maintainer: keep going through 0b/0c now, or land 0b first.

**Verify:** `cargo test --lib` (293+ pass); Metal cosim parity:
`cargo run --release --features metal --bin jacquard -- cosim
tests/xprop_cosim/xprop_demo_synth.gv --config
tests/xprop_cosim/sim_config.json --output-vcd /tmp/o.vcd --max-clock-edges
100 --check-with-cpu` (mismatch report is a pre-existing clock-flag artifact,
see Findings).

## Done this session (all merged to `main` unless noted)

- **#108 reg_init value-injection** (PR #110, merged) — `reg_init` in
  testbench JSON, `$deposit` at t0. `src/testbench.rs`,
  `src/sim/trace_signals.rs` (`resolve_to_input_state_pos`),
  `src/sim/cosim_metal.rs`. **+ fixed zero-SRAM cosim panic** (nil
  `MTLBuffer`, sized `max(1)`).
- **#96 bidir tristate read-back** (PR #111, merged) — `Y = OE ? A : external`
  mux in `src/aig.rs` (`bi_24t` branch); ADR 0016 updated; truth-table test.
- **#95 closed** (was already done via `296cc12`). Cross-linked #102/#103/#106/#108.
- **CI / runners** (PR #112, merged): re-enabled **CUDA + HIP on the new
  GitHub-hosted T4 `tesla4-runner`** (every push); Metal light jobs offload to
  **`macos-latest-xlarge`** on `main`/`ci:metal-xl` label (gated, billed),
  else free self-hosted `macos-runner-1`; **`pull_request:` trigger
  un-filtered** (stacked PRs now get CI); workflow `concurrency` cancel.
  **Maintainer set a $200/mo Actions budget** — that's what un-stuck the T4
  (was `Shutdown` with no budget).
- **#113 cross-backend equivalence test** (merged) — `scripts/ci/
  compare_backend_vcds.py` + `backend-equivalence` job diffs CUDA/HIP/Metal
  output VCDs (functional + `--xprop`). **Verified bit-identical.**
- **#114 cosim portability ADR + plan** (merged) — ADR 0017 *Amendment
  2026-06-05*; `docs/plans/cosim-backend-portability.md` (#105 phasing).
- **#115 Phase 0 step 1** (OPEN) — consolidated cosim's ~265-line private
  `simulate_block_v1` duplicate onto `cpu_reference`; verified
  behaviour-preserving (release `--check-with-cpu` report identical).

## Carry-forward / open threads

- **v0.1.0 is pushed but NEVER TAGGED.** Release commit `7fed695` is on
  `main`; to finish: `git tag -a v0.1.0 -m "v0.1.0" <commit> && git push
  --tags` → `release.yml` draft → Homebrew tap PR (`gpu-eda/homebrew-tap`) →
  `netlist-graph` PyPI + tag. (Deliberately maintainer-triggered; see
  `docs/release-process.md`.)
- **#105 cosim portability — Phase 0 remaining**: 0b de-Metal
  `ScheduleBuffers` → `Vec<BitOp>` (`cosim_metal.rs:3072`,
  `update_model_driven_in_ops`/`update_reset_in_ops` ~`:3643-3678`); **0c
  `CosimBackend` trait + `MetalBackend` impl** (the big one); 0d move ~20
  `device.new_buffer` allocations into the backend. Then Phase 1 (CpuBackend +
  Linux cosim CI), Phase 2 (CUDA/HIP GPU cosim, **now T4-testable**), Phase 3
  (port GPU IO kernels). Full map in the plan doc + ADR 0017 amendment.
- **Latent debug-only bug** (flagged in #115): `cpu_reference::
  simulate_block_v1`'s `mask & (-(mask as i32)) as u32` panics on `mask` bit
  31 in **debug** builds ("negate with overflow"); release wraps OK. One-line
  fix: `mask & mask.wrapping_neg()`. Worth a tiny standalone PR — it makes
  `cosim --check-with-cpu` runnable in debug.
- **Verify the `macos-latest-xlarge` path actually schedules** on a `main`
  push (only the gated branch path has run; the xlarge label hasn't been
  exercised end-to-end yet).
- **#104** (CUDA/HIP `sim` timing) — Metal-only today; now T4-testable.
  **#106/#107** (x-assert detection/SVA). **#103** (multi-SRAM preload).
- **Single-source `simulate_block_v1` macro-prelude** (cross-shader compute
  kernel) — now safe to attempt because #113 guards it. Optional.

## Key decisions & findings

- **cosim is per-edge dispatch on every backend** → it sidesteps CUDA
  cooperative `grid.sync` (the hardest-to-port feature, used only by `sim`).
  This is why cosim portability is more tractable than it looks.
- **Peripheral models are already backend-agnostic CPU Rust**
  (`src/sim/models/*.rs`); on-GPU IO kernels are a Metal perf optimization,
  not a correctness prerequisite. `cpu_reference::simulate_block_v1` is the
  CPU design stepper; the `--check-with-cpu` path is a working prototype.
- **Backends are bit-identical** on `dff_test` (functional + xprop) —
  proven against real CI artifacts. CUDA/HIP code did NOT bitrot despite no CI
  since May.
- **Cross-shader tools rejected (for now):** Ferrox = no HIP, v0.1, ML-shaped.
  Slang = mature but AMD via Vulkan (not ROCm/HIP), no grid.sync, full
  rewrite. Neither closes the two real gaps. (Slang issue #9592 the maintainer
  spotted is *cooperative matrix*/tensor-core, unrelated to *cooperative
  groups* grid.sync.) Decision: in-house macro-prelude + equivalence test.
- **Stacked-PR + rebase-merge gotcha:** each merge gives new SHAs, so the next
  PR up owes a `git rebase origin/main` (patch-id-skips the dupes). Cost is
  inherent to rebase-merging a stack.

## `--check-with-cpu` mismatch (NOT a bug)

cosim `--check-with-cpu` reports a persistent INPUT mismatch at `word[0]`
bits [0,1] (clock/posedge flags) + one downstream output — present **before
and after** #115 (verified by stashing). It's a pre-existing modelling
artifact (GPU `state_prep` clock-flag injection vs the CPU check path), not a
real divergence and not in scope.

## References

- cosim portability: ADR 0017 (Amendment 2026-06-05),
  `docs/plans/cosim-backend-portability.md`, issue #105.
- Equivalence guard: `scripts/ci/compare_backend_vcds.py`, `backend-equivalence`
  CI job, PR #113.
- Runners: `.github/workflows/ci.yml`, `.github/actionlint.yaml`
  (`tesla4-runner` registered), `ci:metal-xl` label.
- Release: `docs/release-process.md`, ADR 0018, `packaging/README.md`.
