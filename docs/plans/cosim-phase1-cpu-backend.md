# Cosim Phase 1 — `CpuBackend` + Linux cosim CI (implementation plan)

**Status:** Proposed (implementation plan for Phase 1 of #105).
**Parent plan:** [`cosim-backend-portability.md`](./cosim-backend-portability.md) (staging).
**Architecture:** [ADR 0017 — Amendment 2026-06-07](../adr/0017-cosim-execution-model.md).
**Base:** stacks on Phase 0 (branch `cosim-backend-seam-phase0`, PR #118 —
`CosimBackend` trait + `MetalBackend` extracted, Metal bit-identical).

## Goal

`jacquard cosim` runs on a **CPU reference backend** with no GPU feature,
reusing the scheduler, peripheral models, and VCD machinery — and the existing
cosim regression fixtures run on free `ubuntu-latest` CI. Throughput is *not* a
goal (the CPU backend is the oracle); correctness parity with Metal is.

## The core problem

After Phase 0 the `CosimBackend` trait exists, but **everything still lives in
`src/sim/cosim_metal.rs` gated `#[cfg(feature = "metal")]`** — the trait,
`BitOp`, `run_cosim`, the patchers, `MetalBackend`. `run_cosim`'s setup + loop
still touch Metal directly at ~54 `.contents()` / 20 `new_buffer` / 19
`MTLResourceOptions` sites (xprop X-mask seeding, flash-state init, SRAM fill,
`set_flash_din`, `--check-with-cpu` reads, VCD-ring drain, UART/bus channel
drains). For a `CpuBackend` to compile and run without `metal`, the
orchestration must become backend-agnostic and these sites must move behind the
seam.

Site categories (run_cosim body): flash ~38, sram ~22, bus/wb-trace ~21,
states ~13, uart ~12, vcd-ring 3.

## Chosen interface approach — "fat backend constructor" (ADR Layer 1/2 split)

The backend **owns its allocation and initialisation**. `run_cosim` becomes
generic over `B: CosimBackend` and only: builds the backend-agnostic
*descriptions* (schedule ops, init seeds, peripheral configs), calls
`B::new(...)`, then drives the per-edge loop through trait methods. Each backend
allocates + initialises its own storage inside `new`. This is the clean Layer-1
(agnostic orchestration) / Layer-2 (backend) split; it keeps the trait surface
small at the cost of larger per-backend constructors (the Metal constructor is
just today's setup block relocated).

### Trait additions (beyond Phase 0's `init_schedule`/`edge_ops_mut`/`edge_ops`/`edges_per_period`/`gcd_ps`/`run_edges`/`wait`)

```rust
// Design state (Phase 0 deferral #1).
fn state(&self) -> &[u32];            // full [2 × state_size]
fn state_mut(&mut self) -> &mut [u32];
fn sram(&self) -> &[u32];             // for --check-with-cpu / final dump

// Flash control surface (the CPU + GPU flash FSMs both need these).
fn flash_set_in_reset(&mut self, in_reset: bool);
fn flash_d_i(&self) -> u8;

// Per-edge output snapshot for VCD — replaces run_edges' `metal::Buffer`
// (Phase 0 deferral #2). run_edges snapshots into a backend-owned ring;
// the orchestration drains agnostic &[u32] slots.
fn vcd_snapshot(&self, edge_in_batch: usize) -> &[u32];  // [2 × state_size]
```

`run_edges` loses its `Option<&metal::Buffer>` parameter; whether to capture
snapshots becomes backend state set at construction (`enable_vcd: bool`).

### Peripheral output decode — the one real design wrinkle

Input-driving peripherals already run CPU-side (`PeripheralModel`,
`models/*.rs`) via the patchers — backend-agnostic, no change. **Output**
decode differs by backend:

| Peripheral | Metal (today) | CpuBackend (Phase 1) |
|---|---|---|
| SPI flash | `gpu_flash_model_step` kernel | **`CppSpiFlash` FFI** (exists, `cosim_metal.rs:16`) |
| Bus trace (APB) | `gpu_io_step` kernel | **`BusTraceDecoder`** (already CPU, `models/bus_trace.rs`) |
| UART TX decode | `gpu_io_step` kernel | **new CPU decoder** (port the `UartDecoderState` FSM, currently a GPU-side mirror, to a `step(output_state)` CPU fn) |

So CpuBackend's `run_edges` (N=1): `state_prep` (output→input copy + apply edge
`BitOp`s + clear driven X-mask — the loop at `cosim_metal.rs:4286–4301`) →
`cpu_reference::simulate_block_v1` per block → CPU flash/UART/bus decode reading
the new output state. The bus + flash decoders exist; only the UART TX decoder
needs a CPU port (small: shift-register baud FSM).

## Module split (required, not cosmetic)

`src/sim/cosim_metal.rs` → `src/sim/cosim/`:

- **`cosim/mod.rs`** (NOT gated): `CosimBackend` trait, `BitOp`, `StatePrepParams`,
  the patchers + `ModelDrivenClockState`, the agnostic `run_cosim<B>`, the
  scheduler/VCD/drain glue, and `CpuBackend`. Public API (`run_cosim`,
  `CosimOpts`, `CosimResult`) re-exported here.
- **`cosim/metal.rs`** (`#[cfg(feature = "metal")]`): `MetalBackend`,
  `MetalSimulator`, `ScheduleBuffers`, the `encode_*`/`profile_gpu_kernels`
  methods, `create_ops_buffer`/`create_prep_params_buffer`, GPU IO struct
  definitions.
- `src/sim/mod.rs`: `pub mod cosim;` (drop the gated `cosim_metal`).

## Sub-commit sequencing (each gated by the bit-identical Metal harness)

1. **De-Metal the trait surface** — add `state`/`state_mut`/`sram`/
   `flash_*`/`vcd_snapshot`; move the VCD ring into `MetalBackend`; drop
   `metal::Buffer` from `run_edges`. Route the existing run_cosim diagnostic
   reads through the new accessors. Metal bit-identical.
2. **Move buffer setup+init into `MetalBackend::new`** — relocate the 20
   allocs + 54 init/access sites; `run_cosim` calls `MetalBackend::new(...)`.
   Still `#[cfg(metal)]`. Metal bit-identical. *(Largest step.)*
3. **Make `run_cosim` generic** `run_cosim<B: CosimBackend>(...)`; Metal path
   constructs `MetalBackend`. Metal bit-identical.
4. **Module split** `cosim_metal.rs` → `cosim/{mod,metal}.rs` (mechanical move;
   audit `pub use`, `src/sim/mod.rs`, `src/bin/jacquard.rs`, docs, CI). Metal
   bit-identical.
5. **Implement `CpuBackend`** — `Vec<u32>` state/sram, `Vec<Vec<BitOp>>`
   schedule, `run_edges` via `cpu_reference::simulate_block_v1`, flash via
   `CppSpiFlash`, bus via `BusTraceDecoder`, new CPU UART decoder.
6. **Wire `cmd_cosim`** — select `CpuBackend` when no GPU feature (or explicit
   `--backend cpu`); remove the hard-error (`jacquard.rs:507`).
7. **Linux CI job** — `ubuntu-latest` runs `xprop_cosim` (cosim mode),
   `dual_uart`, `apb_trace` via `CpuBackend`, asserting their checkers.

## Testing strategy

- **Metal bit-identical** after steps 1–4: `/tmp/claude/cosim_fixtures.sh` +
  `shasum -c` against the Phase 0 golden; `jtag_minimal` 4M PASS.
- **CpuBackend correctness** (steps 5–7): extend the #113 cross-backend harness
  (`scripts/ci/compare_backend_vcds.py`) to cosim — run the same fixture on CPU
  vs Metal, assert byte-identical output VCDs. The CPU `PeripheralModel` is
  ground truth.
- `cargo test --lib` (no feature) must compile + pass once the trait/`CpuBackend`
  are non-gated.

## Risks

- **Step 2 size** — relocating 54 sites is the bulk; the bit-identical gate +
  per-site categories above bound it. Do it as several internal checkpoints.
- **UART CPU decoder divergence** — equivalence-test against Metal's
  `gpu_io_step` output on `dual_uart` before relying on it.
- **`simulate_block_v1` X-mask handling** — the CPU stepper must replicate the
  GPU `state_prep` X-mask clear for `--xprop` parity (`xprop_cosim` fixture is
  the guard).
- **Generic `run_cosim<B>` monomorphisation** — only two backends; negligible.
- **Stacked-PR rebase** — #118 must merge (or this rebases) before Phase 2; the
  handoff's stacked-PR gotcha applies.

## Out of scope (→ Phase 2)

CUDA/HIP backend + Tier-2 GPU peripherals; the `GpuPeripheral` trait. Phase 1
defines no GPU-peripheral seam — only the CPU reference path + the agnostic
orchestration that Phase 2 builds on.
