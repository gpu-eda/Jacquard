// SPDX-FileCopyrightText: Copyright (c) 2024 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0
//! HIP GPU backend for co-simulation (Stage B/C — design step + GPU UART/bus +
//! SPI flash peripherals): `HipBackend` + the `run_cosim_hip` shim. Gated
//! `#[cfg(feature = "hip")]` via the `mod hip;` declaration in the parent.
//!
//! `HipBackend` mirrors `MetalBackend`/`CudaBackend`:
//!   1. State/SRAM/IO storage is device-resident `UVec` instead of `metal::Buffer`.
//!   2. `run_edges` runs a *batched* launch — all edges' kernels enqueued on the
//!      default stream with a single end-of-batch `synchronize()` (mirrors
//!      Metal's `encode_and_commit_gpu_batch`), not a per-edge sync. Per edge:
//!      `cosim_state_prep` → `gpu_apply_flash_din` →
//!      `cosim_simulate_stage × num_major_stages` → `gpu_flash_model_step` →
//!      `gpu_io_step` (UART + bus capture) → `cosim_snapshot` (VCD ring).
//!   3. UART/bus peripherals run on the GPU (`gpu_io_step`); the SPI flash FSM
//!      runs on the GPU (`gpu_apply_flash_din` + `gpu_flash_model_step`, Stage C),
//!      with the persistent `FlashState` device-resident. `drain_uart_tx` /
//!      `drain_bus_beats` read the device ring buffers; `flash_d_i` /
//!      `flash_debug_snapshot` read the device `FlashState`.
//!
//! Batching note: the per-edge ops buffers are pre-uploaded once and cached
//! (`ops_uvecs`), re-uploaded only when `edge_ops_mut` marks them dirty (the
//! reset stimulus, once). This avoids a blocking host→device copy per edge —
//! ulib's HIP H2D path (`hipMemcpy`) is synchronous, so re-uploading fresh ops
//! each edge would serialise the batch. Mirrors Metal's build-schedule-once
//! `ScheduleBuffers`.
//!
//! The backend keeps the `UnsafeCell` interior-mutability pattern from
//! CpuBackend so `run_edges(&self)` can mutate its device buffers; cosim
//! dispatch is strictly sequential, so no concurrent borrow ever exists.
//!
//! IO `#[repr(C)]` structs cross the FFI as untyped `UVec<u8>` byte buffers
//! (the event-buffer pattern): ulib `UVec<T>` requires `T: UniversalCopy`, which
//! the IO structs are not, so the launchers take `u8*` and cast.

use std::cell::UnsafeCell;

use ulib::{AsUPtrMut, Device, NullUPtr, UVec};

use crate::aig::AIG;
use crate::flatten::FlattenedScriptV1;
use crate::sim::setup::LoadedDesign;
use crate::testbench::TestbenchConfig;
use netlistdb::NetlistDB;

use super::{
    build_bus_trace_params, build_flash_buffers_dev, build_wb_trace_params, channel_buf,
    host_bytes_to_dev, rd_u32, read_flash_debug, run_cosim_generic, slice_to_dev, struct_to_dev,
    BitOp, BusTraceEntry, BusTraceLane, CosimBackend, CosimOpts, CosimResult, FlashDebug,
    GpioMapping, UartChannel, UartDecoderState, UartParams, UartPerChannelConfig, WbTraceEntry,
    BUS_TRACE_CHANNEL_CAP, FLASH_STATE_D_I_OFF, FLASH_STATE_IN_RESET_OFF, MAX_UARTS,
    UART_CHANNEL_CAP, WB_TRACE_CHANNEL_CAP,
};

// ucc-generated universal bindings for the launchers in `csrc/kernel_v1.hip.cpp`
// (`*_hip` suffix stripped; a trailing `device: ulib::Device` arg appended).
// Same include pattern as `sim_hip` in `src/bin/jacquard.rs`.
mod ucci_hip {
    include!(concat!(env!("OUT_DIR"), "/uccbind/kernel_v1_hip.rs"));
}

/// The HIP device this backend runs on. Single-GPU like `sim_hip`.
const DEVICE: Device = Device::HIP(0);

/// HIP cosim backend — design step + GPU UART/bus peripherals (Stage B).
struct HipBackend {
    /// Full design state `[input_state | output_state]`, `2 × state_size`
    /// words, device-resident. `state_prep` reads the output slot and writes
    /// the input slot; `simulate_stage` reads the input slot and writes the
    /// output slot — both operate on this single UVec across launches.
    state: UnsafeCell<UVec<u32>>,
    /// SRAM backing store, sized `max(1)` like Metal/CpuBackend; `sram()`
    /// returns the `[..sram_len]` view. Device-resident.
    sram: UnsafeCell<UVec<u32>>,
    /// SRAM X-mask shadow (xprop); sized like `sram` when xprop is on, else 1.
    sram_xmask: UnsafeCell<UVec<u32>>,
    /// Per-edge schedule (host source of truth): one `Vec<BitOp>` per edge.
    schedule: Vec<Vec<BitOp>>,
    /// Cached device ops buffers, one per scheduler edge — pre-uploaded so the
    /// batch runs without a per-edge blocking H2D. Rebuilt lazily in `run_edges`
    /// when the matching `ops_dirty` flag is set (by `edge_ops_mut`).
    ops_uvecs: UnsafeCell<Vec<UVec<u32>>>,
    /// Per-edge dirty flags; `edge_ops_mut` sets one, `run_edges` clears it
    /// after re-uploading. All true after `init_schedule` (lazy first upload).
    ops_dirty: UnsafeCell<Vec<bool>>,
    edges_per_period: usize,
    gcd_ps: u64,
    /// Words per state slot (`effective_state_size()`).
    state_size: usize,
    /// True SRAM word count (`sram_storage_size`); `sram()` view length.
    sram_len: usize,
    /// Device-resident block program (copied from `script`).
    blocks_start: UVec<usize>,
    blocks_data: UVec<u32>,
    num_blocks: usize,
    num_major_stages: usize,
    /// X-mask word offset within a slot (`reg_io_state_size` when xprop on,
    /// else 0). Mirrors the GPU `state_prep` `xmask_state_offset` sentinel.
    xmask_state_offset: usize,
    // ── GPU SPI-flash buffers (Stage C), device-resident `UVec<u8>` ──────────
    /// Persistent `FlashState` (decode FSM state across edges). `in_reset` is
    /// updated host-side by `flash_set_in_reset`; `d_i` is read by `flash_d_i`.
    flash_state: UnsafeCell<UVec<u8>>,
    /// `FlashDinParams` (const after `new`): MISO input positions + xmask offset.
    flash_din_params: UVec<u8>,
    /// `FlashModelParams` (const after `new`): clk/csn/d_out output positions.
    flash_model_params: UVec<u8>,
    /// 16 MiB firmware image (const after `new`); read by `gpu_flash_model_step`.
    flash_data: UVec<u8>,
    // ── GPU peripheral IO buffers (Stage B), device-resident `UVec<u8>` ──────
    /// `UartDecoderState[MAX_UARTS]`, persistent across edges.
    uart_state: UnsafeCell<UVec<u8>>,
    /// `UartParams` (const after `new`).
    uart_params: UVec<u8>,
    /// `UartChannel[MAX_UARTS]` byte ring buffers (GPU writes, host drains).
    uart_channel: UnsafeCell<UVec<u8>>,
    /// `WbTraceParams` (const after `new`).
    wb_params: UVec<u8>,
    /// `WbTraceChannel` header + entries (legacy Wishbone; no Stage-B fixture).
    wb_channel: UnsafeCell<UVec<u8>>,
    /// `BusTraceParamsAll` (const after `new`).
    bus_params: UVec<u8>,
    /// `BusTraceChannel` header + entries (AHB/APB, GPU writes, host drains).
    bus_channel: UnsafeCell<UVec<u8>>,
    /// Number of active UART channels (`uart_configs.len()`).
    n_uarts: usize,
    /// Per-UART ring read cursors; `drain_uart_tx` advances each to `write_head`.
    uart_read_heads: Vec<u32>,
    /// AHB/APB bus-trace ring read cursor.
    bus_trace_read_head: u32,
    /// Device-resident VCD snapshot ring (`batch × 2×state_size` u32s); each
    /// edge's `[input|output]` slot is copied here by `cosim_snapshot`. Grown
    /// lazily to the largest batch seen.
    vcd_ring_dev: UnsafeCell<UVec<u32>>,
    /// Host-side per-edge `[input | output]` snapshots for the most recent
    /// `run_edges`, read back from `vcd_ring_dev` after the batch sync. `None`
    /// until `enable_vcd_ring`.
    vcd_ring: UnsafeCell<Option<Vec<Vec<u32>>>>,
    enable_vcd: bool,
}

impl HipBackend {
    #[inline]
    #[allow(clippy::mut_from_ref)]
    fn state_inner_mut(&self) -> &mut UVec<u32> {
        // SAFETY: cosim dispatch is sequential; no other live borrow exists.
        unsafe { &mut *self.state.get() }
    }
}

impl CosimBackend for HipBackend {
    fn new(
        aig: &AIG,
        netlistdb: &NetlistDB,
        script: &FlattenedScriptV1,
        config: &TestbenchConfig,
        gpio_map: &GpioMapping,
        uart_configs: &[(String, usize, usize, u32)],
        sched_ticks_per_sys_clk_cycle: u64,
        state_size: usize,
        num_blocks: usize,
        num_major_stages: usize,
        _arrival_state_offset: u32,
        _timing_constraints: &Option<Vec<u32>>,
    ) -> (Self, Vec<BusTraceLane>) {
        // Stage B scope guards (mirror CpuBackend): timed cosim rides the GPU
        // arrival ring (not wired here), and the shared `simulate_block_v1`
        // path the cosim stage kernel calls would read SRAM as always-known
        // under xprop without a real SRAM X-mask exercise.
        assert!(
            !script.timing_arrivals_enabled,
            "HipBackend: timed cosim not supported — arrival readback rides the \
             GPU ring; use the Metal backend for --timing-vcd cosim."
        );
        assert!(
            !(script.xprop_enabled && script.sram_storage_size > 0),
            "HipBackend: --xprop with SRAM not supported — the cosim stage \
             kernel has no SRAM X-mask exercise, so SRAM cells would read as \
             always-known; use the Metal backend."
        );

        // Design state: [input | output], zeroed, with the xprop X-mask seed
        // mirroring CpuBackend::new / build_state_buffers (both slots seeded so
        // edge-0 state_prep's output→input copy doesn't wipe the seed).
        let mut state = vec![0u32; 2 * state_size];
        if script.xprop_enabled {
            let rio = script.reg_io_state_size as usize;
            let xmask = crate::sim::vcd_io::xprop_xmask_template_cosim(script);
            state[rio..2 * rio].copy_from_slice(&xmask);
            state[state_size + rio..state_size + 2 * rio].copy_from_slice(&xmask);
            clilog::info!(
                "cosim X-propagation enabled (HipBackend): {} reg/io words, \
                 X-mask seeded (both slots) for uninitialised state",
                rio
            );
        }
        let mut state_uvec: UVec<u32> = state.into();
        // Force the device copy to materialise up front (mirrors `sim_hip`'s
        // `input_states_uvec.as_mut_uptr(device)`); all launches share it.
        state_uvec.as_mut_uptr(DEVICE);

        // SRAM backing store (sized max(1) like Metal; sliced to the real len).
        let sram_alloc_len = (script.sram_storage_size as usize).max(1);
        let mut sram = vec![0u32; sram_alloc_len];

        // SRAM X-mask shadow (all-X when xprop, sized 1 dummy otherwise). The
        // (xprop && sram>0) assert above means xprop with real SRAM never
        // reaches here; this stays for layout parity with `sim_hip`.
        let sram_xmask_len = if script.xprop_enabled {
            (script.sram_storage_size as usize).max(1)
        } else {
            1
        };
        let mut sram_xmask =
            vec![if script.xprop_enabled { 0xFFFF_FFFFu32 } else { 0u32 }; sram_xmask_len];

        // SRAM preload (issue #80) — same path as CpuBackend::new. Logic
        // fixtures declare no `sram_init`, so this is untaken; wired for
        // correctness.
        if let Some(sram_init) = config.sram_init.as_ref() {
            let elf_path = std::path::Path::new(&sram_init.elf_path);
            let chunks = crate::sim::sram_preload::parse_elf_chunks(elf_path)
                .unwrap_or_else(|e| panic!("sram_init: {e}"));
            let (cellid, storage_offset) =
                crate::sim::sram_preload::resolve_single_sram(&script.sram_cell_storage_offsets)
                    .unwrap_or_else(|e| panic!("sram_init: {e}"));
            let total_bytes: usize = chunks.iter().map(|c| c.bytes.len()).sum();
            let mut xmask_slice = if script.xprop_enabled {
                Some(sram_xmask.as_mut_slice())
            } else {
                None
            };
            crate::sim::sram_preload::apply_chunks(
                &mut sram,
                storage_offset,
                &chunks,
                xmask_slice.as_deref_mut(),
            )
            .unwrap_or_else(|e| panic!("sram_init: {e}"));
            clilog::info!(
                "SRAM preload (HipBackend): {} bytes from {} → cell {} at storage offset {}",
                total_bytes,
                elf_path.display(),
                cellid,
                storage_offset
            );
        }

        let mut sram_uvec: UVec<u32> = sram.into();
        sram_uvec.as_mut_uptr(DEVICE);
        let mut sram_xmask_uvec: UVec<u32> = sram_xmask.into();
        sram_xmask_uvec.as_mut_uptr(DEVICE);

        // Device-resident block program (copied from `script` so the backend
        // owns its storage). Materialise the device copies up front.
        let mut blocks_start: UVec<usize> =
            script.blocks_start.iter().copied().collect::<Vec<_>>().into();
        blocks_start.as_mut_uptr(DEVICE);
        let mut blocks_data: UVec<u32> =
            script.blocks_data.iter().copied().collect::<Vec<_>>().into();
        blocks_data.as_mut_uptr(DEVICE);

        // ── GPU peripheral IO buffers (mirror MetalBackend::build_io_buffers) ──
        let n_uarts = uart_configs.len();

        // UartDecoderState[MAX_UARTS] — IDLE, TX line idle high (last_tx = 1).
        let uart_decoder_states: Vec<UartDecoderState> = (0..MAX_UARTS)
            .map(|_| UartDecoderState {
                state: 0,
                last_tx: 1,
                start_cycle: 0,
                bits_received: 0,
                value: 0,
                current_cycle: 0,
            })
            .collect();
        let uart_state = slice_to_dev(&uart_decoder_states, DEVICE);

        // UartParams (const). cycles_per_bit counts scheduler edges (not clock
        // cycles), mirroring build_io_buffers.
        let mut up = UartParams {
            state_size: state_size as u32,
            n_uarts: n_uarts as u32,
            _pad: [0; 2],
            channels: [UartPerChannelConfig::default(); MAX_UARTS],
        };
        for (i, (_, tx_gpio, _, cpb)) in uart_configs.iter().enumerate() {
            up.channels[i].tx_out_pos = gpio_map.output_bits.get(tx_gpio).copied().unwrap_or(0);
            up.channels[i].cycles_per_bit = cpb * sched_ticks_per_sys_clk_cycle as u32;
        }
        let uart_params = struct_to_dev(&up, DEVICE);

        // UartChannel[MAX_UARTS] — zeroed header with capacity set per channel.
        let uart_channel = host_bytes_to_dev(
            channel_buf(std::mem::size_of::<UartChannel>(), MAX_UARTS, UART_CHANNEL_CAP as u32),
            DEVICE,
        );

        // WbTraceParams (const) + WbTraceChannel (header + entries). No Stage-B
        // fixture exercises WB, but the kernel binds the buffers unconditionally.
        let wb = build_wb_trace_params(aig, netlistdb, script);
        let wb_params = struct_to_dev(&wb, DEVICE);
        let wb_channel = host_bytes_to_dev(
            channel_buf(
                16 + WB_TRACE_CHANNEL_CAP * std::mem::size_of::<WbTraceEntry>(),
                1,
                WB_TRACE_CHANNEL_CAP as u32,
            ),
            DEVICE,
        );

        // BusTraceParamsAll (const) + BusTraceChannel + CPU decoder lanes.
        let (bus_all, bus_lanes) =
            build_bus_trace_params(aig, netlistdb, script, config.effective_bus_traces());
        let bus_params = struct_to_dev(&bus_all, DEVICE);
        let bus_channel = host_bytes_to_dev(
            channel_buf(
                16 + BUS_TRACE_CHANNEL_CAP * std::mem::size_of::<BusTraceEntry>(),
                1,
                BUS_TRACE_CHANNEL_CAP as u32,
            ),
            DEVICE,
        );

        // ── GPU SPI-flash buffers (Stage C; mirror build_flash_buffers) ────────
        // FlashState (persistent) + const FlashDinParams/FlashModelParams + the
        // 16 MiB firmware, loaded from config.flash. No-flash configs get
        // has_flash=0 (gpu_apply_flash_din early-returns), so this is unconditional.
        let (flash_state, flash_din_params, flash_model_params, flash_data) =
            build_flash_buffers_dev(config, script, state_size, gpio_map, DEVICE);

        let backend = HipBackend {
            state: UnsafeCell::new(state_uvec),
            sram: UnsafeCell::new(sram_uvec),
            sram_xmask: UnsafeCell::new(sram_xmask_uvec),
            schedule: Vec::new(),
            ops_uvecs: UnsafeCell::new(Vec::new()),
            ops_dirty: UnsafeCell::new(Vec::new()),
            edges_per_period: 0,
            gcd_ps: 0,
            state_size,
            sram_len: script.sram_storage_size as usize,
            blocks_start,
            blocks_data,
            num_blocks,
            num_major_stages,
            xmask_state_offset: 0,
            flash_state: UnsafeCell::new(flash_state),
            flash_din_params,
            flash_model_params,
            flash_data,
            uart_state: UnsafeCell::new(uart_state),
            uart_params,
            uart_channel: UnsafeCell::new(uart_channel),
            wb_params,
            wb_channel: UnsafeCell::new(wb_channel),
            bus_params,
            bus_channel: UnsafeCell::new(bus_channel),
            n_uarts,
            uart_read_heads: vec![0u32; n_uarts],
            bus_trace_read_head: 0,
            vcd_ring_dev: UnsafeCell::new(UVec::default()),
            vcd_ring: UnsafeCell::new(None),
            enable_vcd: false,
        };
        (backend, bus_lanes)
    }

    fn init_schedule(
        &mut self,
        per_edge_ops: Vec<Vec<BitOp>>,
        _state_size: u32,
        xmask_state_offset: u32,
        gcd_ps: u64,
    ) {
        let n = per_edge_ops.len();
        self.edges_per_period = n;
        self.schedule = per_edge_ops;
        self.gcd_ps = gcd_ps;
        self.xmask_state_offset = xmask_state_offset as usize;
        // Pre-size the device ops cache; all dirty so the first run_edges
        // uploads each edge's (post-reset-patch) ops exactly once.
        *self.ops_uvecs.get_mut() = (0..n).map(|_| UVec::default()).collect();
        *self.ops_dirty.get_mut() = vec![true; n];
    }

    #[inline]
    fn edge_ops_mut(&mut self, edge_idx: usize) -> &mut [BitOp] {
        // Mutating the host ops invalidates the cached device copy.
        self.ops_dirty.get_mut()[edge_idx] = true;
        &mut self.schedule[edge_idx]
    }
    #[inline]
    fn edge_ops(&self, edge_idx: usize) -> &[BitOp] {
        &self.schedule[edge_idx]
    }
    #[inline]
    fn edges_per_period(&self) -> usize {
        self.edges_per_period
    }
    #[inline]
    fn gcd_ps(&self) -> u64 {
        self.gcd_ps
    }

    fn state(&self) -> &[u32] {
        // SAFETY: sequential dispatch. After `run_edges` the device buffer was
        // synchronized; indexing a UVec returns the host-visible slice (D2H).
        let state: &UVec<u32> = unsafe { &*self.state.get() };
        &state[..]
    }
    fn state_mut(&mut self) -> &mut [u32] {
        // Host-side stimulus deposits happen before any launch; the mutated
        // host copy uploads on the next `as_mut_uptr(DEVICE)` in `run_edges`.
        &mut self.state.get_mut()[..]
    }
    fn sram(&self) -> &[u32] {
        // SAFETY: sequential dispatch — no concurrent borrow.
        let sram: &UVec<u32> = unsafe { &*self.sram.get() };
        &sram[..self.sram_len]
    }

    fn flash_set_in_reset(&mut self, in_reset: bool) {
        // Write FlashState.in_reset host-side; the mutated host copy uploads on
        // the next `as_mut_uptr(DEVICE)` in `run_edges`.
        let fs = self.flash_state.get_mut();
        let v = u32::from(in_reset);
        fs[FLASH_STATE_IN_RESET_OFF..FLASH_STATE_IN_RESET_OFF + 4]
            .copy_from_slice(&v.to_ne_bytes());
    }

    fn enable_vcd_ring(&mut self) {
        self.enable_vcd = true;
        *self.vcd_ring.get_mut() = Some(Vec::new());
    }

    fn vcd_snapshot(&self, edge_in_batch: usize) -> &[u32] {
        // SAFETY: sequential dispatch; the ring is populated by run_edges.
        let ring = unsafe { &*self.vcd_ring.get() };
        &ring
            .as_ref()
            .expect("vcd_snapshot called without enable_vcd_ring")[edge_in_batch]
    }

    fn flash_d_i(&self) -> u8 {
        // Read FlashState.d_i (u8 @ offset 28) from the device buffer. Indexing
        // forces the post-batch device→host copy (sequential dispatch → safe).
        // SAFETY: cosim dispatch is sequential; no concurrent borrow.
        let fs = unsafe { &*self.flash_state.get() };
        fs[FLASH_STATE_D_I_OFF]
    }

    fn flash_debug_snapshot(&self) -> FlashDebug {
        // Decode the real FlashState fields from the device buffer (Stage C).
        // SAFETY: sequential dispatch; the host copy reflects the last sync.
        let fs = unsafe { &*self.flash_state.get() };
        read_flash_debug(&fs[..])
    }

    fn drain_uart_tx(&mut self) -> Vec<(usize, u8)> {
        let mut out = Vec::new();
        let ch = self.uart_channel.get_mut();
        // Indexing forces the device→host copy of the post-batch ring.
        let bytes = &ch[..];
        let sz = std::mem::size_of::<UartChannel>();
        for i in 0..self.n_uarts {
            let base = i * sz;
            let write_head = rd_u32(bytes, base);
            let cap = rd_u32(bytes, base + 4);
            while self.uart_read_heads[i] < write_head {
                // data[] starts at byte offset 16 within each UartChannel.
                let data_off = base + 16 + (self.uart_read_heads[i] % cap) as usize;
                out.push((i, bytes[data_off]));
                self.uart_read_heads[i] += 1;
            }
        }
        out
    }

    fn drain_bus_beats(&mut self) -> Vec<crate::sim::models::bus_trace::RawBeat> {
        use crate::sim::models::bus_trace::RawBeat;
        let mut out = Vec::new();
        let ch = self.bus_channel.get_mut();
        let bytes = &ch[..];
        // BusTraceChannel header: write_head(0), capacity(4); entries at 16.
        let write_head = rd_u32(bytes, 0);
        let cap = rd_u32(bytes, 4);
        let esz = std::mem::size_of::<BusTraceEntry>();
        while self.bus_trace_read_head < write_head {
            let idx = (self.bus_trace_read_head % cap) as usize;
            let eoff = 16 + idx * esz;
            // BusTraceEntry: tick(0), flags(4), addr(8), wdata(12), rdata(16).
            let tick = rd_u32(bytes, eoff);
            let flags = rd_u32(bytes, eoff + 4);
            let addr = rd_u32(bytes, eoff + 8);
            let wdata = rd_u32(bytes, eoff + 12);
            let rdata = rd_u32(bytes, eoff + 16);
            let bus_id = (flags >> 8) & 0xFF;
            out.push(RawBeat {
                tick: tick as u64,
                bus_id,
                write: (flags & 1) != 0,
                err: (flags >> 1) & 1 != 0,
                addr: addr as u64,
                wdata: wdata as u64,
                rdata: rdata as u64,
            });
            self.bus_trace_read_head += 1;
        }
        out
    }

    fn run_edges(&self, batch: usize, schedule_offset: usize) -> u64 {
        let state_size = self.state_size;
        let two_slot = 2 * state_size;
        let state = self.state_inner_mut();
        // SAFETY: sequential dispatch — these buffers are only touched here and
        // in &mut-self methods, never concurrently.
        let sram = unsafe { &mut *self.sram.get() };
        let sram_xmask = unsafe { &mut *self.sram_xmask.get() };
        let ring_host = unsafe { &mut *self.vcd_ring.get() };
        let ops_uvecs = unsafe { &mut *self.ops_uvecs.get() };
        let ops_dirty = unsafe { &mut *self.ops_dirty.get() };
        let uart_state = unsafe { &mut *self.uart_state.get() };
        let uart_channel = unsafe { &mut *self.uart_channel.get() };
        let wb_channel = unsafe { &mut *self.wb_channel.get() };
        let bus_channel = unsafe { &mut *self.bus_channel.get() };
        let flash_state = unsafe { &mut *self.flash_state.get() };
        let vcd_dev = unsafe { &mut *self.vcd_ring_dev.get() };

        let xmask_off = self.xmask_state_offset as u32;

        // Grow the VCD device ring to hold this batch's per-edge snapshots.
        if self.enable_vcd && vcd_dev.len() < batch * two_slot {
            *vcd_dev = UVec::<u32>::new_zeroed(batch * two_slot, DEVICE);
        }

        for e in 0..batch {
            let sched_idx = (schedule_offset + e) % self.edges_per_period;

            // (Re)upload this edge's ops to the device only if dirty — a
            // one-time cost after the reset patch, so the batch stays async.
            if ops_dirty[sched_idx] {
                let ops = &self.schedule[sched_idx];
                // SAFETY: BitOp is #[repr(C)] of two u32 fields → 2*len u32s.
                let flat: Vec<u32> = unsafe {
                    std::slice::from_raw_parts(ops.as_ptr() as *const u32, ops.len() * 2)
                }
                .to_vec();
                let mut u: UVec<u32> = flat.into();
                u.as_mut_uptr(DEVICE);
                ops_uvecs[sched_idx] = u;
                ops_dirty[sched_idx] = false;
            }
            let num_ops = self.schedule[sched_idx].len() as u32;

            // ── GPU state_prep ─────────────────────────────────────────────
            ucci_hip::cosim_state_prep(
                &mut *state,
                state_size as u32,
                num_ops,
                xmask_off,
                &mut ops_uvecs[sched_idx],
                DEVICE,
            );

            // ── Inject flash MISO (FlashState.d_i → input state) before sim ──
            // Mirrors the Metal order: state_prep → apply_flash_din → simulate.
            ucci_hip::gpu_apply_flash_din(
                &mut *state,
                &*flash_state,
                &self.flash_din_params,
                DEVICE,
            );

            // ── Simulate every major stage (timing OFF in cosim) ────────────
            for stage in 0..self.num_major_stages {
                // SAFETY: NullUPtr yields a null device pointer; the kernel
                // treats a null `timing_constraints` as "timing off".
                let tc_null = unsafe { NullUPtr::new_ref() };
                let ev_null = unsafe { NullUPtr::new_mut() };
                ucci_hip::cosim_simulate_stage(
                    self.num_blocks,
                    &self.blocks_start,
                    &self.blocks_data,
                    &mut *sram,
                    &mut *sram_xmask,
                    state_size,
                    &mut *state,
                    stage,
                    tc_null,
                    ev_null,
                    0,
                    DEVICE,
                );
            }

            // ── GPU flash model step (dual-step SPI FSM; sees SPI CLK after
            // this edge). Mirrors the Metal order: simulate → flash_model_step.
            ucci_hip::gpu_flash_model_step(
                &mut *state,
                &mut *flash_state,
                &self.flash_model_params,
                &self.flash_data,
                DEVICE,
            );

            // ── GPU peripherals: UART + bus capture for this edge ───────────
            ucci_hip::gpu_io_step(
                &mut *state,
                &mut *uart_state,
                &self.uart_params,
                &mut *uart_channel,
                &mut *wb_channel,
                &self.wb_params,
                &mut *bus_channel,
                &self.bus_params,
                DEVICE,
            );

            // ── VCD snapshot: copy this edge's [input|output] slot into the
            // device ring (so it survives the next edge overwriting `state`).
            if self.enable_vcd {
                ucci_hip::cosim_snapshot(&*state, &mut *vcd_dev, two_slot, e, DEVICE);
            }
        }

        // Single end-of-batch barrier (mirrors Metal's one signal/commit).
        DEVICE.synchronize();

        // Read the device ring back once (D2H) and slice into per-edge host
        // snapshots for `vcd_snapshot`.
        if self.enable_vcd {
            if let Some(r) = ring_host.as_mut() {
                r.clear();
                let host = &vcd_dev[..batch * two_slot];
                for e in 0..batch {
                    r.push(host[e * two_slot..(e + 1) * two_slot].to_vec());
                }
            }
        }

        // The device was synchronized inline; `wait` is a no-op, token unused.
        0
    }

    fn wait(&self, _token: u64) {
        // run_edges synchronizes at end-of-batch; nothing left to await.
    }
}

/// Public HIP-backend cosim entry point (Stage B). Drives the same agnostic
/// `run_cosim_generic<B>` orchestration as the Metal/CPU shims, with the HIP
/// GPU backend. Gated `#[cfg(feature = "hip")]`.
pub fn run_cosim_hip(
    design: &mut LoadedDesign,
    config: &TestbenchConfig,
    opts: &CosimOpts,
    timing_constraints: &Option<Vec<u32>>,
) -> CosimResult {
    run_cosim_generic::<HipBackend>(design, config, opts, timing_constraints)
}
