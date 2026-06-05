# Cosim Backend Portability

**Status:** Active — design captured, not yet scheduled.
**Issue:** [#105](https://github.com/gpu-eda/Jacquard/issues/105).
**Architecture:** [ADR 0017 — Cosim execution model](../adr/0017-cosim-execution-model.md)
(see the *Amendment 2026-06-05: backend portability* section).

cosim is Metal-only today: `run_cosim` lives in `src/sim/cosim_metal.rs`
(gated `#[cfg(feature = "metal")]`) and `cmd_cosim` hard-errors on other
backends. This plan factors the driver into a backend-agnostic orchestration
layer plus a `CosimBackend` trait, then adds a CPU implementation (Path A)
and a CUDA/HIP implementation (Path B).

## Goal & non-goals

**Goal:** `jacquard cosim` runs on CPU (reference, no GPU) and on CUDA/HIP,
not just Metal — reusing the existing scheduler, peripheral models, and VCD
machinery unchanged.

**Non-goals:**
- Changing the batch/scheduler execution model of ADR 0017 (untouched).
- Matching Metal cosim throughput on the CPU path (it's a reference/oracle).
- Porting the on-GPU IO-model kernels to CUDA/HIP up front — peripherals run
  on the CPU first; GPU IO models are a later optimization (Phase 3).

## What's already portable (no work needed)

- **Peripheral protocol models** — `src/sim/models/*.rs` (gpio, uart, i2c,
  spi, jtag, bus_trace) are pure CPU Rust implementing `PeripheralModel`
  (`models/mod.rs:56`). Reusable as-is across all backends.
- **CPU design step** — `cpu_reference::simulate_block_v1` is the exact CPU
  equivalent of one `simulate_v1_stage` threadgroup. The existing
  `run_cosim` `--check-with-cpu` path (`cosim_metal.rs:4282–4504`) already
  runs `state_prep` + `apply_flash_din` + `simulate_block_v1` per block on
  the CPU — a working prototype of the Path A backend step.
- **The scheduler / batching / VCD / event drain** logic is GPU-agnostic in
  intent; it operates on `&[u32]` state and `Vec<BitOp>` ops.

## The seam

```rust
/// Per-edge design execution + state ownership. One impl per backend.
/// The orchestration layer (scheduler, models, VCD, drains) calls this.
trait CosimBackend {
    /// output→input copy + apply scheduled BitOps + clear driven X-mask.
    fn state_prep(&mut self, ops: &[BitOp], xmask_state_offset: u32);
    /// Run the design for one edge (all major stages).
    fn simulate_edge(&mut self);
    /// Read-only view of the design output slot (VCD, model step_edge, drains).
    fn output_state(&self) -> &[u32];
    /// Mutable input slot (reset/constant init, flash MISO injection).
    fn input_state_mut(&mut self) -> &mut [u32];
    // Phase 3 (optional, perf): on-GPU peripheral hooks.
    //   fn apply_flash_din(&mut self, ...); fn flash_model_step(&mut self, ...);
    //   fn io_step(&mut self) -> IoEvents;
}
```

`MetalSimulator` becomes the `MetalBackend` impl; `CpuBackend` and
`Cuda`/`HipBackend` are added. The orchestration keeps peripherals on the
CPU (calling `PeripheralModel::step_edge` → `ModelOverrides` → BitOps) and
only delegates the design step + state ownership to the backend.

### Why this is tractable

cosim is **per-edge dispatch on every backend** (reactive inputs), so the
CUDA/HIP path dispatches the design kernel one edge at a time exactly as
Metal does — it does **not** need the `sim` command's cooperative
single-launch + `grid.sync`, the one CUDA feature hardest to port. See ADR
0017 amendment, fact (1).

## Phases

### Phase 0 — Extract the seam (Metal-only refactor, zero behaviour change)

Refactor `cosim_metal.rs` into `cosim/mod.rs` (orchestration) + a
`CosimBackend` trait + `cosim/metal.rs` (`MetalBackend`). No new backend
yet — this is a pure restructuring that must leave Metal cosim
bit-identical.

- Make `ScheduleBuffers.edge_buffers` backend-neutral: store
  `Vec<(StatePrepParams, Vec<BitOp>)>`; the backend materialises native
  buffers (Metal buffers today). Currently it holds `metal::Buffer` pairs
  (`cosim_metal.rs:3072`).
- `update_model_driven_in_ops` / `update_reset_in_ops` write into
  `Vec<BitOp>`, not Metal shared memory (`cosim_metal.rs:3643–3678`).
- Consolidate the private `simulate_block_v1` copies in `cosim_metal.rs`
  (`:2130–2413`) onto `cpu_reference`.
- Move the ~20 ad-hoc `simulator.device.new_buffer(...)` allocations in
  `run_cosim` into `MetalBackend`.

**Entry:** #113 (sim cross-backend equivalence) merged.
**Exit:** Metal cosim CI (`dual_uart`, `apb_trace`, JTAG-minimal,
`xprop_cosim`) all green, byte-identical output VCDs vs pre-refactor.

### Phase 1 — Path A: `CpuBackend` + Linux cosim CI

- Implement `CpuBackend`: `state_prep` (output→input copy + BitOps +
  X-mask clear — the loop at `cosim_metal.rs:4286–4301`), `simulate_edge`
  via `cpu_reference::simulate_block_v1` per block, plain `Vec<u32>` state.
- Peripherals stay on CPU (already are). The GPU flash kernel's FSM is a
  simple SPI state machine; reimplement it as CPU Rust (or reuse the
  existing `CppSpiFlash` FFI at `cosim_metal.rs:2493`) so flash cosim works
  without Metal.
- Wire `cmd_cosim` to select `CpuBackend` when `--features metal` is absent
  (or via an explicit `--backend cpu`), removing the hard-error.
- **Move the cosim regression tests to a Linux CI job** — they're
  Metal-only today; CPU cosim lets `xprop_cosim` (cosim mode), `dual_uart`,
  and `apb_trace` run on free `ubuntu-latest`.

**Entry:** Phase 0 merged.
**Exit:** `cargo run --bin jacquard -- cosim …` (no GPU features) runs the
existing cosim fixtures on Linux CI and passes their checkers.

### Phase 2 — Path B: `Cuda`/`HipBackend` (design on GPU, peripherals on CPU)

- Implement the backend over the existing CUDA/HIP `simulate` kernel,
  dispatched **per-edge** (not the cooperative single launch). State lives
  in device memory exposed to the orchestration via host copy / pinned /
  unified memory for the per-edge `output_state()` read.
- Peripherals remain on the CPU (per-edge CPU↔GPU sync). Accept the
  round-trip cost for the first cut; it's still the real Linux GPU path.
- Gate the GPU-backed cosim CI on `tesla4-runner` (now available).

**Entry:** Phase 1 merged (seam + CPU reference proven).
**Exit:** `cosim --features cuda` runs a fixture on the T4 and its output
VCD matches the CPU/Metal backends (cross-backend equivalence, below).

### Phase 3 — (Deferred, perf) port on-GPU IO models to CUDA/HIP

Port `gpu_apply_flash_din` / `gpu_flash_model_step` / `gpu_io_step` to
CUDA/HIP to eliminate the per-edge CPU↔GPU round-trip from Phase 2. Pure
throughput optimization; correctness already established by Phase 2.

## Testing strategy

- **Reuse existing fixtures**: `tests/xprop_cosim/` (cosim mode),
  `tests/dual_uart/`, `tests/apb_trace/`, `tests/jtag_minimal/`.
- **Cross-backend equivalence**: extend the #113 harness
  (`scripts/ci/compare_backend_vcds.py`) to a reactive design — run the same
  cosim on CPU and Metal (and CUDA in Phase 2) and assert byte-identical
  output VCDs. This is the correctness backstop for the whole effort.
- **Linux CI**: Phase 1 is the unlock — cosim regression coverage on free
  `ubuntu-latest` instead of the single self-hosted Metal runner.

## Risks

- **Refactor drift (Phase 0):** the bit-identical-Metal exit criterion is
  load-bearing; the equivalence test + existing checkers guard it.
- **Flash FSM reimplementation (Phase 1):** the GPU flash kernel and the
  `CppSpiFlash` FFI must agree. Prefer reusing `CppSpiFlash` to avoid a
  third copy of the SPI FSM.
- **Per-edge device-memory read (Phase 2):** reading `output_state` every
  edge across the PCIe boundary is slow; that's the motivation for Phase 3,
  not a correctness risk.

## Sequencing relative to other backend-alignment work

- Independent of [#104](https://github.com/gpu-eda/Jacquard/issues/104)
  (CUDA/HIP `sim` timing) — both bring CUDA/HIP toward Metal parity and are
  now T4-testable, but touch different code paths.
- Complements the `sim` cross-backend equivalence test (#113) and the
  proposed single-source `simulate_block_v1` macro prelude — those harden
  the `sim` compute kernel; this plan adds the cosim *driver*.
