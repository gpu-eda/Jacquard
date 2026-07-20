# Cosim runtime

*Reference — the current design as it stands in the code. The why lives in the
decision records linked from each section; the not-yet-built work is under
[Implementation status](#implementation-status). Present tense means "true
today"; if a claim here is stale, the code moved and this doc didn't.*

Cosim runs a GPU-simulated design alongside reactive peripheral models that
drive and observe the design's pins every clock edge. Unlike `sim`, which
replays a fixed input VCD, cosim lets an input depend on a design output
cycle by cycle: a SPI flash serves firmware in response to the addresses the
design issues, a UART decodes the bytes it transmits, a JTAG client single-steps
it. Rationale for the execution model: [ADR 0017](../adr/0017-cosim-execution-model.md).

```d2
# Colours and font are not set here on purpose: theme/d2-diagrams.css recolours
# every shape from the active mdBook theme's CSS variables, so the diagram tracks
# light / navy / coal / ayu instead of baking one palette. pad trims D2's default
# 100px canvas border.
vars: { d2-config: { pad: 16 } }
direction: down

# The three top-level regions carry a sentinel fill (#4f8bff / #3cbe78 / #aa78f0)
# purely as a CSS hook: theme/d2-diagrams.css remaps each to a translucent,
# theme-adaptive tint. See that file's "Region accents" section.
cpu: CPU (between batches) {
  style.fill: "#4f8bff"
  orch: Orchestration driver {
    sched: MultiClockScheduler
    policy: "Batch policy\n(force_single_edge)"
  }
  models: "Peripheral models (Tier 1)\nobserve → advance FSM → drive"
  drain: Ring drain
}

gpu: "GPU — one batch, per edge" {
  style.fill: "#3cbe78"
  prep: state_prep
  inject: "inject\n(flash MISO)"
  sim: simulate xN
  sample: "sample + advance\n(bidirectional)"
  observe: "observe\n(UART, bus trace)"
  prep -> inject -> sim -> sample -> observe
}

rings: "GPU→CPU ring buffers" {
  style.fill: "#aa78f0"
}

cpu.orch.policy -> gpu: dispatch batch
cpu.models -> gpu.prep: pin drives (BitOps)
gpu.observe -> rings: write
gpu.sample -> rings: write
rings -> cpu.drain: drain after batch
cpu.drain -> cpu.models: outputs
```

## The per-edge pipeline

The unit of time is a **scheduler edge**, not a clock cycle (see [Time: edges,
not cycles](#time-edges-not-cycles)). Each edge runs this sequence on the GPU:

```
state_prep            apply clock, GPIO, JTAG pin drives via BitOps
  → inject            bidirectional peripherals write design inputs (flash MISO)
    → simulate ×N     combinational logic evaluation, N pipeline stages
  → sample + advance  bidirectional peripherals read outputs, step their FSM
  → observe           output-only peripherals decode into ring buffers (UART, bus trace)
```

CPU-side peripheral models run *between* batches of edges, not within them, so
they see the design's output state only at batch boundaries.

## Batch dispatch

Consecutive scheduler edges are grouped into **batches** of up to
`BATCH_SIZE = 1024` and dispatched as one GPU command buffer. Between batches the
CPU steps its peripheral models, drains the ring buffers, and patches the next
batch's pin drives. A batch runs with no CPU interaction inside it, which is why
cosim is GPU-bound rather than round-trip-bound.

The batch collapses to a single edge whenever any peripheral model reports
`is_active()`. A JTAG replay mid-transmission, or a connected interactive debug
client, holds the design at one edge per dispatch for as long as it needs
per-edge coupling. This is the `force_single_edge` path; it is correct but slow,
and it is the exception, not the norm. The why, and the measured batch-utilisation
that justifies the fixed size, are in [ADR 0017](../adr/0017-cosim-execution-model.md).

## Backends

One orchestration driver runs above a `CosimBackend` trait; each backend owns the
design state and runs N consecutive edges per call.

- **CpuBackend** — the per-edge reference model. It is the cross-backend
  equivalence oracle and the reason cosim regressions run on free Linux CI.
  Throughput is not its job.
- **MetalBackend** — runs a batch in one command buffer with GPU peripherals
  inside it. Unified memory makes the pin drives zero-copy: the CPU's write to
  an edge's ops *is* the upload.
- **CudaBackend / HipBackend** — run the same simulate kernel over managed
  memory, sidestepping the cooperative-launch grid sync that only `sim` needs.
  They ship with their GPU peripherals so reactive designs batch from the start;
  ops upload only for edges the CPU actually changed.

Every backend produces a byte-identical output VCD on the same reactive design.
That equivalence is the correctness contract for the whole runtime, checked in
CI against CpuBackend goldens.

## Peripherals

A peripheral is one shape on either substrate: **observe** some design-output
bits, **advance an FSM** over persistent state, then **drive** some design-input
bits and/or **emit** decoded records. Where it runs follows a rule: a model that
drives inputs each edge (must react to output) can run on the CPU; a model that
only observes outputs, or exchanges data bidirectionally, runs on the GPU for
zero-copy access to the state buffer. Rationale and the full contract:
[ADR 0013](../adr/0013-plural-peripheral-configs.md).

GPU peripherals come in two patterns:

- **Observe-only** — one post-simulate kernel per edge, reading output state into
  a ring buffer. UART TX decode and bus trace.
- **Bidirectional** — a pre-simulate kernel injecting into input state plus a
  post-simulate kernel sampling and advancing the FSM. SPI flash and QSPI PSRAM.

Three tiers back each peripheral. Tier 1 is the CPU `PeripheralModel` (always
present, the semantic ground truth). Tier 2 is a hand-written GPU kernel for the
core set — flash, UART, bus trace — one shared `*_impl.cuh` for CUDA and HIP plus
one `.metal`. Tier 3, single-source peripheral compilation for user-defined
peripherals, is [not yet built](#implementation-status).

Config is plural where instances can repeat, following one back-compat pattern
(a legacy singular key folds into instance 0 of a new `Vec`):

| Peripheral | Plural | State |
|---|---|---|
| Clocks, GPIO, UART | yes | built |
| QSPI memory (flash + writable PSRAM) | yes | built; independent backing stores, CS-gated shared bus |
| Bus trace | yes | built for APB3; AHB-Lite/AHB5 not yet |
| JTAG | no (TAP daisy-chain suffices) | replay + interactive server |

## GPU→CPU ring buffers

GPU peripherals write into fixed-size ring buffers in device memory; the CPU
drains each after a batch completes, from a local `read_head` up to the
GPU-written `write_head`. No synchronisation beyond command-buffer completion is
needed. This is the only channel out of the batch today, and it is the GPU→CPU
half of the [flow-controlled I/O work](#implementation-status). When VCD output
is on, a separate per-edge snapshot ring lets a batch stay larger than one edge
without a CPU read after every edge.

## Time: edges, not cycles

The `MultiClockScheduler` computes a deterministic interleaving of edges across
clock domains. The scheduler tick is the GCD of all half-periods and phase
offsets; the schedule repeats at the LCM of all full periods. A **scheduler
edge** is one tick; a **clock cycle** is two half-periods of one domain. UART
baud dividers, reset duration, and the `--max-clock-edges` flag all count edges,
not cycles — conflating the two is a real bug class, not a pedantic distinction.
The schedule length is capped at one million ticks, which rejects
non-commensurable clock ratios rather than building an unbounded schedule.

## Constraints

- Synchronous logic only, inherited from the simulation engine.
- A CPU-side model cannot observe intra-batch state; anything needing per-edge
  visibility must set `is_active()` and pay the single-edge dispatch.
- The scheduler rejects clock sets that don't repeat within one million ticks.
- Timed (arrival-annotated) cosim runs on Metal only; the structured
  `--timing-report` JSON is `sim`-only.

## Implementation status

Built and in use: batch dispatch, the multi-clock scheduler, all four backends
with cross-backend equivalence, the CPU and GPU peripheral tiers (1 and 2), plural
configs, and the interactive JTAG server. These are the sections above without a
"not yet."

Decided but not yet built:

- **Flow-controlled external I/O across the batch boundary** — bounded pipes, an
  adaptive commit window replacing the fixed `BATCH_SIZE`, per-tap projection, a
  socket reactor, and a CPU→GPU input pipe. Proposed in
  [ADR 0022](../adr/0022-flow-controlled-io.md); nothing built.
- **Tier 3 single-source peripherals** — one peripheral definition compiled to
  CPU and every GPU backend. [ADR 0017](../adr/0017-cosim-execution-model.md).
- **Bus trace beyond APB3** — AHB-Lite/AHB5 decode and annotated-VCD output;
  migrating the hardcoded Wishbone trace onto the config-driven monitor.
  [ADR 0013](../adr/0013-plural-peripheral-configs.md).
- **Timed cosim on CPU/CUDA/HIP**, and the cosim structured timing report.
  [ADR 0017](../adr/0017-cosim-execution-model.md).
- **Min-heap scheduler** to remove the one-million-tick cap.
  [Multi-clock and stimulus architecture](../plans/multi-clock-and-stimulus-architecture.md).

Scheduling for the larger items lives in
[cosim backend portability](../plans/cosim-backend-portability.md) and the
[multi-clock plan](../plans/multi-clock-and-stimulus-architecture.md).

## Decisions behind this

- [ADR 0013](../adr/0013-plural-peripheral-configs.md) — peripheral model
  architecture: the CPU/GPU split, the two GPU patterns, ring buffers, plural
  configs.
- [ADR 0017](../adr/0017-cosim-execution-model.md) — execution model: batch
  dispatch, the multi-clock scheduler, edges versus cycles, the backend seam.
- [ADR 0012](../adr/0012-cdc-jitter-injection.md) — CDC jitter injection, which
  uses the scheduler's edge timestamps as its injection point.
- [ADR 0022](../adr/0022-flow-controlled-io.md) — flow-controlled I/O (proposed;
  the future of the batch boundary).
