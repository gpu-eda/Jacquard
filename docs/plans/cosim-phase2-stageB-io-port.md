# Stage B (T2.1) port spec — `gpu_io_step` on CUDA/HIP + batched `run_edges`

**Status:** Spec for review — not yet implemented. Sibling detail doc to
[`cosim-phase2-cuda-hip.md`](cosim-phase2-cuda-hip.md) (the authoritative plan;
Stage B = its checkpoint **T2.1**). Resumed from
`docs/handoffs/backend-alignment-handoff.md`.

**Goal:** bring `CudaBackend`/`HipBackend` from design-step-only (Stage A) to
**GPU peripherals (UART + bus) running *inside* a batched launch**, byte-identical
to the CPU/Metal goldens, validated CI-only on `tesla4-runner`. Two decisions
already taken by the maintainer this session:
1. **GPU peripherals**, not a CPU-peripheral crutch (the dropped "2a").
2. **Batch in this stage** — rework `run_edges` off its current per-edge
   `DEVICE.synchronize()` into one async launch sequence + a single end-of-batch
   sync, mirroring Metal's `encode_and_commit_gpu_batch`.

CI gate: `dual_uart` + `apb_trace` cosim VCD/CSV byte-identical to
`tests/{dual_uart,apb_trace}/expected/` on cuda **and** hip.

---

## 1. Reference map (port *from* → *to*)

| Concern | Metal / CPU source (read) | CUDA/HIP target (write) |
|---|---|---|
| Kernel body | `csrc/kernel_v1.metal:1170` `gpu_io_step` | `csrc/kernel_v1_impl.cuh` new `__global__ gpu_io_step` |
| GPU structs | `kernel_v1.metal:1041-1163` | device header in `kernel_v1_impl.cuh` |
| Launcher | (Metal `encode_io_step`) | `kernel_v1.cu` / `.hip.cpp` `gpu_io_step_{cuda,hip}` |
| Rust `#[repr(C)]` ABI | `metal.rs:113-250` | `cuda.rs`/`hip.rs` (or shared in `mod.rs`) |
| IO buffer build | `metal.rs:1446` `build_io_buffers` | `CudaBackend::new` |
| Bus params/positions | `mod.rs:149` `build_bus_trace` (agnostic, already shared) | reuse as-is |
| Batched encode | `metal.rs:643` `encode_and_commit_gpu_batch` | `CudaBackend::run_edges` |
| UART drain | `metal.rs:2116` `drain_uart_tx` | `CudaBackend::drain_uart_tx` |
| Bus drain | `metal.rs:2132` `drain_bus_beats` | `CudaBackend::drain_bus_beats` |
| CPU equivalence oracle | `mod.rs:3805-4045` (`CpuBackend` FSM) | (test target, no change) |

**Stage-B fixtures exercise UART + APB3 only.** The legacy Wishbone (`WbTrace*`)
path in `gpu_io_step` is dead for `dual_uart`/`apb_trace` (`has_trace == 0`,
`n_buses` drives APB). Port the WB block for ABI/structural parity with Metal but
it is not on the Stage-B critical path (no WB fixture until later).

---

## 2. GPU-struct ABI to replicate (device header in `kernel_v1_impl.cuh`)

Exact field order/padding from `kernel_v1.metal:1041-1163`. Constants:
`MAX_UARTS`, `UART_CHANNEL_CAP` (already defined for Metal — confirm the values
and re-declare in the `.cuh`), `WB_TRACE_MAX_ADR_BITS=30`, `…_DAT_BITS=32`,
`WB_TRACE_CHANNEL_CAP=16384`, `MAX_BUS_TRACES=4`, `BUS_TRACE_MAX_ADR_BITS=32`,
`…_DAT_BITS=32`, `BUS_TRACE_CHANNEL_CAP=16384`, `BUS_PROTO_APB3=0`.

Structs (CUDA `struct`, plain — no `device`/`constant` qualifiers):
`UartDecoderState{state,last_tx,start_cycle,bits_received,value,current_cycle}`,
`UartPerChannelConfig{tx_out_pos,cycles_per_bit}`,
`UartParams{state_size,n_uarts,_pad[2],channels[MAX_UARTS]}`,
`UartChannel{write_head,capacity,_pad[2],data[UART_CHANNEL_CAP]}`,
`WbTraceParams`/`WbTraceEntry`/`WbTraceChannel`,
`BusTraceParams{protocol,addr_bits,data_bits,sel_pos,enable_pos,ready_pos,write_pos,resp_pos,addr_pos[32],wdata_pos[32],rdata_pos[32]}`,
`BusTraceParamsAll{n_buses,_pad[3],buses[MAX_BUS_TRACES]}`,
`BusTraceEntry{tick,flags,addr,wdata,rdata}`,
`BusTraceChannel{write_head,capacity,current_tick,prev_gate}` (+ entries at
byte-offset 16).

**ABI guard (required):** `static_assert(sizeof(X) == …)` on both the C side and
`const _: () = assert!(size_of::<X>() == …)` on the Rust side for every struct
that crosses FFI. ABI drift is the #1 risk (silent corruption); the Rust ABI
already exists at `metal.rs:113-250` and is the size-of-truth.

The Rust `#[repr(C)]` mirrors at `metal.rs:113-250` are `#[cfg(feature="metal")]`.
**Action:** lift `UartParams`/`UartPerChannelConfig`/`UartChannel`/
`UartDecoderState`/`BusTraceParamsAll`/`BusTraceParams`/`BusTraceEntry`/
`BusTraceChannel` (+ WB structs) into the **agnostic parent** (`mod.rs`,
non-gated) so all three backends share one ABI definition, then have `metal.rs`,
`cuda.rs`, `hip.rs` `use super::` them. This removes the triple-maintenance the
plan's Risk "two kernel families" warns about. (`build_bus_trace`/
`BusTracePositions` are already agnostic in `mod.rs` — this extends that pattern.)

---

## 3. Kernel: `gpu_io_step` (shared `kernel_v1_impl.cuh`)

Direct transliteration of `kernel_v1.metal:1180-1354`. Metal→CUDA mechanical
substitutions:
- `kernel void` → `__global__ void`; `[[buffer(n)]]` args → plain pointers;
  `tid [[thread_position_in_threadgroup]]` → `threadIdx.x`. Single-thread work:
  keep the `if (tid != 0) return;` guard → `if (threadIdx.x != 0) return;`.
- `device`/`constant` qualifiers dropped. `uchar` → `unsigned char` / `u8`,
  `u32` already aliased in the `.cuh`.
- The `READ_OUT_BIT` macro (`metal.rs`-shader `:1186`) ports verbatim (reads
  `states[state_size + (pos>>5)]` — the **output** slot; `0xFFFFFFFF` ⇒ 0).
- No barriers needed — pure thread-0 serial logic, no `simd_*`/`threadfence`.

The UART FSM (4-state: IDLE/START/DATA/STOP), WB trace, and APB3 rising-edge
gate logic copy line-for-line. **The CPU FSM at `mod.rs:3923-4039` is the
already-verified Rust twin** — cross-check the port against it (same
`cycles_per_bit/2` midpoints, same `value |= tx << bits_received`, same
`gate = sel & en & rdy` + rising-edge `(prev>>b)&1 == 0`).

---

## 4. Launcher: `gpu_io_step_{cuda,hip}` (`kernel_v1.cu` / `.hip.cpp`)

Follow the existing `cosim_state_prep_cuda` (`kernel_v1.cu:84`) pattern exactly —
`extern "C"`, raw pointer args, `<<<1, 256>>>` launch, `checkCudaErrors`. ucc
strips `_cuda`/`_hip` and appends the `Device` arg, surfacing
`ucci::gpu_io_step(...)` automatically (no build.rs change). Signature mirrors
the kernel: `(u32* states, UartDecoderState* uart_state, const UartParams* uart_params, UartChannel* uart_channel, WbTraceChannel* wb_channel, const WbTraceParams* wb_params, BusTraceChannel* bus_channel, const BusTraceParamsAll* bus_params)`.
HIP launcher is the same body in `.hip.cpp` (shared `.cuh` kernel).

**VCD-ring snapshot launcher (needed for §6 batching):** add
`cosim_snapshot_{cuda,hip}` — `cudaMemcpyAsync(ring + edge_off*2*state_size,
states, 2*state_size*4, cudaMemcpyDeviceToDevice, 0)`. This is the CUDA analog of
Metal's per-edge blit (`metal.rs:732-742`); it lets each edge's `[input|output]`
slot be retained in a device ring while `states` is overwritten by the next
edge — without it, batching (no per-edge sync/readback) loses all but the final
snapshot.

---

## 5. Backend wiring — `CudaBackend::new` (IO buffers)

Mirror `build_io_buffers` (`metal.rs:1446`), substituting device-resident `UVec`
for `metal::Buffer`. New backend fields (all `UnsafeCell<UVec<…>>` or `UVec`):
- `uart_state: UVec<UartDecoderState>` — `MAX_UARTS`, init `state=0,last_tx=1`.
- `uart_params: UVec<UartParams>` — `state_size`, `n_uarts`, per-channel
  `tx_out_pos = gpio_map.output_bits[tx_gpio]`,
  `cycles_per_bit = cpb * sched_ticks_per_sys_clk_cycle` (the two args
  **currently prefixed `_`** in `CudaBackend::new` — un-prefix `_gpio_map`,
  `_uart_configs`, `_sched_ticks_per_sys_clk_cycle`).
- `uart_channel: UVec<UartChannel>` — `MAX_UARTS`, `capacity=UART_CHANNEL_CAP`,
  `write_head=0`.
- `wb_params`/`wb_channel` (via `build_wb_trace_params` — also agnostic-lift or
  cfg-gate; low priority, no Stage-B fixture).
- `bus_params: UVec<u8>`-backed `BusTraceParamsAll` + `bus_channel` (header +
  `BUS_TRACE_CHANNEL_CAP` entries, byte-sized buffer). Build params from
  `build_bus_trace(aig, netlistdb, script, config.effective_bus_traces())`
  (`mod.rs:149`) → pack `BusTracePositions` into `BusTraceParams` (the packing
  loop lives in `metal.rs` `build_bus_trace_params:1126` — extract the
  positions→`BusTraceParamsAll` packer into `mod.rs` so cuda/hip/metal share it,
  same move already done for the lanes).
- Per-channel read cursors `uart_read_heads: Vec<u32>`, `bus_trace_read_head: u32`
  (host-side, mirror `MetalBackend`).

`new` returns `(Self, bus_lanes)` — `bus_lanes` now real (from `build_bus_trace`),
replacing the Stage-A empty `vec![]`.

---

## 6. Backend wiring — `run_edges` (the batching rework)

**Current Stage A** (`cuda.rs:342`): per edge → `state_prep`, `simulate_stage ×
N`, **`DEVICE.synchronize()`**, host read-back into VCD ring. The per-edge sync
is what makes it not-yet-batched.

**Target (mirror `encode_and_commit_gpu_batch`):** enqueue *all* edges' kernels
on the default stream with **no intervening sync**, one `DEVICE.synchronize()` at
the very end, then read the ring + drain channels once. Per edge in `0..batch`:
1. upload edge ops UVec (**retain in a `Vec` for the whole batch** — async
   launches read it after the call returns; dropping early = UB),
2. `ucci::cosim_state_prep(...)`,
3. `ucci::cosim_simulate_stage(...)` × `num_major_stages`,
4. `ucci::gpu_io_step(...)` (NEW — UART/bus capture into the device rings),
5. if `enable_vcd`: `ucci::cosim_snapshot(...)` → device ring slot `edge_offset`.

Then **one** `DEVICE.synchronize()`. Then: read the VCD ring UVec back to host →
`Vec<Vec<u32>>` for `vcd_snapshot`; UART/bus channels are managed/`UVec` so
`drain_*` reads them after the sync.

Notes / invariants:
- Flash kernels (`gpu_apply_flash_din`, `gpu_flash_model_step`) are **Stage C** —
  omit from the dispatch chain here (Metal's `encode_*flash*` calls are skipped).
- `wait`/`vcd_snapshot` semantics unchanged from Stage A (token still unused; the
  single end-of-batch sync replaces per-edge).
- Ops-buffer lifetime is the one new correctness hazard vs Stage A (which synced
  immediately so the UVec could drop). Collect `Vec<UVec<u32>>`, drop after sync.
- The VCD ring UVec is sized `batch_capacity * 2 * state_size`; grow lazily to the
  largest `batch` seen (mirror Metal's ring sizing).

---

## 7. Backend wiring — drains

- `drain_uart_tx` (`cuda.rs:332`): replace `vec![]` with the
  `metal.rs:2116` loop over `uart_channel[i].write_head` vs `uart_read_heads[i]`,
  reading `data[head % capacity]`. UVec → ensure host-visible (post-sync read).
- `drain_bus_beats` (`cuda.rs:337`): replace `vec![]` with the `metal.rs:2132`
  loop reading `BusTraceEntry` at byte-offset 16, building `RawBeat` (same flag
  decode: `bus_id = flags>>8`, `write = flags&1`, `err = (flags>>1)&1`).
- `flash_d_i`/`flash_debug_snapshot` stay Stage-A stubs (flash is Stage C).

---

## 8. CI

The check script already supports `dual_uart` + `apb_trace` under
`COSIM_SCOPE=all` (`scripts/ci/cosim_cpu_check.sh:97-110`). Stage A restricted the
cuda/hip steps to `COSIM_SCOPE=logic` (`ci.yml:580`/`:734`). **Stage B change:**
flip those two steps to `COSIM_SCOPE=all` (or a new `logic+io` scope that adds
just UART/bus, deferring flash/JTAG to Stage C). The goldens at
`tests/{dual_uart,apb_trace}/expected/` already exist (Phase-1 CPU goldens, =
Metal). No new fixtures, no `compare_backend_vcds.py` change.

---

## 9. Risks / open questions to resolve during implementation

1. **ulib stream/async semantics.** The whole batching rework assumes the ucci
   launchers enqueue async on the default stream and `DEVICE.synchronize()` is the
   only barrier (true of the current launchers — they `<<<>>>` + `cudaGetLastError`,
   no inner sync). **Verify** ulib `UVec` host→device upload doesn't itself force a
   sync mid-batch (if `&mut ops_uvec` triggers a blocking copy each call, the
   "batch" still serialises on the host — acceptable functionally, but not the
   perf win; flag if so). This is the one assumption that could force a design
   change (e.g. a single batched C launcher that loops internally).
2. **VCD ring memory.** `batch * 2 * state_size * 4 B` device-resident. Large
   designs × large batch could be significant; Metal already pays this. Size to
   the max batch lazily.
3. **ABI drift** — mitigated by §2 static asserts both sides + the byte-identical
   gate.
4. **`UartChannel` is 16 + UART_CHANNEL_CAP bytes** — large struct in a `UVec`;
   confirm `UVec<UartChannel>` device alloc handles the `[u8; CAP]` inline array
   (vs a flat `UVec<u8>` view). A flat byte-buffer + manual offset (like the bus
   channel) may be cleaner than `UVec<UartChannel>`.
5. **CUDA + HIP land together** — shared `.cuh` kernel + ABI; HIP is a second
   launcher in `.hip.cpp` + `mod ucci_hip` in `hip.rs`. Keep `cuda.rs`/`hip.rs`
   diffs identical (they're 452-line twins today).

---

## 10. Checkpoint sequencing (each = one CI round-trip)

| Step | Deliverable | Local gate | T4 gate |
|---|---|---|---|
| B0 | Lift IO structs → `mod.rs` (agnostic) + ABI static-asserts; Metal still builds bit-identical | `cargo test --features metal` (298), fixtures byte-identical | — |
| B1 | `gpu_io_step` + `cosim_snapshot` kernels + `_cuda`/`_hip` launchers | `cargo check` reasoning only | cuda+hip build green |
| B2 | `CudaBackend`/`HipBackend` IO buffers + batched `run_edges` + drains | — | `dual_uart`+`apb_trace` == golden on cuda+hip |
| B3 | CI: flip `COSIM_SCOPE` to include UART/bus on cuda/hip | — | green |

B0 is local-verifiable (Metal) and de-risks the ABI before any T4 round-trip —
do it first. B1+B2 batch into one push (kernel is useless without the backend).
