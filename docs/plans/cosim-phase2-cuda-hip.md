# CUDA/HIP Parity for Release — cosim Phase 2 + #104 sim timing

**Status:** Active — planned 2026-06-17, not yet started.
**Goal (maintainer):** "close off CUDA and HIP before release" — bring both GPU
backends to Metal parity on the two paths that lag today:
1. **#104** — `sim` setup/hold timing-violation detection (Metal-only today).
2. **#105 Phase 2** — `cosim` on CUDA/HIP **with batching** (Metal-only today;
   a `--features cuda` build currently falls through to `CpuBackend`).

**Architecture:** [ADR 0017](../adr/0017-cosim-execution-model.md) (Layer 1/2/3 +
peripheral contract); staging in
[cosim-backend-portability.md](cosim-backend-portability.md) (this is the
Phase-2 detail doc, sibling to `cosim-phase1-cpu-backend.md`).

**Release bar (decided):** full batched cosim (checkpoints 2a **and** 2b), not a
per-edge-only intermediate. Per-edge-only CUDA is an unusably-slow artifact
(PCIe round-trip per edge); 2b is what makes it a real backend.

---

## Hard constraint: validation is CI-only on the T4

This work cannot be built or run on the dev machine (Apple Silicon → Metal
only). **Every CUDA/HIP iteration is a CI round-trip on `tesla4-runner`.** The
workflow consequences:

- Maximise local confidence before each push: `cargo check`-equivalent reasoning,
  mirror the proven Metal path line-for-line, keep diffs reviewable.
- **Batch changes per push** to minimise CI cycles; expect red→green iteration on
  the T4 for genuinely GPU-dependent bugs (struct ABI mismatch, fence scope,
  `__shfl` width).
- The cross-backend equivalence harness (`compare_backend_vcds.py`) and the
  Phase-1 CPU goldens (`tests/*/expected/`) are the correctness oracle — CUDA/HIP
  output must be byte-identical to CPU/Metal.

---

## Track 0 — #104: CUDA/HIP `sim` timing (warm-up, Rust-only)

**Why first:** smallest, independent, zero kernel work, and it exercises the
full CUDA/HIP build + T4 CI loop before the hard cosim work. Confirmed by
investigation:

- The kernel-side timing logic is **already in the shared
  `csrc/kernel_v1_impl.cuh`** (`simulate_block_v1` arrival writeback at :530-532,
  setup/hold `write_event` at :546-553).
- The **timed C launchers already exist**: `simulate_v1_noninteractive_timed_cuda`
  (`kernel_v1.cu:50`), `..._timed_hip` (`kernel_v1.hip.cpp:70`) — both call the
  same kernel as Metal, just passing `timing_constraints` + `event_buffer`
  through instead of `nullptr`.
- `ucc::bindgen` will auto-surface them as `ucci::simulate_v1_noninteractive_timed`
  / `ucci_hip::...` (suffix stripped) with no build.rs change.
- Today `sim_cuda`/`sim_hip` accept `timing_constraints` but call the **untimed**
  `simulate_v1_noninteractive_simple_scan` (`jacquard.rs:1196` / `:1365`) and
  drop the arg.

**The gap is entirely Rust (~120-150 lines):**

1. Un-gate `TimingReportConfig` (`jacquard.rs:13`) and its impl from
   `#[cfg(feature="metal")]` → `cfg(any(metal,cuda,hip))` (struct has no
   Metal-specific fields).
2. Un-gate `report_cfg` construction in `cmd_sim` (`jacquard.rs:518-550`) for
   cuda/hip.
3. `sim_cuda`: add `report_cfg: &TimingReportConfig` param; when
   `timing_constraints.is_some()` → allocate `Box<EventBuffer>`, call
   `ucci::simulate_v1_noninteractive_timed(...)`, then `process_events()` once
   post-run feeding `ReportBuilder`; else keep `simple_scan`. Add the
   `expand_states_for_arrivals` call (mirror `sim_metal` :804-807) when
   `script.timing_arrivals_enabled`.
4. `sim_hip`: identical.
5. Update the two `cmd_sim` call sites (`jacquard.rs:564-580`) to pass
   `&report_cfg`.

**Known limitation (acceptable, pre-existing):** the CUDA/HIP `sim` path is a
single bulk cooperative launch, so the `EventBuffer` is drained once at
end-of-run, not per-cycle. `MAX_EVENTS=1024` with an `overflow` flag; no
early-exit on `$finish` from events. Out of scope to change here.

**Verification:** extend the `cuda`/`hip` CI jobs to run the existing timing
fixture (`tests/timing_test/dff_test_synth.gv` with constraints) and assert the
timing report matches Metal's. Add the timing VCD to `backend-equivalence`.

**Exit:** `jacquard sim --features cuda … --timing-report` produces the same
violations/report as Metal on the T4; `--timed` arrival VCD matches.

---

## Track 1 — Phase 2a: CUDA/HIP cosim design-step backend (per-edge)

The permanent correctness/fallback path: GPU design step, **CPU peripherals**
(Tier 1, reused as-is), per-edge orchestration (the proven Phase-1 path
generalised). No batching yet.

### 1a. Kernels (shared `kernel_v1_impl.cuh`, two thin launchers)

CUDA/HIP have **zero** cosim kernels today. Add, mirroring the Metal shader:

- **`simulate_v1_stage`** — a *non-cooperative* per-stage `__global__`
  (`num_blocks` blocks × 256 threads), one major stage per launch; host loops
  stages, each launch is the grid barrier. The body is the already-shared
  `simulate_block_v1` (impl.cuh:32) — the new global is a thin wrapper indexing
  `blocks_start[stage_i*num_blocks + blockIdx.x]`, input slot `states[0]`, output
  slot `states[state_size]` (`current_cycle=0`, 2-slot ping-pong). This avoids
  cooperative launch entirely for cosim (the hard-to-port `grid.sync` stays
  `sim`-only). Mirror the Metal `SimParams` ABI exactly.
- **`state_prep`** — port from shader:679 (output→input copy + per-`BitOp` set +
  driven-bit X-mask clear at `xmask_state_offset`). Use `__threadfence()` (device
  scope) between copy and bit-ops to match Metal's `mem_device` barrier.

Add `extern "C"` `_cuda`/`_hip` launchers to `kernel_v1.cu`/`.hip.cpp`. Replicate
`SimParams` and `StatePrepParams` `#[repr(C)]` structs in a device header with
identical field order/padding (see the GPU-struct catalogue captured in the
Phase-2 investigation, folded into the appendix below).

### 1b. The unified-memory abstraction (the central porting decision)

Metal uses `StorageModeShared` (one physical pointer; CPU write = upload). CUDA/
HIP have no arbitrary-host zero-copy. **Decision: use managed memory**
(`cudaMallocManaged`/`hipMallocManaged`) for the v1 backend — it is the closest
functional analog to Metal's unified buffers and keeps the backend code
structurally identical to `MetalBackend` (CPU casts the pointer directly; the
driver migrates pages). Revisit pinned-host + explicit-mirror only if managed
memory's page-migration cost dominates (profile on the T4 in 2b). The
`edge_ops_mut` / `state` / `sram` / drain accessors in the trait already isolate
this — only the backend struct changes.

Buffers to allocate in `CudaBackend::new` (sizes per the Metal catalogue):
`states` (2×state_size), `sram_data`, `sram_xmask`, `blocks_start`,
`blocks_data` (reuse the `ulib` UVec device copies where they exist),
`event_buffer`, optional `timing_constraints`, and the per-edge schedule
`(StatePrepParams, Vec<BitOp>)` pairs.

### 1c. `CudaBackend`/`HipBackend` Rust impls

New `src/sim/cosim/cuda.rs` (and `hip.rs`), `#[cfg(feature="cuda")]` /
`#[cfg(feature="hip")]`, implementing `CosimBackend` exactly like `MetalBackend`:

- `new` — allocate managed buffers (1b).
- `init_schedule` — store per-edge `(params, ops)` in managed buffers.
- `edge_ops_mut` — slice over the managed ops buffer (write = visible to GPU
  after the next launch in-stream; no explicit flush with managed memory, but
  add `cudaStreamSynchronize` discipline at the documented points).
- `run_edges` — per edge: launch `state_prep` → (CPU peripherals: apply flash
  d_i into `state_mut`) → loop `simulate_v1_stage × num_major_stages` →
  read back output slot → run CPU `PeripheralModel`s (flash FSM via existing
  `CppSpiFlash`, UART/bus via Tier-1 models) → snapshot to VCD ring. All on one
  `cudaStream_t`; stream ordering gives sequential execution (no inter-kernel
  barriers needed).
- `state`/`state_mut`/`sram` — direct managed-pointer slices.
- `drain_uart_tx`/`drain_bus_beats`/`flash_d_i` — for 2a these come from the
  **CPU** Tier-1 models (the `CpuBackend` already has working CPU ports — reuse
  that decode logic, do not re-port).
- `wait` — `cudaEvent_t` recorded at end-of-batch + `cudaEventSynchronize`
  (replaces Metal `SharedEvent`).

### 1d. cmd_cosim dispatch

`jacquard.rs:1784-1789` currently: `metal` → `run_cosim`, else → `run_cosim_cpu`.
Add `run_cosim_cuda`/`run_cosim_hip` shims (`run_cosim_generic::<CudaBackend>`)
with the same priority order as `sim` (metal > cuda > hip > cpu).

**Verification (2a exit):** add a `jacquard cosim` invocation to the `cuda`/`hip`
CI jobs on a small fixture (e.g. `xprop_cosim`), output VCD **byte-identical** to
the CPU golden in `tests/*/expected/` and to Metal. Telemetry shows `batch=1`
(per-edge) — expected for 2a. JTAG-style CPU-driven-clock fixture also matches
(this path stays per-edge permanently).

---

## Track 2 — Phase 2b: Tier-2 GPU peripherals → batched (the perf path)

Port the three peripheral kernels so peripherals run *inside* the batch,
eliminating the per-edge PCIe round-trip. This is what makes CUDA/HIP cosim
performant.

### 2b kernels (shared impl header + two launchers each)

- **`gpu_apply_flash_din`** (shader:904) — write `FlashState.d_i` → input state.
- **`gpu_flash_model_step`** (shader:943) — SPI/QSPI flash FSM, dual-step setup
  delay; needs the 16 MiB firmware buffer + `FlashState` persistent struct.
- **`gpu_io_step`** (shader:1170) — UART-TX decoder (4-state FSM ×4 channels) +
  APB3 bus-trace beat extraction + legacy WB trace; writes ring buffers
  (`UartChannel`, `BusTraceChannel`).

Each is single-thread (thread 0) work — straightforward ports. Replicate the
full GPU-struct ABI (`FlashState`, `FlashDinParams`, `FlashModelParams`,
`UartParams`/`UartChannel`/`UartDecoderState`, `BusTraceParamsAll`/
`BusTraceChannel`, etc.) — exhaustive field layouts captured in the appendix.

### 2b backend changes

- `run_edges` batches `BATCH_SIZE` edges into one stream with no host
  intervention: per edge `state_prep → gpu_apply_flash_din → simulate×N →
  gpu_flash_model_step → gpu_io_step → (memcpy state→vcd_ring slot)`. One
  `cudaEvent` at end-of-batch.
- `drain_uart_tx`/`drain_bus_beats`/`flash_d_i` now read the **GPU** ring buffers
  (managed-memory cursors), exactly like `MetalBackend::drain_*`.
- Define the `GpuPeripheral` seam (ADR 0017 Layer 3) here so Phase 3 (Tier-3
  single-source) can slot in later.
- Keep the 2a CPU-peripheral path as the documented fallback for CPU-side-model
  designs (JTAG replay) and as the per-kernel equivalence oracle.

**Equivalence-test each kernel against its Tier-1 CPU model** (the CPU port from
Phase 1 is ground truth) before wiring it into the batch.

**Verification (2b exit):** `cosim --features cuda` on a GPU-peripheral fixture
runs **batched** (telemetry `batch > 1`), output VCD matching all backends; the
per-edge fallback fixture also matches. Gate on `tesla4-runner`.

---

## CI: the cross-backend cosim equivalence gate

`compare_backend_vcds.py` is N-way and backend-agnostic (no change needed).
Add to the `cuda` and `hip-on-nvidia` jobs a `jacquard cosim` step per fixture,
upload the VCDs, and extend `backend-equivalence` with cosim comparisons
(cuda vs hip vs metal vs the committed CPU golden). This closes the current gap:
cosim is CI-covered on CPU only; sim equivalence covers GPU only.

---

## Sequencing & checkpoints (each = one reviewable PR-sized push, CI-gated)

| # | Track | Deliverable | CI gate |
|---|-------|-------------|---------|
| T0 | #104 | CUDA/HIP sim timing wired (Rust-only) | timing report == Metal on T4 |
| T1.1 | 2a | `simulate_v1_stage` + `state_prep` CUDA/HIP kernels + launchers compile | `cuda`/`hip` build green |
| T1.2 | 2a | `CudaBackend`/`HipBackend` + cmd_cosim dispatch (managed mem, CPU peripherals) | cosim VCD == CPU golden, batch=1 |
| T2.1 | 2b | `gpu_apply_flash_din` + `gpu_flash_model_step` ported, equivalence-tested | flash cosim == golden |
| T2.2 | 2b | `gpu_io_step` ported; batched `run_edges`; GPU drains | batched cosim == golden, batch>1 |
| T2.3 | 2b | `GpuPeripheral` seam + CI equivalence gate | backend-equivalence (cosim) green |

CUDA and HIP land **together** at each step (shared `*_impl.cuh`; HIP = a thin
second launcher + `mod ucci_hip`).

## Risks

- **CI-only validation** — the dominant friction. Mitigate by mirroring Metal
  exactly and batching pushes.
- **GPU-struct ABI drift** — any field-order/padding mismatch between the device
  header and Rust `#[repr(C)]` silently corrupts. Add static-size asserts both
  sides; the equivalence test catches behavioural drift.
- **Managed-memory perf** — page migration may dominate; profile in 2b, fall
  back to pinned+mirror only if needed (isolated behind the trait).
- **`__shfl_down_sync` width / fence scope** — `simd_shuffle_down` → `__shfl_down_sync(0xFFFFFFFF,…)`; `mem_device` → `__threadfence()`. Classic per-arch GPU bugs; surface only on the T4.
- **Two kernel families per peripheral** (CUDA+HIP shared impl, Metal `.metal`) —
  equivalence tests against the CPU model are the guard; Tier 3 removes the
  hand-maintenance eventually.

## Appendix — authoritative source material

The exhaustive Metal cosim spec (every kernel signature, the per-edge dispatch
ordering, all device-buffer layouts, the full GPU `#[repr(C)]` struct catalogue,
and the Metal-specific constructs needing CUDA analogs) was captured during the
2026-06-17 Phase-2 investigation. Primary sources to port from:
`csrc/kernel_v1.metal` (kernels), `src/sim/cosim/metal.rs` (`MetalBackend`,
`run_edges`, `encode_*`, drains), `csrc/kernel_v1_impl.cuh` (shared
`simulate_block_v1`). The #104 gap analysis (Rust-only, ~120-150 lines) and the
build/FFI/CI mechanism (ucc `bindgen` auto-surfaces `_cuda`/`_hip` launchers; no
build.rs change to add kernels) are recorded in the same investigation.
</content>
</invoke>
