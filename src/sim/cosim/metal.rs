// SPDX-FileCopyrightText: Copyright (c) 2024 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0
//! Metal GPU backend for co-simulation: `MetalBackend`, `MetalSimulator`,
//! `ScheduleBuffers`, every GPU `#[repr(C)]` IO struct, the `build_*`/`encode_*`
//! buffer setup, and the `run_cosim` shim. Gated `#[cfg(feature = "metal")]` via
//! the `mod metal;` declaration in the parent module.
//!
//! Backend-agnostic items (the `CosimBackend` trait, `BitOp`, `FlashDebug`,
//! `CosimOpts`/`CosimResult`, `run_cosim_generic`, the scheduler, the CPU
//! baseline helpers, `BusTraceLane`, `GpioMapping`, `set_bit`/`set_flash_din`,
//! `resolve_signal_pos`) are imported from the parent via `super::`.

use crate::aig::AIG;
use crate::flatten::FlattenedScriptV1;
use crate::sim::setup::LoadedDesign;
use crate::testbench::TestbenchConfig;
use metal::{
    CommandQueue, ComputePipelineState, Device as MTLDevice, MTLResourceOptions, MTLSize,
    SharedEvent,
};
use netlistdb::NetlistDB;
use ulib::{AsUPtr, Device};

use super::{
    build_bus_trace_params, build_wb_trace_params, run_cosim_generic, BitOp, BusTraceLane,
    CosimBackend, CosimOpts, CosimResult, FlashDebug, GpioMapping, BATCH_SIZE, MAX_UARTS,
};
// GPU peripheral `#[repr(C)]` IO structs + constants, lifted to the parent
// module (gated to GPU builds) so Metal/CUDA/HIP share one ABI (Stage B B0).
use super::{
    BusTraceChannel, BusTraceEntry, BusTraceParamsAll, FlashDinParamsAll, FlashModelParamsAll,
    FlashState, UartChannel, UartDecoderState, UartParams, UartPerChannelConfig, WbTraceChannel,
    WbTraceEntry, WbTraceParams, BUS_TRACE_CHANNEL_CAP, MAX_QSPI_MEMS, UART_CHANNEL_CAP,
    WB_TRACE_CHANNEL_CAP,
};

// ── Simulation Parameters (must match Metal shader) ──────────────────────────

#[repr(C)]
struct SimParams {
    num_blocks: u64,
    num_major_stages: u64,
    num_cycles: u64,
    state_size: u64,
    current_cycle: u64,
    current_stage: u64,
    arrival_state_offset: u64,
}

/// Parameters for the state_prep kernel (must match Metal shader StatePrepParams struct).
#[repr(C)]
struct StatePrepParams {
    state_size: u32,   // number of u32 words per state slot
    num_ops: u32,      // number of bit set/clear operations
    num_monitors: u32, // number of peripheral monitors to check (0 = skip)
    tick_number: u32,  // current tick number
    // X-mask word offset within a slot (= reg_io_state_size). 0 disables
    // X-mask maintenance (xprop off). When nonzero, each driven bit also has
    // its X-mask cleared (driven ⇒ known) so undriven inputs stay X (#95 ph3).
    xmask_state_offset: u32,
}

// The GPU peripheral `#[repr(C)]` IO + flash structs (UART / Wishbone / AHB-APB
// bus trace / SPI flash) + their constants live in the parent module (`super::`,
// gated to GPU builds) so Metal, CUDA, and HIP share one ABI. Imported via the
// `use super::` block at the top of this file. See `mod.rs` "Shared GPU
// peripheral ABI" / "SPI-flash GPU structs".

/// Pre-allocated Metal buffers for each scheduler edge's ops.
///
/// This is the Metal backend's native schedule storage: the edge ops are a
/// tiny, fixed, repeating set (`edges_per_period` entries) built once and
/// retained. Mutation goes through [`ScheduleBuffers::edge_ops_mut`], which
/// returns a slice straight over the shared `MTLBuffer` — zero-copy, the write
/// *is* the upload. The accessor name and signature deliberately match the
/// `edge_ops_mut` method on the forthcoming backend-portable `CosimBackend`
/// trait (ADR 0017, *Amendment 2026-06-07*), so callers need no renaming when
/// this struct is later subsumed into `MetalBackend`; a CUDA/HIP backend would
/// back the same accessor with a host mirror + dirty-flag + lazy upload.
struct ScheduleBuffers {
    /// Per-scheduler-edge: (params, ops). Length = scheduler.schedule.len()
    /// (= 2 for single-domain: edge 0 = falling, edge 1 = rising).
    edge_buffers: Vec<(metal::Buffer, metal::Buffer)>,
    /// Number of ops per scheduler edge (for CPU verification).
    edge_ops_lens: Vec<usize>,
    /// Time per scheduler edge in picoseconds (= MultiClockScheduler::gcd_ps).
    gcd_ps: u64,
}

impl ScheduleBuffers {
    /// Mutable view of one edge's ops (reset / model-driven / clock-edge
    /// patching). On Metal this is a slice straight over the shared
    /// `MTLBuffer` backing that edge — zero-copy, so the write *is* the
    /// upload. Takes `&self` (not `&mut self`) because the storage is
    /// interior-mutable GPU shared memory; this lets the per-edge patching
    /// closures in the cosim loop share a borrow of the schedule.
    ///
    /// # Safety / aliasing
    /// The returned slice aliases shared GPU memory. Callers must not hold two
    /// overlapping slices live at once, and must not call this while the GPU is
    /// concurrently reading the buffer (the cosim loop only patches ops between
    /// command-buffer completions).
    #[inline]
    fn edge_ops_mut(&self, edge_idx: usize) -> &mut [BitOp] {
        let ops_buf = &self.edge_buffers[edge_idx].1;
        let len = self.edge_ops_lens[edge_idx];
        unsafe { std::slice::from_raw_parts_mut(ops_buf.contents() as *mut BitOp, len) }
    }

    /// Read-only view of one edge's ops (e.g. the `--check-with-cpu` replay).
    /// Reborrows [`Self::edge_ops_mut`] as immutable — same shared buffer, no
    /// second `unsafe` block.
    #[inline]
    fn edge_ops(&self, edge_idx: usize) -> &[BitOp] {
        self.edge_ops_mut(edge_idx)
    }
}

// ── Metal Simulator ──────────────────────────────────────────────────────────

struct MetalSimulator {
    device: metal::Device,
    command_queue: CommandQueue,
    pipeline_state: ComputePipelineState,
    /// Pipeline state for the state_prep kernel (no monitors).
    state_prep_pipeline: ComputePipelineState,
    /// Pipeline state for gpu_apply_flash_din kernel.
    gpu_apply_flash_din_pipeline: ComputePipelineState,
    /// Pipeline state for gpu_flash_model_step kernel.
    gpu_flash_model_step_pipeline: ComputePipelineState,
    /// Pipeline state for gpu_io_step kernel (UART + bus trace combined).
    gpu_io_step_pipeline: ComputePipelineState,
    /// Pre-allocated params buffers for each stage (shared memory, rewritten each dispatch).
    /// We need one per stage since multi-stage designs encode all stages before commit.
    params_buffers: Vec<metal::Buffer>,
    /// Shared event for GPU↔CPU synchronization within a single command buffer.
    shared_event: SharedEvent,
    /// Monotonic counter for shared event signaling
    event_counter: std::cell::Cell<u64>,
}

impl MetalSimulator {
    fn new(num_stages: usize) -> Self {
        let device = MTLDevice::system_default().expect("No Metal device found");
        clilog::info!("Using Metal device: {}", device.name());

        // The Metal kernel library is embedded into the binary at compile
        // time. The build script compiles `csrc/kernel_v1.metal` to a
        // `.metallib` and exposes its path via `METALLIB_PATH`; we
        // `include_bytes!` that at build time and load it from memory.
        // Embedding (rather than loading the file at runtime) makes the
        // binary relocatable — a shipped / `cargo binstall`ed binary has
        // no build-tree path to read from. See ADR 0018 and
        // docs/plans/distribution.md (Phase 1a).
        const METALLIB_BYTES: &[u8] = include_bytes!(env!("METALLIB_PATH"));
        let library = device
            .new_library_with_data(METALLIB_BYTES)
            .expect("Failed to load embedded metallib");

        let kernel_function = library
            .get_function("simulate_v1_stage", None)
            .expect("Failed to get kernel function");

        let pipeline_state = device
            .new_compute_pipeline_state_with_function(&kernel_function)
            .expect("Failed to create pipeline state");

        let state_prep_fn = library
            .get_function("state_prep", None)
            .expect("Failed to get state_prep function");
        let state_prep_pipeline = device
            .new_compute_pipeline_state_with_function(&state_prep_fn)
            .expect("Failed to create state_prep pipeline");

        let flash_din_fn = library
            .get_function("gpu_apply_flash_din", None)
            .expect("Failed to get gpu_apply_flash_din function");
        let gpu_apply_flash_din_pipeline = device
            .new_compute_pipeline_state_with_function(&flash_din_fn)
            .expect("Failed to create gpu_apply_flash_din pipeline");

        let flash_step_fn = library
            .get_function("gpu_flash_model_step", None)
            .expect("Failed to get gpu_flash_model_step function");
        let gpu_flash_model_step_pipeline = device
            .new_compute_pipeline_state_with_function(&flash_step_fn)
            .expect("Failed to create gpu_flash_model_step pipeline");

        let io_step_fn = library
            .get_function("gpu_io_step", None)
            .expect("Failed to get gpu_io_step function");
        let gpu_io_step_pipeline = device
            .new_compute_pipeline_state_with_function(&io_step_fn)
            .expect("Failed to create gpu_io_step pipeline");

        let command_queue = device.new_command_queue();

        // Pre-allocate one params buffer per stage
        let params_buffers: Vec<_> = (0..num_stages.max(1))
            .map(|_| {
                device.new_buffer(
                    std::mem::size_of::<SimParams>() as u64,
                    MTLResourceOptions::StorageModeShared,
                )
            })
            .collect();

        let shared_event = device.new_shared_event();

        Self {
            device,
            command_queue,
            pipeline_state,
            state_prep_pipeline,
            gpu_apply_flash_din_pipeline,
            gpu_flash_model_step_pipeline,
            gpu_io_step_pipeline,
            params_buffers,
            shared_event,
            event_counter: std::cell::Cell::new(0),
        }
    }

    /// Dispatch a single stage (standalone, with own command buffer).
    /// Used as fallback when dual-tick pattern isn't applicable.
    #[allow(dead_code)]
    #[inline]
    fn dispatch_stage(
        &self,
        num_blocks: usize,
        num_major_stages: usize,
        state_size: usize,
        cycle_i: usize,
        stage_i: usize,
        blocks_start_buffer: &metal::Buffer,
        blocks_data_buffer: &metal::Buffer,
        sram_data_buffer: &metal::Buffer,
        states_buffer: &metal::Buffer,
        event_buffer_metal: &metal::Buffer,
        timing_constraints_buffer: Option<&metal::Buffer>,
        sram_xmask_buffer: &metal::Buffer,
    ) {
        self.write_params(
            stage_i,
            num_blocks,
            num_major_stages,
            state_size,
            cycle_i,
            0,
        );

        let command_buffer = self.command_queue.new_command_buffer();
        self.encode_dispatch(
            &command_buffer,
            num_blocks,
            stage_i,
            blocks_start_buffer,
            blocks_data_buffer,
            sram_data_buffer,
            states_buffer,
            event_buffer_metal,
            timing_constraints_buffer,
            sram_xmask_buffer,
        );
        command_buffer.commit();
        command_buffer.wait_until_completed();
    }

    /// Write params for a given stage into the pre-allocated shared memory buffer.
    #[inline]
    fn write_params(
        &self,
        stage_i: usize,
        num_blocks: usize,
        num_major_stages: usize,
        state_size: usize,
        cycle_i: usize,
        arrival_state_offset: u32,
    ) {
        let params = SimParams {
            num_blocks: num_blocks as u64,
            num_major_stages: num_major_stages as u64,
            num_cycles: 1,
            state_size: state_size as u64,
            current_cycle: cycle_i as u64,
            current_stage: stage_i as u64,
            arrival_state_offset: arrival_state_offset as u64,
        };
        unsafe {
            std::ptr::write(
                self.params_buffers[stage_i].contents() as *mut SimParams,
                params,
            );
        }
    }

    /// Encode a compute dispatch into an existing command buffer (no commit).
    #[inline]
    fn encode_dispatch(
        &self,
        command_buffer: &metal::CommandBufferRef,
        num_blocks: usize,
        stage_i: usize,
        blocks_start_buffer: &metal::Buffer,
        blocks_data_buffer: &metal::Buffer,
        sram_data_buffer: &metal::Buffer,
        states_buffer: &metal::Buffer,
        event_buffer_metal: &metal::Buffer,
        timing_constraints_buffer: Option<&metal::Buffer>,
        sram_xmask_buffer: &metal::Buffer,
    ) {
        let encoder = command_buffer.new_compute_command_encoder();
        encoder.set_compute_pipeline_state(&self.pipeline_state);
        encoder.set_buffer(0, Some(blocks_start_buffer), 0);
        encoder.set_buffer(1, Some(blocks_data_buffer), 0);
        encoder.set_buffer(2, Some(sram_data_buffer), 0);
        encoder.set_buffer(3, Some(states_buffer), 0);
        encoder.set_buffer(4, Some(&self.params_buffers[stage_i]), 0);
        encoder.set_buffer(5, Some(event_buffer_metal), 0);
        encoder.set_buffer(6, timing_constraints_buffer.map(|v| &**v), 0);
        // buffer(7) = SRAM X-mask shadow (ADR 0016); the kernel only reads
        // it when the partition is X-capable, so a dummy is fine off-xprop.
        encoder.set_buffer(7, Some(sram_xmask_buffer), 0);

        let threads_per_threadgroup = MTLSize::new(256, 1, 1);
        let threadgroups = MTLSize::new(num_blocks as u64, 1, 1);

        encoder.dispatch_thread_groups(threadgroups, threads_per_threadgroup);
        encoder.end_encoding();
    }

    /// Spin-wait for shared event to reach target value.
    #[inline]
    fn spin_wait(&self, target: u64) {
        while self.shared_event.signaled_value() < target {
            std::hint::spin_loop();
        }
    }

    /// Encode a state_prep dispatch into an existing command buffer.
    #[inline]
    fn encode_state_prep(
        &self,
        command_buffer: &metal::CommandBufferRef,
        states_buffer: &metal::Buffer,
        prep_params_buffer: &metal::Buffer,
        ops_buffer: &metal::Buffer,
    ) {
        let encoder = command_buffer.new_compute_command_encoder();
        encoder.set_compute_pipeline_state(&self.state_prep_pipeline);
        encoder.set_buffer(0, Some(states_buffer), 0);
        encoder.set_buffer(1, Some(prep_params_buffer), 0);
        encoder.set_buffer(2, Some(ops_buffer), 0);
        let tpg = MTLSize::new(256, 1, 1);
        encoder.dispatch_thread_groups(MTLSize::new(1, 1, 1), tpg);
        encoder.end_encoding();
    }

    /// Encode a gpu_apply_flash_din dispatch into an existing command buffer.
    #[inline]
    fn encode_apply_flash_din(
        &self,
        command_buffer: &metal::CommandBufferRef,
        states_buffer: &metal::Buffer,
        flash_state_buffer: &metal::Buffer,
        flash_din_params_buffer: &metal::Buffer,
    ) {
        let encoder = command_buffer.new_compute_command_encoder();
        encoder.set_compute_pipeline_state(&self.gpu_apply_flash_din_pipeline);
        encoder.set_buffer(0, Some(states_buffer), 0);
        encoder.set_buffer(1, Some(flash_state_buffer), 0);
        encoder.set_buffer(2, Some(flash_din_params_buffer), 0);
        let tpg = MTLSize::new(256, 1, 1);
        encoder.dispatch_thread_groups(MTLSize::new(1, 1, 1), tpg);
        encoder.end_encoding();
    }

    /// Encode a gpu_flash_model_step dispatch into an existing command buffer.
    #[inline]
    fn encode_flash_model_step(
        &self,
        command_buffer: &metal::CommandBufferRef,
        states_buffer: &metal::Buffer,
        flash_state_buffer: &metal::Buffer,
        flash_model_params_buffer: &metal::Buffer,
        flash_data_buffer: &metal::Buffer,
    ) {
        let encoder = command_buffer.new_compute_command_encoder();
        encoder.set_compute_pipeline_state(&self.gpu_flash_model_step_pipeline);
        encoder.set_buffer(0, Some(states_buffer), 0);
        encoder.set_buffer(1, Some(flash_state_buffer), 0);
        encoder.set_buffer(2, Some(flash_model_params_buffer), 0);
        encoder.set_buffer(3, Some(flash_data_buffer), 0);
        let tpg = MTLSize::new(256, 1, 1);
        encoder.dispatch_thread_groups(MTLSize::new(1, 1, 1), tpg);
        encoder.end_encoding();
    }

    /// Encode a gpu_io_step dispatch (UART + bus trace) into an existing command buffer.
    #[inline]
    fn encode_io_step(
        &self,
        command_buffer: &metal::CommandBufferRef,
        states_buffer: &metal::Buffer,
        uart_state_buffer: &metal::Buffer,
        uart_params_buffer: &metal::Buffer,
        uart_channel_buffer: &metal::Buffer,
        wb_trace_channel_buffer: &metal::Buffer,
        wb_trace_params_buffer: &metal::Buffer,
        bus_trace_channel_buffer: &metal::Buffer,
        bus_trace_params_buffer: &metal::Buffer,
    ) {
        let encoder = command_buffer.new_compute_command_encoder();
        encoder.set_compute_pipeline_state(&self.gpu_io_step_pipeline);
        encoder.set_buffer(0, Some(states_buffer), 0);
        encoder.set_buffer(1, Some(uart_state_buffer), 0);
        encoder.set_buffer(2, Some(uart_params_buffer), 0);
        encoder.set_buffer(3, Some(uart_channel_buffer), 0);
        encoder.set_buffer(4, Some(wb_trace_channel_buffer), 0);
        encoder.set_buffer(5, Some(wb_trace_params_buffer), 0);
        encoder.set_buffer(6, Some(bus_trace_channel_buffer), 0);
        encoder.set_buffer(7, Some(bus_trace_params_buffer), 0);
        let tpg = MTLSize::new(256, 1, 1);
        encoder.dispatch_thread_groups(MTLSize::new(1, 1, 1), tpg);
        encoder.end_encoding();
    }

    /// Encode and commit a GPU-only batch of K edges in a single command buffer.
    ///
    /// No per-edge CPU interaction — flash and UART are handled entirely on GPU.
    /// A single signal at the end notifies the CPU that the batch is complete.
    ///
    /// Each edge encodes:
    ///   1. state_prep (per-edge ops: clk + posedge/negedge flags for any
    ///      domains active at this scheduler edge)
    ///   2. gpu_apply_flash_din
    ///   3. simulate_v1_stage × num_stages
    ///   4. gpu_flash_model_step (sees the SPI CLK after this edge)
    ///   5. gpu_io_step (UART + bus trace)
    ///
    /// The flash model still runs after every dispatch — for single-domain that's
    /// twice per cycle (once after the falling edge, once after the rising edge),
    /// matching the previous "dual-step per tick" behavior. This is critical
    /// because the SPI CLK signal passes through clock gating logic
    /// (Q = system_CLK & EN_latch), so the flash sees CLK=0 after the falling
    /// edge and CLK=EN_latch after the rising edge. The d_in from the falling-
    /// edge flash step is picked up by the rising-edge apply_flash_din,
    /// allowing the flash controller to see the MISO response within the same
    /// cycle.
    ///
    /// gpu_io_step also runs after every edge — UART decoder's `current_cycle`
    /// counter therefore advances at edge granularity, matching chipflow's
    /// events_reference timestamp convention. UART `cycles_per_bit` must be
    /// expressed in edges (= 2× clock cycles for single-domain).
    ///
    /// Returns the event value that signals batch completion.
    #[allow(clippy::too_many_arguments)]
    fn encode_and_commit_gpu_batch(
        &self,
        batch_size: usize,
        num_blocks: usize,
        num_major_stages: usize,
        state_size: usize,
        blocks_start_buffer: &metal::Buffer,
        blocks_data_buffer: &metal::Buffer,
        sram_data_buffer: &metal::Buffer,
        states_buffer: &metal::Buffer,
        event_buffer_metal: &metal::Buffer,
        schedule_buffers: &ScheduleBuffers,
        schedule_offset: usize,
        flash_state_buffer: &metal::Buffer,
        flash_din_params_buffer: &metal::Buffer,
        flash_model_params_buffer: &metal::Buffer,
        flash_data_buffer: &metal::Buffer,
        uart_state_buffer: &metal::Buffer,
        uart_params_buffer: &metal::Buffer,
        uart_channel_buffer: &metal::Buffer,
        wb_trace_channel_buffer: &metal::Buffer,
        wb_trace_params_buffer: &metal::Buffer,
        bus_trace_channel_buffer: &metal::Buffer,
        bus_trace_params_buffer: &metal::Buffer,
        sram_xmask_buffer: &metal::Buffer,
        timing_constraints_buffer: Option<&metal::Buffer>,
        arrival_state_offset: u32,
        vcd_ring_buffer: Option<&metal::Buffer>,
    ) -> u64 {
        let batch_done = self.event_counter.get() + 1;
        let cb = self.command_queue.new_command_buffer();
        let edges_per_period = schedule_buffers.edge_buffers.len();
        let snapshot_bytes = (2 * state_size * std::mem::size_of::<u32>()) as u64;

        for edge_offset in 0..batch_size {
            let sched_idx = (schedule_offset + edge_offset) % edges_per_period;
            let (ref edge_params, ref edge_ops) = schedule_buffers.edge_buffers[sched_idx];

            // state_prep (clk + edge flags) + flash_din + simulate
            self.encode_state_prep(cb, states_buffer, edge_params, edge_ops);
            self.encode_apply_flash_din(
                cb,
                states_buffer,
                flash_state_buffer,
                flash_din_params_buffer,
            );
            for stage_i in 0..num_major_stages {
                self.write_params(
                    stage_i,
                    num_blocks,
                    num_major_stages,
                    state_size,
                    0,
                    arrival_state_offset,
                );
                self.encode_dispatch(
                    cb,
                    num_blocks,
                    stage_i,
                    blocks_start_buffer,
                    blocks_data_buffer,
                    sram_data_buffer,
                    states_buffer,
                    event_buffer_metal,
                    timing_constraints_buffer,
                    sram_xmask_buffer,
                );
            }

            // Flash model + io_step after this edge.
            self.encode_flash_model_step(
                cb,
                states_buffer,
                flash_state_buffer,
                flash_model_params_buffer,
                flash_data_buffer,
            );
            self.encode_io_step(
                cb,
                states_buffer,
                uart_state_buffer,
                uart_params_buffer,
                uart_channel_buffer,
                wb_trace_channel_buffer,
                wb_trace_params_buffer,
                bus_trace_channel_buffer,
                bus_trace_params_buffer,
            );

            if let Some(ring) = vcd_ring_buffer {
                let blit = cb.new_blit_command_encoder();
                blit.copy_from_buffer(
                    states_buffer,
                    0,
                    ring,
                    edge_offset as u64 * snapshot_bytes,
                    snapshot_bytes,
                );
                blit.end_encoding();
            }
        }

        // Single signal at end of entire batch
        cb.encode_signal_event(&self.shared_event, batch_done);
        cb.commit();

        self.event_counter.set(batch_done);
        batch_done
    }

    /// Run GPU kernel profiling: dispatch each kernel in its own command buffer,
    /// wait for completion, and measure GPU execution time.
    #[allow(clippy::too_many_arguments)]
    fn profile_gpu_kernels(
        &self,
        num_ticks: usize,
        num_blocks: usize,
        num_major_stages: usize,
        state_size: usize,
        blocks_start_buffer: &metal::Buffer,
        blocks_data_buffer: &metal::Buffer,
        sram_data_buffer: &metal::Buffer,
        states_buffer: &metal::Buffer,
        event_buffer_metal: &metal::Buffer,
        schedule_buffers: &ScheduleBuffers,
        flash_state_buffer: &metal::Buffer,
        flash_din_params_buffer: &metal::Buffer,
        flash_model_params_buffer: &metal::Buffer,
        flash_data_buffer: &metal::Buffer,
        uart_state_buffer: &metal::Buffer,
        uart_params_buffer: &metal::Buffer,
        uart_channel_buffer: &metal::Buffer,
        wb_trace_channel_buffer: &metal::Buffer,
        wb_trace_params_buffer: &metal::Buffer,
        bus_trace_channel_buffer: &metal::Buffer,
        bus_trace_params_buffer: &metal::Buffer,
        sram_xmask_buffer: &metal::Buffer,
        timing_constraints_buffer: Option<&metal::Buffer>,
    ) {
        // Use schedule position 0 for profiling (all patterns have same kernel cost).
        let (ref edge_prep_params_buffer, ref edge_ops_buffer) = schedule_buffers.edge_buffers[0];
        #[inline]
        fn gpu_times(cb: &metal::CommandBufferRef) -> (f64, f64) {
            unsafe {
                let obj: *mut objc::runtime::Object =
                    &*(cb as *const _ as *const objc::runtime::Object) as *const _ as *mut _;
                let start: f64 = msg_send![obj, GPUStartTime];
                let end: f64 = msg_send![obj, GPUEndTime];
                (start, end)
            }
        }

        println!("\n=== GPU Kernel Profiling ({} edges) ===\n", num_ticks);

        let encode_full_edge = |cb: &metal::CommandBufferRef| {
            self.encode_state_prep(cb, states_buffer, edge_prep_params_buffer, edge_ops_buffer);
            self.encode_apply_flash_din(
                cb,
                states_buffer,
                flash_state_buffer,
                flash_din_params_buffer,
            );
            for stage_i in 0..num_major_stages {
                self.write_params(stage_i, num_blocks, num_major_stages, state_size, 0, 0);
                self.encode_dispatch(
                    cb,
                    num_blocks,
                    stage_i,
                    blocks_start_buffer,
                    blocks_data_buffer,
                    sram_data_buffer,
                    states_buffer,
                    event_buffer_metal,
                    timing_constraints_buffer,
                    sram_xmask_buffer,
                );
            }
            self.encode_flash_model_step(
                cb,
                states_buffer,
                flash_state_buffer,
                flash_model_params_buffer,
                flash_data_buffer,
            );
            self.encode_io_step(
                cb,
                states_buffer,
                uart_state_buffer,
                uart_params_buffer,
                uart_channel_buffer,
                wb_trace_channel_buffer,
                wb_trace_params_buffer,
                bus_trace_channel_buffer,
                bus_trace_params_buffer,
            );
        };

        // Warmup
        for _ in 0..10 {
            let cb = self.command_queue.new_command_buffer();
            encode_full_edge(cb);
            cb.commit();
            cb.wait_until_completed();
        }

        let mut time_state_prep = 0.0f64;
        let mut time_flash_din = 0.0f64;
        let mut time_simulate = 0.0f64;
        let mut time_flash_step = 0.0f64;
        let mut time_io_step = 0.0f64;
        let mut time_full_edge = 0.0f64;

        let wall_start = std::time::Instant::now();

        for _edge in 0..num_ticks {
            // state_prep — isolated
            let cb1 = self.command_queue.new_command_buffer();
            self.encode_state_prep(cb1, states_buffer, edge_prep_params_buffer, edge_ops_buffer);
            cb1.commit();
            cb1.wait_until_completed();
            let (s, e) = gpu_times(cb1);
            time_state_prep += e - s;

            // gpu_apply_flash_din — isolated
            let cb1b = self.command_queue.new_command_buffer();
            self.encode_apply_flash_din(
                cb1b,
                states_buffer,
                flash_state_buffer,
                flash_din_params_buffer,
            );
            cb1b.commit();
            cb1b.wait_until_completed();
            let (s, e) = gpu_times(cb1b);
            time_flash_din += e - s;

            // simulate — isolated per stage
            for stage_i in 0..num_major_stages {
                self.write_params(stage_i, num_blocks, num_major_stages, state_size, 0, 0);
                let cb2 = self.command_queue.new_command_buffer();
                self.encode_dispatch(
                    cb2,
                    num_blocks,
                    stage_i,
                    blocks_start_buffer,
                    blocks_data_buffer,
                    sram_data_buffer,
                    states_buffer,
                    event_buffer_metal,
                    timing_constraints_buffer,
                    sram_xmask_buffer,
                );
                cb2.commit();
                cb2.wait_until_completed();
                let (s, e) = gpu_times(cb2);
                time_simulate += e - s;
            }

            // gpu_flash_model_step — isolated
            let cb3 = self.command_queue.new_command_buffer();
            self.encode_flash_model_step(
                cb3,
                states_buffer,
                flash_state_buffer,
                flash_model_params_buffer,
                flash_data_buffer,
            );
            cb3.commit();
            cb3.wait_until_completed();
            let (s, e) = gpu_times(cb3);
            time_flash_step += e - s;

            // gpu_io_step — isolated
            let cb4 = self.command_queue.new_command_buffer();
            self.encode_io_step(
                cb4,
                states_buffer,
                uart_state_buffer,
                uart_params_buffer,
                uart_channel_buffer,
                wb_trace_channel_buffer,
                wb_trace_params_buffer,
                bus_trace_channel_buffer,
                bus_trace_params_buffer,
            );
            cb4.commit();
            cb4.wait_until_completed();
            let (s, e) = gpu_times(cb4);
            time_io_step += e - s;

            // Full edge in single CB for comparison
            let cb_full = self.command_queue.new_command_buffer();
            encode_full_edge(cb_full);
            cb_full.commit();
            cb_full.wait_until_completed();
            let (s, e) = gpu_times(cb_full);
            time_full_edge += e - s;
        }

        let wall_elapsed = wall_start.elapsed();
        let n = num_ticks as f64;

        let total_isolated =
            time_state_prep + time_flash_din + time_simulate + time_flash_step + time_io_step;

        let print_kernel = |name: &str, t: f64| {
            let us = t / n * 1e6;
            let pct = if total_isolated > 0.0 {
                100.0 * t / total_isolated
            } else {
                0.0
            };
            println!("  {:<36} {:>8.2}μs/edge  {:>5.1}%", name, us, pct);
        };

        println!("Per-kernel GPU time (isolated command buffers):");
        print_kernel("state_prep", time_state_prep);
        print_kernel("gpu_apply_flash_din", time_flash_din);
        print_kernel("simulate_v1_stage", time_simulate);
        print_kernel("gpu_flash_model_step", time_flash_step);
        print_kernel("gpu_io_step", time_io_step);
        println!(
            "  {:<36} {:>8.2}μs/edge",
            "TOTAL (isolated sum)",
            total_isolated / n * 1e6
        );
        println!();
        println!(
            "  {:<36} {:>8.2}μs/edge",
            "Full edge (single CB)",
            time_full_edge / n * 1e6
        );
        println!(
            "  {:<36} {:>8.2}μs/edge",
            "Wall clock (2× edges, profiling)",
            wall_elapsed.as_secs_f64() / n * 1e6
        );
        println!();
        println!(
            "  Simulation kernels total:     {:>8.2}μs/edge  ({:.1}%)",
            time_simulate / n * 1e6,
            100.0 * time_simulate / total_isolated
        );
        println!(
            "  IO model kernels total:       {:>8.2}μs/edge  ({:.1}%)",
            (time_flash_din + time_flash_step + time_io_step) / n * 1e6,
            100.0 * (time_flash_din + time_flash_step + time_io_step) / total_isolated
        );
        println!(
            "  State prep total:             {:>8.2}μs/edge  ({:.1}%)",
            time_state_prep / n * 1e6,
            100.0 * time_state_prep / total_isolated
        );
        println!();
        println!(
            "  CB submission overhead:        {:>8.2}μs/edge (wall - GPU)",
            (wall_elapsed.as_secs_f64() - total_isolated - time_full_edge) / n * 1e6
        );
    }
}

// `resolve_bus_signal`, `build_wb_trace_params`, and `build_bus_trace_params`
// live in the parent module (`super::`, gated to GPU builds) so Metal and
// CUDA/HIP share the WB/APB param-resolution logic. Imported via `use super::`.

/// Write decoded bus transactions to a CSV file (ADR 0013). Columns:
/// `tick,bus,protocol,dir,addr,data,resp,burst`. Addresses and data are
/// hex (`0x…`); `burst` is `beat/len` for AHB bursts, empty otherwise.

/// Create a Metal buffer containing a StatePrepParams struct.
fn create_prep_params_buffer(
    device: &metal::Device,
    state_size: u32,
    num_ops: u32,
    num_monitors: u32,
    tick_number: u32,
    xmask_state_offset: u32,
) -> metal::Buffer {
    let buf = device.new_buffer(
        std::mem::size_of::<StatePrepParams>() as u64,
        MTLResourceOptions::StorageModeShared,
    );
    unsafe {
        std::ptr::write(
            buf.contents() as *mut StatePrepParams,
            StatePrepParams {
                state_size,
                num_ops,
                num_monitors,
                tick_number,
                xmask_state_offset,
            },
        );
    }
    buf
}

/// Create a Metal buffer containing a BitOp array.
fn create_ops_buffer(device: &metal::Device, ops: &[BitOp]) -> metal::Buffer {
    let size = if ops.is_empty() {
        std::mem::size_of::<BitOp>() as u64 // minimum 1 element
    } else {
        (ops.len() * std::mem::size_of::<BitOp>()) as u64
    };
    let buf = device.new_buffer(size, MTLResourceOptions::StorageModeShared);
    if !ops.is_empty() {
        unsafe {
            std::ptr::copy_nonoverlapping(ops.as_ptr(), buf.contents() as *mut BitOp, ops.len());
        }
    }
    buf
}

/// Metal implementation of [`CosimBackend`]: owns the `MetalSimulator`
/// (pipelines, queue, device), the design state buffer, the per-edge schedule
/// storage, and every GPU IO buffer. `run_edges`/`profile_kernels` forward to
/// the unchanged `MetalSimulator::encode_and_commit_gpu_batch` /
/// `profile_gpu_kernels` using these owned fields, so the GPU-encoding logic is
/// byte-for-byte unchanged from the pre-seam call site. Fields are
/// module-visible so the cosim loop can read the IO buffers directly
/// (`flash_state_buffer.in_reset` writes, `--check-with-cpu` state reads).
struct MetalBackend {
    sim: MetalSimulator,
    /// Set once by `init_schedule`; `None` until then.
    schedule: Option<ScheduleBuffers>,
    state_size: usize,
    /// Logical SRAM length in words (`script.sram_storage_size`). The backing
    /// `sram_data_buffer` is allocated `max(1)`, so this can differ from / be
    /// less than the buffer's capacity, and is 0 for a SRAM-less design.
    sram_len: usize,
    num_blocks: usize,
    num_major_stages: usize,
    arrival_state_offset: u32,
    // Design state + program.
    states_buffer: metal::Buffer,
    blocks_start_buffer: metal::Buffer,
    blocks_data_buffer: metal::Buffer,
    event_buffer_metal: metal::Buffer,
    sram_data_buffer: metal::Buffer,
    sram_xmask_buffer: metal::Buffer,
    timing_constraints_buffer: Option<metal::Buffer>,
    // GPU peripheral IO buffers.
    flash_state_buffer: metal::Buffer,
    flash_din_params_buffer: metal::Buffer,
    flash_model_params_buffer: metal::Buffer,
    flash_data_buffer: metal::Buffer,
    uart_state_buffer: metal::Buffer,
    uart_params_buffer: metal::Buffer,
    uart_channel_buffer: metal::Buffer,
    wb_trace_params_buffer: metal::Buffer,
    wb_trace_channel_buffer: metal::Buffer,
    bus_trace_params_buffer: metal::Buffer,
    bus_trace_channel_buffer: metal::Buffer,
    /// Per-edge `[input | output]` snapshot ring for VCD; `None` until
    /// `enable_vcd_ring`. `run_edges` blits each edge's state into it.
    vcd_ring: Option<metal::Buffer>,
    /// Number of active UART channels (== `uart_configs.len()`); bounds the
    /// `drain_uart_tx` loop.
    n_uarts: usize,
    /// Per-UART ring read cursors (decoded-byte drain). Sized `n_uarts`,
    /// zeroed; `drain_uart_tx` advances each from its cursor to `write_head`.
    uart_read_heads: Vec<u32>,
    /// Wishbone-trace ring read cursor (legacy debug drain).
    wb_trace_read_head: u32,
    /// AHB/APB bus-trace ring read cursor.
    bus_trace_read_head: u32,
    /// Leaked `Box<EventBuffer>` raw pointer (`$stop`/`$finish`/assertions).
    /// The Metal event buffer wraps this no-copy; freed by `Drop for
    /// MetalBackend` at end-of-scope / early-return (all GPU work has
    /// completed by then). Owns the box — do NOT free it elsewhere.
    event_buffer_ptr: *mut crate::event_buffer::EventBuffer,
}

impl Drop for MetalBackend {
    fn drop(&mut self) {
        // SAFETY: `event_buffer_ptr` was produced by `Box::into_raw` in
        // `build_state_buffers` and is owned solely by this backend. All GPU
        // work referencing the no-copy event buffer has completed before the
        // backend drops (end-of-run or the profile early-return), so freeing
        // the box here is sound and cannot use-after-free.
        unsafe {
            drop(Box::from_raw(self.event_buffer_ptr));
        }
    }
}

impl MetalBackend {
    /// Borrow the schedule storage, asserting `init_schedule` has run.
    #[inline]
    fn schedule(&self) -> &ScheduleBuffers {
        self.schedule
            .as_ref()
            .expect("MetalBackend::init_schedule must be called before use")
    }

    /// Allocate + initialise the four GPU SPI-flash buffers (Phase 1 step 2a:
    /// relocated from `run_cosim`'s inline setup). Returns
    /// `(state, din_params, model_params, data)`. Loads firmware into the data
    /// buffer when `config.flash` is set.
    fn build_flash_buffers(
        device: &metal::Device,
        config: &TestbenchConfig,
        script: &FlattenedScriptV1,
        state_size: usize,
        gpio_map: &GpioMapping,
    ) -> (metal::Buffer, metal::Buffer, metal::Buffer, metal::Buffer) {
        // ADR 0013 plural QSPI memory (Stage B). Each instance owns a
        // FlashState slot + a slice of the concatenated `flash_data` backing
        // store, so memories advance independently. N=1 stays byte-identical.
        const FLASH_SIZE_DEFAULT: usize = 16 * 1024 * 1024;
        let qspi = config.effective_qspi_memory();
        assert!(
            qspi.len() <= MAX_QSPI_MEMS,
            "too many QSPI memories ({}); GPU backend supports at most {}",
            qspi.len(),
            MAX_QSPI_MEMS
        );
        let n = qspi.len();

        // Per-instance backing-store sizes + byte offsets into the shared buffer.
        let sizes: Vec<usize> = qspi
            .iter()
            .map(|m| m.size_bytes.unwrap_or(FLASH_SIZE_DEFAULT))
            .collect();
        let mut offsets = vec![0u32; n];
        let mut total = 0usize;
        for i in 0..n {
            offsets[i] = total as u32;
            total += sizes[i];
        }
        let total = total.max(1); // avoid a 0-byte buffer when n == 0

        // FlashState: one slot per instance (>=1 to avoid a 0-byte buffer).
        let n_slots = n.max(1);
        let flash_state_buffer = device.new_buffer(
            (n_slots * std::mem::size_of::<FlashState>()) as u64,
            MTLResourceOptions::StorageModeShared,
        );
        unsafe {
            let slots = flash_state_buffer.contents() as *mut FlashState;
            for s in 0..n_slots {
                let fs = &mut *slots.add(s);
                *fs = std::mem::zeroed();
                fs.data_width = 1; // SPI single-bit mode
                fs.prev_csn = 1; // CSN starts high (deselected)
                fs.model_prev_csn = 1; // model internal edge detection starts high
                fs.d_i = 0x0F; // flash output starts high
                fs.in_reset = 1; // start in reset
            }
        }

        // FlashDinParamsAll (constant): per-instance MISO input bit positions.
        let flash_din_params_buffer = device.new_buffer(
            std::mem::size_of::<FlashDinParamsAll>() as u64,
            MTLResourceOptions::StorageModeShared,
        );
        unsafe {
            let all = &mut *(flash_din_params_buffer.contents() as *mut FlashDinParamsAll);
            *all = std::mem::zeroed();
            all.n_flashes = n as u32;
            for (idx, m) in qspi.iter().enumerate() {
                let p = &mut all.flashes[idx];
                p.has_flash = 1;
                p.xmask_state_offset = script.xprop_state_offset;
                for i in 0..4 {
                    p.d_in_pos[i] = gpio_map
                        .input_bits
                        .get(&(m.d0_gpio + i))
                        .copied()
                        .unwrap_or(0xFFFFFFFF);
                }
            }
        }

        // FlashModelParamsAll (constant): per-instance pin positions + the
        // backing slice (flash_data_size + data_offset into `flash_data`).
        let flash_model_params_buffer = device.new_buffer(
            std::mem::size_of::<FlashModelParamsAll>() as u64,
            MTLResourceOptions::StorageModeShared,
        );
        unsafe {
            let all = &mut *(flash_model_params_buffer.contents() as *mut FlashModelParamsAll);
            *all = std::mem::zeroed();
            all.n_flashes = n as u32;
            for (idx, m) in qspi.iter().enumerate() {
                let p = &mut all.flashes[idx];
                p.state_size = state_size as u32;
                p.clk_out_pos = gpio_map.output_bits.get(&m.clk_gpio).copied().unwrap_or(0);
                p.csn_out_pos = gpio_map.output_bits.get(&m.csn_gpio).copied().unwrap_or(0);
                for i in 0..4 {
                    p.d_out_pos[i] = gpio_map
                        .output_bits
                        .get(&(m.d0_gpio + i))
                        .copied()
                        .unwrap_or(0xFFFFFFFF);
                }
                p.flash_data_size = sizes[idx] as u32;
                p.data_offset = offsets[idx];
                // RAM-mode (QSPI PSRAM) config — sentinels/zero for plain flash.
                let ram = super::flash_ram_params(m);
                p.writable = ram.writable;
                p.enter_qpi_cmd = ram.enter_qpi_cmd;
                p.quad_write_cmd = ram.quad_write_cmd;
                p.qpi_read_dummy = ram.qpi_read_dummy;
            }
            clilog::info!(
                "FlashModelParamsAll: n_flashes={}, total_backing={} bytes",
                n,
                total
            );
        }

        // Concatenated backing store. Per-instance fill: 0xFF (erased flash) or,
        // for a writable QSPI PSRAM, 0x00 (power-on, cocotb `bytearray(size)`).
        // Each instance's firmware (if any) is overlaid into its own slice below.
        let flash_data_buffer =
            device.new_buffer(total as u64, MTLResourceOptions::StorageModeShared);
        unsafe {
            std::ptr::write_bytes(flash_data_buffer.contents() as *mut u8, 0xFF, total);
            for (idx, m) in qspi.iter().enumerate() {
                if m.writable {
                    let base = (flash_data_buffer.contents() as *mut u8).add(offsets[idx] as usize);
                    std::ptr::write_bytes(base, 0x00, sizes[idx]);
                }
            }
        }
        for (idx, m) in qspi.iter().enumerate() {
            if let Some(fw_path) = m.firmware.as_ref() {
                use std::io::Read;
                let mut file = std::fs::File::open(std::path::Path::new(fw_path))
                    .expect("Failed to open firmware file");
                let mut data = Vec::new();
                file.read_to_end(&mut data)
                    .expect("Failed to read firmware");
                let within = m.firmware_offset;
                assert!(
                    within + data.len() <= sizes[idx],
                    "Firmware too large for QSPI memory {idx} backing store"
                );
                let abs = offsets[idx] as usize + within;
                unsafe {
                    let dest = (flash_data_buffer.contents() as *mut u8).add(abs);
                    std::ptr::copy_nonoverlapping(data.as_ptr(), dest, data.len());
                }
                clilog::info!(
                    "Loaded {} bytes firmware into QSPI memory {} at 0x{:X} (abs 0x{:X})",
                    data.len(),
                    idx,
                    within,
                    abs
                );
            }
        }

        (
            flash_state_buffer,
            flash_din_params_buffer,
            flash_model_params_buffer,
            flash_data_buffer,
        )
    }

    /// Allocate + initialise the GPU `gpu_io_step` peripheral buffers (Phase 1
    /// step 2b: relocated from `run_cosim`). Returns the seven buffers (UART
    /// state/params/channel, Wishbone-trace params/channel, bus-trace
    /// params/channel) plus the CPU-side `bus_lanes` decoders the drain uses.
    #[allow(clippy::type_complexity)]
    fn build_io_buffers(
        device: &metal::Device,
        aig: &AIG,
        netlistdb: &NetlistDB,
        script: &FlattenedScriptV1,
        config: &TestbenchConfig,
        gpio_map: &GpioMapping,
        state_size: usize,
        uart_configs: &[(String, usize, usize, u32)],
        sched_ticks_per_sys_clk_cycle: u64,
    ) -> (
        metal::Buffer,
        metal::Buffer,
        metal::Buffer,
        metal::Buffer,
        metal::Buffer,
        metal::Buffer,
        metal::Buffer,
        Vec<BusTraceLane>,
    ) {
        let n_uarts = uart_configs.len();

        // UartDecoderState[MAX_UARTS] (shared, persistent across ticks)
        let uart_state_buffer = device.new_buffer(
            (std::mem::size_of::<UartDecoderState>() * MAX_UARTS) as u64,
            MTLResourceOptions::StorageModeShared,
        );
        unsafe {
            let base = uart_state_buffer.contents() as *mut UartDecoderState;
            for i in 0..MAX_UARTS {
                let us = &mut *base.add(i);
                us.state = 0; // IDLE
                us.last_tx = 1; // TX line idle high
                us.start_cycle = 0;
                us.bits_received = 0;
                us.value = 0;
                us.current_cycle = 0;
            }
        }

        // UartParams (constant, with per-channel configs)
        let uart_params_buffer = device.new_buffer(
            std::mem::size_of::<UartParams>() as u64,
            MTLResourceOptions::StorageModeShared,
        );
        unsafe {
            let p = &mut *(uart_params_buffer.contents() as *mut UartParams);
            p.state_size = state_size as u32;
            p.n_uarts = n_uarts as u32;
            p._pad = [0; 2];
            p.channels = [UartPerChannelConfig::default(); MAX_UARTS];
            for (i, (_, tx_gpio, _, cpb)) in uart_configs.iter().enumerate() {
                p.channels[i].tx_out_pos = gpio_map.output_bits.get(tx_gpio).copied().unwrap_or(0);
                // GPU decoder counts scheduler edges, not clock cycles
                p.channels[i].cycles_per_bit = cpb * sched_ticks_per_sys_clk_cycle as u32;
            }
        }

        // UartChannel[MAX_UARTS] (shared ring buffers, CPU drains after each batch)
        let uart_channel_buffer = device.new_buffer(
            (std::mem::size_of::<UartChannel>() * MAX_UARTS) as u64,
            MTLResourceOptions::StorageModeShared,
        );
        unsafe {
            let base = uart_channel_buffer.contents() as *mut UartChannel;
            for i in 0..MAX_UARTS {
                let ch = &mut *base.add(i);
                ch.write_head = 0;
                ch.capacity = UART_CHANNEL_CAP as u32;
                ch._pad = [0; 2];
            }
        }

        // ── GPU Wishbone Bus Trace buffers ────────────────────────────────
        let wb_trace_params = build_wb_trace_params(aig, netlistdb, script);
        let wb_trace_params_buffer = device.new_buffer(
            std::mem::size_of::<WbTraceParams>() as u64,
            MTLResourceOptions::StorageModeShared,
        );
        unsafe {
            let p = &mut *(wb_trace_params_buffer.contents() as *mut WbTraceParams);
            *p = wb_trace_params;
        }

        // WbTraceChannel: header (16 bytes) + entries array
        let wb_channel_byte_size = std::mem::size_of::<WbTraceChannel>()
            + WB_TRACE_CHANNEL_CAP * std::mem::size_of::<WbTraceEntry>();
        let wb_trace_channel_buffer = device.new_buffer(
            wb_channel_byte_size as u64,
            MTLResourceOptions::StorageModeShared,
        );
        unsafe {
            let ch = &mut *(wb_trace_channel_buffer.contents() as *mut WbTraceChannel);
            ch.write_head = 0;
            ch.capacity = WB_TRACE_CHANNEL_CAP as u32;
            ch.current_tick = 0;
            ch.prev_flags = 0;
        }

        // ── GPU AHB/APB Bus Transaction Trace buffers (ADR 0013) ──────────
        // Always allocated (the kernel binds slots 6/7 unconditionally); the
        // kernel skips capture when n_buses == 0.
        let (bus_trace_params, bus_lanes) =
            build_bus_trace_params(aig, netlistdb, script, config.effective_bus_traces());
        let bus_trace_params_buffer = device.new_buffer(
            std::mem::size_of::<BusTraceParamsAll>() as u64,
            MTLResourceOptions::StorageModeShared,
        );
        unsafe {
            let p = &mut *(bus_trace_params_buffer.contents() as *mut BusTraceParamsAll);
            *p = bus_trace_params;
        }
        let bus_channel_byte_size = std::mem::size_of::<BusTraceChannel>()
            + BUS_TRACE_CHANNEL_CAP * std::mem::size_of::<BusTraceEntry>();
        let bus_trace_channel_buffer = device.new_buffer(
            bus_channel_byte_size as u64,
            MTLResourceOptions::StorageModeShared,
        );
        unsafe {
            let ch = &mut *(bus_trace_channel_buffer.contents() as *mut BusTraceChannel);
            ch.write_head = 0;
            ch.capacity = BUS_TRACE_CHANNEL_CAP as u32;
            ch.current_tick = 0;
            ch.prev_gate = 0;
        }

        (
            uart_state_buffer,
            uart_params_buffer,
            uart_channel_buffer,
            wb_trace_params_buffer,
            wb_trace_channel_buffer,
            bus_trace_params_buffer,
            bus_trace_channel_buffer,
            bus_lanes,
        )
    }

    /// Allocate + initialise the design-state, SRAM, blocks-program, and event
    /// buffers (Phase 1 step 2c: relocated from `run_cosim`). This covers the
    /// buffer-intrinsic init only — the xprop X-mask seed (both slots), the
    /// SRAM data/X-mask fills, the SRAM ELF preload, the no-copy wrappers over
    /// the `script` blocks UVecs, and the leaked-`Box` event buffer. The
    /// backend-agnostic stimulus deposits (reg_init / reset / constant_ports /
    /// set_flash_din) run after construction in `run_cosim`, through
    /// `state_mut()`.
    ///
    /// Returns the four GPU buffers, the blocks no-copy buffers, the event
    /// buffer, **and `event_buffer_ptr`** — the `*mut EventBuffer` from
    /// `Box::into_raw`. `MetalBackend::new` stores it in the `event_buffer_ptr`
    /// field; `Drop for MetalBackend` frees the box (Phase 1 step 3b-ii-a).
    #[allow(clippy::type_complexity)]
    fn build_state_buffers(
        device: &metal::Device,
        script: &FlattenedScriptV1,
        config: &TestbenchConfig,
        state_size: usize,
    ) -> (
        metal::Buffer,
        metal::Buffer,
        metal::Buffer,
        metal::Buffer,
        metal::Buffer,
        metal::Buffer,
        *mut crate::event_buffer::EventBuffer,
    ) {
        // States: [input state (state_size)] [output state (state_size)]
        let states_buffer = device.new_buffer(
            (2 * state_size * std::mem::size_of::<u32>()) as u64,
            MTLResourceOptions::StorageModeShared,
        );
        let states: &mut [u32] = unsafe {
            std::slice::from_raw_parts_mut(states_buffer.contents() as *mut u32, 2 * state_size)
        };
        states.fill(0);

        // Selective X-propagation (ADR 0016): seed the input slot's X-mask
        // half so uninitialised DFF/SRAM start as X. `expand_states_for_xprop`
        // builds the template (0xFFFF…=X for everything, cleared for primary
        // inputs); we copy its [value|xmask] block into the input slot. The
        // arrivals region (if timing is enabled) stays 0. Phase 3 will refine
        // undriven inputs back to X; here inputs start known, matching the
        // sim-path template. See docs/plans/cosim-xprop.md.
        if script.xprop_enabled {
            let rio = script.reg_io_state_size as usize;
            // Cosim template: X at uninitialised DFF/SRAM AND at every primary
            // input (undriven until a model/clock/reset/constant drives it —
            // #95 phase 3). `state_prep` clears the X-mask of each bit it drives
            // every edge, so driven inputs resolve to known and truly-undriven
            // ones stay X.
            let xmask = crate::sim::vcd_io::xprop_xmask_template_cosim(script);
            // Seed the X-mask half of BOTH slots. The reactive loop's first GPU op
            // every edge is `state_prep`, which copies output slot → input slot
            // (kernel_v1.metal); the output slot is therefore the initial condition
            // the first `simulate` reads. Seeding only the input slot lets edge-0
            // state_prep copy the all-zero output xmask over the seed, wiping
            // uninitialised-DFF X before it can propagate (the #95 "X never
            // surfaces" bug). The value halves stay zero (states.fill(0) above).
            states[rio..2 * rio].copy_from_slice(&xmask);
            states[state_size + rio..state_size + 2 * rio].copy_from_slice(&xmask);
            clilog::info!(
                "cosim X-propagation enabled: {} reg/io words, X-mask seeded (both slots) for uninitialised state",
                rio
            );
        }

        // SRAM storage. Allocate at least one word: a SRAM-less design has
        // `sram_storage_size == 0`, and `new_buffer(0)` returns a nil MTLBuffer
        // whose `.contents()` is null (foreign-types asserts non-null). Sizing to
        // `max(1)` while slicing to the real length keeps SRAM-free designs (e.g.
        // pure-logic cosim) working; the kernel guards SRAM reads on the cell map.
        let sram_data_len = (script.sram_storage_size as usize).max(1);
        let sram_data_buffer = device.new_buffer(
            (sram_data_len * std::mem::size_of::<u32>()) as u64,
            MTLResourceOptions::StorageModeShared,
        );
        let sram_data: &mut [u32] = unsafe {
            std::slice::from_raw_parts_mut(
                sram_data_buffer.contents() as *mut u32,
                script.sram_storage_size as usize,
            )
        };
        sram_data.fill(0);

        // SRAM X-mask shadow (ADR 0016): one word per SRAM cell, all X
        // (0xFFFF_FFFF) initially so unread/unwritten cells read as X.
        // Bound at buffer(7) of simulate_v1_stage; sized 1 (dummy) when
        // xprop is off — the kernel guards on is_x_capable before reading.
        let sram_xmask_len = if script.xprop_enabled {
            (script.sram_storage_size as usize).max(1)
        } else {
            1
        };
        let sram_xmask_buffer = device.new_buffer(
            (sram_xmask_len * std::mem::size_of::<u32>()) as u64,
            MTLResourceOptions::StorageModeShared,
        );
        {
            let m: &mut [u32] = unsafe {
                std::slice::from_raw_parts_mut(
                    sram_xmask_buffer.contents() as *mut u32,
                    sram_xmask_len,
                )
            };
            m.fill(if script.xprop_enabled { 0xFFFF_FFFF } else { 0 });
        }

        // SRAM preload (issue #80, ADR 0011 § "SRAM preload"). When
        // `sim_config.json` declares `sram_init`, parse the named ELF
        // file's PT_LOAD segments and pack them into the design's
        // single SRAM's backing region before the kernel launches.
        // Multi-SRAM preload is gated on a future schema extension —
        // errors cleanly today.
        if let Some(sram_init) = config.sram_init.as_ref() {
            let elf_path = std::path::Path::new(&sram_init.elf_path);
            let chunks = crate::sim::sram_preload::parse_elf_chunks(elf_path)
                .unwrap_or_else(|e| panic!("sram_init: {e}"));
            let (cellid, storage_offset) =
                crate::sim::sram_preload::resolve_single_sram(&script.sram_cell_storage_offsets)
                    .unwrap_or_else(|e| panic!("sram_init: {e}"));
            let total_bytes: usize = chunks.iter().map(|c| c.bytes.len()).sum();
            // Under xprop, clear the X-mask shadow for preloaded (known) cells so
            // they don't read X forever (#95). The shadow is parallel to sram_data.
            let mut xmask_slice = if script.xprop_enabled {
                Some(unsafe {
                    std::slice::from_raw_parts_mut(
                        sram_xmask_buffer.contents() as *mut u32,
                        sram_xmask_len,
                    )
                })
            } else {
                None
            };
            crate::sim::sram_preload::apply_chunks(
                sram_data,
                storage_offset,
                &chunks,
                xmask_slice.as_deref_mut(),
            )
            .unwrap_or_else(|e| panic!("sram_init: {e}"));
            clilog::info!(
                "SRAM preload: {} bytes from {} → cell {} at storage offset {}",
                total_bytes,
                elf_path.display(),
                cellid,
                storage_offset
            );
        }
        let _ = sram_data;

        // Create Metal buffers for script data (read-only, use UVec's Metal path)
        let uvec_device = Device::Metal(0);
        let blocks_start_ptr = script.blocks_start.as_uptr(uvec_device);
        let blocks_data_ptr = script.blocks_data.as_uptr(uvec_device);

        let blocks_start_buffer = device.new_buffer_with_bytes_no_copy(
            blocks_start_ptr as *const _,
            (script.blocks_start.len() * std::mem::size_of::<usize>()) as u64,
            MTLResourceOptions::StorageModeShared,
            None,
        );
        let blocks_data_buffer = device.new_buffer_with_bytes_no_copy(
            blocks_data_ptr as *const _,
            (script.blocks_data.len() * std::mem::size_of::<u32>()) as u64,
            MTLResourceOptions::StorageModeShared,
            None,
        );

        // Event buffer (for $stop/$finish/assertions). Leaked via
        // `Box::into_raw`; the caller owns the raw pointer and frees it.
        let event_buffer = Box::new(crate::event_buffer::EventBuffer::new());
        let event_buffer_ptr = Box::into_raw(event_buffer);
        let event_buffer_metal = device.new_buffer_with_bytes_no_copy(
            event_buffer_ptr as *const _,
            std::mem::size_of::<crate::event_buffer::EventBuffer>() as u64,
            MTLResourceOptions::StorageModeShared,
            None,
        );

        (
            states_buffer,
            sram_data_buffer,
            sram_xmask_buffer,
            blocks_start_buffer,
            blocks_data_buffer,
            event_buffer_metal,
            event_buffer_ptr,
        )
    }
}

impl CosimBackend for MetalBackend {
    /// Fat constructor (ADR 0017 Layer 1/2; Phase 1 step 3b-ii-a/b). Assembles
    /// the full Metal backend: `MetalSimulator::new` → `build_state_buffers` →
    /// `build_flash_buffers` → `build_io_buffers` → the timing-constraints
    /// buffer → the struct literal → the per-stage `write_params` pre-write.
    /// Returns the constructed backend AND the agnostic CPU bus decoders
    /// (`bus_lanes`) — `run_cosim`'s orchestration (ADR 0017 Layer 1) owns
    /// those, so they ride out separately rather than being trapped in the
    /// Metal struct.
    ///
    /// The order here is the exact pre-refactor order, preserved for
    /// bit-identicality. Buffer-intrinsic init (states.fill + xprop X-mask
    /// seed, SRAM data/X-mask fills + ELF preload, blocks no-copy, the leaked
    /// event box) happens inside the builders; the backend-agnostic stimulus
    /// deposits (reg_init / reset / constant_ports / set_flash_din) run
    /// *after* construction at the call site, through `state_mut()`.
    #[allow(clippy::too_many_arguments)]
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
        arrival_state_offset: u32,
        timing_constraints: &Option<Vec<u32>>,
    ) -> (Self, Vec<BusTraceLane>) {
        let simulator = MetalSimulator::new(num_major_stages);

        // Design-state, SRAM, blocks-program, and event buffers. The builder
        // owns the buffer-intrinsic init and returns the leaked `*mut
        // EventBuffer`, which becomes the `event_buffer_ptr` field (freed by
        // `Drop for MetalBackend`).
        let (
            states_buffer,
            sram_data_buffer,
            sram_xmask_buffer,
            blocks_start_buffer,
            blocks_data_buffer,
            event_buffer_metal,
            event_buffer_ptr,
        ) = MetalBackend::build_state_buffers(&simulator.device, script, config, state_size);

        // GPU SPI-flash IO buffers.
        let (
            flash_state_buffer,
            flash_din_params_buffer,
            flash_model_params_buffer,
            flash_data_buffer,
        ) = MetalBackend::build_flash_buffers(
            &simulator.device,
            config,
            script,
            state_size,
            gpio_map,
        );

        // GPU IO peripheral buffers (uart/wb/bus). Returns the 7 buffers + the
        // agnostic CPU bus decoders.
        let (
            uart_state_buffer,
            uart_params_buffer,
            uart_channel_buffer,
            wb_trace_params_buffer,
            wb_trace_channel_buffer,
            bus_trace_params_buffer,
            bus_trace_channel_buffer,
            bus_lanes,
        ) = MetalBackend::build_io_buffers(
            &simulator.device,
            aig,
            netlistdb,
            script,
            config,
            gpio_map,
            state_size,
            uart_configs,
            sched_ticks_per_sys_clk_cycle,
        );

        // Timing constraint buffer for GPU-side setup/hold checking.
        let timing_constraints_buffer = timing_constraints.as_ref().map(|buf| {
            simulator.device.new_buffer_with_data(
                buf.as_ptr() as *const _,
                (buf.len() * std::mem::size_of::<u32>()) as u64,
                MTLResourceOptions::StorageModeShared,
            )
        });

        let n_uarts = uart_configs.len();
        let backend = MetalBackend {
            sim: simulator,
            schedule: None,
            state_size,
            sram_len: script.sram_storage_size as usize,
            num_blocks,
            num_major_stages,
            arrival_state_offset,
            states_buffer,
            blocks_start_buffer,
            blocks_data_buffer,
            event_buffer_metal,
            sram_data_buffer,
            sram_xmask_buffer,
            timing_constraints_buffer,
            flash_state_buffer,
            flash_din_params_buffer,
            flash_model_params_buffer,
            flash_data_buffer,
            uart_state_buffer,
            uart_params_buffer,
            uart_channel_buffer,
            wb_trace_params_buffer,
            wb_trace_channel_buffer,
            bus_trace_params_buffer,
            bus_trace_channel_buffer,
            vcd_ring: None,
            n_uarts,
            uart_read_heads: vec![0u32; n_uarts],
            wb_trace_read_head: 0,
            bus_trace_read_head: 0,
            event_buffer_ptr,
        };

        // Pre-write params for all simulation stages (they don't change between
        // ticks). Moved here from `run_cosim` in P1.3b-ii-b — it ran right
        // after `new` before, so this is order-equivalent and bit-identical.
        for stage_i in 0..num_major_stages {
            backend.sim.write_params(
                stage_i,
                num_blocks,
                num_major_stages,
                state_size,
                0,
                arrival_state_offset,
            );
        }

        (backend, bus_lanes)
    }

    /// Metal-only GPU kernel profiling (forwards to the simulator). Overrides
    /// the trait's default no-op — a CPU backend has no GPU kernels to profile.
    fn profile_kernels(&self, num_ticks: usize) {
        self.sim.profile_gpu_kernels(
            num_ticks,
            self.num_blocks,
            self.num_major_stages,
            self.state_size,
            &self.blocks_start_buffer,
            &self.blocks_data_buffer,
            &self.sram_data_buffer,
            &self.states_buffer,
            &self.event_buffer_metal,
            self.schedule(),
            &self.flash_state_buffer,
            &self.flash_din_params_buffer,
            &self.flash_model_params_buffer,
            &self.flash_data_buffer,
            &self.uart_state_buffer,
            &self.uart_params_buffer,
            &self.uart_channel_buffer,
            &self.wb_trace_channel_buffer,
            &self.wb_trace_params_buffer,
            &self.bus_trace_channel_buffer,
            &self.bus_trace_params_buffer,
            &self.sram_xmask_buffer,
            self.timing_constraints_buffer.as_ref(),
        );
    }

    fn init_schedule(
        &mut self,
        per_edge_ops: Vec<Vec<BitOp>>,
        state_size: u32,
        xmask_state_offset: u32,
        gcd_ps: u64,
    ) {
        let device = &self.sim.device;
        let mut edge_buffers = Vec::with_capacity(per_edge_ops.len());
        let mut edge_ops_lens = Vec::with_capacity(per_edge_ops.len());
        for ops in &per_edge_ops {
            let len = ops.len();
            let ops_buf = create_ops_buffer(device, ops);
            // `xmask_state_offset` is reg_io_state_size when xprop is on and 0
            // otherwise — the sentinel state_prep uses to decide whether to
            // clear each driven bit's X-mask (driven ⇒ known, #95 ph3).
            let params =
                create_prep_params_buffer(device, state_size, len as u32, 0, 0, xmask_state_offset);
            edge_buffers.push((params, ops_buf));
            edge_ops_lens.push(len);
        }
        self.schedule = Some(ScheduleBuffers {
            edge_buffers,
            edge_ops_lens,
            gcd_ps,
        });
    }

    #[inline]
    fn edge_ops_mut(&mut self, edge_idx: usize) -> &mut [BitOp] {
        self.schedule().edge_ops_mut(edge_idx)
    }

    #[inline]
    fn edge_ops(&self, edge_idx: usize) -> &[BitOp] {
        self.schedule().edge_ops(edge_idx)
    }

    #[inline]
    fn edges_per_period(&self) -> usize {
        self.schedule().edge_buffers.len()
    }

    #[inline]
    fn gcd_ps(&self) -> u64 {
        self.schedule().gcd_ps
    }

    fn state(&self) -> &[u32] {
        unsafe {
            std::slice::from_raw_parts(
                self.states_buffer.contents() as *const u32,
                2 * self.state_size,
            )
        }
    }

    fn state_mut(&mut self) -> &mut [u32] {
        unsafe {
            std::slice::from_raw_parts_mut(
                self.states_buffer.contents() as *mut u32,
                2 * self.state_size,
            )
        }
    }

    fn sram(&self) -> &[u32] {
        unsafe {
            std::slice::from_raw_parts(
                self.sram_data_buffer.contents() as *const u32,
                self.sram_len,
            )
        }
    }

    fn flash_set_in_reset(&mut self, in_reset: bool) {
        // Drive the reset line for EVERY QSPI memory instance, not just slot 0,
        // else the extra memories stay stuck in reset (d_i=0x0F) forever.
        unsafe {
            let n_slots =
                self.flash_state_buffer.length() as usize / std::mem::size_of::<FlashState>();
            let slots = self.flash_state_buffer.contents() as *mut FlashState;
            for s in 0..n_slots {
                (*slots.add(s)).in_reset = if in_reset { 1 } else { 0 };
            }
        }
    }

    fn enable_vcd_ring(&mut self) {
        if self.vcd_ring.is_some() {
            return;
        }
        let ring_bytes = BATCH_SIZE * 2 * self.state_size * std::mem::size_of::<u32>();
        self.vcd_ring = Some(
            self.sim
                .device
                .new_buffer(ring_bytes as u64, MTLResourceOptions::StorageModeShared),
        );
        clilog::info!(
            "VCD ring buffer: {} ticks × {} words = {:.1} MB",
            BATCH_SIZE,
            2 * self.state_size,
            ring_bytes as f64 / (1024.0 * 1024.0)
        );
    }

    #[inline]
    fn vcd_snapshot(&self, edge_in_batch: usize) -> &[u32] {
        let ring = self
            .vcd_ring
            .as_ref()
            .expect("vcd_snapshot called without enable_vcd_ring");
        let slot_words = 2 * self.state_size;
        unsafe {
            std::slice::from_raw_parts(
                (ring.contents() as *const u32).add(edge_in_batch * slot_words),
                slot_words,
            )
        }
    }

    fn flash_d_i(&self) -> u8 {
        unsafe {
            let fs = &*(self.flash_state_buffer.contents() as *const FlashState);
            fs.d_i
        }
    }

    fn flash_debug_snapshot(&self) -> FlashDebug {
        unsafe {
            let fs = &*(self.flash_state_buffer.contents() as *const FlashState);
            FlashDebug {
                d_i: fs.d_i,
                data_width: fs.data_width,
                prev_csn: fs.prev_csn,
                in_reset: fs.in_reset,
                bit_count: fs.bit_count,
                byte_count: fs.byte_count,
                addr: fs.addr,
                curr_byte: fs.curr_byte,
                command: fs.command,
                out_buffer: fs.out_buffer,
                prev_clk: fs.prev_clk,
                prev_d_out: fs.prev_d_out,
                last_error_cmd: fs.last_error_cmd,
                model_prev_csn: fs.model_prev_csn,
            }
        }
    }

    fn debug_flash_raw_tick0(&self) {
        unsafe {
            let fs = &*(self.flash_state_buffer.contents() as *const FlashState);
            // Dump raw bytes at flash state location (Metal-ABI debug artifact).
            let raw = std::slice::from_raw_parts(
                self.flash_state_buffer.contents() as *const u8,
                std::mem::size_of::<FlashState>(),
            );
            eprintln!("  FlashState raw bytes (tick 0): {:02X?}", raw);
            eprintln!(
                "  FlashState fields: bit_count={}, byte_count={}, data_width={}, addr=0x{:08X}",
                fs.bit_count, fs.byte_count, fs.data_width, fs.addr
            );
            eprintln!(
                "  FlashState fields: curr_byte=0x{:02X}, command=0x{:02X}, out_buffer=0x{:02X}",
                fs.curr_byte, fs.command, fs.out_buffer
            );
            eprintln!(
                "  FlashState fields: prev_clk={}, prev_csn={}, d_i=0x{:02X}",
                fs.prev_clk, fs.prev_csn, fs.d_i
            );
            eprintln!("  FlashState fields: prev_d_out=0x{:02X}, in_reset={}, last_error_cmd={}, model_prev_csn={}",
                fs.prev_d_out, fs.in_reset, fs.last_error_cmd, fs.model_prev_csn);
            eprintln!(
                "  FlashState offsetof d_i = {}",
                (&fs.d_i as *const u8 as usize) - (fs as *const FlashState as usize)
            );
        }
    }

    fn drain_uart_tx(&mut self) -> Vec<(usize, u8)> {
        let mut out = Vec::new();
        unsafe {
            let base = self.uart_channel_buffer.contents() as *const UartChannel;
            for i in 0..self.n_uarts {
                let channel = &*base.add(i);
                while self.uart_read_heads[i] < channel.write_head {
                    let byte = channel.data[(self.uart_read_heads[i] % channel.capacity) as usize];
                    out.push((i, byte));
                    self.uart_read_heads[i] += 1;
                }
            }
        }
        out
    }

    fn drain_bus_beats(&mut self) -> Vec<crate::sim::models::bus_trace::RawBeat> {
        use crate::sim::models::bus_trace::RawBeat;
        let mut out = Vec::new();
        unsafe {
            let ch = &*(self.bus_trace_channel_buffer.contents() as *const BusTraceChannel);
            let entries_ptr = (self.bus_trace_channel_buffer.contents() as *const u8)
                .add(std::mem::size_of::<BusTraceChannel>())
                as *const BusTraceEntry;
            while self.bus_trace_read_head < ch.write_head {
                let idx = (self.bus_trace_read_head % ch.capacity) as usize;
                let e = &*entries_ptr.add(idx);
                let bus_id = (e.flags >> 8) & 0xFF;
                out.push(RawBeat {
                    tick: e.tick as u64,
                    bus_id,
                    write: (e.flags & 1) != 0,
                    err: (e.flags >> 1) & 1 != 0,
                    addr: e.addr as u64,
                    wdata: e.wdata as u64,
                    rdata: e.rdata as u64,
                });
                self.bus_trace_read_head += 1;
            }
        }
        out
    }

    fn drain_wb_trace_debug(&mut self) {
        unsafe {
            let ch = &*(self.wb_trace_channel_buffer.contents() as *const WbTraceChannel);
            let entries_ptr = (self.wb_trace_channel_buffer.contents() as *const u8).add(16)
                as *const WbTraceEntry;
            while self.wb_trace_read_head < ch.write_head {
                let idx = (self.wb_trace_read_head % ch.capacity) as usize;
                let e = &*entries_ptr.add(idx);
                let ibus_cyc = e.flags & 1;
                let ibus_stb = (e.flags >> 1) & 1;
                let dbus_cyc = (e.flags >> 2) & 1;
                let dbus_stb = (e.flags >> 3) & 1;
                let dbus_we = (e.flags >> 4) & 1;
                let spi_ack = (e.flags >> 5) & 1;
                let sram_ack = (e.flags >> 6) & 1;
                let _csr_ack = (e.flags >> 7) & 1;
                if ibus_cyc != 0 && ibus_stb != 0 {
                    eprintln!(
                        "WB T{:>5}: IBUS adr=0x{:08X} rdata=0x{:08X} spi_ack={} sram_ack={}",
                        e.tick, e.ibus_adr, e.ibus_rdata, spi_ack, sram_ack
                    );
                }
                if dbus_cyc != 0 && dbus_stb != 0 {
                    eprintln!(
                        "WB T{:>5}: DBUS adr=0x{:08X} we={} sram_ack={}",
                        e.tick, e.dbus_adr, dbus_we, sram_ack
                    );
                }
                if ibus_cyc == 0 && ibus_stb == 0 && dbus_cyc == 0 && dbus_stb == 0 {
                    eprintln!("WB T{:>5}: bus idle (flags=0x{:02X})", e.tick, e.flags);
                }
                self.wb_trace_read_head += 1;
            }
        }
    }

    fn uart_decoder_debug(&self, ch: usize) -> (u32, u32) {
        unsafe {
            let us = &*(self.uart_state_buffer.contents() as *const UartDecoderState).add(ch);
            (us.state, us.current_cycle)
        }
    }

    fn run_edges(&self, batch: usize, schedule_offset: usize) -> u64 {
        self.sim.encode_and_commit_gpu_batch(
            batch,
            self.num_blocks,
            self.num_major_stages,
            self.state_size,
            &self.blocks_start_buffer,
            &self.blocks_data_buffer,
            &self.sram_data_buffer,
            &self.states_buffer,
            &self.event_buffer_metal,
            self.schedule(),
            schedule_offset,
            &self.flash_state_buffer,
            &self.flash_din_params_buffer,
            &self.flash_model_params_buffer,
            &self.flash_data_buffer,
            &self.uart_state_buffer,
            &self.uart_params_buffer,
            &self.uart_channel_buffer,
            &self.wb_trace_channel_buffer,
            &self.wb_trace_params_buffer,
            &self.bus_trace_channel_buffer,
            &self.bus_trace_params_buffer,
            &self.sram_xmask_buffer,
            self.timing_constraints_buffer.as_ref(),
            self.arrival_state_offset,
            self.vcd_ring.as_ref(),
        )
    }

    #[inline]
    fn wait(&self, token: u64) {
        self.sim.spin_wait(token);
    }
}

// ── Public Entry Point ───────────────────────────────────────────────────────

/// Run a GPU co-simulation with testbench config.
///
/// Timing data must be loaded before this is called (via the
/// `--timing-ir` CLI flag during `setup::load_design`). The previous
/// `config.timing.sdf_file` fallback was removed in WS3 phase 3.4 — the
/// hand-rolled SDF parser is gone and the cosim subcommand does not yet
/// re-route SDF input through `opensta-to-ir` (deferred follow-up).
pub fn run_cosim(
    design: &mut LoadedDesign,
    config: &TestbenchConfig,
    opts: &CosimOpts,
    timing_constraints: &Option<Vec<u32>>,
) -> CosimResult {
    run_cosim_generic::<MetalBackend>(design, config, opts, timing_constraints)
}
