// SPDX-FileCopyrightText: Copyright (c) 2024 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0
//! HIP GPU backend for co-simulation (Stage A — design step only, no
//! peripherals): `HipBackend` + the `run_cosim_hip` shim. Gated
//! `#[cfg(feature = "hip")]` via the `mod hip;` declaration in the parent.
//!
//! `HipBackend` is `CpuBackend` (see `mod.rs`) with three changes:
//!   1. State/SRAM storage is device-resident `UVec<u32>` instead of `Vec<u32>`.
//!   2. `run_edges` runs the design step on the GPU via the `cosim_state_prep` /
//!      `cosim_simulate_stage` launchers (one launch per stage, host-driven
//!      major-stage loop) instead of `cpu_reference::simulate_block_v1`.
//!   3. Stage A carries NO peripherals: the UART/bus/flash decode methods are
//!      contract stubs (empty drains, idle MISO nibble, zeroed FlashDebug).
//!
//! The backend keeps the `UnsafeCell` interior-mutability pattern from
//! CpuBackend so `run_edges(&self)` can mutate its device buffers; cosim
//! dispatch is strictly sequential, so no concurrent borrow ever exists.
//!
//! Backend-agnostic items (`CosimBackend`, `BitOp`, `FlashDebug`,
//! `BusTraceLane`, `run_cosim_generic`, `build_bus_trace`, the GPIO/flash
//! const-param helpers) are imported from the parent via `super::`.

use std::cell::UnsafeCell;

use ulib::{AsUPtrMut, Device, NullUPtr, UVec};

use crate::aig::AIG;
use crate::flatten::FlattenedScriptV1;
use crate::sim::setup::LoadedDesign;
use crate::testbench::TestbenchConfig;
use netlistdb::NetlistDB;

use super::{
    build_bus_trace, run_cosim_generic, BitOp, BusTraceLane, CosimBackend, CosimOpts, CosimResult,
    FlashDebug, GpioMapping,
};

// ucc-generated universal bindings for the cosim launchers in `csrc/kernel_v1.hip.cpp`
// (`cosim_state_prep_hip` → `cosim_state_prep`, `cosim_simulate_stage_hip` →
// `cosim_simulate_stage`; ucc strips the `_hip` suffix and appends a trailing
// `device: ulib::Device` arg). Same include pattern as `sim_hip` in
// `src/bin/jacquard.rs`.
mod ucci_hip {
    include!(concat!(env!("OUT_DIR"), "/uccbind/kernel_v1_hip.rs"));
}

/// The HIP device this backend runs on. Single-GPU like `sim_hip`.
const DEVICE: Device = Device::HIP(0);

/// HIP cosim backend — design step on the GPU, no peripherals (Stage A).
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
    /// Device-resident. (Stage A asserts `!(xprop && sram>0)`, so a real SRAM
    /// X-mask is never exercised; carried for layout parity with `sim_hip`.)
    sram_xmask: UnsafeCell<UVec<u32>>,
    /// Per-edge schedule: one `Vec<BitOp>` per scheduler edge (host-resident;
    /// each edge's ops are uploaded as a fresh UVec at launch time).
    schedule: Vec<Vec<BitOp>>,
    edges_per_period: usize,
    gcd_ps: u64,
    /// Words per state slot (`effective_state_size()`).
    state_size: usize,
    /// True SRAM word count (`sram_storage_size`); `sram()` view length.
    sram_len: usize,
    /// Device-resident block program (copied from `script`), passed to every
    /// `simulate_stage` launch. `blocks_start` indexes per `(stage, block)`.
    blocks_start: UVec<usize>,
    blocks_data: UVec<u32>,
    num_blocks: usize,
    num_major_stages: usize,
    /// X-mask word offset within a slot (`reg_io_state_size` when xprop on,
    /// else 0). Mirrors the GPU `state_prep` `xmask_state_offset` sentinel.
    xmask_state_offset: usize,
    /// Flash reset line (logic fixtures have no flash; carried for the
    /// contract, no effect in Stage A).
    flash_in_reset: bool,
    /// Per-edge `[input | output]` host snapshots for the most recent
    /// `run_edges`, one slot per edge in the batch (N=1 per edge). `None`
    /// until enabled. Read back from the device after each edge's synchronize.
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
        _gpio_map: &GpioMapping,
        _uart_configs: &[(String, usize, usize, u32)],
        _sched_ticks_per_sys_clk_cycle: u64,
        state_size: usize,
        num_blocks: usize,
        num_major_stages: usize,
        _arrival_state_offset: u32,
        _timing_constraints: &Option<Vec<u32>>,
    ) -> (Self, Vec<BusTraceLane>) {
        // Stage A scope guards (mirror CpuBackend): timed cosim rides the GPU
        // arrival ring (not wired here), and the shared `simulate_block_v1`
        // path the cosim stage kernel calls would read SRAM as always-known
        // under xprop without a real SRAM X-mask exercise.
        assert!(
            !script.timing_arrivals_enabled,
            "HipBackend: timed cosim not supported (Stage A) — arrival readback \
             rides the GPU ring; use the Metal backend for --timing-vcd cosim."
        );
        assert!(
            !(script.xprop_enabled && script.sram_storage_size > 0),
            "HipBackend: --xprop with SRAM not supported (Stage A) — the cosim \
             stage kernel has no SRAM X-mask exercise, so SRAM cells would read \
             as always-known; use the Metal backend."
        );

        // Design state: [input | output], zeroed, with the xprop X-mask seed
        // mirroring CpuBackend::new / build_state_buffers (both slots seeded so
        // edge-0 state_prep's output→input copy doesn't wipe the seed). Built
        // on the host, then uploaded as a device-resident UVec.
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
        // `input_states_uvec.as_mut_uptr(device)`); all subsequent launches
        // share this device buffer.
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
        let mut blocks_start: UVec<usize> = script.blocks_start.iter().copied().collect::<Vec<_>>().into();
        blocks_start.as_mut_uptr(DEVICE);
        let mut blocks_data: UVec<u32> = script.blocks_data.iter().copied().collect::<Vec<_>>().into();
        blocks_data.as_mut_uptr(DEVICE);

        // Stage A has no peripherals: still resolve bus-trace lanes so the
        // orchestration's CSV path stays wired (the GPU decode is a no-op here;
        // `drain_bus_beats` returns empty). The CPU lanes are owned by Layer 1.
        let (_bus_positions, bus_lanes) =
            build_bus_trace(aig, netlistdb, script, config.effective_bus_traces());

        let backend = HipBackend {
            state: UnsafeCell::new(state_uvec),
            sram: UnsafeCell::new(sram_uvec),
            sram_xmask: UnsafeCell::new(sram_xmask_uvec),
            schedule: Vec::new(),
            edges_per_period: 0,
            gcd_ps: 0,
            state_size,
            sram_len: script.sram_storage_size as usize,
            blocks_start,
            blocks_data,
            num_blocks,
            num_major_stages,
            xmask_state_offset: 0,
            flash_in_reset: false,
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
        self.edges_per_period = per_edge_ops.len();
        self.schedule = per_edge_ops;
        self.gcd_ps = gcd_ps;
        self.xmask_state_offset = xmask_state_offset as usize;
    }

    #[inline]
    fn edge_ops_mut(&mut self, edge_idx: usize) -> &mut [BitOp] {
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
        // synchronized and read back to host; before any launch the host copy
        // is authoritative. Indexing a UVec returns the host-visible slice.
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
        self.flash_in_reset = in_reset;
    }

    fn enable_vcd_ring(&mut self) {
        self.enable_vcd = true;
        // Lazily fill on first run_edges; N=1 → one slot per edge.
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
        // Stage A has no flash; idle MISO nibble is all-high (matches CpuBackend).
        0x0F
    }

    fn flash_debug_snapshot(&self) -> FlashDebug {
        // No flash FSM on the Stage A GPU design path; report a zeroed snapshot
        // (matches CpuBackend).
        FlashDebug {
            d_i: 0x0F,
            data_width: 0,
            prev_csn: 0,
            in_reset: self.flash_in_reset as u32,
            bit_count: 0,
            byte_count: 0,
            addr: 0,
            curr_byte: 0,
            command: 0,
            out_buffer: 0,
            prev_clk: 0,
            prev_d_out: 0,
            last_error_cmd: 0,
            model_prev_csn: 0,
        }
    }

    fn drain_uart_tx(&mut self) -> Vec<(usize, u8)> {
        // Stage A: no peripherals — nothing decoded.
        vec![]
    }

    fn drain_bus_beats(&mut self) -> Vec<crate::sim::models::bus_trace::RawBeat> {
        // Stage A: no peripherals — nothing decoded.
        vec![]
    }

    fn run_edges(&self, batch: usize, schedule_offset: usize) -> u64 {
        let state_size = self.state_size;
        let state = self.state_inner_mut();
        // SAFETY: sequential dispatch — sram/sram_xmask/vcd_ring are only
        // touched here and in &mut-self methods, never concurrently.
        let sram = unsafe { &mut *self.sram.get() };
        let sram_xmask = unsafe { &mut *self.sram_xmask.get() };
        let ring = unsafe { &mut *self.vcd_ring.get() };

        if let Some(r) = ring.as_mut() {
            r.clear();
        }

        let xmask_off = self.xmask_state_offset as u32;

        for e in 0..batch {
            let sched_idx = (schedule_offset + e) % self.edges_per_period;

            // ── GPU state_prep (kernel `cosim_state_prep`) ─────────────────
            // Copies output slot → input slot, then applies this edge's BitOps
            // to the input slot (driving a bit clears its X-mask when
            // `xmask_off != 0`). Build a flat u32 ops UVec from the edge's
            // BitOps: BitOp is `#[repr(C)] { position: u32, value: u32 }`, so a
            // `&[BitOp]` reinterprets as `&[u32]` of length `2 * num_ops`
            // (position, value pairs) — exactly what the kernel expects.
            let ops = &self.schedule[sched_idx];
            let num_ops = ops.len() as u32;
            let ops_flat: Vec<u32> = {
                // SAFETY: BitOp is #[repr(C)] of two u32 fields, so the slice
                // reinterprets bit-for-bit as 2*len u32s.
                let raw = unsafe {
                    std::slice::from_raw_parts(ops.as_ptr() as *const u32, ops.len() * 2)
                };
                raw.to_vec()
            };
            let mut ops_uvec: UVec<u32> = ops_flat.into();

            ucci_hip::cosim_state_prep(
                &mut *state,
                state_size as u32,
                num_ops,
                xmask_off,
                &mut ops_uvec,
                DEVICE,
            );

            // ── Simulate every major stage for this edge ───────────────────
            // One launch per stage; the host drives the major-stage loop (the
            // cosim launchers are non-cooperative). Timing is OFF in Stage A:
            // pass null `timing_constraints` / `event_buffer` (the kernel
            // checks `timing_constraints != nullptr` internally) and
            // `arrival_state_offset = 0`. The whole `states` slot is passed
            // (not split value/xmask) — the shared `simulate_block_v1` handles
            // xprop via the packed slot + `sram_xmask`, mirroring `sim_hip`.
            for stage in 0..self.num_major_stages {
                // SAFETY: NullUPtr yields a null device pointer for both the
                // const and mut argument positions; the kernel treats a null
                // `timing_constraints` as "timing off".
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

            // Ensure this edge's output slot is ready on the device before the
            // next edge's state_prep reads it (and before any host read-back).
            DEVICE.synchronize();

            // ── VCD snapshot: full [input | output] slot for this edge ─────
            // Read the device buffer back to host (synchronize above guarantees
            // it is complete) and stash a host copy in the ring.
            if self.enable_vcd {
                if let Some(r) = ring.as_mut() {
                    r.push(state[..].to_vec());
                }
            }
        }

        // The device was synchronized inline per edge, so `wait` is a no-op and
        // the token is unused (mirrors CpuBackend).
        0
    }

    fn wait(&self, _token: u64) {
        // run_edges synchronizes inline per edge; nothing left to await.
    }
}

/// Public HIP-backend cosim entry point (Stage A). Drives the same agnostic
/// `run_cosim_generic<B>` orchestration as the Metal/CPU shims, with the HIP
/// design-step backend. Gated `#[cfg(feature = "hip")]`.
pub fn run_cosim_hip(
    design: &mut LoadedDesign,
    config: &TestbenchConfig,
    opts: &CosimOpts,
    timing_constraints: &Option<Vec<u32>>,
) -> CosimResult {
    run_cosim_generic::<HipBackend>(design, config, opts, timing_constraints)
}
