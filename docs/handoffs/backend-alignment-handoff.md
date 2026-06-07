# Handoff — backend alignment (CUDA/HIP/Metal) + cosim portability

**Created:** 2026-06-07 (updated 2026-06-07, second session)
**Branch:** `main` @ `c3e9e0f` (#115 merged + handoff + JTAG CI pin). Two
PRs open off it: **#116** (debug-build mask fix), **#117** (telemetry +
rename + **target-architecture docs**).
**Working tree:** clean (after this handoff commit).

## Goal & next-up

**Goal:** bring CUDA/HIP up to Metal parity. Two tracks: (a) cross-backend
*equivalence* of the `sim` kernel (done — guard in CI), (b) **cosim backend
portability (#105)** — Phase 0 (the seam extraction).

**Now (pick up here):** Phase 0 implementation is **not started** — the
design review concluded and the **target architecture is now in ADR 0017**
(*Amendment 2026-06-07*, shipped in #117). Read it before any code. Key
decisions (maintainer-approved):
- `CosimBackend` trait is **batch-granular** (`run_edges`, not per-edge) —
  a literal per-edge trait regresses Metal ~1000× (measurements in the ADR).
- **GPU peripherals are the primary path to CUDA/HIP batching** (a discrete
  GPU's per-edge PCIe round-trip is untenable). 3-tier `GpuPeripheral`:
  Tier 1 CPU model (reference/oracle/fallback), Tier 2 hand-written kernels
  (CUDA+HIP share `*_impl.cuh` ⇒ 2 impls, + `.metal`), Tier 3 single-source
  user-extensible (later).
- Plan re-sequenced: P0 seam → P1 CpuBackend+Linux CI → P2 CUDA *correctness*
  (per-edge, CI gate, NOT perf) → **P3 GPU peripherals = the perf path** →
  P4 single-source. See `docs/plans/cosim-backend-portability.md`.

When resuming Phase 0: trait can land in-place in `cosim_metal.rs` first
(module split to `cosim/{mod,metal}.rs` is separable/cosmetic). The
load-bearing friction is de-Metaling the in-place `*mut BitOp` shared-memory
ops mutation (the trait's explicit upload point removes it).

Open PRs: **#116** (green, trivial — merge anytime), **#117** (telemetry +
rename + ADR/plan target-architecture rewrite — **needs design review**).
Both off `main`; merge in either order.

**Bit-identical harness ready:** `/tmp/claude/cosim_fixtures.sh <outdir>`
runs all 7 Metal cosim fixtures; golden baseline at `/tmp/claude/golden`
(sums in `/tmp/claude/golden.sums`). Re-run + `cmp` after any 0b/0c step.

**Verify:** `cargo test --lib` (293+ pass). Metal cosim parity via the
fixtures script above (all PASSED, byte-identical to golden).

## Done this session (2nd session, 2026-06-07)

- **#115 merged** (rebase-merge → `main` @ `1371c52`). CPU
  `simulate_block_v1` consolidation; CI green pre-merge.
- **#116 (OPEN, green)** — `fix(cpu_reference)`: `mask & mask.wrapping_neg()`
  replaces `mask & (-(mask as i32)) as u32` (2 sites in `cpu_reference.rs`),
  fixing the debug-build "negate with overflow" panic on bit 31 → makes
  `cosim --check-with-cpu` runnable in debug. (Carry-forward item below, now
  done.)
- **#117 (OPEN, needs design review)** — `feat(cosim)`, three commits:
  (1) batch-utilisation telemetry in the run summary (behaviour-preserving,
  7 fixtures byte-identical); (2) rename `edges_per_sys_clk*` →
  `sched_ticks_per_sys_clk*` + comment fix (it counts dense scheduler ticks
  per sys_clk period, not the always-2 transitions); (3) **ADR 0017 rewritten
  to the target architecture** + plan re-sequenced (see Now section).
  **Key data:** GPU-peripheral designs (dual_uart, apb_trace, xprop) run
  **100% batched**; `jtag_minimal` batches 97.4% of edges but emits **102,310
  single-edge commits** (96% of submits) — the measured MC.3 bottleneck.
- **JTAG CI pinned to self-hosted** (`main` @ `c3e9e0f`) — the
  `macos-latest-xlarge` path (first exercised on a main push) is ~3× slower
  for the per-edge JTAG-replay cosim and **timed out at the 15-min step cap**
  (run 27088699240, 1.8M/4M). Pinned `jtag-minimal-cosim` to `macos-runner-1`;
  main green again (JTAG 10.8 min). Resolves the xlarge carry-forward item.

## Done (1st session, all merged to `main` unless noted)

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
- **#105 cosim portability — Phase 0 remaining** (PAUSED for design review):
  0b de-Metal `ScheduleBuffers` → `Vec<BitOp>` (`cosim_metal.rs:3072`,
  `update_model_driven_in_ops`/`update_reset_in_ops` ~`:3643-3678`); **0c
  `CosimBackend` trait + `MetalBackend` impl** (the big one); 0d move ~20
  `device.new_buffer` allocations into the backend. **Design correction from
  this session (see #117 / ADR 0017 amendment):** 0b and 0c are entangled —
  the shared-memory in-place ops mutation (`*mut BitOp` via `.contents()`) is
  the load-bearing Metal coupling; de-Metaling it needs the explicit upload
  point the trait provides. And the trait must be **batch-granular** ("run N
  edges, snapshot each to ring") to preserve Metal's batched fast path, not
  the literal per-edge `simulate_edge` in the original plan sketch. The
  module split (`cosim/mod.rs` + `cosim/metal.rs`) is separable/cosmetic —
  the trait can land in-place first. Then Phase 1 (CpuBackend + Linux cosim
  CI), Phase 2 (CUDA/HIP GPU cosim, **now T4-testable**), Phase 3 (port GPU
  IO kernels). Full map in the plan doc + ADR 0017 amendment.
- **CDC/island batching (multi-clock plan MC.1→MC.4)** is the long-term fix
  for JTAG's single-edge tail — fast `sys_clk` island runs ahead/batched
  while only the model-driven `tck` boundary needs per-edge handover. MC.4
  needs the MC.1 island partitioner; **orthogonal to and larger than the #105
  seam.** Trigger now measured (#117).
- ~~**Latent debug-only bug** (mask bit-31 overflow)~~ — **DONE in #116**
  (open, green). `mask & mask.wrapping_neg()` in `cpu_reference.rs`.
- ~~**Verify the `macos-latest-xlarge` path schedules on a main push**~~ —
  **RESOLVED**: it does schedule, but is ~3× slower for heavy per-edge cosim
  → JTAG timed out and is now pinned to `macos-runner-1` (`c3e9e0f`). The
  light `metal` job still uses xlarge on main. Re-evaluate the pin after
  multi-clock/island batching cuts JTAG's per-edge tail (MC.3/MC.4).
- **#104** (CUDA/HIP `sim` timing) — Metal-only today; now T4-testable.
  **#106/#107** (x-assert detection/SVA). **#103** (multi-SRAM preload).
- **Single-source `simulate_block_v1` macro-prelude** (cross-shader compute
  kernel) — now safe to attempt because #113 guards it. Optional.

## Key decisions & findings

- **Batch utilisation measured (this session, #117).** GPU-peripheral cosim
  runs 100% batched; only CPU-side models (JTAG replay) + diagnostic modes
  (`--check-with-cpu`/`--trace-signals`/dff-dump/deep-diag) force `batch=1`.
  Drives the *batch-capable trait* refinement above. Telemetry now in the run
  summary (`Batch utilisation:` line). Reusable harness: `cosim_fixtures.sh`.
- **cosim is per-edge dispatch on every backend** (reactive path) → it
  sidesteps CUDA cooperative `grid.sync` (hardest-to-port, `sim`-only). But
  Metal's *production* path is batched (see batch refinement above) — the
  trait must accommodate both. This is why cosim portability is more tractable
  than `sim` but the seam is subtler than the original sketch.
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
