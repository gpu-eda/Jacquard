# Handoff — backend alignment (CUDA/HIP/Metal) + cosim portability

**Created:** 2026-06-07 (updated 2026-06-07, second session)
**Branch:** `main` @ `1371c52` (#115 merged). Two PRs open off it:
**#116** (debug-build mask fix), **#117** (batch-utilisation telemetry +
seam-design findings).
**Working tree:** clean (after this handoff commit).

## Goal & next-up

**Goal:** bring CUDA/HIP up to Metal parity. Two tracks: (a) cross-backend
*equivalence* of the `sim` kernel (done — guard in CI), (b) **cosim backend
portability (#105)** — executing **Phase 0** (the seam extraction).

**Now (pick up here):** Phase 0b/0c is **paused for design review** (maintainer
chose "record findings, pause impl"). Before resuming, read the new
**seam-design refinement** (ADR 0017 amendment, *Measured batch utilisation*,
shipped in #117): the `CosimBackend` trait must be **batch-capable, not naive
per-edge** — Metal's production path batches up to `BATCH_SIZE` edges + GPU
peripherals + ring drain per command buffer, so a literal `simulate_edge`
would regress it ~1000×. The plan's trait sketch
(`docs/plans/cosim-backend-portability.md`) is updated accordingly.

Open PRs to land first: **#116** (green, trivial — merge anytime), **#117**
(CI watching, telemetry + docs). Both are off `main`; merge in either order.

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
- **#117 (OPEN)** — `feat(cosim)`: batch-utilisation telemetry in the run
  summary + recorded seam-design findings (ADR 0017 amendment, the
  portability plan, the multi-clock plan). Behaviour-preserving (7 fixtures
  byte-identical). **Key data:** GPU-peripheral designs (dual_uart, apb_trace,
  xprop) run **100% batched**; `jtag_minimal` batches 97.4% of edges but emits
  **102,310 single-edge commits** (96% of submits) — the measured MC.3
  bottleneck trigger.

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
- **Verify the `macos-latest-xlarge` path actually schedules** on a `main`
  push (only the gated branch path has run; the xlarge label hasn't been
  exercised end-to-end yet).
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
