# Cosim Backend Portability

**Status:** Active — design captured, not yet scheduled.
**Issue:** [#105](https://github.com/gpu-eda/Jacquard/issues/105).
**Architecture:** [ADR 0017 — Cosim execution model](../adr/0017-cosim-execution-model.md)
(see *Amendment 2026-06-07: backend-portable cosim — target architecture*).

cosim is Metal-only today: `run_cosim` lives in `src/sim/cosim_metal.rs`
(gated `#[cfg(feature = "metal")]`) and `cmd_cosim` hard-errors on other
backends. This plan is the **staging** to reach the target architecture in
ADR 0017: a backend-agnostic orchestration layer, a batch-granular
`CosimBackend` trait, and a 3-tier `GpuPeripheral` model — then CPU and
CUDA/HIP backends.

## Goal & non-goals

**Goal:** `jacquard cosim` runs on CPU (reference, no GPU) and on CUDA/HIP
**with batching**, not just Metal — reusing the existing scheduler,
peripheral models, and VCD machinery.

**Non-goals:**
- Changing the batch/scheduler execution model of ADR 0017 (untouched; the
  trait is made batch-granular to *preserve* it, not change it).
- Matching Metal cosim throughput on the **CPU** path (it's a
  reference/oracle).
- The user-extensible single-source peripheral API (Tier 3) up front — core
  peripherals get hand-written GPU kernels first (Phase 2); the portable
  authoring path is Phase 3.

## What's already portable (no work needed)

- **Peripheral protocol models (Tier 1)** — `src/sim/models/*.rs` (gpio,
  uart, i2c, spi, jtag, bus_trace) are pure CPU Rust implementing
  `PeripheralModel` (`models/mod.rs:56`). The semantic ground truth and the
  cross-backend equivalence oracle; reusable as-is across all backends.
- **CPU design step** — `cpu_reference::simulate_block_v1` is the exact CPU
  equivalent of one `simulate_v1_stage` threadgroup. The existing
  `run_cosim` `--check-with-cpu` path (`cosim_metal.rs:4282–4504`) already
  runs `state_prep` + `apply_flash_din` + `simulate_block_v1` per block on
  the CPU — a working prototype of the Path A backend step.
- **The scheduler / batch-size policy / VCD / event drain** logic is
  GPU-agnostic in intent; it operates on `&[u32]` state and `Vec<BitOp>` ops.

## The seam (batch-granular)

```rust
/// Design execution + state ownership. One impl per backend. The
/// orchestration layer (scheduler, models, VCD, drains) calls this.
///
/// Batch-granular by construction: measurements (ADR 0017) show Metal runs
/// 100% batched on GPU-peripheral designs, so a single-edge method would
/// regress it ~1000×. The orchestration decides N (`force_single_edge`);
/// the backend runs N edges however it likes.
trait CosimBackend {
    /// Build native schedule storage ONCE from the backend-agnostic
    /// description; the backend retains+owns it (opaque to orchestration).
    fn init_schedule(&mut self, edges: Vec<(StatePrepParams, Vec<BitOp>)>);
    /// Mutable view of one edge's ops (reset/model/clock-edge patching).
    /// Metal: slice over the shared MTLBuffer (zero-copy; write IS upload).
    /// CUDA/HIP: slice over host mirror, marks edge dirty (uploaded lazily).
    fn edge_ops_mut(&mut self, edge_idx: usize) -> &mut [BitOp];
    /// output→input copy + apply that edge's BitOps + clear driven X-mask.
    fn state_prep(&mut self, edge_idx: usize, xmask_state_offset: u32);
    /// Run N consecutive scheduler edges from `start_edge`, snapshotting each
    /// output slot into the ring. Flushes dirty edges first (no-op on Metal).
    /// Metal: one command buffer w/ GPU peripherals inside. CPU/CUDA-fallback:
    /// per-edge loop with CPU peripherals.
    fn run_edges(&mut self, start_edge: usize, n: usize, ring: &mut Ring);
    /// Read-only view of the current output slot (VCD, model step_edge, drains).
    fn output_state(&self) -> &[u32];
    /// Mutable input slot (reset/constant init, flash MISO injection).
    fn input_state_mut(&mut self) -> &mut [u32];
}

/// GPU-side peripheral kernel (Tier 2). Runs inside the backend's batch so
/// reactive designs can batch on a discrete GPU. CPU `PeripheralModel`
/// (Tier 1) is the reference + fallback when no GpuPeripheral exists.
trait GpuPeripheral {
    fn encode_step(&self, /* backend-specific encoder */);
}
```

`MetalSimulator` becomes the `MetalBackend` impl; `CpuBackend` and
`Cuda`/`HipBackend` are added. **The backend owns the schedule storage** —
the orchestration hands it the description once via `init_schedule` and keeps
only scalars (`edges_per_period`, `gcd_ps`); it does *not* hold a parallel
`Vec` the backend re-materialises (that would add a per-dispatch copy and
regress Metal's zero-copy path). Mutation goes through `edge_ops_mut`
(zero-copy on Metal; host-mirror + dirty-flag + lazy upload on CUDA/HIP). The
orchestration owns the batch-size policy and the CPU peripheral models; it
delegates the design step, state, and (Phase 2+) GPU peripheral steps to the
backend.

### Why batching must be a backend concern

cosim is per-edge *dispatch* in the reactive sense (inputs depend on
outputs), which is why CUDA/HIP avoid the `sim` command's cooperative
single-launch + `grid.sync` — the hardest CUDA feature to port. But
*reactive* and *per-command-buffer* are different axes: Metal batches up to
`BATCH_SIZE` reactive edges into one command buffer because its peripherals
run on the GPU inside the batch. On a discrete GPU, going per-command-buffer
means a PCIe round-trip every edge, which is why batching (hence GPU
peripherals) is *required* for CUDA/HIP perf, not optional — so the CUDA/HIP
backend ships *with* its GPU peripherals (Phase 2), not as a later add-on.
See ADR 0017, *Layer 3* and *Consequences*.

## Phases

### Phase 0 — Extract the seam (Metal-only refactor, zero behaviour change)

Introduce the batch-granular `CosimBackend` trait and make the schedule
backend-neutral; `MetalSimulator` becomes `MetalBackend`. The physical
module split (`cosim/mod.rs` orchestration + `cosim/metal.rs`) is separable
— the trait can land in-place in `cosim_metal.rs` first, split later. Must
leave Metal cosim bit-identical.

- Move the schedule storage *into* `MetalBackend` (built once via
  `init_schedule`); the orchestration keeps only `edges_per_period` + `gcd_ps`.
  Currently `ScheduleBuffers` holds `metal::Buffer` pairs at the call site.
- Convert the ops-update helpers (`update_model_driven_in_ops`,
  `update_reset_in_ops`, `patch_model_clock_edges`) from in-place `*mut BitOp`
  writes over `contents()` to `backend.edge_ops_mut(edge_idx)`. On Metal this
  returns a slice over the same shared buffer (zero-copy, identical
  behaviour). **This conversion is the main refactor friction** (and resolves
  the closure-borrow issue) — but it stays zero-copy on Metal.
- ~~Consolidate the private `simulate_block_v1` copies onto
  `cpu_reference`.~~ **Done — #115.**
- Move the ~20 ad-hoc `simulator.device.new_buffer(...)` allocations in
  `run_cosim` into `MetalBackend`.

**Entry:** #113 (sim cross-backend equivalence) merged. ✅
**Exit:** Metal cosim CI (`dual_uart`, `apb_trace`, JTAG-minimal,
`xprop_cosim`) all green, byte-identical output VCDs vs pre-refactor
(harness: re-run the fixtures, `cmp` against a pre-refactor golden).

### Phase 1 — Path A: `CpuBackend` + Linux cosim CI

- Implement `CpuBackend`: `state_prep` (output→input copy + BitOps +
  X-mask clear — the loop at `cosim_metal.rs:4286–4301`), `run_edges` via
  `cpu_reference::simulate_block_v1` per block (N effectively 1), plain
  `Vec<u32>` state.
- Peripherals stay on CPU (Tier 1, already are). The GPU flash kernel's FSM
  is a simple SPI state machine; reuse the existing `CppSpiFlash` FFI
  (`cosim_metal.rs:2493`) rather than writing a third copy.
- Wire `cmd_cosim` to select `CpuBackend` when `--features metal` is absent
  (or via an explicit `--backend cpu`), removing the hard-error.
- **Move the cosim regression tests to a Linux CI job** — Metal-only today;
  CPU cosim lets `xprop_cosim` (cosim mode), `dual_uart`, and `apb_trace`
  run on free `ubuntu-latest`.

**Entry:** Phase 0 merged.
**Exit:** `cargo run --bin jacquard -- cosim …` (no GPU features) runs the
existing cosim fixtures on Linux CI and passes their checkers.

### Phase 2 — Path B: `Cuda`/`HipBackend` with GPU peripherals (the perf path)

Lands the CUDA/HIP backend **and** its Tier-2 GPU peripherals together —
*not* a per-edge-only intermediate first. Per-edge-only CUDA would be an
unusably-slow artifact (PCIe round-trip per edge, potentially slower than
`CpuBackend`), and Phase 1 already proves the per-edge orchestration path, so
a separate per-edge milestone would re-validate known-good code and then
rework the same `run_edges`/dispatch path to add batching. One phase, with an
internal checkpoint:

- **Checkpoint 2a — design step on GPU (per-edge correctness).** Implement the
  backend over the existing CUDA/HIP `simulate` kernel + `init_schedule` /
  `edge_ops_mut` (device buffers, dirty-edge upload) + device state with
  per-edge read-back. Peripherals on the CPU (Tier 1), reusing Phase 1's
  per-edge orchestration. Validate via cross-backend equivalence on the T4.
  *This is the permanent fallback path*, not a throwaway — CPU-side models
  (e.g. JTAG replay) always run here.
- **Checkpoint 2b — Tier-2 GPU peripherals → batched.** Port the GPU
  peripheral kernels (`gpu_apply_flash_din`, `gpu_flash_model_step`,
  `gpu_io_step`) to CUDA/HIP so peripherals run *inside* the batch. Because
  CUDA and HIP share `kernel_v1_impl.cuh`, each is **two impls** (shared
  `*_impl.cuh` + the existing `.metal`). Define the `GpuPeripheral` seam here;
  equivalence-test each kernel against its Tier-1 CPU model.

Gate GPU-backed cosim CI on `tesla4-runner` (now available).

**Entry:** Phase 1 merged (seam + CPU reference + per-edge orchestration proven).
**Exit:** `cosim --features cuda` on a GPU-peripheral fixture runs **batched**
(telemetry shows `batch > 1`), output VCD matching all backends; the per-edge
fallback (CPU-side-model fixture, e.g. JTAG) also matches.

### Phase 3 — (Future) single-source peripherals (Tier 3, user-extensible API)

A user authors a peripheral *once* (restricted-Rust subset or a small
peripheral-FSM IR) that compiles to the CPU model + each GPU backend's
kernel — the user-extensible peripheral API. Slots into the `GpuPeripheral`
seam defined in Phase 2 without reworking orchestration. Big effort; demand-
driven (first external/user-defined peripheral type).

## Testing strategy

- **Reuse existing fixtures**: `tests/xprop_cosim/` (cosim mode),
  `tests/dual_uart/`, `tests/apb_trace/`, `tests/jtag_minimal/`.
- **Cross-backend equivalence** (the correctness backstop): extend the #113
  harness (`scripts/ci/compare_backend_vcds.py`) to reactive designs — run
  the same cosim on CPU / Metal / CUDA and assert byte-identical output VCDs;
  the CPU `PeripheralModel` is ground truth and each Tier-2 GPU kernel must
  match it.
- **Bit-identical Phase 0 gate**: capture a pre-refactor golden of all
  fixtures, re-run + `cmp` after each refactor step.
- **Linux CI**: Phase 1 is the unlock — cosim regression coverage on free
  `ubuntu-latest` instead of the single self-hosted Metal runner.

## Risks

- **Refactor drift (Phase 0):** the bit-identical-Metal exit criterion is
  load-bearing; the equivalence test + golden `cmp` + existing checkers
  guard it.
- **Shared-memory ops mutation (Phase 0):** the in-place `*mut BitOp` writes
  are the trickiest part to de-Metal; the trait's explicit upload point is
  the fix.
- **Flash FSM (Phase 1):** the GPU flash kernel and the `CppSpiFlash` FFI
  must agree. Prefer reusing `CppSpiFlash` to avoid a third copy.
- **Per-edge device read (Phase 2 fallback path):** the CPU-peripheral
  fallback reads device state every edge — slow by design, used only for
  CPU-side models; checkpoint 2b (GPU peripherals) is what keeps the common
  case batched. Not a correctness risk.
- **Tier-2 kernel divergence (Phase 2 checkpoint 2b):** two kernel families
  (CUDA+HIP shared `*_impl.cuh`, Metal `.metal`) per peripheral; equivalence
  tests against the CPU model are the guard, and Tier 3 eventually removes the
  hand-maintenance.
- **Phase 2 size:** merging the backend bring-up with the GPU-peripheral
  ports makes Phase 2 large; the internal 2a/2b checkpoint keeps it
  incrementally verifiable (per-edge equivalence before adding batching).

## Sequencing relative to other backend-alignment work

- Independent of [#104](https://github.com/gpu-eda/Jacquard/issues/104)
  (CUDA/HIP `sim` timing) — both bring CUDA/HIP toward Metal parity and are
  now T4-testable, but touch different code paths.
- Complements the `sim` cross-backend equivalence test (#113) and the
  proposed single-source `simulate_block_v1` macro prelude — those harden
  the `sim` compute kernel; this plan adds the cosim *driver*.
- **CDC/island batching** (multi-clock plan MC.1→MC.4) is the long-term fix
  for the per-edge tail of CPU-side-model designs (e.g. JTAG) — orthogonal
  to and larger than this seam; not a prerequisite.
