# GPU Frame Capture (`.gputrace`)

## Overview

`jacquard cosim` (Metal backend) can emit an Xcode **`.gputrace`** document
capturing the exact Metal command stream of the GPU simulation — every
compute dispatch, buffer binding, and blit for a bounded window of edge
batches. This is the detailed counterpart to a system-level
`xctrace --template "Metal System Trace"` recording: the system trace shows
*when* GPU work runs relative to the CPU, while a `.gputrace` shows *what*
each dispatch does (threadgroup sizes, buffer arguments, per-dispatch
timing, dependency DAG, and — after an Xcode replay — shader occupancy and
memory-bandwidth counters).

Capture is **env-gated and zero-overhead when off**: with no
`JACQUARD_GPU_CAPTURE` set, none of the capture code runs.

## Quick start

```bash
# Capture one steady-state batch (skip the reset/warm-up batch 0).
# METAL_CAPTURE_ENABLED=1 is REQUIRED — Metal refuses programmatic
# .gputrace capture without it.
METAL_CAPTURE_ENABLED=1 \
JACQUARD_GPU_CAPTURE=/tmp/cosim.gputrace \
JACQUARD_GPU_CAPTURE_SKIP=1 \
  cargo run -r --features metal --bin jacquard -- cosim \
    tests/qspi_psram/qspi_psram_dut_synth.gv \
    --config tests/qspi_psram/sim_config.json \
    --top-module qspi_psram_dut \
    --max-clock-edges 200 \
    --output-vcd target/test-out/qspi_psram.vcd

open /tmp/cosim.gputrace   # opens in Xcode's GPU debugger
```

## Environment variables

| Variable | Default | Meaning |
|----------|---------|---------|
| `JACQUARD_GPU_CAPTURE` | *(unset)* | Output path for the `.gputrace` bundle. Setting it enables capture. |
| `JACQUARD_GPU_CAPTURE_SKIP` | `0` | Batches to run before opening the capture window. Skip the reset/warm-up batches to grab a steady-state one. |
| `JACQUARD_GPU_CAPTURE_BATCHES` | `1` | Number of consecutive batches to capture. Keep small — each batch is thousands of dispatches. |
| `METAL_CAPTURE_ENABLED` | *(unset)* | **Required** by Metal for programmatic `.gputrace` capture. Without it, capture is skipped with a warning and the run continues uncaptured. |

## How it works

The Metal backend commits **one command buffer per edge batch**
(`MetalSimulator::encode_and_commit_gpu_batch`). Each batch encodes, for
every edge in it, the full per-edge dispatch chain:

```
state_prep → apply_flash_din → simulate_v1_stage × N → flash_model_step → io_step → VCD blit
```

Capture brackets an `MTLCaptureManager` scope (destination
`GpuTraceDocument`, scoped to the simulator's command queue) around the
window `[SKIP, SKIP + BATCHES)`:

- **Start** — immediately before the first in-window batch's command buffer
  is created, so the trace opens on a clean batch boundary.
- **Stop** — after the last in-window batch is committed; that command
  buffer is waited to completion first so its GPU work is recorded before
  `stopCapture()` finalizes the document.

A stale bundle at the target path is removed first (Metal refuses to
overwrite). Any failure (missing `METAL_CAPTURE_ENABLED`, unsupported
destination) is logged and the simulation continues normally, uncaptured.

## Choosing a batch to capture

Batch 0 is the reset/warm-up batch and is usually small and unrepresentative.
`JACQUARD_GPU_CAPTURE_SKIP=1` (or higher) captures a steady-state batch.
The batch size is `BATCH_SIZE` edges (see `src/sim/cosim/mod.rs`), so a
single captured batch already contains the complete per-edge dispatch stream
repeated across the batch — enough to profile the kernel without a giant
multi-batch trace.

## Shader performance counters

The `.gputrace` records the command stream and per-dispatch timing out of
the box. **Occupancy, bandwidth, and cache-hit counters require an Xcode
replay with shader profiling enabled**: open the trace in Xcode, press
**Replay**, and Xcode writes the counter `streamData` back into the bundle.
Only then do the `profiler_gpu_counters` / `profiler_gpu_scheduling`
analyses have data to read.

## Relationship to system-level tracing

For CPU↔GPU timeline correlation (where the sim spends wall-clock, GPU duty
cycle, inter-batch CPU gaps) use a Metal System Trace instead:

```bash
xctrace record --template "Metal System Trace" --output cosim.trace \
  --launch -- jacquard cosim ...
```

That `.trace` answers "is the sim CPU- or GPU-bound?"; the `.gputrace` here
answers "is each GPU dispatch efficient?". They are complementary.
