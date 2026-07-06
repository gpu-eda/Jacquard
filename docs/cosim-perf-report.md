# Cosim Perf Report (`--cosim-perf-json`)

## Overview

`jacquard cosim` reports a per-edge timing breakdown of the co-simulation
loop, including **ground-truth GPU-execution time from device
timestamps** — free of the instrumentation overhead that a full GPU
trace (Metal System Trace / nsys) imposes on a workload of thousands of
tiny dispatches per batch.

The breakdown always prints in the human-readable profiling summary at
the end of a run. Passing `--cosim-perf-json <PATH>` additionally writes
a flat JSON record for CI to log and track.

## The measurement

The cosim loop batches edges into GPU command buffers (`BATCH_SIZE`
edges each). Per batch, the CPU: builds the command buffer (*encode*),
commits it, then waits for the GPU to finish (*spin*), then drains ring
buffers (*VCD/UART*). The existing summary times these CPU phases with
`Instant` deltas. This report adds the **GPU-side** truth:

- **Metal**: `MTLCommandBuffer.GPUStartTime`/`GPUEndTime` — host-clock
  timestamps the driver records for free; `GPUEndTime − GPUStartTime` is
  the batch's GPU-execution wall. Read after `waitUntilCompleted` (the
  driver posts the timestamps only at full completion).
- **CUDA / HIP**: `cudaEvent` / `hipEvent` elapsed time — *follow-up*
  (the trait seam exists; the impls land in a later PR validated on the
  GPU CI runner).
- **CPU backend**: none — `gpu_*` fields are `0` / `gpu_exec_recorded:
  false`.

The seam is one `CosimBackend` trait method,
`last_batch_gpu_seconds() -> Option<f64>`, called by the orchestration
after each batch's `wait()`. The generic loop owns the CPU timing and
report assembly; each backend supplies only its GPU clock.

## Human summary

```
=== Profiling Breakdown ===
  Batch encode + commit             7.6μs/tick   10.3%
  GPU wait (spin)                  66.3μs/tick   89.3%
  ...
  TOTAL (instrumented)             74.2μs/tick  100.0%
  GPU exec (device ts)             65.8μs/tick   88.7%  (GPU-busy share of wall)
```

`GPU exec` is a *separate* measure from the CPU categories — it overlaps
the `GPU wait (spin)` line (the CPU wall spent waiting *is* the GPU
execution). Its `%` is the GPU-busy share of the instrumented wall.

## JSON schema (report-only)

```json
{
  "backend": "metal",
  "edges": 500000,
  "wall_seconds": 40.1,
  "total_us_per_edge": 74.24,
  "gpu_exec_us_per_edge": 65.85,
  "gpu_util_pct": 88.7,
  "gpu_exec_recorded": true,
  "cpu_encode_us_per_edge": 7.56,
  "gpu_wait_spin_us_per_edge": 66.31,
  "drain_us_per_edge": 0.001,
  "output_vcd_us_per_edge": 0.36,
  "batches": 550,
  "mean_batch": 909.1
}
```

This is a report format, not a stable contract — it may gain fields.

## CI

The `mcu-soc-metal` job runs the mcu_soc cosim with `--cosim-perf-json`
and posts the JSON to the GitHub step summary (**report-only** — no
perf gate). This builds a timing history before we commit to a
regression threshold: GPU/thermal noise on shared runners needs a
measured noise floor first, and a premature gate would flake. Adding a
gate (fail if `total_us_per_edge` regresses beyond a tuned threshold vs
a committed baseline) is the natural follow-up once the history exists,
alongside the CUDA/HIP `last_batch_gpu_seconds` impls so every tested
architecture reports.

## Why not just use a GPU trace?

Metal System Trace / nsys instrument every Metal/CUDA call. For a batch
of ~2048 tiny dispatches that overhead is large and *systematically
distorts* the picture — it inflated an early measurement to make the GPU
look ~88% idle when device timestamps show it ~88% **busy**. Command-buffer
timestamps cost nothing and are the trustworthy signal for steady-state
throughput; reserve full traces for one-off per-dispatch investigation
(see `docs/gpu-capture.md`).
