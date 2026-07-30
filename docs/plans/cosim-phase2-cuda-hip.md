# CUDA/HIP Parity for Release — cosim Phase 2 + #104 sim timing

**Status:** ✅ **SHIPPED** — merged to `main` in PR #120 (2026-06-21,
rebase). Track 0 (#104 sim timing) + Stages A/B/C (T1.1–T2.2) all done and
T4-green. Remaining: T2.3 (optional, below) + the performance follow-up
(issue #122 — managed-memory profiling/tuning; the track closed on
*correctness*, performance is untuned).
**Goal (maintainer):** "close off CUDA and HIP before release" — bring both GPU
backends to Metal parity on the two paths that lag today:
1. **#104** — `sim` setup/hold timing-violation detection (Metal-only today).
2. **#105 Phase 2** — `cosim` on CUDA/HIP **with batching** (Metal-only today;
   a `--features cuda` build currently falls through to `CpuBackend`).

**Architecture:** [Decision 0017](../architecture/decisions/0017-cosim-execution-model.md) (Layer 1/2/3 +
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

## Decision (2026-06-19): one backend, no CPU-peripheral intermediate

**The originally-planned checkpoint 2a (a CUDA backend with *CPU* peripherals,
per-edge) is dropped.** No production backend works that way — Metal always runs
peripherals on the GPU, falling to `batch=1` (not to CPU peripherals) for
model-driven-clock designs (JTAG). A CPU-peripheral CUDA variant would be a
bring-up crutch that exists nowhere else and muddies the architecture.

**Target: a single `CudaBackend`/`HipBackend` that mirrors `MetalBackend`** —
GPU design step + GPU peripherals + variable batching + managed memory.
`CpuBackend` remains the pure-CPU reference oracle. Bisectability (2a's only real
benefit) is recovered by **staging on the fixtures**, since each exercises a
different kernel subset — no separate backend:

- **Stage A** — `CudaBackend` with the design step only (`cosim_state_prep` +
  `cosim_simulate_stage`, landed in T1.1) + batched orchestration + managed
  memory + VCD ring. Validate against **`xprop_cosim`** (4 logic fixtures: no
  flash/UART/bus). Proves the whole pipeline end-to-end with zero peripherals.
- **Stage B** — port `gpu_io_step` (UART + bus). Validate **`dual_uart`** +
  **`apb_trace`**.
- **Stage C** — port the flash kernels (`gpu_apply_flash_din`,
  `gpu_flash_model_step`). Validate the flash/JTAG fixtures.

Every stage is the *real* architecture, gated against the committed CPU/Metal
goldens on the T4.

## Track 1 — CUDA/HIP cosim backend (mirrors MetalBackend)

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
- `run_edges` — batch `BATCH_SIZE` edges into one `cudaStream_t`, mirroring
  Metal's `encode_and_commit_gpu_batch`. Per edge: `cosim_state_prep` →
  `gpu_apply_flash_din` → `cosim_simulate_stage × num_major_stages` →
  `gpu_flash_model_step` → `gpu_io_step` → memcpy output→VCD-ring slot. Stream
  ordering gives sequential execution (no inter-kernel barriers). (Stage A wires
  only state_prep + simulate; B adds `gpu_io_step`; C adds the flash kernels.)
- `state`/`state_mut`/`sram` — direct managed-pointer slices.
- `drain_uart_tx`/`drain_bus_beats`/`flash_d_i` — read the **GPU** ring buffers
  (managed-memory cursors), exactly like `MetalBackend::drain_*` (Stages B/C).
- `wait` — `cudaEvent_t` recorded at end-of-batch + `cudaEventSynchronize`
  (replaces Metal `SharedEvent`).

### 1d. cmd_cosim dispatch

`jacquard.rs:1784-1789` currently: `metal` → `run_cosim`, else → `run_cosim_cpu`.
Add `run_cosim_cuda`/`run_cosim_hip` shims (`run_cosim_generic::<CudaBackend>`)
with the same priority order as `sim` (metal > cuda > hip > cpu).

**Verification:** add `jacquard cosim` invocations to the `cuda`/`hip` CI jobs,
output VCD **byte-identical** to the CPU golden in `tests/*/expected/` and to
Metal. Stage A: `xprop_cosim` (batched, no peripherals). Stage B: `dual_uart` +
`apb_trace`. Stage C: flash/JTAG fixtures. Model-driven-clock designs (JTAG) run
GPU peripherals at `batch=1` within the same backend — not a separate path.

---

## Peripheral kernels (Stages B/C)

Port the three GPU peripheral kernels so peripherals run *inside* the batch
(eliminating any per-edge round-trip) — the same model as Metal.

### peripheral kernels (shared impl header + two launchers each)

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

As each peripheral kernel lands, wire it into `CudaBackend::run_edges` (Stage B
= `gpu_io_step`; Stage C = the flash kernels) and switch the corresponding
`drain_*`/`flash_d_i` accessors to read the **GPU** ring buffers. Define the
`GpuPeripheral` seam (Decision 0017 Layer 3) here so Phase 3 (Tier-3 single-source)
can slot in later. **`CpuBackend` (Tier-1) is the per-kernel equivalence
oracle** — equivalence-test each GPU kernel against its CPU model.

**Verification:** `cosim --features cuda` on each fixture (Stage B/C) runs
**batched** (telemetry `batch > 1`), output VCD byte-identical to the CPU/Metal
goldens on `tesla4-runner`. Model-driven-clock designs (JTAG) match at `batch=1`.

---

## CI: the cross-backend cosim equivalence gate

`compare_backend_vcds.py` is N-way and backend-agnostic (no change needed).
Add to the `cuda` and `hip-on-nvidia` jobs a `jacquard cosim` step per fixture,
upload the VCDs, and extend `backend-equivalence` with cosim comparisons
(cuda vs hip vs metal vs the committed CPU golden). This closes the current gap:
cosim is CI-covered on CPU only; sim equivalence covers GPU only.

---

## Sequencing & checkpoints (each = one reviewable PR-sized push, CI-gated)

| # | Stage | Deliverable | CI gate |
|---|-------|-------------|---------|
| T0 | #104 | CUDA/HIP sim timing wired (Rust-only) | ✅ timing report == Metal on T4 |
| T1.1 | — | `cosim_state_prep` + `cosim_simulate_stage` kernels + launchers compile | ✅ `cuda`/`hip` build green |
| T1.2 | A | `CudaBackend`/`HipBackend` (managed mem, batched, design-step only) + cmd_cosim dispatch + cosim CI | ✅ `xprop_cosim` cosim VCD == CPU/Metal golden |
| T2.1 | B | `gpu_io_step` ported + wired; GPU UART/bus drains | ✅ `dual_uart` + `apb_trace` == golden |
| T2.2 | C | `gpu_apply_flash_din` + `gpu_flash_model_step` ported + wired; GPU flash drain | ✅ `mcu_soc` flash cosim == golden (T4) |
| T2.3 | — | `GpuPeripheral` seam + cross-backend cosim equivalence gate | ⬜ optional/not started — backend-equivalence (cosim) green |

**T0–T2.2 all ✅ merged in PR #120 (2026-06-21).** Only T2.3 remains (optional;
the flash gate already achieves cross-backend equivalence transitively by diffing
each backend against the same committed golden). Performance tuning of the v1
managed-memory backend is tracked separately in **issue #122**.

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
