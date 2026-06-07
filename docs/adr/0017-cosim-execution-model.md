# ADR 0017 — Cosim execution model

**Status:** Accepted

## Context

The cosim mode runs a GPU-simulated design alongside reactive
peripheral models (flash, UART, JTAG, GPIO) that drive and observe
design pins each clock edge. The execution model must balance two
competing needs: GPU throughput (which favours large batches of
edges dispatched as a single command buffer) and peripheral
responsiveness (which requires CPU-side model updates between edges).

This ADR documents the batch dispatch loop, the multi-clock
scheduler, and the time-domain abstractions that tie them together.

## Decision

### Batch dispatch loop

The cosim main loop groups consecutive scheduler edges into
**batches** of up to `BATCH_SIZE = 1024` edges. Each batch is
encoded into a single Metal command buffer and dispatched to the
GPU. Between batches, CPU-side peripheral models (`PeripheralModel::
step_edge`) run, ring buffers are drained, and model overrides are
compiled into `BitOp` arrays for the next batch.

Per-edge execution within a batch:

```
state_prep (apply clk/gpio/jtag pin drives via BitOps)
  → gpu_apply_flash_din (inject flash MISO into input state)
    → simulate_v1_stage ×N (combinational logic evaluation)
  → gpu_flash_model_step (read MOSI, advance flash FSM)
  → gpu_io_step (UART TX decode + Wishbone bus trace)
```

CPU-side models cannot observe intra-batch state changes — they see
the output state only after the batch completes. For peripherals
that require per-edge responsiveness (e.g. JTAG replay with tight
hold-cycle requirements), the batch is forced to size 1 when any
model reports `is_active() == true`.

### Why BATCH_SIZE = 1024

The batch size trades off GPU utilisation against peripheral
latency. Smaller batches → more Metal command buffer submissions
per second → higher overhead. Larger batches → staler CPU-side
model state. 1024 was chosen empirically as a sweet spot:

- For peripheral-free simulation: amortises ~1ms of command buffer
  overhead across 1024 edges ≈ 1µs/edge overhead.
- For active peripherals (JTAG, stimulus-driven): the `is_active`
  fallback to batch=1 ensures correctness regardless of batch size.
- The batch size only affects cosim; the `sim` command processes the
  entire VCD in one GPU dispatch.

### Pre-allocated schedule buffers

Each scheduler edge has pre-allocated Metal buffers for its
`StatePrepParams` and `BitOp` array (`ScheduleBuffers::edge_buffers`).
These are allocated once at startup — not per-dispatch — to avoid
allocation latency in the hot loop. The schedule repeats with period
`edges_per_period` (= LCM schedule length); edge `N` reuses
buffer `N % edges_per_period`.

### Multi-clock scheduler

The `MultiClockScheduler` computes a deterministic interleaving of
edges across clock domains. Given N clocks with potentially
different periods and phase offsets:

1. Compute `gcd_ps` = GCD of all half-periods and phase offsets.
   This is the **scheduler tick** — the minimum time quantum.
2. Compute `lcm_ps` = LCM of all full periods. This is the
   **schedule period** — the point at which the edge pattern repeats.
3. `schedule_len = lcm_ps / gcd_ps` — number of ticks per period.
4. For each tick, compute which domains have rising/falling edges
   based on `(tick_ps - phase_offset) % half_period == 0`.

The schedule length is capped at 1,000,000 ticks. This prevents
degenerate clock ratios (e.g. primes) from producing unbounded
schedules. If the cap is hit, the assertion fires with a message
suggesting the clocks may not be commensurable at the configured
resolution.

### Time units: edges vs clock cycles

A **scheduler edge** is one tick of the scheduler (duration =
`gcd_ps`). A **clock cycle** is two half-periods of a given domain
(= rising + falling edge). The ratio `sched_ticks_per_sys_clk_cycle =
clock_period_ps / gcd_ps` converts between them. Note this ratio is the
number of scheduler ticks per sys_clk *period*, which is 2 only when
`gcd_ps` equals the half-period (single-clock or harmonic multi-clock);
non-commensurate periods or phase offsets make it larger.

This distinction is load-bearing for peripheral timing:
- UART baud rate dividers count **edges**, not clock cycles.
- Reset duration counts **edges**.
- The `--max-clock-edges` CLI flag counts **edges**.

Confusing edges with clock cycles was the root cause of the UART
baud rate bug fixed in commit `a263e47` — `edges_per_period` (the
LCM schedule length) was used where `sched_ticks_per_sys_clk_cycle` was
needed, doubling the bit time in multi-clock designs.

### GPU→CPU ring buffer drain

After each batch completes, the CPU drains three categories of
GPU-side state:

1. **Peripheral ring buffers** — UART channels and Wishbone trace
   channel, drained from local `read_head` to GPU-written
   `write_head`. See ADR 0013 for struct conventions.
2. **VCD snapshot buffer** — when `--stimulus-vcd` or `--output-vcd` is
   enabled, a separate ring buffer (`2 × state_size` words per edge)
   captures per-tick output state on the GPU. The CPU drains it
   after each batch to write VCD transitions. This mechanism is what
   enables `BATCH_SIZE > 1` even with VCD output — without it, the
   CPU would need to read output state after every single edge.
3. **CPU reference check** — when `--check-with-cpu` is active, the
   CPU replays the batch with the reference kernel and compares.

No synchronisation beyond Metal's command buffer completion is
needed — all drains happen after `waitUntilCompleted`.

## Consequences

- The batch dispatch model means CPU-side peripheral models see
  output state with up to `BATCH_SIZE` edges of latency. This is
  acceptable for all current peripherals; models that need tighter
  coupling set `is_active() = true`.
- The 1M tick schedule cap prevents pathological memory use but
  rejects exotic clock ratios. A min-heap scheduler (proposed in
  `docs/plans/multi-clock-and-stimulus-architecture.md` as MC.2)
  would remove this limit.
- The edges-vs-cycles distinction must be maintained carefully in
  any code that converts user-facing "cycles" to internal "ticks".
  The `sched_ticks_per_sys_clk_cycle` helper exists for this purpose.
- Pre-allocated schedule buffers consume `O(schedule_len)` Metal
  buffer pairs at startup. Each schedule entry creates two Metal
  buffer objects (params + ops). For typical single-clock designs
  this is 2 entries = 4 buffer objects; for complex multi-clock
  designs it can reach thousands of entries, but each buffer is
  small (tens of bytes).

## Amendment 2026-06-07: backend-portable cosim — target architecture (#105)

The execution model above is **Metal-only** — `run_cosim` lives in
`cosim_metal.rs` (gated `#[cfg(feature = "metal")]`) and `cmd_cosim`
hard-errors on other backends. This amendment records the **target
architecture** for making cosim backend-portable (CPU reference + CUDA/HIP),
tracked as [#105](https://github.com/gpu-eda/Jacquard/issues/105). It
supersedes the incremental 2026-06-05 note (whose "per-edge on every
backend" framing the measurements below correct). It describes the
steady-state design; the staging to reach it lives in
[`docs/plans/cosim-backend-portability.md`](../plans/cosim-backend-portability.md).
It does **not** change the batch/scheduler model above — it factors *where*
each part runs and along what seam.

### The evidence: measured batch utilisation (2026-06-07)

The cosim loop was instrumented (telemetry in the run summary:
`single_edge_batches`, mean/max batch) to measure how often the batched
fast path (`batch > 1`) runs versus forced single-edge dispatch
(`force_single_edge = any_model_active`, plus diagnostic modes). Per-edge
handover is the only mode needing a true per-edge CPU↔GPU round-trip.

| Fixture | Edges | Batched (edges) | Single-edge commits | Commits |
|---|---|---|---|---|
| `dual_uart` | 10,000 | 100% | 0 | 11 |
| `apb_trace` | 200 | 100% | 0 | 2 |
| `xprop_cosim` | 40 | 100% | 0 | 2 |
| `jtag_minimal` | 4,000,000 | 97.4% | 102,310 | 106,117 |

Designs whose peripherals have GPU-side halves (UART, APB bus-trace, SPI
flash — the `gpu_io_step` / `gpu_flash_model_step` kernels) run **100%
batched**: CPU↔GPU handover is at `BATCH_SIZE`-edge boundaries, not per
clock edge. Even `jtag_minimal` (CPU-side JTAG replay, the most
per-edge-heavy fixture) batches 97% of *edges* — but its 102k single-edge
commits are 96% of all submits and dominate its wall-clock. **Batching is
the dominant path; per-edge is the exception (CPU-side models + diagnostic
modes).** This drives every decision below.

### Layer 1 — backend-agnostic orchestration (the shared `cosim` driver)

Everything that is not GPU-specific moves above the seam and operates on
`&[u32]` state + `Vec<BitOp>` edge ops: the `MultiClockScheduler`,
`build_edge_ops`, the **batch-size policy** (`force_single_edge`), peripheral
*coordination*, the input dispatcher, reset/constant init, VCD writing, and
event/ring-buffer draining. The batch-size decision stays here, unchanged.

### Layer 2 — the `CosimBackend` trait (one impl per backend)

Owns the `[2 × state_size]` design state and runs the design. Crucially it
is **batch-granular, not single-edge** — the measurements show a literal
`simulate_edge`-per-edge trait would collapse Metal's 100%-batched designs
to one command-buffer submit per edge (~1000× regression). The trait method
is therefore *"run N consecutive scheduler edges, applying each edge's ops
and snapshotting each output slot to the ring"*, plus `state_prep`
(output→input copy + apply `BitOp`s + clear driven X-mask) and
`input_state()/output_state()` accessors. `MetalSimulator` becomes
`MetalBackend`; `CpuBackend` and `Cuda`/`HipBackend` are added.

**The backend owns the schedule storage (opaque to the orchestration).** The
edge ops are a tiny, fixed, repeating set (`edges_per_period` entries, =2 for
single-clock) built *once*; the orchestration must not hold a parallel copy
that the backend re-materialises each dispatch (that would add a per-dispatch
copy and, on Metal, *regress* today's zero-copy unified-memory path). Instead:

- `init_schedule(edges: Vec<(StatePrepParams, Vec<BitOp>)>)` hands the
  backend-agnostic description to the backend *once*; the backend materialises
  its native buffers and retains them. The orchestration keeps only scalars
  (`edges_per_period`, `gcd_ps`).
- `edge_ops_mut(edge_idx) -> &mut [BitOp]` is how reset / model-driven /
  clock-edge patching mutates ops. **Metal** returns a slice straight over the
  shared `MTLBuffer` — zero-copy, the write *is* the upload (exactly today's
  behaviour). **CUDA/HIP** return a slice over a host mirror and mark the edge
  dirty; `run_edges` uploads only dirty edges before launch. Ops change rarely
  (reset transitions; only while a CPU-side model is active), so this is near
  zero in practice — and the buffers are KB-scale regardless.

This replaces the earlier "neutral `Vec` + backend re-materialises" sketch,
which risked needless CPU↔GPU traffic and double bookkeeping.

- **`MetalBackend`** runs N edges in one command buffer with GPU peripherals
  inside (today's `encode_and_commit_gpu_batch`).
- **`CpuBackend`** runs the per-edge loop via `cpu_reference::simulate_block_v1`
  — the reference/oracle, and the unlock for **cosim regression on free
  Linux CI** (today Metal-only). N is effectively 1; throughput is not the
  point. It also validates the per-edge orchestration path that the CUDA/HIP
  fallback reuses.
- **`Cuda`/`HipBackend`** run the existing `simulate` kernel, sidestepping the
  `cooperative_groups` grid-sync that only the `sim` command needs (the
  hardest CUDA feature to port). They ship **with** their Tier-2 GPU
  peripherals (Layer 3) so reactive designs batch from the start; the per-edge
  path is the permanent fallback for CPU-side models (e.g. JTAG replay).

### Layer 3 — the `GpuPeripheral` abstraction (3-tier, GPU peripherals primary)

Batching a *reactive* design requires the peripheral to run **inside** the
batch — i.e. on the GPU — because the peripheral consumes each edge's output
to drive the next edge's input. On Metal this is hidden by unified memory;
on a discrete GPU, per-edge means a **PCIe round-trip every edge** (~1–2 µs
each way), which over millions of edges is likely *slower than the CPU
backend*. **Therefore GPU-side peripherals are architecturally required for
the CUDA/HIP perf story — not an optional optimization.** The decision
(2026-06-07) is to make GPU peripherals the **primary** path, with a 3-tier
model mirroring the `CosimBackend` seam:

- **Tier 1 — CPU reference model** (`PeripheralModel`, `src/sim/models/*.rs`,
  exists). The semantic ground truth, the cross-backend equivalence oracle,
  and the fallback for any (backend, peripheral) lacking a GPU kernel. Always
  present.
- **Tier 2 — hand-written GPU kernels for core peripherals** (now). Because
  CUDA and HIP already share `kernel_v1_impl.cuh`, a core peripheral is **two
  implementations, not three**: one shared `*_impl.cuh` (CUDA + HIP) and one
  `.metal`. Tractable for the small in-core set (flash, UART, bus-trace) and
  matching the existing `simulate`-kernel precedent.
- **Tier 3 — single-source peripheral compilation** (later; the
  user-extensible peripheral API). Hand-written kernels don't scale to
  user-defined peripherals; the endgame is a user writing a peripheral *once*
  (restricted-Rust subset or a small peripheral-FSM IR) that compiles to CPU
  + every GPU backend. This domain (peripheral FSMs) is far narrower than the
  general cross-shader-tool port previously rejected, so an in-house IR is
  the tractable route.

The `GpuPeripheral` seam is defined at Tier 2 so Tier 3 slots in without
reworking the orchestration.

#### The peripheral contract — one shape, input *and* output, every model

The seam above only sketched Tier 2 as `encode_step(encoder)`. This fills in
*what* a peripheral is, so the CPU model (Tier 1), the GPU kernel (Tier 2), and
the eventual single source (Tier 3) all express the **same** contract rather
than two parallel ones.

Every peripheral — on either substrate, input-driving or output-observing — is
the same shape:

> **observe** some design-output bits → **advance an FSM** (over persistent
> state + const params) → **drive** some design-input bits and/or **emit**
> decoded records.

The CPU `PeripheralModel` trait (`src/sim/models/mod.rs:56`) is *already* this
contract and *already* bidirectional:

```rust
fn step_edge(&mut self,
    output_state: &[u32],              // OBSERVE design outputs
    overrides: &mut ModelOverrides,    // DRIVE design inputs (position → value)
    emitted: &mut Vec<EmittedEvent>);  // EMIT decoded records
fn driven_positions(&self) -> &[u32]; // the input bits it may drive
fn is_active(&self) -> bool;           // forces batch=1 mid-transmission
```

One trait already covers the whole spectrum via *optional halves*: GPIO is
input-only (default `step_edge` just contributes overrides), UART-TX decode /
bus-trace are output-only (empty `driven_positions`), SPI flash is
bidirectional. So a single interface genuinely covers input and output — the
doubt about "enough commonality" is unfounded: the commonality is the
FSM-over-IO-bits shape, and six models already share it.

The GPU half is **not** yet unified — it is three bespoke kernels with the same
skeleton but no common trait:

| Kernel | reads | FSM state | writes | role |
|---|---|---|---|---|
| `gpu_apply_flash_din` | `states`, `FlashState` | — | `states` (MISO) | **input** inject |
| `gpu_flash_model_step` | `states`, `flash_data` | `FlashState` | `FlashState` | **output** observe + FSM |
| `gpu_io_step` | `states` | `UartDecoderState` | `UartChannel`/`BusTraceChannel` rings | **output** decode → ring |

Every kernel is `kernel(device u32* states, device FsmState* state, constant
Params& params, [device Ring* out], [const Data* in])`.

**Target shape — one logical contract, two substrates:**

- **CPU substrate** = today's `step_edge` (Rust over `&[u32]`; drives via
  `ModelOverrides`).
- **GPU substrate** (`GpuPeripheral`) = `encode_step(encoder, states_buf,
  fsm_buf, params_buf, ring_buf)` running the *same FSM* over `device u32*
  states`.
- **Consistency anchor = the shared `#[repr(C)]` FSM-state + params structs.**
  These already exist on both sides but are **hand-synced** ("`must match Metal
  UartChannel`", `cosim_metal.rs:178`); that duplication is the tax Tier 3
  removes by generating the Rust `step`, the GPU kernel, **and** the one struct
  from a single FSM definition.

**The decision that makes it consistent: all input drives are `(position,
value)` pairs applied through the one `state_prep`/ops path.** Today this is the
*inconsistent* part — CPU models drive indirectly (`overrides → BitOps →
state_prep`, so drives land clock-edge-aligned), but `gpu_apply_flash_din`
writes `states` directly. They are the same logical operation done two ways.
Normalising flash's direct-write into FSM-produced ops applied by `state_prep`
makes input application uniform across every peripheral and both substrates, and
removes flash's special case. (Flash writes directly today only because its MISO
depends on the FSM computed that same edge — expressible as ops the FSM emits.)

**What deliberately does *not* unify** (substrate detail below the contract):
output draining (GPU needs ring buffers because the CPU cannot observe
intra-batch state; CPU models emit immediately — same `events out` contract,
different plumbing), and the FSM body itself (Rust vs kernel, single-sourced
only by Tier 3's IR). These divergences are expected and bounded.

**Phase-1 implication:** the CPU UART-TX decoder added in Phase 1 (no GPU
equivalent exists today — `models/uart.rs` only has an RX-line *receiver*
decode) must be written to this contract, with its FSM state mirroring
`UartDecoderState`'s fields, so the Phase-2 `GpuPeripheral` kernel and the
Tier-3 single source fold into one definition rather than a third parallel one.

### Correctness contract

The CPU `PeripheralModel` (Tier 1) is ground truth. The cross-backend
equivalence harness ([#113](https://github.com/gpu-eda/Jacquard/pull/113),
today sim-only) extends to cosim: every backend's output VCD must be
byte-identical on the same reactive design, and every GPU-peripheral kernel
(Tier 2) must match its CPU model. This is the backstop for the whole effort.

### Considered alternative (not adopted as primary)

**Speculative batching** — keep peripherals on the CPU, batch optimistically,
and roll back on divergence (the multi-clock plan's MC.4 island run-ahead /
MC.5 record-and-replay). It avoids writing GPU kernels but is
non-deterministic in throughput and substantially more complex. Rejected as
the primary path; retained as the natural fallback for *user* CPU
peripherals that are not GPU-portable. Cross-shader tools (Slang, Ferrox)
remain rejected for the design kernel; Tier 3's narrow peripheral-FSM IR is
the cross-GPU answer for peripherals.

### Relationship to the multi-clock plan

The 102k JTAG single-edge round-trips are exactly the "cosim CPU↔GPU
round-trip measured as the bottleneck" trigger for **MC.3** (streaming
stimulus) and the motivation for **MC.4** (per-island multi-rate batching).
Both are *orthogonal to and larger than* this seam (MC.4 needs the MC.1
island partitioner) — the long-term fix for the per-edge tail, not a
prerequisite here.

### Consequences

- The `sim` cooperative-launch model and the cosim per-edge/batch model
  remain distinct *per backend*; this unifies the cosim *driver*, not the two
  execution models.
- `ScheduleBuffers` currently stores `metal::Buffer` pairs and the ops-update
  helpers write Metal shared memory in place. These move *into the backend*
  (built once via `init_schedule`, mutated via `edge_ops_mut`) rather than
  becoming an orchestration-owned `Vec` the backend re-materialises — which
  keeps Metal zero-copy and lets CUDA/HIP upload only dirty edges. Converting
  the in-place `*mut BitOp` shared-memory mutation to `edge_ops_mut` is the
  main refactor friction (and resolves the closure-borrow issue too).
- CUDA/HIP cosim ships **with Tier-2 GPU peripherals** so reactive designs
  batch from the start (Phase 2 lands the backend + GPU peripherals together,
  rather than a per-edge-only intermediate that would be unusably slow). The
  per-edge path remains as the fallback for CPU-side models (e.g. JTAG
  replay), where the per-edge tail is addressed later by the multi-clock
  plan's MC.3/MC.4, not by this seam.

## Cross-references

- ADR 0012 — CDC jitter injection (uses the scheduler's edge
  timestamps as the injection point).
- ADR 0013 — Peripheral model architecture (documents GPU-side
  model patterns and ring buffers).
- [`docs/plans/multi-clock-and-stimulus-architecture.md`](../plans/multi-clock-and-stimulus-architecture.md)
  — design-space doc for the multi-clock scheduler.
- [`docs/plans/cosim-backend-portability.md`](../plans/cosim-backend-portability.md)
  — implementation plan for backend portability (#105).
