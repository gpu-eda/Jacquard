# Handoff — CUDA/HIP cosim performance measurement (parity track shipped; perf is untuned)

**Created:** 2026-06-21 (rolling handoff for the CUDA/HIP backend workstream; the
*correctness* parity track shipped to `main` in PR #120, so this now scopes the
performance follow-up + parked threads)
**Working tree:** clean
**Branch:** `main` @ `e74b2fe`

## Goal & next-up

**What shipped:** CUDA/HIP cosim is now at full functional parity with Metal —
byte-identical across all backends, T4-green. PR #120 (rebase-merged) landed
Track 0 (#104 sim timing) + cosim Phase 2 Stages A/B/C. The track closed on
**correctness**; performance is **untuned**.

**Next session should pick up: issue #122 — CUDA/HIP cosim performance
(measurement-driven).** Maintainer-agreed sequence was *merge first, then tune
with measurement* — first half done. The concrete first action is a **profiling
pass on the T4** (`nsys`/`ncu`) against the heavy `mcu_soc` flash fixture, to test
whether `cudaMallocManaged` page-migration is the per-edge tax (see hypothesis +
data below). Then A/B managed vs pinned-host + explicit mirror. Full plan + data
table live in **issue #122**; the memory-model decision + documented fallback are
in [`cosim-phase2-cuda-hip.md`](../plans/cosim-phase2-cuda-hip.md) §1b.

**Verification command (confirm the shipped state):**
```sh
git log --oneline -3 origin/main
# expect: e74b2fe ci skip-docs · 4d3eb34 handoff re-point · 83ad55d handoff Stage C
# Stage C correctness oracle — no GPU needed (CpuBackend == Metal == CUDA/HIP):
cargo build -r --bin jacquard
JACQUARD_BIN=target/release/jacquard COSIM_SCOPE=flash bash scripts/ci/cosim_cpu_check.sh
# Expect: "PASS  mcu_flash" / "all 1 cosim fixtures PASS"
```

## Done this session (all on `main`)

| Commit | Subject |
|---|---|
| `a071326` | feat(cosim): CUDA/HIP GPU SPI-flash kernels — Stage C C3 (#105) |
| `7c54448` | ci(cosim): gate CUDA/HIP flash cosim on mcu_soc golden — Stage C C4 (#105) |
| `83ad55d`,`4d3eb34` | docs(handoff): Stage C complete + re-point to perf (#122) |
| `e74b2fe` | ci: skip the matrix on docs-only pushes to main (PR #123) |

- **Stage C C3/C4** — CUDA/HIP flash kernels + CI gate. As-built detail folded
  into [`cosim-phase2-stageC-flash.md`](../plans/cosim-phase2-stageC-flash.md)
  "Done" + the checkpoint table in
  [`cosim-phase2-cuda-hip.md`](../plans/cosim-phase2-cuda-hip.md).
- **PR #120 rebase-merged** → CUDA/HIP cosim parity track shipped.
- **PR #123** — docs-only pushes to `main` now skip the billed matrix
  (`paths-ignore` on the **push** trigger only; deliberately **not** on
  `pull_request` — required status checks would never report → merge deadlock).
- **Issue #122 opened** — the performance follow-up (full data + plan).

## Open follow-ups (priority-ordered)

### 1. CUDA/HIP cosim performance — measurement-driven (#122) ← next

The v1 CUDA/HIP backend uses `cudaMallocManaged`/`hipMallocManaged` (closest
analog to Metal `StorageModeShared`). **Hypothesis:** managed-memory
page-migration is the per-edge tax that erases CUDA's launch-efficiency lead on
heavy designs. Evidence (correctness-CI byproduct timing, 100%-batched both
backends, 3 different machines — order-of-magnitude only):

| Fixture (10k edges) | CUDA (T4) | Metal |
|---|---|---|
| `dual_uart` (light) | **19.2 µs/edge** | 48.8 µs (CI runner) — ~2.5× CUDA |
| `mcu_soc` flash (heavy, 19.6 MB) | **64.9 µs/edge** | 67.3 µs (local M-series) — ≈ parity |

Plan: (1) profile on T4 (`nsys`/`ncu`) — real repeated-trial baseline + migration
traffic on the per-edge kernel chain (`state_prep → apply_flash_din → simulate×N
→ flash_model_step → gpu_io_step → snapshot`); (2) A/B managed vs pinned-host +
explicit mirror (isolated behind the `CosimBackend` trait — only the backend
struct in `src/sim/cosim/cuda.rs`/`hip.rs` changes); (3) land only if measured,
record in an ADR/perf doc. **CI/T4-bound** (dev box is Metal-only). Start a fresh
branch. Mind the $200/mo Actions budget; `mcu_soc` costs ~40s partitioning/run.

### 2. T2.3 — `GpuPeripheral` seam + cross-backend cosim equivalence gate (optional)

Extend `backend-equivalence` (`scripts/ci/compare_backend_vcds.py`) with an
explicit N-way cosim diff (cuda vs hip vs metal vs CPU golden) + define the
peripheral seam for Phase 3 (Tier-3 single-source). Lower priority — the flash
gate already achieves equivalence transitively (each backend diffs the same
committed golden). Scoped in `cosim-phase2-cuda-hip.md` (T2.3 row + "Peripheral
kernels" section); ADR 0017 Layer 3.

### 3. #104 deferred follow-ups (low priority; #104 is functionally done)

(a) Cross-backend **timed-VCD** equivalence in `backend-equivalence` (needs
artifact-flow restructuring — `metal-outputs` upload precedes the timing step).
(b) A **violating fixture** — `inv_chain_pnr` has 0 violations, so the
event observe/drain path runs empty; a real setup/hold-violating design would
exercise `write_event`→drain→`observe` + a cross-backend violation-count assert.

### 4. `v0.1.0` still untagged

Release commit `7fed695` is on `main` but never tagged. To finish: tag +
push → `release.yml` draft → Homebrew tap PR (`gpu-eda/homebrew-tap`) →
`netlist-graph` PyPI + tag. Deliberately maintainer-triggered; procedure in
[`docs/release-process.md`](../release-process.md).

## Critical context

- **Validation is CI-only on the T4.** This dev box is Apple-Silicon/Metal-only
  — `cuda`/`hip` cannot be built or run locally (nvcc/hipcc absent). Mirror the
  Metal path line-for-line, batch pushes to minimise billed CI cycles. The
  Phase-1 CPU goldens (`tests/*/expected/`) + Metal are the correctness oracles.
- **ABI drift is the #1 CI failure mode** for the GPU `#[repr(C)]` structs.
  Shared structs carry `size_of`/`static_assert` guards; `FlashState` offsets are
  derived via `std::mem::offset_of!`. Keep both sides in lockstep.
- **Bit-identical gate harness:** `JACQUARD_BIN=… COSIM_SCOPE={logic|all|flash}
  bash scripts/ci/cosim_cpu_check.sh` runs the committed-golden fixtures on any
  backend binary. `flash` is mcu_soc (heavy, kept separate from `all`).
- **Merge mechanics:** `main` requires no review but 7 status checks on the head
  SHA (`strict`, `enforce_admins:false`). A docs-only PR-head would deadlock (no
  CI on it) — PR #123 mitigates push-side; for PRs, keep a code change in the
  head or admin-merge.
- **Stale local branches:** the merged `cuda-hip-parity` branch + several older
  `feat/`,`fix/` branches still exist locally; prune at will.

## References

- Issues: **#122** (perf follow-up), #120 (parity PR, merged), #123 (CI docs-skip).
- Plans: [`cosim-phase2-cuda-hip.md`](../plans/cosim-phase2-cuda-hip.md),
  [`cosim-phase2-stageC-flash.md`](../plans/cosim-phase2-stageC-flash.md).
- ADR 0017 (cosim execution model + peripheral contract).

---

**Resume in a new session with:**
```
/resume_handoff docs/handoffs/backend-alignment-handoff.md
```
