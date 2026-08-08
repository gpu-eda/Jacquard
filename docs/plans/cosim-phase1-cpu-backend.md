# Cosim Phase 1 — `CpuBackend` + Linux cosim CI (implementation plan)

**Status:** Proposed (implementation plan for Phase 1 of #105).
**Parent plan:** [`cosim-backend-portability.md`](./cosim-backend-portability.md) (staging).
**Architecture:** [Decision 0017 — Amendment 2026-06-07](../architecture/decisions/0017-cosim-execution-model.md).
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

## Chosen interface approach — "fat backend constructor" (decision Layer 1/2 split)

The backend **owns its allocation and initialisation**. `run_cosim` becomes
generic over `B: CosimBackend` and only: builds the backend-agnostic
*descriptions* (schedule ops, init seeds, peripheral configs), calls
`B::new(...)`, then drives the per-edge loop through trait methods. Each backend
allocates + initialises its own storage inside `new`. This is the clean Layer-1
(agnostic orchestration) / Layer-2 (backend) split; it keeps the trait surface
small at the cost of larger per-backend constructors (the Metal constructor is
just today's setup block relocated).

### Trait additions (beyond Phase 0's `init_schedule`/`edge_ops_mut`/`edge_ops`/`edges_per_period`/`gcd_ps`/`run_edges`/`wait`)

> **Revised per plan review (2026-06-07).** `flash_d_i` is *not* a trait method
> — Metal and CPU update flash `d_i` at different points in the dispatch cycle
> (GPU `gpu_flash_model_step` vs `CppSpiFlash::step`), so surfacing it through
> the seam couples ordering. Instead `CpuBackend::run_edges` calls
> `CppSpiFlash::step()` internally and injects the result into the input state;
> `d_i` never crosses the seam. `flash_set_in_reset` stays (called *before*
> `run_edges`).

```rust
// Design state (Phase 0 deferral #1). Full [2 × effective_state_size] —
// input slot followed by output slot.
fn state(&self) -> &[u32];
fn state_mut(&mut self) -> &mut [u32];
fn sram(&self) -> &[u32];             // final dump / equivalence compare

// Flash reset line (set before run_edges; both flash FSMs honour it).
fn flash_set_in_reset(&mut self, in_reset: bool);

// Per-edge output snapshot for VCD — replaces run_edges' `metal::Buffer`
// (Phase 0 deferral #2). Slot layout is `[input_state | output_state]`,
// matching today's Metal ring. The orchestration drains agnostic &[u32]
// slots: `for i in 0..batch { let s = backend.vcd_snapshot(i); .. }`.
// CpuBackend (N=1) fills slot 0 before run_edges returns.
fn vcd_snapshot(&self, edge_in_batch: usize) -> &[u32];  // [2 × eff_state_size]
```

`run_edges` loses its `Option<&metal::Buffer>` parameter; whether to capture
snapshots becomes backend state set at construction (`enable_vcd: bool`). The
current raw `ring.contents()` pointer-math drain (`cosim_metal.rs` ~4065–4198)
is refactored to the `vcd_snapshot(i)` loop in step 1.

### Peripheral output decode — the one real design wrinkle

Input-driving peripherals already run CPU-side (`PeripheralModel`,
`models/*.rs`) via the patchers — backend-agnostic, no change. **Output**
decode differs by backend:

| Peripheral | Metal (today) | CpuBackend (Phase 1) |
|---|---|---|
| SPI flash | `gpu_flash_model_step` kernel | **`CppSpiFlash` FFI** (exists, `cosim_metal.rs:16`) |
| Bus trace (APB) | `gpu_io_step` kernel | **`BusTraceDecoder`** (already CPU, `models/bus_trace.rs`) |
| UART TX decode | `gpu_io_step` kernel | **new CPU decoder** (port the `UartDecoderState` FSM, currently a GPU-side mirror, to a `step(output_state)` CPU fn) |
| Wishbone trace (legacy `WbTraceParams`) | `gpu_io_step` kernel | **stub** — debug-only `eprintln!` path; CpuBackend leaves `write_head=0` so the drain is a silent no-op (acknowledged, not ported) |

GPIO and JTAG/UART-RX are **input-only drivers** (`models/*.rs`, already CPU)
— no output decode. The `step_edge(&[], ..)` placeholder for model output-state
(`cosim_metal.rs` ~3907) stays empty in Phase 1: the Phase-1 fixtures
(`dual_uart`, `apb_trace`, `xprop_cosim`) have no model that reads design
output, so this is sound; wiring `state()` into `step_edge` is deferred with
the I²C/SPI-model work.

So CpuBackend's `run_edges` (N=1, iterating **blocks × `num_major_stages`**):
`state_prep` (output→input copy + apply edge `BitOp`s + clear driven X-mask —
the loop at `cosim_metal.rs` ~4286–4301) → `cpu_reference::simulate_block_v1`
per block/stage → CPU flash/UART/bus decode reading the new output state. The
bus + flash decoders exist; only the UART TX decoder needs a CPU port (small:
shift-register baud FSM).

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

1. **De-Metal the trait surface + VCD ring.** Add `state`/`state_mut`/`sram`/
   `flash_set_in_reset`/`vcd_snapshot`; move the VCD ring into `MetalBackend`;
   drop `metal::Buffer` from `run_edges`; refactor the raw `ring.contents()`
   drain (`~4065–4198`) to the `vcd_snapshot(i)` loop. Route the loop-body
   diagnostic reads (`--check-with-cpu`, dff-dump `~4205`, trace-signals
   `~4278`, deep-diag `~4634`, `post_reset_state_snapshot`) — ~100 lines of
   direct `states_buffer.contents()` — through `state()`/`sram()`. Metal
   bit-identical. *Exit assertion: `grep metal:: ` over the run-cosim body +
   trait shows only the soon-to-move construction block.*
2. **Move buffer setup+init into `MetalBackend::new`**, as **three checkpoints**,
   each Metal bit-identical:
   - **2a** — flash (state/din/model/data) buffer alloc + init.
   - **2b** — UART + WB + bus-trace buffer alloc + init.
   - **2c** — states/sram/sram-xmask/event/blocks alloc + init (incl. xprop
     X-mask seeding). `run_cosim` ends up calling `MetalBackend::new(...)`.
3. **De-Metal the `run_cosim` body + make it generic.** Split into three
   bit-identical checkpoints (the loop body, not the construction block, is the
   real work — step 1 deferred the diagnostic-read routing, so it lands here):
   - **3a** *(done, `d5a029f`)* — add `state`/`state_mut`/`sram` to the trait +
     `MetalBackend` (`sram_len` field); route the ~15 read-only
     `states_buffer`/`sram_data_buffer` loop-body reads through `state()`/
     `sram()`. `run_cosim` stays concrete-typed.
   - **3b-i** — route the remaining concrete-field reads (groups C+D) off
     `MetalBackend` fields, **decoded-records seam** (Decision 0017 Layer 3):
     - *Flash diagnostics (C):* `FlashModelParams`/`FlashDinParams` reads become
       agnostic locals (derived from `config`+`gpio_map`, as `build_flash_buffers`
       does). `FlashState` reads → `flash_d_i() -> u8` (functional, for
       `--check-with-cpu`) + `flash_debug_snapshot() -> FlashDebug` (agnostic
       struct mirroring the printed fields); the tick-0 raw-bytes/offsetof dump
       stays Metal-internal behind a debug trait method (not deleted).
     - *Peripheral drains (D):* `drain_uart_tx() -> Vec<(usize,u8)>`,
       `drain_bus_beats() -> Vec<RawBeat>`, `drain_wb_trace_debug()` (legacy
       eprintln, Metal-internal; CpuBackend no-op), `uart_decoder_debug(ch)`.
       The read cursors (`uart_read_heads`, `wb/bus_trace_read_head`) move into
       `MetalBackend`; `bus_lanes`/`uart_names`/event-dispatch/`tick` stay in the
       agnostic orchestration. `run_cosim` stays concrete-typed.
       *Exit: zero `backend.<field>.contents()` in the loop body.*
   - **3b-ii** — assemble `MetalBackend::new` (the deferred step-2 finale:
     `build_flash`+`build_io`+`build_state`+struct literal+`init_schedule`),
     route the pre-loop stimulus deposits through `state_mut()`, flip to
     `run_cosim<B: CosimBackend>` constructing the backend via the fat
     constructor, update the `jacquard.rs` call site. Metal bit-identical.
     *Exit: no `metal::` / `MTLResourceOptions` token outside the Metal
     constructor + impl.*
4. **Module split** `cosim_metal.rs` → `cosim/{mod,metal}.rs`. `mod.rs` must
   import **zero** `metal::*`; `ScheduleBuffers`/`MetalSimulator`/
   `create_ops_buffer`/`create_prep_params_buffer` and the GPU IO structs live
   entirely in `cosim/metal.rs` (gated). Audit `pub use`, `src/sim/mod.rs`,
   `src/bin/jacquard.rs`, docs, CI. **Gate: `cargo check --lib` (no feature)
   must compile** (it currently can't — `cosim_metal` is fully gated). Metal
   bit-identical with `--features metal`.
5. **Implement `CpuBackend`** — `Vec<u32>` state sized
   `effective_state_size() * 2`, `Vec<u32>` sram, `Vec<Vec<BitOp>>` schedule.
   `run_edges` (blocks × stages) via `cpu_reference::simulate_block_v1`; flash
   via `CppSpiFlash::step` (internal, injects `d_i` into input state); bus via
   `BusTraceDecoder`; new CPU UART decoder. **`CpuBackend::new` asserts**
   `!script.timing_arrivals_enabled` and `!(xprop_enabled && sram_storage_size > 0)`
   (see Risks). `--check-with-cpu` becomes a no-op-with-warning under CpuBackend
   (the backend *is* the reference — never compare it to itself).
6. **Wire `cmd_cosim`** — select `CpuBackend` when no GPU feature (or explicit
   `--backend cpu`); remove the hard-error (`jacquard.rs` ~1684–1691, the
   `cmd_cosim` branch — *not* the `sim` hard-error at ~507).
7. **Linux CI job** — `ubuntu-latest` runs `xprop_cosim` (cosim mode),
   `dual_uart`, `apb_trace` via `CpuBackend`, asserting their checkers + the
   cross-backend VCD equivalence (below).

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

- **Step 2 size** — relocating ~54 sites is the bulk; split into 2a/2b/2c
  (above), each Metal bit-identical, so the diff stays reviewable/bisectable.
- **Module-split compile gate (step 4)** — any stray `metal::` import in
  `mod.rs` breaks `cargo check --lib` (no feature). Step 1's exit assertion +
  step 3's token sweep front-load this; step 4 only *moves* code.
- **UART CPU decoder divergence** — equivalence-test against Metal's
  `gpu_io_step` output on `dual_uart` before relying on it.
- **`simulate_block_v1` X-mask handling** — the CPU stepper must replicate the
  GPU `state_prep` X-mask clear for `--xprop` parity (`xprop_cosim` is the
  guard — but note it is SRAM-less, so SRAM-xprop is *not* covered, below).
- **SRAM xprop unsupported on CpuBackend** — `cpu_reference::simulate_block_v1`
  (`cpu_reference.rs:17`) takes no `sram_xmask`, so `--xprop` on a
  SRAM-containing design would read SRAM as always-known (no X). `CpuBackend::new`
  **asserts** `!(xprop_enabled && sram_storage_size > 0)` until resolved. Logic-
  only xprop (the `xprop_cosim` fixture) is fine.
- **Timed cosim deferred** — arrival readback rides the GPU ring;
  `CpuBackend::new` **asserts** `!timing_arrivals_enabled`. `effective_state_size()`
  (`flatten.rs:1582`) still sizes the Vec, but the arrival section stays zero.
- **`--check-with-cpu` self-comparison** — disabled-with-warning under CpuBackend
  (it would compare `simulate_block_v1` to itself). Step 5.
- **Generic `run_cosim<B>` monomorphisation** — only two backends; negligible.
- **Stacked-PR rebase** — #118 must merge (or this rebases) before Phase 2; the
  handoff's stacked-PR gotcha applies.

## Out of scope (→ Phase 2)

CUDA/HIP backend + Tier-2 GPU peripherals; the `GpuPeripheral` trait. Phase 1
defines no GPU-peripheral seam — only the CPU reference path + the agnostic
orchestration that Phase 2 builds on.
