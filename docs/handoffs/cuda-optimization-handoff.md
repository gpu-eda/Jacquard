# Handoff — CUDA cosim optimization (measurement done; implement the fix)

**Created:** 2026-06-22 (rolling CUDA/HIP backend handoff; parity shipped in
PR #120, the perf *measurement* track in #122 is now done — this scopes the
actual optimization work it pointed to)
**Working tree:** clean
**Branch:** `main` @ `fa5073d`

## Goal & next-up

**What's resolved:** the #122 perf *measurement* track is complete. The
`cudaMallocManaged` page-migration hypothesis is **refuted** (zero UM page
faults, all data movement explicit memcpy, mem ops ~5% of GPU time). ncu on the
80%-hotspot kernel `cosim_simulate_stage` gives the real verdict — it is
**latency / under-utilization bound, not compute- or memory-bound**:

| Metric | Value |
|---|---|
| Compute (SM) Throughput | **7.9%** |
| Memory Throughput | **11.7%** |
| **Waves Per SM** | **0.80** (< 1 → grid doesn't fill the GPU once) |
| Achieved / Theoretical Occupancy | 27% / 50% |
| Registers Per Thread | 124 (→ Block-Limit-Registers = 2/SM, the occ cap) |

**Next session should pick up: the CUDA cosim optimization itself.** The
data-backed lever is **parallelism per launch** — each per-edge
`simulate_stage` grid is too small to fill the T4's 40 SMs (0.8 waves). The
batched fast path already runs 100% batched at the *orchestration* level, but
each major-stage is still its own small grid launch. Candidate approaches
(measure each with the harness before/after):

1. **Cross-edge stage batching** — launch the same major-stage for *many edges*
   in one grid (more blocks per launch). Likely the biggest win.
2. **Stage fusion** — fewer, larger launches per edge (cut relaunch/latency).
3. **Concurrent streams** — overlap the small per-stage launches.

This is the MC.4 "per-island multi-rate batching" direction ADR 0017 gestures
at; see the multi-clock plan. **Full data + the revised direction live in
[issue #122](https://github.com/gpu-eda/Jacquard/issues/122) comments** — read
those first. Drop the managed-vs-pinned A/B (it optimizes a non-cost).

**Verification (confirm shipped state + re-baseline):**
```sh
git log --oneline -3 origin/main     # expect fa5073d / 5ced8c2 / b8e9ee2 (#122 harness)
# Re-run the profiler on the T4 (workflow_dispatch-only; ~minutes):
gh workflow run cuda-cosim-profile.yml --ref main -f ncu=true -f ncu_edges=20
gh run download <run-id> -n cuda-cosim-profile -D /tmp/prof   # read summary.md
# Baseline today: ~64 µs/edge, 100% batched (mean batch 909); simulate_stage
# 80% of kernel time, Waves/SM 0.80. A good optimization raises Waves/SM and
# Compute/Memory %, and lowers µs/edge.
```

## Done this session (all on `main`)

| Commit(s) | Subject |
|---|---|
| `9f27feb`,`b6366a3` | PR #121 merged (admin rebase-merge): gf180 SRAM-IP/ID-stub cell classification for cosim |
| `96f0066` | docs(jtag): plan + ADR 0013/0017 amendments for interactive `--jtag-server` (#124) |
| `79e895e`..`fa5073d` | #122 profiling harness, iterated to working end-to-end |

- **#122 harness** (`scripts/ci/cuda_cosim_profile.sh` +
  `.github/workflows/cuda-cosim-profile.yml`, `workflow_dispatch`-only):
  repeated-trial baseline + nsys (UM page-fault tracing, auto-discovered report
  names) + ncu-via-sudo + in-CI report extraction. Reusable to measure any
  future change.
- **#122 conclusion:** hypothesis refuted; `simulate_stage` under-utilization
  identified (above). Recorded in #122 comments.
- **PR #121** merged; **#124** plan + ADRs landed (implementation not started).

## Open follow-ups (priority-ordered)

### 1. CUDA cosim optimization (#122) ← next, the actual work
Implement parallelism-per-launch (options above). CI/T4-bound (dev box is
Metal-only). Measure each change with the harness; land only what measurably
moves Waves/SM + µs/edge. **Fold the #122 findings + whatever optimization
lands into a permanent home** (an ADR 0017 amendment or a new perf doc) — right
now the measurement results live only in #122 comments.

### 2. T2.3 — cross-backend cosim equivalence gate (optional, carry-over)
N-way cosim diff (cuda vs hip vs metal vs CPU golden) + the `GpuPeripheral`
seam. Lower priority — the flash gate achieves equivalence transitively.
Scoped in `docs/plans/cosim-phase2-cuda-hip.md` (T2.3) + ADR 0017 Layer 3.

### 3. #124 interactive `--jtag-server` (carry-over; plan ready)
Plan + ADR amendments landed (`docs/plans/jtag-debug-server.md`). Host-side
only; staged J1–J5; CI gate is a loopback test (no OpenOCD). Not started.

### 4. `v0.1.0` still untagged (carry-over)
Release commit on `main`, never tagged. Procedure in
`docs/release-process.md`. Maintainer-triggered.

## Critical context

- **All CUDA validation is CI/T4-only.** Dev box is Apple-Silicon/Metal-only
  (no nvcc/ncu). ncu reports **cannot be read locally** — extraction runs in-CI
  into `ncu_summary.txt`. Mind the $200/mo Actions budget; `mcu_soc` costs ~40s
  partitioning/run.
- **The profiling harness gotchas (now solved — reuse, don't re-discover):**
  - ncu needs **`sudo`** on the T4 (`ERR_NVGPUCTRPERM`); the harness auto-uses
    passwordless `sudo -E`.
  - ncu must be **imported under sudo too** — collection (root) deploys ncu's
    Sections cache under `$HOME` as root, so a non-root `ncu --import` throws
    `Permission denied .../Sections/version.txt`. Harness chowns `$OUTDIR` after.
  - `ncu --set full` × many launches **blows the 60-min job timeout** → bounded
    to `-k cosim_simulate_stage --launch-count 20 --set basic`.
  - nsys UM page-fault report names are **version-specific** (the `cuda_um_*`
    names don't exist in 2024.6.2) → harness auto-discovers via `--help-reports`.
  - `gh run watch --exit-status` can **return 0 even on cancelled/failed** runs
    — always re-check `gh run view --json conclusion`.
- **ABI drift** is still the #1 CI failure mode for the `#[repr(C)]` GPU structs
  if any optimization touches kernel signatures — keep `size_of`/`offset_of`
  guards in lockstep.

## References
- Issues: **#122** (perf — full data + revised direction in comments), #124
  (jtag-server), #120 (parity, merged).
- ADR 0017 (cosim execution model; MC.4 per-island batching gesture), ADR 0013
  (peripheral architecture).
- Plans: `docs/plans/cosim-phase2-cuda-hip.md`,
  `docs/plans/multi-clock-and-stimulus-architecture.md` (MC.4),
  `docs/plans/jtag-debug-server.md`.

---

**Resume in a new session with:**
```
/resume_handoff docs/handoffs/cuda-optimization-handoff.md
```
