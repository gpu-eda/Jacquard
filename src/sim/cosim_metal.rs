// SPDX-FileCopyrightText: Copyright (c) 2024 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0
//! Metal GPU co-simulation engine with SPI flash, UART, and Wishbone bus trace models.
//!
//! Extracted from the `gpu_sim` binary. All IO models (flash, UART, bus trace)
//! run as GPU kernels — no per-tick CPU interaction needed.

use std::collections::HashMap;

use crate::aig::{DriverType, AIG};
use crate::flatten::FlattenedScriptV1;
use crate::sim::setup::LoadedDesign;
// Shared with the `xsources` driven-input computation so cosim's GPIO
// mapping and the static X-source query stay consistent (issue #98).
use crate::sim::x_sources::parse_gpio_index;
use crate::testbench::{CppSpiFlash, PortMapping, TestbenchConfig, UartEvent};
use metal::{
    CommandQueue, ComputePipelineState, Device as MTLDevice, MTLResourceOptions, MTLSize,
    SharedEvent,
};
use netlistdb::{Direction, GeneralPinName, NetlistDB};
use ulib::{AsUPtr, Device};

/// Runtime options for co-simulation.
pub struct CosimOpts {
    /// Limit number of simulated clock edges. One full clock cycle = 2
    /// edges (posedge + negedge) for single-domain. Matches chipflow's
    /// cxxrtl `num_steps` 1:1.
    pub max_clock_edges: Option<usize>,
    pub num_blocks: usize,
    pub flash_verbose: bool,
    pub check_with_cpu: bool,
    pub gpu_profile: bool,
    pub clock_period: Option<u64>,
    /// Path to write stimulus VCD (all primary inputs driven by cosim).
    pub stimulus_vcd: Option<std::path::PathBuf>,
    /// Path to write the output VCD (chip outputs + traced nets). Timed
    /// with per-signal arrival offsets when timing data is available;
    /// otherwise functional (transitions at clock edges).
    pub output_vcd: Option<std::path::PathBuf>,
    /// Path to dump DFF Q-values per cycle (for debugging/comparison).
    /// Forces single-tick mode for the first N cycles.
    pub dump_dff: Option<std::path::PathBuf>,
    /// Number of cycles to dump DFF states (default 20).
    pub dump_dff_cycles: usize,
    /// Optional path to a recorded `remote_bitbang` byte stream. When
    /// set alongside a `jtag` peripheral in `TestbenchConfig`, cosim
    /// instantiates a `JtagReplayModel` driving the configured pins
    /// from this file. Discussion #77 stage 1.
    pub jtag_replay: Option<std::path::PathBuf>,
    /// Cosim edges per JTAG stream byte. Default 4; see
    /// `JtagReplayModel` for the rationale.
    pub jtag_hold_cycles: u32,
    /// Path to run-parameters file for reproducible jitter. See ADR 0012.
    pub run_params: Option<std::path::PathBuf>,
    /// Path to write decoded AHB/APB bus transactions as CSV. Requires
    /// at least one `bus_traces` entry in the config. See ADR 0013.
    pub bus_trace_csv: Option<std::path::PathBuf>,
}

/// Result of a co-simulation run.
pub struct CosimResult {
    pub passed: bool,
    pub uart_events: Vec<UartEvent>,
    pub edges_simulated: usize,
}

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

/// Bit set/clear operation (must match Metal shader BitOp struct).
#[repr(C)]
#[derive(Clone, Copy)]
struct BitOp {
    position: u32, // bit position in state buffer
    value: u32,    // 0 = clear, 1 = set
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

/// GPU-side flash state (must match Metal FlashState struct exactly).
#[repr(C)]
#[derive(Clone, Copy)]
struct FlashState {
    bit_count: i32,
    byte_count: i32,
    data_width: u32,
    addr: u32,
    curr_byte: u8,
    command: u8,
    out_buffer: u8,
    _pad1: u8,
    prev_clk: u32,
    prev_csn: u32,
    d_i: u8,
    _pad2: [u8; 3],
    prev_d_out: u8,
    _pad3: [u8; 3],
    in_reset: u32,
    last_error_cmd: u32,
    model_prev_csn: u32,
}

/// Parameters for gpu_apply_flash_din kernel (must match Metal FlashDinParams).
#[repr(C)]
struct FlashDinParams {
    d_in_pos: [u32; 4],
    has_flash: u32,
    // X-mask word offset (= reg_io_state_size); 0 when xprop off. The flash
    // MISO bits are primary inputs driven here (not via state_prep's edge
    // ops), so this kernel must clear their X-mask too, else they stay X
    // (seeded by the cosim template) and the SPI read drowns in X (#95 ph3).
    xmask_state_offset: u32,
}

/// Parameters for gpu_flash_model_step kernel (must match Metal FlashModelParams).
#[repr(C)]
struct FlashModelParams {
    state_size: u32,
    clk_out_pos: u32,
    csn_out_pos: u32,
    d_out_pos: [u32; 4],
    flash_data_size: u32,
}

const MAX_UARTS: usize = 4;
const UART_CHANNEL_CAP: usize = 4096;

/// GPU-side UART decoder state (must match Metal UartDecoderState).
#[repr(C)]
struct UartDecoderState {
    state: u32, // 0=IDLE, 1=START, 2=DATA, 3=STOP
    last_tx: u32,
    start_cycle: u32,
    bits_received: u32,
    value: u32,
    current_cycle: u32,
}

/// Per-channel config within UartParams (must match Metal UartPerChannelConfig).
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct UartPerChannelConfig {
    tx_out_pos: u32,
    cycles_per_bit: u32,
}

/// Parameters for UART in gpu_io_step kernel (must match Metal UartParams).
#[repr(C)]
struct UartParams {
    state_size: u32,
    n_uarts: u32,
    _pad: [u32; 2],
    channels: [UartPerChannelConfig; MAX_UARTS],
}

/// GPU-side UART channel (must match Metal UartChannel).
#[repr(C)]
struct UartChannel {
    write_head: u32,
    capacity: u32,
    _pad: [u32; 2],
    data: [u8; UART_CHANNEL_CAP],
}

const WB_TRACE_MAX_ADR_BITS: usize = 30;
const WB_TRACE_MAX_DAT_BITS: usize = 32;
const WB_TRACE_CHANNEL_CAP: usize = 16384;

/// Parameters for Wishbone bus trace (must match Metal WbTraceParams).
#[repr(C)]
struct WbTraceParams {
    ibus_cyc_pos: u32,
    ibus_stb_pos: u32,
    ibus_adr_pos: [u32; WB_TRACE_MAX_ADR_BITS],
    ibus_rdata_pos: [u32; WB_TRACE_MAX_DAT_BITS],
    dbus_cyc_pos: u32,
    dbus_stb_pos: u32,
    dbus_we_pos: u32,
    dbus_adr_pos: [u32; WB_TRACE_MAX_ADR_BITS],
    spiflash_ack_pos: u32,
    sram_ack_pos: u32,
    csr_ack_pos: u32,
    has_trace: u32,
}

/// Per-tick bus snapshot (must match Metal WbTraceEntry).
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct WbTraceEntry {
    tick: u32,
    flags: u32,
    ibus_adr: u32,
    ibus_rdata: u32,
    dbus_adr: u32,
}

/// GPU→CPU bus trace ring buffer header (must match Metal WbTraceChannel).
#[repr(C)]
struct WbTraceChannel {
    write_head: u32,
    capacity: u32,
    current_tick: u32,
    prev_flags: u32,
    // entries[capacity] follow in memory
}

// ── Config-driven AHB/APB bus transaction trace (ADR 0013) ──────────────────

const MAX_BUS_TRACES: usize = 4;
const BUS_TRACE_MAX_ADR_BITS: usize = 32;
const BUS_TRACE_MAX_DAT_BITS: usize = 32;
const BUS_TRACE_CHANNEL_CAP: usize = 16384;

/// Per-bus signal positions (must match Metal BusTraceParams).
#[repr(C)]
#[derive(Clone, Copy)]
struct BusTraceParams {
    protocol: u32,
    addr_bits: u32,
    data_bits: u32,
    sel_pos: u32,
    enable_pos: u32,
    ready_pos: u32,
    write_pos: u32,
    resp_pos: u32,
    addr_pos: [u32; BUS_TRACE_MAX_ADR_BITS],
    wdata_pos: [u32; BUS_TRACE_MAX_DAT_BITS],
    rdata_pos: [u32; BUS_TRACE_MAX_DAT_BITS],
}

impl Default for BusTraceParams {
    fn default() -> Self {
        Self {
            protocol: 0,
            addr_bits: 0,
            data_bits: 0,
            sel_pos: 0xFFFFFFFF,
            enable_pos: 0xFFFFFFFF,
            ready_pos: 0xFFFFFFFF,
            write_pos: 0xFFFFFFFF,
            resp_pos: 0xFFFFFFFF,
            addr_pos: [0xFFFFFFFF; BUS_TRACE_MAX_ADR_BITS],
            wdata_pos: [0xFFFFFFFF; BUS_TRACE_MAX_DAT_BITS],
            rdata_pos: [0xFFFFFFFF; BUS_TRACE_MAX_DAT_BITS],
        }
    }
}

/// All-bus params block (must match Metal BusTraceParamsAll).
#[repr(C)]
struct BusTraceParamsAll {
    n_buses: u32,
    _pad: [u32; 3],
    buses: [BusTraceParams; MAX_BUS_TRACES],
}

/// Compact raw beat captured by the GPU (must match Metal BusTraceEntry).
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct BusTraceEntry {
    tick: u32,
    flags: u32,
    addr: u32,
    wdata: u32,
    rdata: u32,
}

/// GPU→CPU bus trace ring buffer header (must match Metal BusTraceChannel).
#[repr(C)]
struct BusTraceChannel {
    write_head: u32,
    capacity: u32,
    current_tick: u32,
    prev_gate: u32,
    // entries[capacity] follow in memory
}

/// CPU-side per-bus lane: pairs a GPU capture slot with its name and
/// protocol decoder. Vec index == GPU `bus_id` packed into entry flags.
struct BusTraceLane {
    name: String,
    decoder: crate::sim::models::bus_trace::BusTraceDecoder,
}

/// Batch size for GPU-only simulation (no per-tick CPU interaction).
const BATCH_SIZE: usize = 1024;

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

// ── GPIO ↔ State Buffer Mapping ──────────────────────────────────────────────

/// Per-clock-domain flag bit positions in the packed state buffer.
///
/// Each clock domain (identified by its netlistdb pin ID) has its own set of
/// posedge/negedge flag bits that gate DFF writeout for that domain.
struct ClockDomainFlags {
    /// Netlistdb pin ID for this clock (key from `clock_pin2aigpins`).
    #[allow(dead_code)]
    clock_pinid: usize,
    /// Human-readable name from pin (e.g. "io$clk$i").
    name: String,
    /// State positions for this domain's posedge flags.
    posedge_flag_bits: Vec<u32>,
    /// State positions for this domain's negedge flags.
    negedge_flag_bits: Vec<u32>,
    /// State position of the clock input port itself (if it's a primary input).
    clock_input_pos: Option<u32>,
    /// GPIO index if this clock is driven from a GPIO.
    clock_gpio: Option<usize>,
}

/// Maps GPIO pin indices to bit positions in the packed u32 state buffer.
pub(crate) struct GpioMapping {
    /// gpio_in[idx] → (aigpin, state bit position)
    input_bits: HashMap<usize, u32>,
    /// gpio_out[idx] → state bit position in output_map
    output_bits: HashMap<usize, u32>,
    /// Per-clock-domain flag bit positions (grouped by netlistdb pin ID).
    clock_domains: Vec<ClockDomainFlags>,
    /// Named input port → state bit position (for non-GPIO ports like por_l, resetb_h)
    named_input_bits: HashMap<String, u32>,
}

/// Set a single bit in a packed u32 state buffer.
#[inline]
fn set_bit(state: &mut [u32], pos: u32, val: u8) {
    let word = &mut state[(pos >> 5) as usize];
    let mask = 1u32 << (pos & 31);
    if val != 0 {
        *word |= mask;
    } else {
        *word &= !mask;
    }
}

/// Clear a single bit in a packed u32 state buffer.
#[inline]
fn clear_bit(state: &mut [u32], pos: u32) {
    state[(pos >> 5) as usize] &= !(1u32 << (pos & 31));
}

/// Convert a 0/1 bit value to a VCD scalar value.
#[inline]
fn bit_to_vcd_value(bit: u8) -> vcd_ng::Value {
    if bit != 0 {
        vcd_ng::Value::V1
    } else {
        vcd_ng::Value::V0
    }
}

/// Build GPIO-to-state-buffer mapping from AIG + FlattenedScript.
///
/// When `port_mapping` is provided, maps GPIO indices to port names explicitly
/// (for designs like ChipFlow that use named ports instead of gpio_in[N]/gpio_out[N]).
/// Falls back to parsing gpio_in[N]/gpio_out[N] from pin names when no mapping given.
pub(crate) fn build_gpio_mapping(
    aig: &AIG,
    netlistdb: &NetlistDB,
    script: &FlattenedScriptV1,
    port_mapping: Option<&PortMapping>,
) -> GpioMapping {
    let mut input_bits: HashMap<usize, u32> = HashMap::new();
    let mut output_bits: HashMap<usize, u32> = HashMap::new();
    let mut named_input_bits: HashMap<String, u32> = HashMap::new();

    // Group clock flags by pinid → ClockDomainFlags.
    // BTreeMap gives deterministic iteration order (ascending pinid).
    let mut clock_domain_map: std::collections::BTreeMap<usize, (Vec<u32>, Vec<u32>)> =
        std::collections::BTreeMap::new();

    // Build reverse lookup: port_name → gpio_index for port_mapping mode
    let input_name_to_gpio: HashMap<String, usize> = port_mapping
        .map(|pm| {
            pm.inputs
                .iter()
                .filter_map(|(k, v)| k.parse::<usize>().ok().map(|idx| (v.clone(), idx)))
                .collect()
        })
        .unwrap_or_default();
    let output_name_to_gpio: HashMap<String, usize> = port_mapping
        .map(|pm| {
            pm.outputs
                .iter()
                .filter_map(|(k, v)| k.parse::<usize>().ok().map(|idx| (v.clone(), idx)))
                .collect()
        })
        .unwrap_or_default();

    // Map input ports → state buffer positions
    for (aigpin_idx, driv) in aig.drivers.iter().enumerate() {
        match driv {
            DriverType::InputPort(pinid) => {
                let pin_name = netlistdb.pinnames[*pinid].dbg_fmt_pin();
                // Try port_mapping first, then fall back to gpio_in[N] parsing
                let gpio_idx = input_name_to_gpio
                    .get(&pin_name)
                    .copied()
                    .or_else(|| parse_gpio_index(&pin_name, "gpio_in"));
                if let Some(gpio_idx) = gpio_idx {
                    if let Some(&pos) = script.input_map.get(&aigpin_idx) {
                        input_bits.insert(gpio_idx, pos);
                        clilog::debug!(
                            "input[{}] = pin '{}' → state pos {}",
                            gpio_idx,
                            pin_name,
                            pos
                        );
                    }
                }
                // Also store by name for constant_ports
                if let Some(&pos) = script.input_map.get(&aigpin_idx) {
                    named_input_bits.insert(pin_name.clone(), pos);
                }
            }
            DriverType::InputClockFlag(pinid, is_negedge) => {
                if let Some(&pos) = script.input_map.get(&aigpin_idx) {
                    let entry = clock_domain_map
                        .entry(*pinid)
                        .or_insert_with(|| (Vec::new(), Vec::new()));
                    if *is_negedge == 0 {
                        entry.0.push(pos);
                    } else {
                        entry.1.push(pos);
                    }
                    let pin_name = netlistdb.pinnames[*pinid].dbg_fmt_pin();
                    clilog::debug!(
                        "ClockFlag aigpin={} pin={} (pinid={}) negedge={} pos={}",
                        aigpin_idx,
                        pin_name,
                        pinid,
                        is_negedge,
                        pos
                    );
                }
            }
            _ => {}
        }
    }

    // Reverse-lookup map for extra observables: name → aigpin. Used
    // below to resolve top-level `inout` ports — those have
    // Direction::Unknown so they don't appear in output_map under
    // their own name, but the AIG-side bidir-pad observability code
    // (see `aig.rs::resolve_bi_24t_observable_base`) registers the
    // core-drive-out aigpin under the synthetic `<port>__out` name.
    // Looking that up gives peripheral observers (UART RX decoder,
    // GPIO output watchers) the right signal. See #79.
    let mut extra_observable_by_name: HashMap<String, usize> = HashMap::new();
    for (&aigpin, names) in aig.extra_observable_names.iter() {
        for name in names {
            extra_observable_by_name.insert(name.clone(), aigpin);
        }
    }

    // Map output ports → state buffer positions
    for i in netlistdb.cell2pin.iter_set(0) {
        let pin_name = netlistdb.pinnames[i].dbg_fmt_pin();
        // Try port_mapping first, then fall back to gpio_out[N] parsing
        let gpio_idx = output_name_to_gpio
            .get(&pin_name)
            .copied()
            .or_else(|| parse_gpio_index(&pin_name, "gpio_out"));
        if let Some(gpio_idx) = gpio_idx {
            let mut aigpin_iv = aig.pin2aigpin_iv[i];
            // Top-level inout: pin2aigpin_iv on a wafer.space-style
            // `bi_24t` pad resolves to the input-side `Y` aigpin
            // (PAD→core), which is non-MAX but absent from
            // output_map. The OUTPUT direction's aigpin (core `A`→PAD)
            // is registered under `<pin>__out` in
            // extra_observable_names; falling back unconditionally
            // (when found) overrides the input-side resolution with
            // the actual output driver. Safe when the synthetic name
            // is absent: aigpin_iv stays as-is and we fall through to
            // the existing diagnostic.
            if netlistdb.pindirect[i] == Direction::Unknown {
                let synth_name = format!("{pin_name}__out");
                if let Some(&aigpin) = extra_observable_by_name.get(&synth_name) {
                    aigpin_iv = aigpin;
                    clilog::debug!(
                        "output[{}] inout pin '{}' resolved via bidir-pad \
                         observable '{}' → aigpin {}",
                        gpio_idx,
                        pin_name,
                        synth_name,
                        aigpin
                    );
                }
            }
            if aigpin_iv == usize::MAX {
                clilog::info!(
                    "output[{}] pin '{}' has no AIG connection (usize::MAX)",
                    gpio_idx,
                    pin_name
                );
                continue;
            }
            if aigpin_iv <= 1 {
                clilog::info!(
                    "output[{}] pin '{}' is constant (aigpin_iv={})",
                    gpio_idx,
                    pin_name,
                    aigpin_iv
                );
                continue;
            }
            if let Some(&pos) = script.output_map.get(&aigpin_iv) {
                output_bits.insert(gpio_idx, pos);
                clilog::debug!(
                    "output[{}] = pin '{}' → state pos {}",
                    gpio_idx,
                    pin_name,
                    pos
                );
            } else if let Some(&pos) = script.output_map.get(&(aigpin_iv ^ 1)) {
                output_bits.insert(gpio_idx, pos);
                clilog::debug!(
                    "output[{}] = pin '{}' → state pos {} (flipped inv)",
                    gpio_idx,
                    pin_name,
                    pos
                );
            } else {
                clilog::info!(
                    "output[{}] pin '{}' aigpin_iv={} (aigpin={}) not in output_map (dir={:?})",
                    gpio_idx,
                    pin_name,
                    aigpin_iv,
                    aigpin_iv >> 1,
                    netlistdb.pindirect[i]
                );
            }
        }
    }

    // Build ClockDomainFlags from grouped clock flags
    // Also resolve GPIO index for each clock domain by looking up the clock pin
    // in the input_bits mapping.
    let mut clock_domains: Vec<ClockDomainFlags> = Vec::new();
    // Build reverse: pinid → (pin_name, gpio_idx, state_pos) from input_bits
    let mut pinid_to_input_info: HashMap<usize, (String, Option<usize>, Option<u32>)> =
        HashMap::new();
    for (aigpin_idx, driv) in aig.drivers.iter().enumerate() {
        if let DriverType::InputPort(pinid) = driv {
            let pin_name = netlistdb.pinnames[*pinid].dbg_fmt_pin();
            let gpio_idx = input_name_to_gpio
                .get(&pin_name)
                .copied()
                .or_else(|| parse_gpio_index(&pin_name, "gpio_in"));
            let pos = script.input_map.get(&aigpin_idx).copied();
            pinid_to_input_info.insert(*pinid, (pin_name, gpio_idx, pos));
        }
    }

    for (pinid, (posedge_bits, negedge_bits)) in &clock_domain_map {
        let pin_name = netlistdb.pinnames[*pinid].dbg_fmt_pin();
        // Try to find the GPIO index and state position for this clock pin
        let (clock_gpio, clock_input_pos) =
            if let Some((_, gpio_idx, pos)) = pinid_to_input_info.get(pinid) {
                (*gpio_idx, *pos)
            } else {
                (None, None)
            };
        clock_domains.push(ClockDomainFlags {
            clock_pinid: *pinid,
            name: pin_name.clone(),
            posedge_flag_bits: posedge_bits.clone(),
            negedge_flag_bits: negedge_bits.clone(),
            clock_input_pos,
            clock_gpio,
        });
        clilog::info!(
            "Clock domain '{}' (pinid={}): {} posedge flags, {} negedge flags, gpio={:?}",
            pin_name,
            pinid,
            posedge_bits.len(),
            negedge_bits.len(),
            clock_gpio
        );
    }

    let total_posedge: usize = clock_domains
        .iter()
        .map(|d| d.posedge_flag_bits.len())
        .sum();
    let total_negedge: usize = clock_domains
        .iter()
        .map(|d| d.negedge_flag_bits.len())
        .sum();
    clilog::info!(
        "GPIO mapping: {} inputs, {} outputs, {} clock domains ({} posedge flags, {} negedge flags)",
        input_bits.len(),
        output_bits.len(),
        clock_domains.len(),
        total_posedge,
        total_negedge
    );

    clilog::info!("Named input ports: {} mapped", named_input_bits.len());

    GpioMapping {
        input_bits,
        output_bits,
        clock_domains,
        named_input_bits,
    }
}

/// Resolve an internal signal name to its output state bit position.
///
/// Searches DFF Q output pins (which appear as cell output pins driving named nets)
/// for names containing `pattern`. Returns the state buffer bit position, or 0xFFFFFFFF.
fn resolve_signal_pos(
    aig: &AIG,
    netlistdb: &NetlistDB,
    script: &FlattenedScriptV1,
    pattern: &str,
) -> u32 {
    // Strategy: scan ALL cell pins looking for names matching pattern.
    // DFF Q outputs are cell output pins whose names match the net they drive.
    for cellid in 0..netlistdb.num_cells {
        for pinid in netlistdb.cell2pin.iter_set(cellid) {
            let pin_name = netlistdb.pinnames[pinid].dbg_fmt_pin();
            if !pin_name.contains(pattern) {
                continue;
            }
            // Only care about output pins (Q of DFFs) — check for output_map presence
            let aigpin_iv = aig.pin2aigpin_iv[pinid];
            if aigpin_iv == usize::MAX || aigpin_iv <= 1 {
                continue;
            }
            if let Some(&pos) = script.output_map.get(&aigpin_iv) {
                return pos;
            }
            if let Some(&pos) = script.output_map.get(&(aigpin_iv ^ 1)) {
                return pos;
            }
        }
    }
    0xFFFFFFFF
}

/// Resolve a bus signal with bit index, e.g. "ibus__adr" with bit 5 → "ibus__adr[5]".
fn resolve_bus_signal(
    aig: &AIG,
    netlistdb: &NetlistDB,
    script: &FlattenedScriptV1,
    base_pattern: &str,
    bit: usize,
) -> u32 {
    let pattern = format!("{}[{}]", base_pattern, bit);
    resolve_signal_pos(aig, netlistdb, script, &pattern)
}

/// Build WbTraceParams by resolving all bus signal names to state positions.
fn build_wb_trace_params(
    aig: &AIG,
    netlistdb: &NetlistDB,
    script: &FlattenedScriptV1,
) -> WbTraceParams {
    let mut params = WbTraceParams {
        ibus_cyc_pos: 0xFFFFFFFF,
        ibus_stb_pos: 0xFFFFFFFF,
        ibus_adr_pos: [0xFFFFFFFF; WB_TRACE_MAX_ADR_BITS],
        ibus_rdata_pos: [0xFFFFFFFF; WB_TRACE_MAX_DAT_BITS],
        dbus_cyc_pos: 0xFFFFFFFF,
        dbus_stb_pos: 0xFFFFFFFF,
        dbus_we_pos: 0xFFFFFFFF,
        dbus_adr_pos: [0xFFFFFFFF; WB_TRACE_MAX_ADR_BITS],
        spiflash_ack_pos: 0xFFFFFFFF,
        sram_ack_pos: 0xFFFFFFFF,
        csr_ack_pos: 0xFFFFFFFF,
        has_trace: 0,
    };

    // ibus (instruction bus)
    params.ibus_cyc_pos = resolve_signal_pos(aig, netlistdb, script, "cpu.fetch.ibus__cyc");
    params.ibus_stb_pos = resolve_signal_pos(aig, netlistdb, script, "cpu.fetch.ibus__stb");
    for i in 0..WB_TRACE_MAX_ADR_BITS {
        params.ibus_adr_pos[i] =
            resolve_bus_signal(aig, netlistdb, script, "cpu.fetch.ibus__adr", i);
    }
    for i in 0..WB_TRACE_MAX_DAT_BITS {
        params.ibus_rdata_pos[i] =
            resolve_bus_signal(aig, netlistdb, script, "cpu.fetch.ibus_rdata", i);
    }

    // dbus (data bus)
    params.dbus_cyc_pos = resolve_signal_pos(aig, netlistdb, script, "cpu.loadstore.dbus__cyc");
    params.dbus_stb_pos = resolve_signal_pos(aig, netlistdb, script, "cpu.loadstore.dbus__stb");
    params.dbus_we_pos = resolve_signal_pos(aig, netlistdb, script, "cpu.loadstore.dbus__we");
    for i in 0..WB_TRACE_MAX_ADR_BITS {
        params.dbus_adr_pos[i] =
            resolve_bus_signal(aig, netlistdb, script, "cpu.loadstore.dbus__adr", i);
    }

    // peripheral acks
    params.spiflash_ack_pos =
        resolve_signal_pos(aig, netlistdb, script, "spiflash.ctrl.wb_bus__ack");
    params.sram_ack_pos = resolve_signal_pos(aig, netlistdb, script, "sram.wb_bus__ack");
    params.csr_ack_pos = resolve_signal_pos(aig, netlistdb, script, "wb_to_csr.wb_bus__ack");

    // Check if we resolved any signals
    let found = [
        params.ibus_cyc_pos,
        params.ibus_stb_pos,
        params.dbus_cyc_pos,
    ]
    .iter()
    .filter(|&&p| p != 0xFFFFFFFF)
    .count();
    if found > 0 {
        params.has_trace = 1;
        let ibus_adr_count = params
            .ibus_adr_pos
            .iter()
            .filter(|&&p| p != 0xFFFFFFFF)
            .count();
        let ibus_rdata_count = params
            .ibus_rdata_pos
            .iter()
            .filter(|&&p| p != 0xFFFFFFFF)
            .count();
        let dbus_adr_count = params
            .dbus_adr_pos
            .iter()
            .filter(|&&p| p != 0xFFFFFFFF)
            .count();
        clilog::info!(
            "WB trace: ibus_cyc={} ibus_stb={} ibus_adr={}/30 ibus_rdata={}/32",
            params.ibus_cyc_pos != 0xFFFFFFFF,
            params.ibus_stb_pos != 0xFFFFFFFF,
            ibus_adr_count,
            ibus_rdata_count
        );
        clilog::info!(
            "WB trace: dbus_cyc={} dbus_stb={} dbus_we={} dbus_adr={}/30",
            params.dbus_cyc_pos != 0xFFFFFFFF,
            params.dbus_stb_pos != 0xFFFFFFFF,
            params.dbus_we_pos != 0xFFFFFFFF,
            dbus_adr_count
        );
        clilog::info!(
            "WB trace: spiflash_ack={} sram_ack={} csr_ack={}",
            params.spiflash_ack_pos != 0xFFFFFFFF,
            params.sram_ack_pos != 0xFFFFFFFF,
            params.csr_ack_pos != 0xFFFFFFFF
        );
    } else {
        clilog::info!("WB trace: no bus signals found in netlist, tracing disabled");
    }

    params
}

/// Build the bus-trace GPU params block and the parallel CPU decoder
/// lanes from the configured bus traces. The Vec index of each returned
/// lane equals the GPU `bus_id` packed into entry flags, so the drain
/// loop can route each beat to the right decoder.
///
/// Only APB3 is wired in Phase 1; AHB-Lite / AHB5 entries are skipped
/// with a warning (see `docs/plans/bus-transaction-tracing.md`).
fn build_bus_trace_params(
    aig: &AIG,
    netlistdb: &NetlistDB,
    script: &FlattenedScriptV1,
    configs: &[crate::testbench::BusTraceConfig],
) -> (BusTraceParamsAll, Vec<BusTraceLane>) {
    use crate::sim::models::bus_trace::{pin_basename, BusTraceDecoder};
    use crate::sim::trace_signals::resolve_to_state_pos;
    use crate::testbench::BusProtocol;

    let mut all = BusTraceParamsAll {
        n_buses: 0,
        _pad: [0; 3],
        buses: [BusTraceParams::default(); MAX_BUS_TRACES],
    };
    let mut lanes: Vec<BusTraceLane> = Vec::new();

    let resolve =
        |name: &str| resolve_to_state_pos(aig, netlistdb, script, name).unwrap_or(0xFFFFFFFF);

    for cfg in configs.iter() {
        if all.n_buses as usize >= MAX_BUS_TRACES {
            clilog::warn!(
                "bus-trace: more than {} buses configured; `{}` and later ignored",
                MAX_BUS_TRACES,
                cfg.name
            );
            break;
        }
        match cfg.protocol {
            BusProtocol::Apb3 => {}
            other => {
                clilog::warn!(
                    "bus-trace `{}`: protocol {:?} not yet implemented (Phase 2); skipping",
                    cfg.name,
                    other
                );
                continue;
            }
        }

        let addr_bits = cfg.addr_bits.min(BUS_TRACE_MAX_ADR_BITS);
        let data_bits = cfg.data_bits.min(BUS_TRACE_MAX_DAT_BITS);
        if cfg.addr_bits > BUS_TRACE_MAX_ADR_BITS || cfg.data_bits > BUS_TRACE_MAX_DAT_BITS {
            clilog::warn!(
                "bus-trace `{}`: addr/data width capped at {}/{} bits (Phase 1)",
                cfg.name,
                BUS_TRACE_MAX_ADR_BITS,
                BUS_TRACE_MAX_DAT_BITS
            );
        }

        let mut p = BusTraceParams {
            protocol: 0, // APB3
            addr_bits: addr_bits as u32,
            data_bits: data_bits as u32,
            sel_pos: resolve(&pin_basename(cfg, "psel")),
            enable_pos: resolve(&pin_basename(cfg, "penable")),
            ready_pos: resolve(&pin_basename(cfg, "pready")),
            write_pos: resolve(&pin_basename(cfg, "pwrite")),
            resp_pos: resolve(&pin_basename(cfg, "pslverr")),
            ..Default::default()
        };
        let abase = pin_basename(cfg, "paddr");
        for b in 0..addr_bits {
            p.addr_pos[b] = resolve(&format!("{abase}[{b}]"));
        }
        let wbase = pin_basename(cfg, "pwdata");
        let rbase = pin_basename(cfg, "prdata");
        for b in 0..data_bits {
            p.wdata_pos[b] = resolve(&format!("{wbase}[{b}]"));
            p.rdata_pos[b] = resolve(&format!("{rbase}[{b}]"));
        }

        if p.sel_pos == 0xFFFFFFFF || p.enable_pos == 0xFFFFFFFF {
            clilog::warn!(
                "bus-trace `{}`: psel/penable did not resolve (prefix `{}`) — \
                 this bus will not capture. Check the prefix / `signals` overrides.",
                cfg.name,
                cfg.prefix
            );
        } else {
            let n_addr = p.addr_pos.iter().filter(|&&x| x != 0xFFFFFFFF).count();
            clilog::info!(
                "bus-trace `{}` (APB3): psel/penable resolved, addr {}/{} bits, \
                 pready={} pslverr={}",
                cfg.name,
                n_addr,
                addr_bits,
                p.ready_pos != 0xFFFFFFFF,
                p.resp_pos != 0xFFFFFFFF
            );
        }

        let idx = all.n_buses as usize;
        all.buses[idx] = p;
        all.n_buses += 1;
        lanes.push(BusTraceLane {
            name: cfg.name.clone(),
            decoder: BusTraceDecoder::new(cfg.protocol),
        });
    }

    (all, lanes)
}

/// Write decoded bus transactions to a CSV file (ADR 0013). Columns:
/// `tick,bus,protocol,dir,addr,data,resp,burst`. Addresses and data are
/// hex (`0x…`); `burst` is `beat/len` for AHB bursts, empty otherwise.
fn write_bus_trace_csv(
    path: &std::path::Path,
    txns: &[(u32, crate::sim::models::bus_trace::BusTransaction)],
    lanes: &[BusTraceLane],
) -> std::io::Result<()> {
    use crate::testbench::BusProtocol;
    use std::io::Write;

    crate::sim::vcd_io::ensure_parent_dir(path)?;
    let f = std::fs::File::create(path)?;
    let mut w = std::io::BufWriter::new(f);
    writeln!(w, "tick,bus,protocol,dir,addr,data,resp,burst")?;
    for (bus_id, t) in txns {
        let name = lanes
            .get(*bus_id as usize)
            .map(|l| l.name.as_str())
            .unwrap_or("?");
        let proto = match t.protocol {
            BusProtocol::Apb3 => "apb3",
            BusProtocol::AhbLite => "ahb-lite",
            BusProtocol::Ahb5 => "ahb5",
        };
        let burst = match t.burst {
            Some(b) => match b.len {
                Some(len) => format!("{}/{}", b.beat, len),
                None => format!("{}/?", b.beat),
            },
            None => String::new(),
        };
        writeln!(
            w,
            "{},{},{},{},0x{:X},0x{:X},{},{}",
            t.tick,
            name,
            proto,
            t.dir.as_str(),
            t.addr,
            t.data,
            t.resp.as_str(),
            burst
        )?;
    }
    w.flush()?;
    Ok(())
}

/// Write flash data input to GPIO state.
fn set_flash_din(state: &mut [u32], gpio_map: &GpioMapping, d0_gpio: usize, din: u8) {
    for i in 0..4 {
        if let Some(&pos) = gpio_map.input_bits.get(&(d0_gpio + i)) {
            set_bit(state, pos, (din >> i) & 1);
        }
    }
}

// ── Multi-Clock Scheduler ────────────────────────────────────────────────────

/// Per-tick edge activity for each clock domain.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct TickEdges {
    /// For each domain index: (has_falling_edge, has_rising_edge).
    domain_edges: Vec<(bool, bool)>,
}

/// Schedules clock edges across multiple domains at GCD granularity.
///
/// For N clock domains with half-periods H1, H2, ..., the GCD tick is
/// `gcd(H1, H2, ...)` and the schedule repeats every `lcm(H1, H2, ...) / gcd` ticks.
/// Each tick in the schedule records which domains have falling/rising edges.
struct MultiClockScheduler {
    /// GCD of all half-periods in picoseconds. One scheduler edge advances
    /// simulated time by `gcd_ps`.
    gcd_ps: u64,
    /// One entry per GCD tick in the LCM cycle.
    schedule: Vec<TickEdges>,
}

/// Compute GCD of two numbers.
fn gcd(a: u64, b: u64) -> u64 {
    if b == 0 {
        a
    } else {
        gcd(b, a % b)
    }
}

/// Compute LCM of two numbers.
fn lcm(a: u64, b: u64) -> u64 {
    a / gcd(a, b) * b
}

/// A clock domain's timing parameters for scheduling.
struct ClockDomainTiming {
    /// Half-period in picoseconds (period_ps / 2).
    half_period_ps: u64,
    /// Phase offset in picoseconds.
    phase_offset_ps: u64,
    /// Index into GpioMapping::clock_domains.
    domain_index: usize,
}

impl MultiClockScheduler {
    /// Build a scheduler from clock domain timings.
    ///
    /// For a single clock with no phase offset, produces a schedule of length 2
    /// alternating between falling (posedge_flag=0) and rising (posedge_flag=1)
    /// edges.
    fn new(timings: &[ClockDomainTiming]) -> Self {
        assert!(!timings.is_empty(), "At least one clock domain is required");

        // Compute GCD of all half-periods and phase offsets
        let mut gcd_ps = timings[0].half_period_ps;
        for t in &timings[1..] {
            gcd_ps = gcd(gcd_ps, t.half_period_ps);
        }
        // Include phase offsets in GCD (non-zero offsets need finer granularity)
        for t in timings {
            if t.phase_offset_ps > 0 {
                gcd_ps = gcd(gcd_ps, t.phase_offset_ps);
            }
        }

        // Compute LCM of all full periods (= 2 * half_period)
        // We need a full period to capture both falling and rising edges.
        let mut lcm_ps = timings[0].half_period_ps * 2;
        for t in &timings[1..] {
            lcm_ps = lcm(lcm_ps, t.half_period_ps * 2);
        }

        let schedule_len = (lcm_ps / gcd_ps) as usize;
        assert!(
            schedule_len <= 1_000_000,
            "Multi-clock schedule too large ({} ticks). \
             Clock periods may not be commensurable at this resolution.",
            schedule_len
        );

        let num_domains = timings.iter().map(|t| t.domain_index + 1).max().unwrap_or(0);
        let mut schedule = Vec::with_capacity(schedule_len);

        for tick in 0..schedule_len {
            let tick_ps = tick as u64 * gcd_ps;
            let mut domain_edges = vec![(false, false); num_domains];

            for timing in timings.iter() {
                let hp = timing.half_period_ps;
                let offset = timing.phase_offset_ps;
                // Adjust tick_ps by phase offset
                if tick_ps < offset {
                    continue;
                }
                let adjusted = tick_ps - offset;
                if adjusted % hp != 0 {
                    continue;
                }
                let edge_count = adjusted / hp;
                // Index by domain_index (position in clock_domains) so
                // build_edge_ops can iterate clock_domains directly.
                if edge_count % 2 == 0 {
                    domain_edges[timing.domain_index].0 = true; // falling edge
                } else {
                    domain_edges[timing.domain_index].1 = true; // rising edge
                }
            }

            schedule.push(TickEdges { domain_edges });
        }

        clilog::info!(
            "MultiClockScheduler: gcd={}ps, schedule_len={} ticks, {} domains",
            gcd_ps,
            schedule_len,
            num_domains
        );

        Self { gcd_ps, schedule }
    }

    /// Build BitOps for a single scheduler edge.
    ///
    /// Handles all clock domains' edges at this tick uniformly: a domain with
    /// a falling edge sets its clk input low and asserts negedge_flag; a domain
    /// with a rising edge sets its clk input high and asserts posedge_flag.
    /// Domains with no edge at this tick leave their clk input untouched and
    /// deassert both flags.
    fn build_edge_ops(
        &self,
        tick_idx: usize,
        gpio_map: &GpioMapping,
        reset_gpio: usize,
        reset_val: u8,
        constant_inputs: &HashMap<String, u8>,
        constant_ports: &HashMap<String, u8>,
        model_driven_positions: &[u32],
    ) -> Vec<BitOp> {
        let tick = &self.schedule[tick_idx % self.schedule.len()];
        let mut ops = Vec::new();

        // Reset
        ops.push(BitOp {
            position: gpio_map.input_bits[&reset_gpio],
            value: reset_val as u32,
        });

        // Per-domain clock + edge flags
        for (i, domain) in gpio_map.clock_domains.iter().enumerate() {
            let (has_fall, has_rise) = tick.domain_edges[i];

            // Set clock input. Falling and rising at the same scheduler tick
            // can't both be true for one domain (they're encoded as different
            // edge_count parities), so the order doesn't matter — but
            // defensively: rising wins, since posedge eval is what DFFs sample.
            if has_fall {
                if let Some(pos) = domain.clock_input_pos {
                    ops.push(BitOp {
                        position: pos,
                        value: 0,
                    });
                }
            }
            if has_rise {
                if let Some(pos) = domain.clock_input_pos {
                    ops.push(BitOp {
                        position: pos,
                        value: 1,
                    });
                }
            }

            // Edge flags: posedge for domains rising at this edge, negedge for
            // falling, both deasserted otherwise.
            for &pos in &domain.posedge_flag_bits {
                ops.push(BitOp {
                    position: pos,
                    value: if has_rise { 1 } else { 0 },
                });
            }
            for &pos in &domain.negedge_flag_bits {
                ops.push(BitOp {
                    position: pos,
                    value: if has_fall { 1 } else { 0 },
                });
            }
        }

        // Constant inputs
        for (gpio_str, val) in constant_inputs {
            if let Ok(gpio_idx) = gpio_str.parse::<usize>() {
                if let Some(&pos) = gpio_map.input_bits.get(&gpio_idx) {
                    ops.push(BitOp {
                        position: pos,
                        value: *val as u32,
                    });
                }
            }
        }
        // Named constant ports
        for (port_name, val) in constant_ports {
            if let Some(&pos) = gpio_map.named_input_bits.get(port_name) {
                ops.push(BitOp {
                    position: pos,
                    value: *val as u32,
                });
            }
        }

        // Placeholders for peripheral-model-driven inputs. Initial value 0;
        // updated each batch in `patch_model_driven_in_ops` to reflect the
        // current model state.
        for &pos in model_driven_positions {
            ops.push(BitOp {
                position: pos,
                value: 0,
            });
        }

        ops
    }
}

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

// ── CPU baseline for verification ────────────────────────────────────────────

/// Direct AIG evaluator: evaluates all AIG pins by traversing the AND-gate
/// tree directly, bypassing the boomerang/FlattenedScript entirely.
/// Returns a map from AIG pin → value (0 or 1).
///
/// This is used to diagnose whether the AIG construction is correct
/// (if direct eval matches FlattenedScript → bug is in AIG; if they differ → bug in flattening).
fn eval_aig_direct(aig: &AIG, input_state: &[u32], script: &FlattenedScriptV1) -> Vec<u8> {
    let mut pin_values = vec![0u8; aig.num_aigpins + 1];
    // pin 0 = constant 0 (Tie0)

    // Initialize all primary inputs from input_state using input_map
    for (&aigpin, &pos) in &script.input_map {
        let word = (pos / 32) as usize;
        let bit = pos & 31;
        if word < input_state.len() {
            pin_values[aigpin] = ((input_state[word] >> bit) & 1) as u8;
        }
    }

    // Topological traversal using topo_traverse_generic
    // endpoints=None means traverse ALL AIG pins
    // is_primary_input = set of pins that are inputs (InputPort, InputClockFlag, DFF Q, SRAM)
    let primary_input_set: indexmap::IndexSet<usize> = script.input_map.keys().copied().collect();
    let order = aig.topo_traverse_generic(None, Some(&primary_input_set));

    // Evaluate each AND gate in topological order
    for &pin in &order {
        if primary_input_set.contains(&pin) {
            // Already initialized from input_state
            continue;
        }
        if let DriverType::AndGate(a_iv, b_iv) = aig.drivers[pin] {
            let a_pin = a_iv >> 1;
            let a_inv = (a_iv & 1) as u8;
            let b_pin = b_iv >> 1;
            let b_inv = (b_iv & 1) as u8;
            let a_val = if a_pin == 0 {
                0
            } else {
                pin_values[a_pin] ^ a_inv
            };
            let b_val = if b_pin == 0 {
                0
            } else {
                pin_values[b_pin] ^ b_inv
            };
            pin_values[pin] = a_val & b_val;
        }
        // Non-AndGate, non-primary-input pins (shouldn't happen in traversal) stay 0
    }

    pin_values
}

/// Compare direct AIG evaluation with FlattenedScript output at a given tick.
/// Prints diagnostic information about any mismatches.
fn compare_aig_vs_flattened(
    aig: &AIG,
    input_state: &[u32],
    output_state: &[u32],
    script: &FlattenedScriptV1,
    state_size: usize,
    tick: usize,
    netlistdb: Option<&NetlistDB>,
) {
    let pin_values = eval_aig_direct(aig, input_state, script);

    // Compare DFF D inputs: direct AIG eval vs FlattenedScript output
    let mut dff_mismatch_count = 0;
    let mut dff_match_count = 0;
    let mut dff_en_active_count = 0;
    let mut dff_d_ones_direct = 0;
    let mut dff_d_ones_flat = 0;
    let mut first_mismatches: Vec<String> = Vec::new();

    for (_cellid, dff) in aig.dffs.iter() {
        let d_pin = dff.d_iv >> 1;
        let d_inv = (dff.d_iv & 1) as u8;
        let en_pin = dff.en_iv >> 1;
        let en_inv = (dff.en_iv & 1) as u8;

        // Direct AIG evaluation
        let d_val_direct = if d_pin == 0 {
            0
        } else {
            pin_values[d_pin] ^ d_inv
        };
        let en_val_direct = if en_pin == 0 {
            0
        } else {
            pin_values[en_pin] ^ en_inv
        };

        if en_val_direct != 0 {
            dff_en_active_count += 1;
        }
        if d_val_direct != 0 {
            dff_d_ones_direct += 1;
        }

        // FlattenedScript result: read from output_state
        if let Some(&pos) = script.output_map.get(&dff.d_iv) {
            let word = (pos / 32) as usize;
            let bit = pos & 31;
            let flat_val = if word < state_size {
                ((output_state[word] >> bit) & 1) as u8
            } else {
                0
            };
            if flat_val != 0 {
                dff_d_ones_flat += 1;
            }

            if d_val_direct != flat_val {
                dff_mismatch_count += 1;
                if first_mismatches.len() < 10 {
                    first_mismatches.push(format!(
                        "DFF d_iv={} pos={} en={}: direct={} flat={} (d_pin={} en_pin={} en_val={})",
                        dff.d_iv, pos, dff.en_iv, d_val_direct, flat_val, d_pin, en_pin, en_val_direct
                    ));
                }
            } else {
                dff_match_count += 1;
            }
        }
    }

    // Also compare primary outputs
    let mut po_mismatch_count = 0;
    for (&aigpin_iv, &pos) in &script.output_map {
        let pin = aigpin_iv >> 1;
        let inv = (aigpin_iv & 1) as u8;
        let direct_val = if pin == 0 { 0u8 } else { pin_values[pin] ^ inv };

        let word = (pos / 32) as usize;
        let bit = pos & 31;
        let flat_val = if word < state_size {
            ((output_state[word] >> bit) & 1) as u8
        } else {
            0
        };

        if direct_val != flat_val {
            po_mismatch_count += 1;
        }
    }

    eprintln!("=== AIG vs FlattenedScript comparison at tick {} ===", tick);
    eprintln!(
        "  DFFs: {} match, {} MISMATCH (out of {} total)",
        dff_match_count,
        dff_mismatch_count,
        aig.dffs.len()
    );
    eprintln!(
        "  DFF en_active={}, d_ones_direct={}, d_ones_flat={}",
        dff_en_active_count, dff_d_ones_direct, dff_d_ones_flat
    );
    eprintln!("  All output_map entries: {} mismatches", po_mismatch_count);
    for m in &first_mismatches {
        eprintln!("  MISMATCH: {}", m);
    }

    // Also check: how many DFF Q values are 1 in the input state?
    let mut q_ones = 0;
    let mut q_hash: u64 = 0;
    let mut q_one_entries: Vec<(usize, u32, String)> = Vec::new(); // (cellid, pos, name)
    for (&cellid, dff) in aig.dffs.iter() {
        if let Some(&pos) = script.input_map.get(&dff.q) {
            let word = (pos / 32) as usize;
            let bit = pos & 31;
            if word < input_state.len() && ((input_state[word] >> bit) & 1) != 0 {
                q_ones += 1;
                let name = if let Some(ndb) = netlistdb {
                    format!("{:?}", ndb.cellnames[cellid])
                } else {
                    format!("cell_{}", cellid)
                };
                q_one_entries.push((cellid, pos, name));
                q_hash = q_hash
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(pos as u64);
            }
        }
    }
    q_one_entries.sort_by_key(|&(cellid, _, _)| cellid);
    eprintln!(
        "  DFF Q values = 1 in input_state: {}/{} (hash=0x{:016X})",
        q_ones,
        aig.dffs.len(),
        q_hash
    );
    if q_ones <= 100 {
        for (cellid, pos, name) in &q_one_entries {
            eprintln!("  Q=1: cell {} pos={} = {}", cellid, pos, name);
        }
    }
}

/// CPU partition executor for `--check-with-cpu` verification. Delegates to
/// the canonical implementation in `cpu_reference` — these were a drifting
/// duplicate of it; consolidated as the first step of the cosim
/// backend-portability seam extraction (#105, ADR 0017). The compute is
/// identical; only the diagnostic-print detail differs.
fn simulate_block_v1(
    script: &[u32],
    input_state: &[u32],
    output_state: &mut [u32],
    sram_data: &mut [u32],
) {
    crate::sim::cpu_reference::simulate_block_v1(
        script, input_state, output_state, sram_data, false,
    );
}

fn simulate_block_v1_diag(
    script: &[u32],
    input_state: &[u32],
    output_state: &mut [u32],
    sram_data: &mut [u32],
) {
    crate::sim::cpu_reference::simulate_block_v1(
        script, input_state, output_state, sram_data, true,
    );
}

// ── Cosim backend seam ─────────────────────────────────────────────────────

/// Design execution + state ownership for cosim. One impl per GPU/CPU backend
/// (ADR 0017, *Amendment 2026-06-07*). The backend-agnostic orchestration
/// (multi-clock scheduler, peripheral models, VCD, ring drains, batch-size
/// policy) lives above this seam and drives the backend through it.
///
/// **Batch-granular by construction.** Measurements (ADR 0017) show Metal runs
/// 100% batched on GPU-peripheral designs, so a per-edge method would regress
/// it ~1000×. `run_edges` therefore runs *N* consecutive scheduler edges in
/// one dispatch; the orchestration decides *N* (`force_single_edge`).
///
/// **The backend owns its schedule storage** (opaque to orchestration): built
/// once via [`CosimBackend::init_schedule`] from a backend-agnostic description,
/// mutated through [`CosimBackend::edge_ops_mut`]. On Metal that is a slice over
/// the shared `MTLBuffer` (zero-copy; the write *is* the upload); a CUDA/HIP
/// backend backs it with a host mirror + dirty-flag + lazy upload.
///
/// Phase 0 (this commit) defines the subset the Metal cosim driver actually
/// calls. The CPU per-edge `state_prep` method (plan's Phase 1) lands with
/// `CpuBackend`; on Metal, state_prep is encoded *inside* `run_edges`' command
/// buffer, so it is not a separate orchestration call. See
/// `docs/plans/cosim-backend-portability.md`.
trait CosimBackend {
    /// Materialise the backend's native per-edge schedule storage *once* from
    /// the backend-agnostic ops description (one `Vec<BitOp>` per scheduler
    /// edge). The orchestration keeps only scalars (`edges_per_period`,
    /// `gcd_ps`) afterwards — never a parallel copy the backend re-materialises.
    fn init_schedule(
        &mut self,
        per_edge_ops: Vec<Vec<BitOp>>,
        state_size: u32,
        xmask_state_offset: u32,
        gcd_ps: u64,
    );
    /// Mutable view of one edge's ops (reset / model-driven / clock-edge
    /// patching). Zero-copy over shared memory on Metal. Takes `&mut self` so
    /// the seam enforces exclusive access at compile time, even though the
    /// Metal storage underneath is interior-mutable shared memory.
    fn edge_ops_mut(&mut self, edge_idx: usize) -> &mut [BitOp];
    /// Read-only view of one edge's ops (e.g. the `--check-with-cpu` replay).
    fn edge_ops(&self, edge_idx: usize) -> &[BitOp];
    /// Number of scheduler edges per LCM period (the schedule repeats).
    fn edges_per_period(&self) -> usize;
    /// Picoseconds per scheduler edge (= `MultiClockScheduler::gcd_ps`).
    fn gcd_ps(&self) -> u64;

    // NOTE: design-state / SRAM read accessors (`state()`, `sram()`) and the
    // mutable `state_mut()` land in step 3/5 alongside the diagnostic-read
    // routing and `CpuBackend` — kept off the trait here to match exactly what
    // the Metal driver calls today.

    /// Set the SPI-flash reset line for the upcoming batch. Both the GPU flash
    /// kernel and the CPU `CppSpiFlash` model honour it. Called before
    /// `run_edges`; `d_i` (MISO) never crosses the seam — each backend injects
    /// it internally (ADR 0017, Layer 3: input drives are backend-internal).
    fn flash_set_in_reset(&mut self, in_reset: bool);

    /// Enable per-edge output snapshotting for VCD: subsequent `run_edges`
    /// calls record each edge's `[input | output]` state into a backend-owned
    /// ring, drained via [`CosimBackend::vcd_snapshot`]. No-op if already on.
    fn enable_vcd_ring(&mut self);
    /// Read one edge's snapshot from the VCD ring: `[input_state |
    /// output_state]`, `2 × effective_state_size` words. Valid only for
    /// `edge_in_batch < batch` of the most recent `run_edges`, and only when
    /// [`CosimBackend::enable_vcd_ring`] was called. (CpuBackend, N=1, fills
    /// slot 0 each `run_edges`.)
    fn vcd_snapshot(&self, edge_in_batch: usize) -> &[u32];

    /// Run `batch` consecutive scheduler edges starting at `schedule_offset`
    /// in one dispatch, snapshotting each output slot into the VCD ring when
    /// enabled. Returns the completion token to pass to [`CosimBackend::wait`].
    fn run_edges(&self, batch: usize, schedule_offset: usize) -> u64;
    /// Block until the dispatch identified by `token` has completed.
    fn wait(&self, token: u64);
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
        // FlashState (shared, persistent across ticks)
        let flash_state_buffer = device.new_buffer(
            std::mem::size_of::<FlashState>() as u64,
            MTLResourceOptions::StorageModeShared,
        );
        unsafe {
            let fs = &mut *(flash_state_buffer.contents() as *mut FlashState);
            *fs = std::mem::zeroed();
            fs.data_width = 1; // SPI single-bit mode (reset by posedge_csn, but first tx has none)
            fs.prev_csn = 1; // CSN starts high (deselected)
            fs.model_prev_csn = 1; // Model internal edge detection starts high
            fs.d_i = 0x0F; // Flash output starts high
            fs.in_reset = 1; // Start in reset
                             // Verify write
            let verify = std::ptr::read_volatile(&fs.d_i);
            assert_eq!(
                verify, 0x0F,
                "FlashState.d_i not written correctly: got 0x{:02X}",
                verify
            );
            clilog::info!(
                "FlashState init: d_i=0x{:02X}, data_width={}, prev_csn={}, in_reset={}",
                fs.d_i,
                fs.data_width,
                fs.prev_csn,
                fs.in_reset
            );
        }

        // FlashDinParams (constant)
        let flash_din_params_buffer = device.new_buffer(
            std::mem::size_of::<FlashDinParams>() as u64,
            MTLResourceOptions::StorageModeShared,
        );
        unsafe {
            let p = &mut *(flash_din_params_buffer.contents() as *mut FlashDinParams);
            p.has_flash = if config.flash.is_some() { 1 } else { 0 };
            p.xmask_state_offset = script.xprop_state_offset;
            for i in 0..4 {
                p.d_in_pos[i] = config
                    .flash
                    .as_ref()
                    .and_then(|f| gpio_map.input_bits.get(&(f.d0_gpio + i)).copied())
                    .unwrap_or(0xFFFFFFFF);
            }
        }

        // FlashModelParams (constant)
        let flash_model_params_buffer = device.new_buffer(
            std::mem::size_of::<FlashModelParams>() as u64,
            MTLResourceOptions::StorageModeShared,
        );
        unsafe {
            let p = &mut *(flash_model_params_buffer.contents() as *mut FlashModelParams);
            p.state_size = state_size as u32;
            p.clk_out_pos = config
                .flash
                .as_ref()
                .and_then(|f| gpio_map.output_bits.get(&f.clk_gpio).copied())
                .unwrap_or(0);
            p.csn_out_pos = config
                .flash
                .as_ref()
                .and_then(|f| gpio_map.output_bits.get(&f.csn_gpio).copied())
                .unwrap_or(0);
            for i in 0..4 {
                p.d_out_pos[i] = config
                    .flash
                    .as_ref()
                    .and_then(|f| gpio_map.output_bits.get(&(f.d0_gpio + i)).copied())
                    .unwrap_or(0xFFFFFFFF);
            }
            p.flash_data_size = 16 * 1024 * 1024; // 16 MB
            clilog::info!("FlashModelParams: state_size={}, clk_out_pos={}, csn_out_pos={}, d_out_pos={:?}, flash_data_size={}",
                p.state_size, p.clk_out_pos, p.csn_out_pos, p.d_out_pos, p.flash_data_size);
        }

        // Log flash din params too
        unsafe {
            let p = &*(flash_din_params_buffer.contents() as *const FlashDinParams);
            clilog::info!(
                "FlashDinParams: has_flash={}, d_in_pos={:?}",
                p.has_flash,
                p.d_in_pos
            );
        }

        // Flash data buffer (16 MB, loaded with firmware)
        let flash_data_buffer =
            device.new_buffer(16 * 1024 * 1024, MTLResourceOptions::StorageModeShared);
        unsafe {
            // Fill with 0xFF (erased flash state)
            std::ptr::write_bytes(
                flash_data_buffer.contents() as *mut u8,
                0xFF,
                16 * 1024 * 1024,
            );
        }
        // Load firmware into flash data buffer
        if let Some(ref flash_cfg) = config.flash {
            use std::io::Read;
            let firmware_path = std::path::Path::new(&flash_cfg.firmware);
            let mut file =
                std::fs::File::open(firmware_path).expect("Failed to open firmware file");
            let mut data = Vec::new();
            file.read_to_end(&mut data)
                .expect("Failed to read firmware");
            let offset = flash_cfg.firmware_offset;
            assert!(
                offset + data.len() <= 16 * 1024 * 1024,
                "Firmware too large for flash buffer"
            );
            unsafe {
                let dest = (flash_data_buffer.contents() as *mut u8).add(offset);
                std::ptr::copy_nonoverlapping(data.as_ptr(), dest, data.len());
            }
            clilog::info!(
                "Loaded {} bytes firmware into GPU flash buffer at offset 0x{:X}",
                data.len(),
                offset
            );
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
                p.channels[i].tx_out_pos =
                    gpio_map.output_bits.get(tx_gpio).copied().unwrap_or(0);
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
        let wb_trace_channel_buffer =
            device.new_buffer(wb_channel_byte_size as u64, MTLResourceOptions::StorageModeShared);
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
        let bus_trace_channel_buffer =
            device.new_buffer(bus_channel_byte_size as u64, MTLResourceOptions::StorageModeShared);
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
    /// set_flash_din) stay in `run_cosim`, operating on a `states` slice
    /// re-derived over the returned `states_buffer`.
    ///
    /// Returns the four GPU buffers, the blocks no-copy buffers, the event
    /// buffer, **and `event_buffer_ptr`** — the `*mut EventBuffer` from
    /// `Box::into_raw`. The caller keeps ownership of the leaked box and is
    /// responsible for the matching `drop(Box::from_raw(event_buffer_ptr))`
    /// (two existing sites in `run_cosim`); no Drop impl is added here.
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

    /// Metal-only GPU kernel profiling (forwards to the simulator). Not part
    /// of `CosimBackend` — a CPU backend has no GPU kernels to profile.
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
}

impl CosimBackend for MetalBackend {
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

    fn flash_set_in_reset(&mut self, in_reset: bool) {
        unsafe {
            let fs = &mut *(self.flash_state_buffer.contents() as *mut FlashState);
            fs.in_reset = if in_reset { 1 } else { 0 };
        }
    }

    fn enable_vcd_ring(&mut self) {
        if self.vcd_ring.is_some() {
            return;
        }
        let ring_bytes = BATCH_SIZE * 2 * self.state_size * std::mem::size_of::<u32>();
        self.vcd_ring = Some(self.sim.device.new_buffer(
            ring_bytes as u64,
            MTLResourceOptions::StorageModeShared,
        ));
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

// ── Free-function ops patchers ─────────────────────────────────────────────
//
// These mutate per-edge ops between command-buffer completions, through the
// backend's `edge_ops_mut` seam (zero-copy over shared GPU memory on Metal).
// They take the backend by shared ref and are called sequentially before
// `run_edges`, so no long-lived borrow is held — this is the "closure-borrow"
// resolution noted in ADR 0017. `&dyn CosimBackend` keeps them backend-neutral
// for Phase 1.

/// Set the reset input bit's value in every edge's ops.
fn patch_reset_in_ops(backend: &mut dyn CosimBackend, reset_pos: u32, reset_val: u8) {
    for sched_idx in 0..backend.edges_per_period() {
        for op in backend.edge_ops_mut(sched_idx).iter_mut() {
            if op.position == reset_pos {
                op.value = reset_val as u32;
            }
        }
    }
}

/// Sync model-driven input bits into every edge's ops (called when a
/// peripheral model's state changes).
fn patch_model_driven_in_ops(
    backend: &mut dyn CosimBackend,
    overrides: &crate::sim::models::ModelOverrides,
) {
    if overrides.is_empty() {
        return;
    }
    for sched_idx in 0..backend.edges_per_period() {
        for op in backend.edge_ops_mut(sched_idx).iter_mut() {
            if let Some(&val) = overrides.get(&op.position) {
                op.value = val as u32;
            }
        }
    }
}

/// Tracks a clock domain whose input pin is driven by a peripheral model
/// (e.g. JTAG TCK), so model transitions can be reflected into the schedule's
/// pre-computed posedge/negedge flags.
struct ModelDrivenClockState {
    clock_input_pos: u32,
    posedge_flag_positions: Vec<u32>,
    negedge_flag_positions: Vec<u32>,
    prev_value: u8,
}

/// After model overrides are applied, patch the edge flags in the current
/// tick's ops to reflect the model's actual clock transitions.
fn patch_model_clock_edges(
    backend: &mut dyn CosimBackend,
    clocks: &mut [ModelDrivenClockState],
    overrides: &crate::sim::models::ModelOverrides,
    sched_offset: usize,
) {
    if clocks.is_empty() {
        return;
    }
    let sched_idx = sched_offset % backend.edges_per_period();
    let ops = backend.edge_ops_mut(sched_idx);

    for clock in clocks.iter_mut() {
        let new_val = overrides
            .get(&clock.clock_input_pos)
            .copied()
            .unwrap_or(clock.prev_value);
        let rising = clock.prev_value == 0 && new_val == 1;
        let falling = clock.prev_value == 1 && new_val == 0;
        clock.prev_value = new_val;

        for op in ops.iter_mut() {
            for &pos in &clock.posedge_flag_positions {
                if op.position == pos {
                    op.value = if rising { 1 } else { 0 };
                }
            }
            for &pos in &clock.negedge_flag_positions {
                if op.position == pos {
                    op.value = if falling { 1 } else { 0 };
                }
            }
        }
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
    let script = &design.script;
    let aig = &design.aig;
    let netlistdb = &design.netlistdb;
    let num_blocks = script.num_blocks;
    let num_major_stages = script.num_major_stages;
    // When timing arrivals are enabled, each state slot includes the arrival section.
    let state_size = script.effective_state_size() as usize;
    let arrival_state_offset = script.arrival_state_offset;

    // ── Build GPIO mapping ───────────────────────────────────────────────

    let gpio_map = build_gpio_mapping(aig, netlistdb, script, config.port_mapping.as_ref());

    // Verify we found the expected GPIO pins
    let clock_gpio = config.clock_gpio;
    let reset_gpio = config.reset_gpio;
    assert!(
        gpio_map.input_bits.contains_key(&clock_gpio),
        "Clock GPIO {} not found in input mapping. Available: {:?}",
        clock_gpio,
        gpio_map.input_bits.keys().collect::<Vec<_>>()
    );
    assert!(
        gpio_map.input_bits.contains_key(&reset_gpio),
        "Reset GPIO {} not found in input mapping. Available: {:?}",
        reset_gpio,
        gpio_map.input_bits.keys().collect::<Vec<_>>()
    );

    if let Some(ref flash_cfg) = config.flash {
        for i in 0..4 {
            let gpio = flash_cfg.d0_gpio + i;
            if gpio_map.input_bits.contains_key(&gpio) {
                clilog::info!(
                    "Flash D{} input GPIO {} -> state pos {}",
                    i,
                    gpio,
                    gpio_map.input_bits[&gpio]
                );
            }
            if gpio_map.output_bits.contains_key(&gpio) {
                clilog::info!(
                    "Flash D{} output GPIO {} -> state pos {}",
                    i,
                    gpio,
                    gpio_map.output_bits[&gpio]
                );
            }
        }
    }

    // ── Diagnostic: resolve key internal signals for debugging ──────────

    let diag_signals = [
        "fetch_enable",
        "ibus__cyc",
        "ibus__stb",
        "dbus__cyc",
        "spiflash",
        "flash_clk",
        "flash_csn",
    ];
    for sig in &diag_signals {
        let pos = resolve_signal_pos(aig, netlistdb, script, sig);
        if pos != 0xFFFFFFFF {
            clilog::info!("Diagnostic signal '{}' → output state pos {}", sig, pos);
        } else {
            clilog::debug!("Diagnostic signal '{}' not found in output_map", sig);
        }
    }

    // ── Initialize peripheral models (CPU-side, kept for --check-with-cpu) ──

    let _flash: Option<CppSpiFlash> = if let Some(ref flash_cfg) = config.flash {
        let mut fl = CppSpiFlash::new(16 * 1024 * 1024);
        fl.set_verbose(opts.flash_verbose);
        let firmware_path = std::path::Path::new(&flash_cfg.firmware);
        match fl.load_firmware(firmware_path, flash_cfg.firmware_offset) {
            Ok(size) => clilog::info!(
                "Loaded {} bytes firmware at offset 0x{:X}",
                size,
                flash_cfg.firmware_offset
            ),
            Err(e) => panic!("Failed to load firmware: {}", e),
        }
        Some(fl)
    } else {
        None
    };

    // CLI --clock-period overrides config file clock_period_ps; default 1000ps (1GHz) if neither set
    let clock_period_ps = opts.clock_period.or(config.clock_period_ps).unwrap_or(1000);
    let clock_hz = 1_000_000_000_000u64 / clock_period_ps;
    let effective_uarts = config.effective_uarts();
    assert!(
        effective_uarts.len() <= MAX_UARTS,
        "Too many UARTs configured ({}, max {})",
        effective_uarts.len(),
        MAX_UARTS
    );
    let uart_configs: Vec<_> = effective_uarts
        .iter()
        .enumerate()
        .map(|(i, u)| {
            let baud = u.baud_rate;
            let cpb = u
                .cycles_per_bit
                .unwrap_or_else(|| (clock_hz / baud as u64) as u32);
            let name = u
                .name
                .clone()
                .unwrap_or_else(|| format!("uart_{}", i));
            clilog::info!(
                "UART '{}': tx_gpio={}, rx_gpio={}, baud={}, cycles_per_bit={}",
                name, u.tx_gpio, u.rx_gpio, baud, cpb
            );
            (name, u.tx_gpio, u.rx_gpio, cpb)
        })
        .collect();
    clilog::info!(
        "Clock period: {} ps ({} MHz), {} UART(s) configured",
        clock_period_ps,
        clock_hz / 1_000_000,
        uart_configs.len()
    );

    // ── Initialize Metal simulator and GPU state buffers ─────────────────

    let timer_init = clilog::stimer!("init_gpu");
    let simulator = MetalSimulator::new(num_major_stages);

    // ── Design-state, SRAM, blocks-program, and event buffers ────────────
    // (built in MetalBackend — Phase 1 step 2c). The builder owns the
    // buffer-intrinsic init (states.fill + xprop X-mask seed, SRAM data/X-mask
    // fills + ELF preload, blocks no-copy wrappers, leaked-Box event buffer)
    // and returns `event_buffer_ptr` so the caller keeps ownership of the
    // leaked box (two `drop(Box::from_raw(..))` sites below). The
    // backend-agnostic stimulus deposits (reg_init / reset / constant_ports /
    // set_flash_din) stay here, operating on a `states` slice re-derived over
    // the returned `states_buffer`.
    let (
        states_buffer,
        sram_data_buffer,
        sram_xmask_buffer,
        blocks_start_buffer,
        blocks_data_buffer,
        event_buffer_metal,
        event_buffer_ptr,
    ) = MetalBackend::build_state_buffers(&simulator.device, script, config, state_size);
    let states: &mut [u32] = unsafe {
        std::slice::from_raw_parts_mut(states_buffer.contents() as *mut u32, 2 * state_size)
    };

    // SRAM write dumper (JACQUARD_SRAM_DUMP=<path>). Opt-in
    // diagnostic that snapshots `sram_storage` per batch and emits
    // a per-block write log at end of simulation. See
    // `src/sim/sram_dump.rs` for the report format.
    let mut sram_dumper =
        crate::sim::sram_dump::SramDumper::from_env(aig, netlistdb, script);

    // (SRAM preload moved into MetalBackend::build_state_buffers — Phase 1
    // step 2c. The states/sram/xmask buffers are now allocated + initialised by
    // the builder above; only the agnostic stimulus deposits remain inline.)

    // Register value-injection (issue #108, ADR 0016). `reg_init` deposits a
    // definite value into chosen registers at tick 0 with `$deposit`
    // semantics — the seed clears the power-up X-mask, then the design drives
    // the register normally. The register sibling of `sram_init` above; the
    // fix path for X-poisoned unreset CDC launch registers (#102). NOT
    // `force`: applied once here, not re-driven each edge.
    //
    // A DFF Q is sequential state, not a primary input in the per-edge BitOp
    // schedule, so a deposit must survive `state_prep`'s output→input copy
    // (kernel_v1.metal): write the value AND clear the X-mask in BOTH slots,
    // mirroring the xprop seed above. The output slot is the source of truth
    // the first `simulate` reads; the input slot covers any pre-copy read.
    if !config.reg_init.is_empty() {
        let rio = script.reg_io_state_size as usize;
        let mut deposited_bits = 0usize;
        for entry in &config.reg_init {
            let width = entry.width.unwrap_or(1);
            for b in 0..width {
                // A scalar register (width 1, however spelled) resolves by its
                // bare name; a wider one resolves each bit `name[b]` (a
                // register's bits need not be contiguous after synthesis).
                let bit_name = if width == 1 {
                    entry.name.clone()
                } else {
                    format!("{}[{}]", entry.name, b)
                };
                let pos = crate::sim::trace_signals::resolve_to_input_state_pos(
                    aig, netlistdb, script, &bit_name,
                )
                .unwrap_or_else(|| {
                    panic!(
                        "reg_init: cannot resolve register '{}' to a DFF state slot \
                         (not a register, or name not found in netlist)",
                        bit_name
                    )
                });
                let val = if b < 64 { ((entry.value >> b) & 1) as u8 } else { 0 };
                // Deposit the value into both slots' value sections.
                set_bit(&mut states[..state_size], pos, val);
                set_bit(&mut states[state_size..2 * state_size], pos, val);
                // Clear the X-mask in both slots so the deposit reads known.
                // The X-mask section starts `rio` words into each slot.
                if script.xprop_enabled {
                    clear_bit(&mut states[rio..state_size], pos);
                    clear_bit(&mut states[state_size + rio..2 * state_size], pos);
                }
                deposited_bits += 1;
            }
        }
        clilog::info!(
            "reg_init: deposited {} register bit(s) from {} config entries (cleared power-up X)",
            deposited_bits,
            config.reg_init.len()
        );
    }

    // Initialize: set reset active
    let reset_val = if config.reset_active_high { 1u8 } else { 0u8 };
    set_bit(
        &mut states[..state_size],
        gpio_map.input_bits[&reset_gpio],
        reset_val,
    );
    clilog::info!(
        "Initial state: reset GPIO {} = {} (active)",
        reset_gpio,
        reset_val
    );

    // Initialize constant ports (e.g. por_l=1, resetb_h=1 for Caravel wrapper)
    for (port_name, val) in &config.constant_ports {
        if let Some(&pos) = gpio_map.named_input_bits.get(port_name) {
            set_bit(&mut states[..state_size], pos, *val);
            clilog::info!(
                "Initial state: port '{}' = {} (pos {})",
                port_name,
                val,
                pos
            );
        } else {
            clilog::warn!("constant_port '{}' not found in named inputs", port_name);
        }
    }

    // Set flash D_IN defaults (high = no data) — initial state before GPU flash takes over
    if let Some(ref flash_cfg) = config.flash {
        set_flash_din(
            &mut states[..state_size],
            &gpio_map,
            flash_cfg.d0_gpio,
            0x0F,
        );
    }

    // (blocks no-copy + event buffer moved into
    // MetalBackend::build_state_buffers — Phase 1 step 2c. `blocks_start_buffer`,
    // `blocks_data_buffer`, `event_buffer_metal`, and `event_buffer_ptr` come
    // from the builder call above; the two `drop(Box::from_raw(event_buffer_ptr))`
    // sites below free the leaked box.)

    // Timing constraint buffer for GPU-side setup/hold checking.
    let timing_constraints_buffer = timing_constraints.as_ref().map(|buf| {
        simulator.device.new_buffer_with_data(
            buf.as_ptr() as *const _,
            (buf.len() * std::mem::size_of::<u32>()) as u64,
            MTLResourceOptions::StorageModeShared,
        )
    });

    clilog::finish!(timer_init);

    // ── Build GPU-side state_prep + IO model buffers ──────────────────────

    let timer_prep = clilog::stimer!("build_state_prep_buffers");

    // Initial reset value
    let reset_val_active = if config.reset_active_high { 1u8 } else { 0u8 };
    let reset_val_inactive = if config.reset_active_high { 0u8 } else { 1u8 };

    // ── Build multi-clock scheduler ────────────────────────────────────────
    let effective_clocks = config.effective_clocks();
    let clock_timings: Vec<ClockDomainTiming> = effective_clocks
        .iter()
        .enumerate()
        .filter_map(|(cfg_idx, clk_cfg)| {
            // Find the clock domain in gpio_map that matches this clock's GPIO
            let domain_idx = gpio_map
                .clock_domains
                .iter()
                .position(|d| d.clock_gpio == Some(clk_cfg.gpio));
            if let Some(di) = domain_idx {
                Some(ClockDomainTiming {
                    half_period_ps: clk_cfg.period_ps / 2,
                    phase_offset_ps: clk_cfg.phase_offset_ps,
                    domain_index: di,
                })
            } else {
                clilog::warn!(
                    "Clock config[{}] gpio={} ({:?}): no matching clock domain found, skipping",
                    cfg_idx,
                    clk_cfg.gpio,
                    clk_cfg.name
                );
                None
            }
        })
        .collect();

    assert!(
        !clock_timings.is_empty(),
        "No clock domains matched from config. Ensure clock_gpio matches a clock input \
         in the netlist. Config clock_gpio={}, available domains: {:?}",
        config.clock_gpio,
        gpio_map
            .clock_domains
            .iter()
            .map(|d| &d.name)
            .collect::<Vec<_>>()
    );
    {
        for (i, clk_cfg) in effective_clocks.iter().enumerate() {
            if let Some(timing) = clock_timings
                .iter()
                .find(|t| gpio_map.clock_domains[t.domain_index].clock_gpio == Some(clk_cfg.gpio))
            {
                let domain = &gpio_map.clock_domains[timing.domain_index];
                clilog::info!(
                    "Clock '{}' (gpio {}, period {}ps, phase {}ps) → domain '{}' \
                     ({} posedge flags, {} negedge flags)",
                    clk_cfg.name.as_deref().unwrap_or("?"),
                    clk_cfg.gpio,
                    clk_cfg.period_ps,
                    clk_cfg.phase_offset_ps,
                    domain.name,
                    domain.posedge_flag_bits.len(),
                    domain.negedge_flag_bits.len()
                );
            }
            let _ = i; // suppress unused warning
        }
    }

    // Build the scheduler up-front so peripheral models that need to know
    // edges-per-cycle (e.g. UART RX driver's baud_div_edges) can be built
    // before the per-edge ops buffers.
    let scheduler = MultiClockScheduler::new(&clock_timings);
    let edges_per_period = scheduler.schedule.len();

    // ── Run-parameters and per-domain jitter PRNGs (ADR 0012) ─────────────
    let run_params = {
        use crate::sim::run_params::RunParams;
        if let Some(ref path) = opts.run_params {
            RunParams::load_or_generate(path)
                .unwrap_or_else(|e| panic!("Failed to load/write run-params at {}: {}", path.display(), e))
        } else if let Some(ref output_vcd_path) = opts.output_vcd {
            let default_path = output_vcd_path.with_file_name("run_params.json");
            RunParams::load_or_generate(&default_path)
                .unwrap_or_else(|e| panic!("Failed to write default run-params at {}: {}", default_path.display(), e))
        } else {
            RunParams::generate()
        }
    };

    let jitter_active = config.effective_clocks().iter().any(|c| c.jitter_ps > 0);
    if jitter_active {
        clilog::info!(
            "CDC jitter enabled (master_seed={}); per-domain streams derived from seed",
            run_params.master_seed
        );
        if opts.check_with_cpu {
            clilog::warn!(
                "--check-with-cpu with jitter active: CPU baseline does not apply \
                 jitter, so timing comparisons are meaningless"
            );
        }
    }

    use rand::SeedableRng;
    use rand_chacha::ChaCha8Rng;
    let mut domain_rngs: Vec<Option<(ChaCha8Rng, i64)>> = gpio_map
        .clock_domains
        .iter()
        .enumerate()
        .map(|(i, domain)| {
            let jitter_ps = config
                .effective_clocks()
                .iter()
                .find(|c| c.gpio == domain.clock_gpio.unwrap_or(usize::MAX))
                .map(|c| c.jitter_ps)
                .unwrap_or(0);
            if jitter_ps > 0 {
                let sub_seed = run_params.domain_seed(&domain.name);
                let rng = ChaCha8Rng::seed_from_u64(sub_seed);
                clilog::info!(
                    "  domain '{}' (idx={}): jitter_ps={}, sub_seed={}",
                    domain.name, i, jitter_ps, sub_seed
                );
                Some((rng, jitter_ps as i64))
            } else {
                None
            }
        })
        .collect();

    // Instantiate CPU-side peripheral models from config. Their driven
    // input positions are appended as placeholder BitOps in each
    // edge_buffer's ops list so state_prep applies them every edge;
    // values are updated dynamically in the main loop as actions fire.
    use crate::sim::models::PeripheralModel;
    let mut models: Vec<Box<dyn PeripheralModel>> = Vec::new();
    let mut model_driven_positions: Vec<u32> = Vec::new();
    let register_model = |model: Box<dyn PeripheralModel>,
                          models: &mut Vec<Box<dyn PeripheralModel>>,
                          positions: &mut Vec<u32>| {
        positions.extend_from_slice(model.driven_positions());
        clilog::info!(
            "Peripheral model `{}` registered: {} driven pin(s)",
            model.name(),
            model.driven_positions().len()
        );
        models.push(model);
    };
    for gcfg in &config.gpios {
        match crate::sim::models::gpio::GpioModel::new(gcfg, |idx| {
            gpio_map.input_bits.get(&idx).copied()
        }) {
            Some(m) => register_model(Box::new(m), &mut models, &mut model_driven_positions),
            None => panic!(
                "GPIO config `{}` references pin indices not present in input \
                 mapping; pins={:?}",
                gcfg.name, gcfg.pins
            ),
        }
    }
    for (name, _tx_gpio, rx_gpio, cpb) in &uart_configs {
        if let Some(&rx_pos) = gpio_map.input_bits.get(rx_gpio) {
            let sched_ticks_per_sys_clk = (clock_period_ps / scheduler.gcd_ps) as u32;
            let baud_div_edges = cpb * sched_ticks_per_sys_clk;
            let driver = crate::sim::models::uart::UartRxDriver::new(
                name.clone(),
                rx_pos,
                baud_div_edges,
            );
            clilog::info!(
                "UART RX driver `{}` baud_div_edges={} (rx_gpio={})",
                name, baud_div_edges, rx_gpio
            );
            register_model(Box::new(driver), &mut models, &mut model_driven_positions);
        } else {
            clilog::warn!(
                "UART '{}' rx_gpio={} not in input mapping; RX driver disabled",
                name, rx_gpio
            );
        }
    }
    // JTAG replay (discussion #77 stage 1) — only wires up when BOTH
    // a `jtag` peripheral is configured AND --jtag-replay <PATH> is
    // supplied. Config without CLI is a soft warn (someone forgot to
    // pass the stream); CLI without config hard-fails (the operator
    // expected JTAG to work and it didn't).
    if let Some(jtag_cfg) = config.jtag.as_ref() {
        if let Some(replay_path) = opts.jtag_replay.as_ref() {
            let bytes = std::fs::read(replay_path).unwrap_or_else(|e| {
                panic!(
                    "jtag: cannot read --jtag-replay {}: {e}",
                    replay_path.display()
                )
            });
            let resolve_pin = |gpio: usize, label: &str| -> u32 {
                *gpio_map.input_bits.get(&gpio).unwrap_or_else(|| {
                    panic!(
                        "jtag: {label}_gpio={gpio} not in input mapping; \
                         check sim_config.json against design pin layout"
                    )
                })
            };
            let tck = resolve_pin(jtag_cfg.tck_gpio, "tck");
            let tms = resolve_pin(jtag_cfg.tms_gpio, "tms");
            let tdi = resolve_pin(jtag_cfg.tdi_gpio, "tdi");
            let trst = jtag_cfg.trst_gpio.map(|g| crate::sim::models::jtag::TrstPin {
                position: resolve_pin(g, "trst"),
                active_low: jtag_cfg.trst_active_low,
            });
            let stream_len = bytes.len();
            let model = crate::sim::models::jtag::JtagReplayModel::new(
                "jtag_0".to_string(),
                tck,
                tms,
                tdi,
                trst,
                opts.jtag_hold_cycles,
                bytes,
            );
            clilog::info!(
                "JTAG replay `jtag_0`: {} stream bytes from {}, hold_edges={} \
                 (tck_gpio={}, tms_gpio={}, tdi_gpio={}, trst_gpio={:?})",
                stream_len,
                replay_path.display(),
                opts.jtag_hold_cycles,
                jtag_cfg.tck_gpio,
                jtag_cfg.tms_gpio,
                jtag_cfg.tdi_gpio,
                jtag_cfg.trst_gpio
            );
            register_model(Box::new(model), &mut models, &mut model_driven_positions);
        } else {
            clilog::warn!(
                "sim_config.json declares a `jtag` peripheral but no \
                 --jtag-replay <PATH> was supplied; JTAG inputs will float"
            );
        }
    } else if opts.jtag_replay.is_some() {
        panic!(
            "--jtag-replay supplied but sim_config.json has no `jtag` \
             peripheral entry to bind it to"
        );
    }
    let _ = register_model;

    // Build per-edge BitOp buffers from multi-clock schedule.
    //
    // The scheduler produces one entry per GCD tick (= one clock edge for
    // single-domain). Each cosim "edge" iteration applies one set of edge
    // ops (clk + posedge/negedge flags for any domains active at this tick)
    // and runs one simulate dispatch. DFFs capture on the dispatch where
    // posedge_flag=1. For single-domain this gives 2 edges per full cycle
    // (one falling, one rising); for multi-domain, schedule_len edges per
    // LCM period at GCD granularity.
    // Backend-agnostic schedule description: one ops vector per scheduler
    // edge. Handed to the backend *once* via `init_schedule` (after backend
    // construction below); the backend materialises its native per-edge
    // buffers and owns them. The orchestration keeps only the scalars
    // (`edges_per_period`, `scheduler.gcd_ps`).
    let per_edge_ops: Vec<Vec<BitOp>> = (0..edges_per_period)
        .map(|edge_idx| {
            scheduler.build_edge_ops(
                edge_idx,
                &gpio_map,
                reset_gpio,
                reset_val_active,
                &config.constant_inputs,
                &config.constant_ports,
                &model_driven_positions,
            )
        })
        .collect();
    clilog::info!(
        "Multi-clock schedule: {} edges per LCM period (gcd={}ps)",
        edges_per_period,
        scheduler.gcd_ps
    );

    // Edge-granularity conversion: config fields `reset_cycles` and
    // `num_cycles` are user-facing in sys_clk cycles, but internal counters
    // (the main loop's `tick` variable, scheduler offsets, UART decoder
    // current_cycle) advance per scheduler edge (one GCD tick).
    //
    // sched_ticks_per_sys_clk_cycle = how many dense scheduler ticks (each
    // `gcd_ps` long) fall in one sys_clk period. NOT the number of sys_clk
    // *transitions* (which is always 2). It is 2 only when `gcd_ps` equals
    // sys_clk's half-period — i.e. single-clock, or multi-clock whose other
    // domains' half-periods and phase offsets are all integer multiples of
    // it. With non-commensurate periods or phase offsets, `gcd_ps` shrinks
    // below the half-period (`MultiClockScheduler::new`, where gcd folds in
    // every half-period and offset) and this is >2. It is the user-cycles →
    // internal-tick conversion factor (the codebase calls GCD ticks "edges",
    // cf. `--max-clock-edges`).
    let sched_ticks_per_sys_clk_cycle = clock_period_ps / scheduler.gcd_ps;
    let reset_edges = config.reset_cycles * sched_ticks_per_sys_clk_cycle as usize;
    // CLI --max-clock-edges is already in edges; config `num_cycles` is in
    // sys_clk cycles and gets multiplied here.
    let max_edges = opts
        .max_clock_edges
        .unwrap_or(config.num_cycles * sched_ticks_per_sys_clk_cycle as usize);

    // ── GPU Flash IO buffers (built in MetalBackend — Phase 1 step 2a) ──────
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
        &gpio_map,
    );

    // ── GPU IO peripheral buffers (built in MetalBackend — Phase 1 step 2b) ──
    let (
        uart_state_buffer,
        uart_params_buffer,
        uart_channel_buffer,
        wb_trace_params_buffer,
        wb_trace_channel_buffer,
        bus_trace_params_buffer,
        bus_trace_channel_buffer,
        mut bus_lanes,
    ) = MetalBackend::build_io_buffers(
        &simulator.device,
        aig,
        netlistdb,
        script,
        config,
        &gpio_map,
        state_size,
        &uart_configs,
        sched_ticks_per_sys_clk_cycle,
    );
    let n_uarts = uart_configs.len();
    // Accumulated decoded transactions (bus_id, transaction), drained
    // each batch. The name is looked up from `bus_lanes` at CSV-write
    // time, so the hot drain path stays allocation-free.
    let mut bus_transactions: Vec<(u32, crate::sim::models::bus_trace::BusTransaction)> =
        Vec::new();

    // Pre-write params for all simulation stages (they don't change between ticks)
    for stage_i in 0..num_major_stages {
        simulator.write_params(
            stage_i,
            num_blocks,
            num_major_stages,
            state_size,
            0,
            arrival_state_offset,
        );
    }

    clilog::finish!(timer_prep);

    // ── Assemble the Metal backend ───────────────────────────────────────
    //
    // Move the simulator, design state/program, and every GPU IO buffer into
    // the backend, which now owns them (ADR 0017, Amendment 2026-06-07). The
    // schedule storage is materialised once, here, via `init_schedule` from the
    // backend-agnostic `per_edge_ops` description built above. After this point
    // the cosim loop drives the design through `backend` rather than the loose
    // locals.
    let mut backend = MetalBackend {
        sim: simulator,
        schedule: None,
        state_size,
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
    };
    backend.init_schedule(
        per_edge_ops,
        state_size as u32,
        script.xprop_state_offset,
        scheduler.gcd_ps,
    );

    // ── GPU Kernel Profiling (optional) ──────────────────────────────────

    if opts.gpu_profile {
        let profile_ticks = opts.max_clock_edges.unwrap_or(1000).min(5000);
        backend.profile_kernels(profile_ticks);

        // Clean up event buffer
        unsafe {
            drop(Box::from_raw(event_buffer_ptr));
        }
        return CosimResult {
            passed: true,
            uart_events: Vec::new(),
            edges_simulated: 0,
        };
    }

    // ── Stimulus VCD setup (optional) ─────────────────────────────────────
    //
    // When --stimulus-vcd is specified, we write all primary input signals
    // to a VCD file for CVC reference simulation.

    let mut stimulus_vcd_state: Option<(
        vcd_ng::Writer<std::io::BufWriter<std::fs::File>>,
        crate::sim::vcd_io::StimulusVCDMapping,
        Vec<u8>, // prev_values for change detection
    )> = if let Some(ref stim_path) = opts.stimulus_vcd {
        crate::sim::vcd_io::ensure_parent_dir(stim_path).unwrap_or_else(|e| {
            panic!("Failed to create stimulus VCD dir for {}: {}", stim_path.display(), e)
        });
        let file = std::fs::File::create(stim_path).unwrap_or_else(|e| {
            panic!(
                "Failed to create stimulus VCD {}: {}",
                stim_path.display(),
                e
            )
        });
        let bufwriter = std::io::BufWriter::new(file);
        let mut writer = vcd_ng::Writer::new(bufwriter);
        let mapping = crate::sim::vcd_io::setup_stimulus_vcd(
            &mut writer,
            netlistdb,
            aig,
            script,
            clock_period_ps,
        );
        let prev_values = vec![0xFFu8; mapping.signals.len()]; // sentinel: force initial write
        clilog::info!(
            "Stimulus VCD enabled: {} signals → {}",
            mapping.signals.len(),
            stim_path.display()
        );
        Some((writer, mapping, prev_values))
    } else {
        None
    };

    // ── Output VCD setup (optional) ──────────────────────────────────────
    //
    // When --output-vcd is specified, we write the output VCD (chip outputs
    // plus traced nets). Transitions carry per-signal arrival-time offsets
    // when timing data is available; otherwise they emit at clock edges.

    let rio = script.reg_io_state_size as usize;
    let mut output_vcd_state: Option<(
        vcd_ng::Writer<std::io::BufWriter<std::fs::File>>,
        crate::sim::vcd_io::OutputVCDMapping,
        Vec<u32>, // prev_values for change detection (0=V0, 1=V1, 2=initial)
    )> = if let Some(ref output_path) = opts.output_vcd {
        crate::sim::vcd_io::ensure_parent_dir(output_path).unwrap_or_else(|e| {
            panic!("Failed to create output VCD dir for {}: {}", output_path.display(), e)
        });
        let file = std::fs::File::create(output_path).unwrap_or_else(|e| {
            panic!(
                "Failed to create output VCD {}: {}",
                output_path.display(),
                e
            )
        });
        let bufwriter = std::io::BufWriter::new(file);
        let mut writer = vcd_ng::Writer::new(bufwriter);
        let mapping =
            crate::sim::vcd_io::setup_cosim_output_vcd(&mut writer, netlistdb, aig, script);
        // Sentinel forces the first write; must differ from 0/1/2(=X).
        let prev_values = vec![u32::MAX; mapping.out2vcd.len()];
        clilog::info!(
            "Output VCD enabled: {} output signals → {}",
            mapping.out2vcd.len(),
            output_path.display()
        );
        Some((writer, mapping, prev_values))
    } else {
        None
    };

    // ── VCD ring buffer (enables batched-mode VCD capture) ─────────────────
    //
    // When stimulus or output VCD is active, we snapshot output_state after
    // each edge via a GPU blit into a ring buffer. This allows batched dispatch
    // (no batch=1 override) while preserving per-tick VCD accuracy.
    let vcd_enabled = stimulus_vcd_state.is_some() || output_vcd_state.is_some();
    if vcd_enabled {
        backend.enable_vcd_ring();
    }

    // ── DFF state dump setup (optional) ────────────────────────────────────
    //
    // When --dump-dff is specified, dump all DFF Q-values per cycle to a text file.
    // Used for comparing internal state with CVC or other reference simulators.

    struct DffDumpEntry {
        cellid: usize,
        q_pos: u32,
        name: String,
    }

    let mut dff_dump_state: Option<(
        std::io::BufWriter<std::fs::File>,
        Vec<DffDumpEntry>,
        usize, // max cycles to dump
    )> = if let Some(ref dump_path) = opts.dump_dff {
        use std::io::Write;
        crate::sim::vcd_io::ensure_parent_dir(dump_path)
            .unwrap_or_else(|e| panic!("Failed to create DFF dump dir for {}: {}", dump_path.display(), e));
        let file = std::fs::File::create(dump_path)
            .unwrap_or_else(|e| panic!("Failed to create DFF dump {}: {}", dump_path.display(), e));
        let mut writer = std::io::BufWriter::new(file);

        // Build sorted mapping from DFF cell_id → (Q position, name)
        let mut entries: Vec<DffDumpEntry> = Vec::new();
        for (&cellid, dff) in aig.dffs.iter() {
            if let Some(&pos) = script.input_map.get(&dff.q) {
                let name = format!("{}", netlistdb.cellnames[cellid]);
                entries.push(DffDumpEntry {
                    cellid,
                    q_pos: pos,
                    name,
                });
            }
        }
        entries.sort_by_key(|e| e.cellid);

        // Write header
        writeln!(writer, "# DFF State Dump").unwrap();
        writeln!(writer, "# Total DFFs: {}", entries.len()).unwrap();
        writeln!(
            writer,
            "# Format: CYCLE <n> hash=<hex> ones=<count>/<total>"
        )
        .unwrap();
        writeln!(writer, "# Then: <dff_name> <0|1>").unwrap();
        writeln!(writer, "#").unwrap();
        writeln!(writer, "# DFF index mapping:").unwrap();
        for (i, e) in entries.iter().enumerate() {
            writeln!(
                writer,
                "# DFF[{}] pos={} cell={} name={}",
                i, e.q_pos, e.cellid, e.name
            )
            .unwrap();
        }
        writeln!(writer, "#").unwrap();

        // dump_dff_cycles is user-facing (sys_clk cycles); internal counter is in edges.
        let max_dump_edges = opts.dump_dff_cycles * sched_ticks_per_sys_clk_cycle as usize;
        clilog::info!(
            "DFF dump enabled: {} DFFs, {} cycles ({} edges) → {}",
            entries.len(),
            opts.dump_dff_cycles,
            max_dump_edges,
            dump_path.display()
        );
        Some((writer, entries, max_dump_edges))
    } else {
        None
    };

    // ── Input stimulus dispatcher (optional) ─────────────────────────────
    //
    // Loads a chipflow-format `input.json` if one is configured. Supports
    // `stop` actions (halt simulation cleanly) and `uart_<N> tx` waits
    // (synchronize on captured UART TX bytes). GPIO / UART RX / I2C / SPI
    // peripheral drivers are follow-ups.
    let mut input_dispatcher = match config.input_commands.as_deref() {
        Some(path_str) => {
            let path = std::path::Path::new(path_str);
            match crate::sim::input_stim::InputDispatcher::from_file(path) {
                Ok(d) => {
                    clilog::info!(
                        "Loaded input stimulus: {} commands from {}",
                        d.len(),
                        path.display()
                    );
                    Some(d)
                }
                Err(e) => {
                    panic!(
                        "Failed to load input commands from {}: {}",
                        path.display(),
                        e
                    );
                }
            }
        }
        None => None,
    };

    // ── GPU-only simulation loop ─────────────────────────────────────────
    //
    // All IO models (flash + UART) run on GPU. No per-tick CPU interaction.
    // CPU just drains decoded UART bytes from the ring buffer after each batch.

    let timer_sim = clilog::stimer!("simulation");
    let sim_start = std::time::Instant::now();

    // Reset input bit position, looked up once for the per-batch reset patch.
    let reset_pos = gpio_map.input_bits[&reset_gpio];

    // Model-driven clock edge tracking: when a peripheral model drives
    // a pin that is also a clock domain's input (e.g. JTAG TCK), the
    // scheduler's pre-computed edge flags don't match the model's actual
    // transitions. The `patch_model_clock_edges` free function detects
    // model-driven clock transitions and patches the edge flags in the
    // current tick's ops.
    let mut model_driven_clocks: Vec<ModelDrivenClockState> = Vec::new();
    {
        let model_pos_set: std::collections::HashSet<u32> =
            model_driven_positions.iter().copied().collect();
        for domain in &gpio_map.clock_domains {
            if let Some(pos) = domain.clock_input_pos {
                if model_pos_set.contains(&pos) {
                    clilog::info!(
                        "Clock domain `{}` is model-driven (pos={}); \
                         edge flags will track model transitions",
                        domain.name, pos
                    );
                    model_driven_clocks.push(ModelDrivenClockState {
                        clock_input_pos: pos,
                        posedge_flag_positions: domain.posedge_flag_bits.clone(),
                        negedge_flag_positions: domain.negedge_flag_bits.clone(),
                        prev_value: 0,
                    });
                }
            }
        }
    }

    // Track schedule position across batches
    let mut schedule_offset: usize = 0;

    // Verify flash state hasn't been corrupted before main loop
    unsafe {
        let fs = &*(backend.flash_state_buffer.contents() as *const FlashState);
        clilog::info!(
            "FlashState before main loop: d_i=0x{:02X}, data_width={}, prev_csn={}, in_reset={}",
            fs.d_i,
            fs.data_width,
            fs.prev_csn,
            fs.in_reset
        );
        assert_eq!(
            fs.d_i, 0x0F,
            "FlashState.d_i corrupted before main loop: got 0x{:02X}",
            fs.d_i
        );
    }

    // UART event collection (CPU-side, populated from channel drain)
    let mut uart_events: Vec<UartEvent> = Vec::new();
    let mut uart_read_heads: Vec<u32> = vec![0u32; n_uarts];
    let uart_names: Vec<String> = uart_configs.iter().map(|(name, _, _, _)| name.clone()).collect();
    let mut wb_trace_read_head: u32 = 0;
    let mut bus_trace_read_head: u32 = 0;

    // Profiling accumulators
    let mut prof_batch_encode: u64 = 0;
    let mut prof_gpu_wait: u64 = 0;
    let mut prof_drain: u64 = 0;
    let mut prof_stimulus_vcd: u64 = 0;
    let mut prof_output_vcd: u64 = 0;
    let mut total_batches: u64 = 0;
    // Batch-utilisation telemetry: how often the GPU-only batched fast path
    // (batch>1) is exercised vs. forced single-edge dispatch. Informs the
    // cosim-backend seam design (#105): edges run under batch=1 are the only
    // ones that need a true per-edge CPU↔GPU handover.
    let mut single_edge_batches: u64 = 0;
    let mut max_batch_seen: usize = 0;

    // Per-tick tracing: run 1 tick at a time for first N ticks after reset
    let trace_ticks = if std::env::var("FLASH_TRACE").is_ok() {
        200
    } else {
        0
    };
    let deep_diag = std::env::var("GEM_DIAG").is_ok();
    let mut diag_prev_flash_addr: u32 = 0;
    let mut diag_prev_flash_cmd: u8 = 0;
    let mut diag_prev_csn: u32 = 1;
    let mut diag_sram_write_count: usize = 0;

    // CPU verification state (--check-with-cpu)
    let mut cpu_states: Vec<u32> = if opts.check_with_cpu {
        vec![0u32; 2 * state_size]
    } else {
        Vec::new()
    };
    let mut cpu_sram: Vec<u32> = if opts.check_with_cpu {
        vec![0u32; script.sram_storage_size as usize]
    } else {
        Vec::new()
    };
    let mut cpu_check_mismatches: usize = 0;
    let cpu_check_max_edges = if opts.check_with_cpu { 500 } else { 0 };

    let mut post_reset_state_snapshot: Option<Vec<u32>> = None;

    let mut tick: usize = 0;
    let mut stop_triggered = false;
    let mut models = models; // re-bind as mutable for the loop

    // Sync each model's INITIAL idle state into the per-edge ops buffers
    // before the first GPU dispatch. Without this, model-driven positions
    // would default to 0 — fine for GPIO inputs (0 is a sensible idle) but
    // wrong for UART RX (idle is high, 1). The placeholder BitOps from
    // `build_edge_ops` start at 0; this seeds them with the models'
    // post-construction values.
    if !models.is_empty() {
        let mut overrides = crate::sim::models::ModelOverrides::new();
        for model in &models {
            model.contribute_overrides(&mut overrides);
        }
        patch_model_driven_in_ops(&mut backend, &overrides);
        // Seed initial clock values (no edge flags — no transition yet).
        for clock in &mut model_driven_clocks {
            if let Some(&val) = overrides.get(&clock.clock_input_pos) {
                clock.prev_value = val;
            }
        }
    }

    while tick < max_edges {
        // Drain queued actions, advance per-edge state, sync overrides
        // and forward emitted events. Active iff a dispatcher is loaded
        // OR any model has per-edge state to advance.
        if input_dispatcher.is_some() || !models.is_empty() {
            let mut overrides = crate::sim::models::ModelOverrides::new();
            let mut emitted_events: Vec<crate::sim::models::EmittedEvent> = Vec::new();
            let mut any_change = false;

            for model in models.iter_mut() {
                let prev_active = model.is_active();
                if let Some(d) = input_dispatcher.as_mut() {
                    let actions = d.pending_actions(model.name());
                    if !actions.is_empty() {
                        for a in &actions {
                            model.apply_action(a);
                        }
                        any_change = true;
                    }
                }
                // step_edge contributes overrides itself (default impl) and
                // pushes any emitted events.
                model.step_edge(
                    // GPU output state — currently unused by GPIO/UART RX,
                    // needed by I²C/SPI once wired. Provide an empty slice
                    // for now (placeholder; wiring follow-up).
                    &[],
                    &mut overrides,
                    &mut emitted_events,
                );
                if prev_active || model.is_active() {
                    any_change = true;
                }
            }

            // Forward emitted events to the dispatcher so `wait` commands
            // can fire on bus transactions (e.g. i2c_0 address, user_spi_0
            // data).
            if let Some(d) = input_dispatcher.as_mut() {
                for ev in &emitted_events {
                    d.on_event(&ev.peripheral, &ev.event, &ev.payload);
                }
            }

            if any_change {
                patch_model_driven_in_ops(&mut backend, &overrides);
                patch_model_clock_edges(
                    &mut backend,
                    &mut model_driven_clocks,
                    &overrides,
                    schedule_offset,
                );
            }
        }

        // Honor a `stop` action from the input dispatcher.
        if let Some(d) = input_dispatcher.as_ref() {
            if d.stopped() {
                clilog::info!(
                    "Stop action triggered at edge {}{}",
                    tick,
                    d.stop_reason()
                        .map(|r| format!(": {}", r))
                        .unwrap_or_default()
                );
                stop_triggered = true;
                break;
            }
        }
        let dff_dump_active = dff_dump_state
            .as_ref()
            .map_or(false, |(_, _, max)| tick < reset_edges + *max);
        // Force single-edge batches only when a peripheral driver is mid-
        // transmission and bit timing depends on per-edge granularity (e.g.
        // UART RX shifting bits onto rx_gpio). Stops fire from dispatcher
        // state at iteration boundaries; waits match against UART events
        // captured at end-of-batch — neither needs single-edge mode. This
        // keeps the boot phase (long autonomous output before any action
        // queues) at full batched-mode throughput even when input.json has
        // pending commands.
        let any_model_active = models.iter().any(|m| m.is_active());
        let force_single_edge = any_model_active;
        // VCD capture uses the ring buffer — no batch=1 override needed.
        let batch = if force_single_edge {
            1
        } else if opts.check_with_cpu && tick < cpu_check_max_edges {
            1 // single tick for CPU comparison
        } else if dff_dump_active {
            1 // single tick for DFF state capture
        } else if trace_ticks > 0 && tick < reset_edges + trace_ticks {
            1 // single tick for tracing
        } else if deep_diag {
            1 // single tick for deep diagnostics
        } else {
            BATCH_SIZE.min(max_edges - tick)
        };

        // Don't cross reset boundary within a batch
        let batch = if tick < reset_edges && tick + batch > reset_edges {
            reset_edges - tick
        } else {
            batch
        };

        // Update reset value and flash in_reset for this batch
        let in_reset = tick < reset_edges;
        let current_reset_val = if in_reset {
            reset_val_active
        } else {
            reset_val_inactive
        };
        patch_reset_in_ops(&mut backend, reset_pos, current_reset_val);

        // Update the flash reset line for this batch.
        backend.flash_set_in_reset(in_reset);

        // Save pre-tick state for CPU verification
        let saved_flash_d_i: u8;
        if opts.check_with_cpu && tick < cpu_check_max_edges && batch == 1 {
            let gpu_states: &[u32] = unsafe {
                std::slice::from_raw_parts(backend.states_buffer.contents() as *const u32, 2 * state_size)
            };
            cpu_states.copy_from_slice(gpu_states);
            let gpu_sram: &[u32] = unsafe {
                std::slice::from_raw_parts(
                    backend.sram_data_buffer.contents() as *const u32,
                    script.sram_storage_size as usize,
                )
            };
            cpu_sram.copy_from_slice(gpu_sram);
            // Save flash d_i before GPU modifies it (apply_flash_din reads this)
            saved_flash_d_i = unsafe {
                let fs = &*(backend.flash_state_buffer.contents() as *const FlashState);
                if tick == 0 {
                    // Dump raw bytes at flash state location
                    let raw = std::slice::from_raw_parts(
                        backend.flash_state_buffer.contents() as *const u8,
                        std::mem::size_of::<FlashState>(),
                    );
                    eprintln!("  FlashState raw bytes (tick 0): {:02X?}", raw);
                    eprintln!("  FlashState fields: bit_count={}, byte_count={}, data_width={}, addr=0x{:08X}",
                        fs.bit_count, fs.byte_count, fs.data_width, fs.addr);
                    eprintln!("  FlashState fields: curr_byte=0x{:02X}, command=0x{:02X}, out_buffer=0x{:02X}",
                        fs.curr_byte, fs.command, fs.out_buffer);
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
                fs.d_i
            };
            if tick == 0 {
                eprintln!(
                    "  saved_flash_d_i = 0x{:02X} (right after assignment)",
                    saved_flash_d_i
                );
            }
        } else {
            saved_flash_d_i = 0;
        }

        // Encode and commit GPU batch
        let t_encode = std::time::Instant::now();
        let batch_done = backend.run_edges(batch, schedule_offset);
        let batch_schedule_start = schedule_offset;
        schedule_offset = (schedule_offset + batch) % backend.edges_per_period();
        prof_batch_encode += t_encode.elapsed().as_nanos() as u64;

        // Wait for GPU batch to complete
        let t_wait = std::time::Instant::now();
        backend.wait(batch_done);
        prof_gpu_wait += t_wait.elapsed().as_nanos() as u64;

        // ── Drain VCD ring buffer (stimulus + timing) ──────────────────────
        if vcd_enabled {
            let mut timed_transitions: Vec<(u64, usize, u32)> = Vec::new();

            for edge_in_batch in 0..batch {
                let slot_base = backend.vcd_snapshot(edge_in_batch);
                let input_state = &slot_base[..state_size];
                let output_state = &slot_base[state_size..];
                let edge_tick = tick + edge_in_batch;

                // Stimulus VCD
                let t_stim = std::time::Instant::now();
                if let Some((ref mut writer, ref mapping, ref mut prev_values)) =
                    stimulus_vcd_state
                {
                    let t_edge = edge_tick as u64 * backend.gcd_ps();
                    writer.timestamp(t_edge).unwrap();
                    for (sig_idx, &(pos, vid, _is_clock)) in mapping.signals.iter().enumerate() {
                        let val = ((input_state[(pos >> 5) as usize] >> (pos & 31)) & 1) as u8;
                        if val != prev_values[sig_idx] {
                            writer
                                .change_scalar(vid, bit_to_vcd_value(val))
                                .unwrap();
                            prev_values[sig_idx] = val;
                        }
                    }
                }
                prof_stimulus_vcd += t_stim.elapsed().as_nanos() as u64;

                // Output VCD
                let t_timing = std::time::Instant::now();
                if let Some((ref mut writer, ref mapping, ref mut prev_values)) =
                    output_vcd_state
                {
                    let half_period = clock_period_ps / 2;
                    let base_timestamp = edge_tick as u64 * clock_period_ps + half_period;

                    // ADR 0012: apply jitter displacement to base timestamp.
                    // Draw from each domain's PRNG at every tick to keep
                    // streams stable regardless of which domain fires.
                    let sched_idx = (batch_schedule_start + edge_in_batch)
                        % backend.edges_per_period();
                    let mut jitter_displacement: i64 = 0;
                    if jitter_active {
                        use rand::Rng;
                        let tick_edges = &scheduler.schedule[sched_idx];
                        for (di, rng_opt) in domain_rngs.iter_mut().enumerate() {
                            if let Some((ref mut rng, budget)) = rng_opt {
                                let b = *budget;
                                let d: i64 = rng.gen_range(-b..=b);
                                if di < tick_edges.domain_edges.len()
                                    && tick_edges.domain_edges[di].1
                                {
                                    jitter_displacement = d;
                                }
                            }
                        }
                    }
                    let base_timestamp = if jitter_displacement >= 0 {
                        base_timestamp + jitter_displacement as u64
                    } else {
                        base_timestamp.saturating_sub((-jitter_displacement) as u64)
                    };
                    timed_transitions.clear();

                    for (i, &(output_aigpin, output_pos, _vid)) in
                        mapping.out2vcd.iter().enumerate()
                    {
                        // value_new encodes the VCD level: 0, 1, or 2 = X.
                        // Under xprop the output slot is [value | xmask | …];
                        // the xmask bit sits `rio` words after the value bit.
                        let value_new = match output_pos {
                            u32::MAX => {
                                assert!(output_aigpin <= 1);
                                output_aigpin as u32
                            }
                            pos => {
                                let w = (pos >> 5) as usize;
                                let v = (output_state[w] >> (pos & 31)) & 1;
                                if script.xprop_enabled
                                    && (output_state[rio + w] >> (pos & 31)) & 1 != 0
                                {
                                    2 // X
                                } else {
                                    v
                                }
                            }
                        };

                        if value_new == prev_values[i] {
                            continue;
                        }
                        prev_values[i] = value_new;

                        let arrival_ps = if output_pos != u32::MAX && arrival_state_offset > 0 {
                            let arrival_section = &output_state[arrival_state_offset as usize..];
                            let word_idx = (output_pos >> 5) as usize;
                            if word_idx < rio {
                                (arrival_section[word_idx] & 0xFFFF) as u64
                            } else {
                                0u64
                            }
                        } else {
                            0u64
                        };

                        let actual_timestamp = base_timestamp + arrival_ps;
                        timed_transitions.push((actual_timestamp, i, value_new));
                    }

                    timed_transitions.sort_by_key(|&(ts, _, _)| ts);
                    let mut current_timestamp = u64::MAX;
                    for &(ts, i, value_new) in &timed_transitions {
                        if ts != current_timestamp {
                            writer.timestamp(ts).unwrap();
                            current_timestamp = ts;
                        }
                        let (_, _, vid) = mapping.out2vcd[i];
                        let vcd_val = match value_new {
                            0 => vcd_ng::Value::V0,
                            1 => vcd_ng::Value::V1,
                            _ => vcd_ng::Value::X, // 2 = X (xprop)
                        };
                        writer.change_scalar(vid, vcd_val).unwrap();
                    }
                }
                prof_output_vcd += t_timing.elapsed().as_nanos() as u64;
            }
        }

        // ── Dump DFF states (if active for this cycle) ──
        if dff_dump_active && tick >= reset_edges {
            use std::io::Write;
            let input_state: &[u32] = unsafe {
                std::slice::from_raw_parts(backend.states_buffer.contents() as *const u32, state_size)
            };
            let output_state_dff: &[u32] = unsafe {
                std::slice::from_raw_parts(
                    (backend.states_buffer.contents() as *const u32).add(state_size),
                    state_size,
                )
            };
            let cycle = tick - reset_edges;
            if let Some((ref mut writer, ref entries, _)) = dff_dump_state {
                // Compute hash for both input_state (pre-capture) and output_state (post-capture)
                let mut in_hash: u64 = 0;
                let mut in_ones: usize = 0;
                let mut out_hash: u64 = 0;
                let mut out_ones: usize = 0;
                let mut diffs: usize = 0;
                for e in entries.iter() {
                    let word = (e.q_pos / 32) as usize;
                    let bit = e.q_pos & 31;
                    let in_val = if word < input_state.len() {
                        ((input_state[word] >> bit) & 1) as u8
                    } else {
                        0
                    };
                    let out_val = if word < output_state_dff.len() {
                        ((output_state_dff[word] >> bit) & 1) as u8
                    } else {
                        0
                    };
                    if in_val != 0 {
                        in_ones += 1;
                        in_hash = in_hash
                            .wrapping_mul(6364136223846793005)
                            .wrapping_add(e.q_pos as u64);
                    }
                    if out_val != 0 {
                        out_ones += 1;
                        out_hash = out_hash
                            .wrapping_mul(6364136223846793005)
                            .wrapping_add(e.q_pos as u64);
                    }
                    if in_val != out_val {
                        diffs += 1;
                    }
                }

                writeln!(writer, "CYCLE {} input_hash={:016X} input_ones={}/{} output_hash={:016X} output_ones={}/{} diffs={}",
                    cycle, in_hash, in_ones, entries.len(),
                    out_hash, out_ones, entries.len(), diffs).unwrap();
                // Write per-DFF values (both input and output)
                for e in entries.iter() {
                    let word = (e.q_pos / 32) as usize;
                    let bit = e.q_pos & 31;
                    let in_val = if word < input_state.len() {
                        ((input_state[word] >> bit) & 1) as u8
                    } else {
                        0
                    };
                    let out_val = if word < output_state_dff.len() {
                        ((output_state_dff[word] >> bit) & 1) as u8
                    } else {
                        0
                    };
                    let marker = if in_val != out_val { " *" } else { "" };
                    writeln!(writer, "{} in={} out={}{}", e.name, in_val, out_val, marker).unwrap();
                }
                writeln!(writer).unwrap();
            }
        }

        // Per-tick flash signal diagnostic
        if trace_ticks > 0 && tick + batch <= reset_edges + trace_ticks as usize {
            let output_state: &[u32] = unsafe {
                std::slice::from_raw_parts(
                    (backend.states_buffer.contents() as *const u32).add(state_size),
                    state_size,
                )
            };
            let fmp =
                unsafe { &*(backend.flash_model_params_buffer.contents() as *const FlashModelParams) };
            let flash_clk_pos = fmp.clk_out_pos;
            let flash_csn_pos = fmp.csn_out_pos;
            let flash_clk =
                (output_state[(flash_clk_pos >> 5) as usize] >> (flash_clk_pos & 31)) & 1;
            let flash_csn =
                (output_state[(flash_csn_pos >> 5) as usize] >> (flash_csn_pos & 31)) & 1;
            let fs = unsafe { &*(backend.flash_state_buffer.contents() as *const FlashState) };
            clilog::debug!("FLASH_TRACE tick {}: clk={} csn={} d_i=0x{:02X} cmd=0x{:02X} addr=0x{:06X} in_reset={}",
                tick + batch, flash_clk, flash_csn, fs.d_i, fs.command, fs.addr, fs.in_reset);
        }

        // ── CPU verification: simulate same edge on CPU and compare ──
        if opts.check_with_cpu && tick < cpu_check_max_edges && batch == 1 {
            // CPU state_prep: copy output → input, apply edge ops at the
            // current schedule position.
            let cpu_sched_idx = schedule_offset % backend.edges_per_period();
            cpu_states.copy_within(state_size..2 * state_size, 0);
            let cpu_edge_ops = backend.edge_ops(cpu_sched_idx);
            for op in cpu_edge_ops {
                let word_idx = op.position as usize >> 5;
                let bit_mask = 1u32 << (op.position & 31);
                if op.value != 0 {
                    cpu_states[word_idx] |= bit_mask;
                } else {
                    cpu_states[word_idx] &= !bit_mask;
                }
            }

            // CPU apply_flash_din
            {
                let p = unsafe { &*(backend.flash_din_params_buffer.contents() as *const FlashDinParams) };
                if tick == 0 {
                    eprintln!(
                        "  CPU flash_din: has_flash={}, d_i=0x{:02X}, d_in_pos={:?}",
                        p.has_flash, saved_flash_d_i, p.d_in_pos
                    );
                }
                if p.has_flash != 0 {
                    for i in 0..4usize {
                        let pos = p.d_in_pos[i];
                        if pos == 0xFFFFFFFF {
                            continue;
                        }
                        let word_idx = (pos >> 5) as usize;
                        let bit_mask = 1u32 << (pos & 31);
                        if (saved_flash_d_i >> i) & 1 != 0 {
                            cpu_states[word_idx] |= bit_mask;
                        } else {
                            cpu_states[word_idx] &= !bit_mask;
                        }
                    }
                }
                if tick == 0 {
                    eprintln!("  CPU after flash_din: word[4]=0x{:08X}", cpu_states[4]);
                }
            }

            // CPU simulate: run all partitions for this edge
            let use_diag = tick == reset_edges; // first edge after reset release
            if use_diag {
                eprintln!(
                    "=== DIAG: Edge simulate at edge {} (first post-reset) ===",
                    tick
                );
                eprintln!(
                    "  input_state[0]=0x{:08X} (bit0=posedge={})",
                    cpu_states[0],
                    cpu_states[0] & 1
                );
            }
            for stage_i in 0..num_major_stages {
                for block_i in 0..num_blocks {
                    let start = script.blocks_start[stage_i * num_blocks + block_i];
                    let end = script.blocks_start[stage_i * num_blocks + block_i + 1];
                    if start == end {
                        continue;
                    }
                    let (input_half, output_half) = cpu_states.split_at_mut(state_size);
                    if use_diag && block_i < 3 {
                        eprintln!(
                            "  Block {}/{} (stage {}): script range {}..{} ({} words)",
                            block_i,
                            num_blocks,
                            stage_i,
                            start,
                            end,
                            end - start
                        );
                        simulate_block_v1_diag(
                            &script.blocks_data[start..end],
                            input_half,
                            output_half,
                            &mut cpu_sram,
                        );
                    } else {
                        simulate_block_v1(
                            &script.blocks_data[start..end],
                            input_half,
                            output_half,
                            &mut cpu_sram,
                        );
                    }
                }
            }

            // Direct AIG evaluation comparison at first few post-reset edges
            if tick >= reset_edges && tick <= reset_edges + 5 {
                let (input_half, output_half) = cpu_states.split_at(state_size);
                compare_aig_vs_flattened(
                    aig,
                    input_half,
                    output_half,
                    script,
                    state_size,
                    tick,
                    Some(netlistdb),
                );
            }

            // Compare GPU input with CPU input (should match after state_prep+flash_din)
            let gpu_states: &[u32] = unsafe {
                std::slice::from_raw_parts(backend.states_buffer.contents() as *const u32, 2 * state_size)
            };
            let mut input_mismatches = 0;
            for i in 0..state_size {
                if gpu_states[i] != cpu_states[i] {
                    if input_mismatches < 5 {
                        let diff = gpu_states[i] ^ cpu_states[i];
                        let mut bits = Vec::new();
                        for b in 0..32 {
                            if (diff >> b) & 1 != 0 {
                                bits.push(i as u32 * 32 + b);
                            }
                        }
                        eprintln!(
                            "  INPUT MISMATCH word[{}]: GPU=0x{:08X} CPU=0x{:08X} bits={:?}",
                            i, gpu_states[i], cpu_states[i], bits
                        );
                    }
                    input_mismatches += 1;
                }
            }

            // Compare GPU output with CPU output
            let gpu_output = &gpu_states[state_size..2 * state_size];
            let cpu_output = &cpu_states[state_size..2 * state_size];
            let mut mismatches = 0;
            let mut _first_mismatch_word = 0;
            for i in 0..state_size {
                if gpu_output[i] != cpu_output[i] {
                    if mismatches < 5 {
                        let diff = gpu_output[i] ^ cpu_output[i];
                        let mut bits = Vec::new();
                        for b in 0..32 {
                            if (diff >> b) & 1 != 0 {
                                bits.push(i as u32 * 32 + b);
                            }
                        }
                        eprintln!(
                            "  OUTPUT MISMATCH word[{}]: GPU=0x{:08X} CPU=0x{:08X} bits={:?}",
                            i, gpu_output[i], cpu_output[i], bits
                        );
                    }
                    if mismatches == 0 {
                        _first_mismatch_word = i;
                    }
                    mismatches += 1;
                }
            }
            // Also compare SRAM
            let gpu_sram: &[u32] = unsafe {
                std::slice::from_raw_parts(
                    backend.sram_data_buffer.contents() as *const u32,
                    script.sram_storage_size as usize,
                )
            };
            let mut sram_mismatches = 0;
            for i in 0..script.sram_storage_size as usize {
                if gpu_sram[i] != cpu_sram[i] {
                    if sram_mismatches < 3 {
                        eprintln!(
                            "  SRAM MISMATCH [{}]: GPU=0x{:08X} CPU=0x{:08X}",
                            i, gpu_sram[i], cpu_sram[i]
                        );
                    }
                    sram_mismatches += 1;
                }
            }
            if input_mismatches > 0 || mismatches > 0 || sram_mismatches > 0 {
                eprintln!(
                    "CHECK-WITH-CPU tick {}: {} input, {} output, {} SRAM mismatches",
                    tick, input_mismatches, mismatches, sram_mismatches
                );
                cpu_check_mismatches += mismatches;
                if cpu_check_mismatches > 200 {
                    eprintln!("Too many mismatches, stopping CPU check");
                }
            } else if tick <= reset_edges + 5 || tick % 50 == 0 {
                eprintln!(
                    "CHECK-WITH-CPU tick {}: OK (state_size={}, sram_size={})",
                    tick, state_size, script.sram_storage_size
                );
            }

            // Per-tick output state change tracking
            if tick >= reset_edges.saturating_sub(2) && tick <= reset_edges + 15 {
                let gpu_output = &gpu_states[state_size..2 * state_size];
                let _changed_words: usize = gpu_output
                    .iter()
                    .zip(cpu_output.iter())
                    .filter(|(a, b)| a != b)
                    .count();
                let output_bits_set: usize =
                    gpu_output.iter().map(|w| w.count_ones() as usize).sum();
                let changed_from_prev: usize = if let Some(ref snap) = post_reset_state_snapshot {
                    gpu_output
                        .iter()
                        .zip(snap.iter())
                        .map(|(a, b)| (a ^ b).count_ones() as usize)
                        .sum()
                } else {
                    0
                };
                eprintln!(
                    "TICK-TRACE {}: output_bits_set={}, changed_from_prev={}",
                    tick, output_bits_set, changed_from_prev
                );
                post_reset_state_snapshot = Some(gpu_output.to_vec());
            }
        }

        // Drain UART channels (CPU reads decoded bytes from GPU ring buffers)
        let t_drain = std::time::Instant::now();
        unsafe {
            let base = backend.uart_channel_buffer.contents() as *const UartChannel;
            for i in 0..n_uarts {
                let channel = &*base.add(i);
                let name = &uart_names[i];
                while uart_read_heads[i] < channel.write_head {
                    let byte =
                        channel.data[(uart_read_heads[i] % channel.capacity) as usize];
                    let ch = if byte >= 32 && byte < 127 {
                        byte as char
                    } else {
                        '.'
                    };
                    clilog::info!("UART '{}' TX: 0x{:02X} '{}'", name, byte, ch);
                    uart_events.push(UartEvent {
                        timestamp: tick,
                        peripheral: name.clone(),
                        event: "tx".to_string(),
                        payload: byte,
                    });
                    if let Some(d) = input_dispatcher.as_mut() {
                        d.on_event(name, "tx", &serde_json::json!(byte));
                    }
                    uart_read_heads[i] += 1;
                }
            }
        }
        // Drain WB trace channel
        unsafe {
            let ch = &*(backend.wb_trace_channel_buffer.contents() as *const WbTraceChannel);
            let entries_ptr =
                (backend.wb_trace_channel_buffer.contents() as *const u8).add(16) as *const WbTraceEntry;
            while wb_trace_read_head < ch.write_head {
                let idx = (wb_trace_read_head % ch.capacity) as usize;
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
                wb_trace_read_head += 1;
            }
        }
        // Drain AHB/APB bus trace channel: route each raw beat to its
        // bus's protocol decoder, collecting completed transactions.
        if !bus_lanes.is_empty() {
            use crate::sim::models::bus_trace::RawBeat;
            unsafe {
                let ch = &*(backend.bus_trace_channel_buffer.contents() as *const BusTraceChannel);
                let entries_ptr = (backend.bus_trace_channel_buffer.contents() as *const u8)
                    .add(std::mem::size_of::<BusTraceChannel>())
                    as *const BusTraceEntry;
                while bus_trace_read_head < ch.write_head {
                    let idx = (bus_trace_read_head % ch.capacity) as usize;
                    let e = &*entries_ptr.add(idx);
                    let bus_id = (e.flags >> 8) & 0xFF;
                    if let Some(lane) = bus_lanes.get_mut(bus_id as usize) {
                        let beat = RawBeat {
                            tick: e.tick as u64,
                            bus_id,
                            write: (e.flags & 1) != 0,
                            err: (e.flags >> 1) & 1 != 0,
                            addr: e.addr as u64,
                            wdata: e.wdata as u64,
                            rdata: e.rdata as u64,
                        };
                        if let Some(txn) = lane.decoder.push(beat) {
                            bus_transactions.push((bus_id, txn));
                        }
                    }
                    bus_trace_read_head += 1;
                }
            }
        }
        prof_drain += t_drain.elapsed().as_nanos() as u64;

        total_batches += 1;
        if batch == 1 {
            single_edge_batches += 1;
        }
        if batch > max_batch_seen {
            max_batch_seen = batch;
        }
        tick += batch;

        // SRAM write dumper: snapshot per batch and accumulate
        // per-block write events. Cheap when disabled (None).
        if let Some(dumper) = sram_dumper.as_mut() {
            let storage: &[u32] = unsafe {
                std::slice::from_raw_parts(
                    backend.sram_data_buffer.contents() as *const u32,
                    script.sram_storage_size as usize,
                )
            };
            dumper.snapshot_and_diff(storage, tick as u64);
        }

        // Deep diagnostics: SRAM activity + flash transaction tracking
        if deep_diag && tick > reset_edges && batch == 1 {
            unsafe {
                let fs = &*(backend.flash_state_buffer.contents() as *const FlashState);
                let fmp = &*(backend.flash_model_params_buffer.contents() as *const FlashModelParams);
                let st = std::slice::from_raw_parts(
                    backend.states_buffer.contents() as *const u32,
                    2 * state_size,
                );
                let read_out_bit = |pos: u32| -> u32 {
                    let word_idx = state_size + (pos as usize >> 5);
                    let bit_idx = pos & 31;
                    (st[word_idx] >> bit_idx) & 1
                };
                let csn_val = read_out_bit(fmp.csn_out_pos);

                // Detect CSN transitions (transaction boundaries)
                if csn_val == 1 && diag_prev_csn == 0 {
                    // CSN rising edge = transaction complete
                    eprintln!(
                        "DIAG T{}: Flash transaction end: cmd=0x{:02X} addr=0x{:06X} bytes={}",
                        tick, fs.command, fs.addr, fs.byte_count
                    );
                }
                if csn_val == 0 && diag_prev_csn == 1 {
                    eprintln!("DIAG T{}: Flash transaction start", tick);
                }
                diag_prev_csn = csn_val;

                // Track address changes
                if fs.addr != diag_prev_flash_addr || fs.command != diag_prev_flash_cmd {
                    if fs.command != 0 {
                        eprintln!("DIAG T{}: Flash addr changed: cmd=0x{:02X} addr=0x{:06X} bytes={} bitc={}",
                            tick, fs.command, fs.addr, fs.byte_count, fs.bit_count);
                    }
                    diag_prev_flash_addr = fs.addr;
                    diag_prev_flash_cmd = fs.command;
                }

                // Check SRAM activity every 100 ticks
                if tick % 100 == 0 {
                    let sram = std::slice::from_raw_parts(
                        backend.sram_data_buffer.contents() as *const u32,
                        script.sram_storage_size as usize,
                    );
                    let nonzero = sram.iter().filter(|&&w| w != 0).count();
                    if nonzero != diag_sram_write_count {
                        eprintln!(
                            "DIAG T{}: SRAM non-zero words: {} (was {})",
                            tick, nonzero, diag_sram_write_count
                        );
                        diag_sram_write_count = nonzero;
                    }
                }
            }
        }

        // Diagnostic: dump flash-related signals
        if trace_ticks > 0 && tick <= reset_edges + trace_ticks && tick > reset_edges {
            unsafe {
                let st = std::slice::from_raw_parts(
                    backend.states_buffer.contents() as *const u32,
                    2 * state_size,
                );
                let read_out_bit = |pos: u32| -> u32 {
                    let word_idx = state_size + (pos as usize >> 5);
                    let bit_idx = pos & 31;
                    (st[word_idx] >> bit_idx) & 1
                };
                let read_in_bit = |pos: u32| -> u32 {
                    let word_idx = (pos as usize) >> 5;
                    let bit_idx = pos & 31;
                    (st[word_idx] >> bit_idx) & 1
                };
                let fmp = &*(backend.flash_model_params_buffer.contents() as *const FlashModelParams);
                let fdp = &*(backend.flash_din_params_buffer.contents() as *const FlashDinParams);
                let fs = &*(backend.flash_state_buffer.contents() as *const FlashState);
                let clk_val = read_out_bit(fmp.clk_out_pos);
                let csn_val = read_out_bit(fmp.csn_out_pos);
                let mut d_out_vals = [0u32; 4];
                for i in 0..4 {
                    if fmp.d_out_pos[i] != 0xFFFFFFFF {
                        d_out_vals[i] = read_out_bit(fmp.d_out_pos[i]);
                    }
                }
                let mut d_in_vals = [0u32; 4];
                for i in 0..4 {
                    if fdp.d_in_pos[i] != 0xFFFFFFFF {
                        d_in_vals[i] = read_in_bit(fdp.d_in_pos[i]);
                    }
                }
                eprintln!("T{:>4}: clk={} csn={} d_o={}{}{}{} d_i={}{}{}{} | fs: d_i=0x{:X} cmd=0x{:02X} bc={} bitc={} dw={} addr=0x{:06X} pclk={} pcsn={} mcsn={} inr={}",
                    tick, clk_val, csn_val,
                    d_out_vals[3], d_out_vals[2], d_out_vals[1], d_out_vals[0],
                    d_in_vals[3], d_in_vals[2], d_in_vals[1], d_in_vals[0],
                    fs.d_i, fs.command, fs.byte_count, fs.bit_count, fs.data_width, fs.addr,
                    fs.prev_clk, fs.prev_csn, fs.model_prev_csn, fs.in_reset);
            }
        }

        // Progress logging: once per ~100k-edge window. Gate on the
        // actual `batch` (not the BATCH_SIZE constant) — VCD/trace flags
        // force single-tick mode (batch=1), where the old constant made
        // this fire BATCH_SIZE times per window and flood the log.
        if tick > 0 && tick % 100000 < batch {
            let elapsed = sim_start.elapsed();
            let us_per_edge = elapsed.as_micros() as f64 / tick as f64;
            // Read first UART's TX bit and decoder state for diagnostics
            let (uart_tx_val, uart_dec_state, uart_dec_cycle) = unsafe {
                let up = &*(backend.uart_params_buffer.contents() as *const UartParams);
                if up.n_uarts > 0 {
                    let st = std::slice::from_raw_parts(
                        backend.states_buffer.contents() as *const u32,
                        2 * state_size,
                    );
                    let tx_pos = up.channels[0].tx_out_pos;
                    let tx_word = state_size + (tx_pos as usize >> 5);
                    let tx_bit = tx_pos & 31;
                    let tx_val = (st[tx_word] >> tx_bit) & 1;
                    let us = &*(backend.uart_state_buffer.contents() as *const UartDecoderState);
                    (tx_val, us.state, us.current_cycle)
                } else {
                    (0, 0, 0)
                }
            };
            clilog::info!(
                "Tick {} / {} ({:.1}μs/tick, batches={}, UART bytes={}, tx={}, uart_st={}, uart_cyc={})",
                tick, max_edges, us_per_edge, total_batches, uart_events.len(),
                uart_tx_val, uart_dec_state, uart_dec_cycle
            );
        }
        if tick >= reset_edges && tick - batch < reset_edges {
            clilog::info!("Reset released at tick {}", reset_edges);
            // Snapshot output state just after reset for change detection
            if post_reset_state_snapshot.is_none() {
                let st = unsafe {
                    std::slice::from_raw_parts(
                        backend.states_buffer.contents() as *const u32,
                        2 * state_size,
                    )
                };
                post_reset_state_snapshot = Some(st[state_size..2 * state_size].to_vec());
            }
        }
    }

    // Print profiling results
    let total_ns =
        prof_batch_encode + prof_gpu_wait + prof_drain + prof_stimulus_vcd + prof_output_vcd;
    let print_prof = |name: &str, ns: u64| {
        let us = ns as f64 / 1000.0 / max_edges as f64;
        let pct = if total_ns > 0 {
            100.0 * ns as f64 / total_ns as f64
        } else {
            0.0
        };
        println!("  {:<32} {:>8.1}μs/tick  {:>5.1}%", name, us, pct);
    };
    println!();
    println!("=== Profiling Breakdown ===");
    print_prof("Batch encode + commit", prof_batch_encode);
    print_prof("GPU wait (spin)", prof_gpu_wait);
    print_prof("UART channel drain", prof_drain);
    print_prof("Stimulus VCD write", prof_stimulus_vcd);
    print_prof("Output VCD write", prof_output_vcd);
    println!(
        "  {:<32} {:>8.1}μs/tick  100.0%",
        "TOTAL (instrumented)",
        total_ns as f64 / 1000.0 / max_edges as f64
    );
    println!();
    println!("  Total batches:                 {}", total_batches);
    let total_edges = tick.max(1);
    let batched_edges = total_edges.saturating_sub(single_edge_batches as usize);
    let mean_batch = total_edges as f64 / total_batches.max(1) as f64;
    println!(
        "  Batch utilisation:             {}/{} edges batched ({:.1}%), \
         {} single-edge commits, mean batch={:.1}, max batch={}",
        batched_edges,
        total_edges,
        100.0 * batched_edges as f64 / total_edges as f64,
        single_edge_batches,
        mean_batch,
        max_batch_seen,
    );

    let sim_elapsed = sim_start.elapsed();
    clilog::finish!(timer_sim);

    // ── Bus transaction trace output (ADR 0013) ──────────────────────────
    if !bus_lanes.is_empty() {
        clilog::info!(
            "bus-trace: decoded {} transaction(s) across {} bus(es)",
            bus_transactions.len(),
            bus_lanes.len()
        );
        if let Some(csv_path) = opts.bus_trace_csv.as_ref() {
            match write_bus_trace_csv(csv_path, &bus_transactions, &bus_lanes) {
                Ok(()) => clilog::info!(
                    "bus-trace: wrote {} transaction(s) to {}",
                    bus_transactions.len(),
                    csv_path.display()
                ),
                Err(e) => clilog::warn!(
                    "bus-trace: failed to write CSV to {}: {}",
                    csv_path.display(),
                    e
                ),
            }
        }
    }

    // ── State buffer diagnostics ─────────────────────────────────────────

    unsafe {
        let st = std::slice::from_raw_parts(backend.states_buffer.contents() as *const u32, 2 * state_size);
        let input_state = &st[..state_size];
        let output_state = &st[state_size..2 * state_size];
        let in_nonzero = input_state.iter().filter(|&&w| w != 0).count();
        let out_nonzero = output_state.iter().filter(|&&w| w != 0).count();
        let in_popcount: u32 = input_state.iter().map(|w| w.count_ones()).sum();
        let out_popcount: u32 = output_state.iter().map(|w| w.count_ones()).sum();
        println!();
        println!("=== State Buffer Diagnostics ===");
        println!(
            "State size: {} words ({} bits)",
            state_size,
            state_size * 32
        );
        println!(
            "Input state:  {} non-zero words, {} bits set",
            in_nonzero, in_popcount
        );
        println!(
            "Output state: {} non-zero words, {} bits set",
            out_nonzero, out_popcount
        );
        // Compare with post-reset snapshot if available
        if let Some(ref snapshot) = post_reset_state_snapshot {
            let mut changed_words = 0;
            let mut changed_bits = 0u32;
            for (i, (&cur, &snap)) in output_state.iter().zip(snapshot.iter()).enumerate() {
                let diff = cur ^ snap;
                if diff != 0 {
                    changed_words += 1;
                    changed_bits += diff.count_ones();
                    if changed_words <= 10 {
                        println!(
                            "  CHANGED output[{}]: 0x{:08X} → 0x{:08X} (diff=0x{:08X})",
                            i, snap, cur, diff
                        );
                    }
                }
            }
            println!(
                "Output state changes since tick {}: {} words, {} bits changed",
                config.reset_cycles + 1,
                changed_words,
                changed_bits
            );
        }
    }

    // ── Check GPU flash state for errors ─────────────────────────────────

    let last_error_cmd = unsafe {
        let fs = &*(backend.flash_state_buffer.contents() as *const FlashState);
        fs.last_error_cmd
    };
    if last_error_cmd != 0 {
        clilog::warn!(
            "GPU flash model encountered unknown command: 0x{:02X}",
            last_error_cmd
        );
    }

    // ── Results ──────────────────────────────────────────────────────────

    println!();
    println!("=== GPU Simulation Results ===");
    let edges_actually_simulated = if stop_triggered { tick } else { max_edges };
    if stop_triggered {
        println!(
            "Edges simulated: {} (stopped early via input.json `stop`)",
            edges_actually_simulated
        );
    } else {
        println!("Edges simulated: {}", edges_actually_simulated);
    }
    println!("UART bytes received: {}", uart_events.len());

    if edges_actually_simulated > 0 {
        let us_per_edge = sim_elapsed.as_micros() as f64 / edges_actually_simulated as f64;
        println!(
            "Time per edge: {:.1}μs ({:.1}s total)",
            us_per_edge,
            sim_elapsed.as_secs_f64()
        );
    }

    if let Some(dumper) = sram_dumper.as_ref() {
        match dumper.write_dump() {
            Ok(n) => {
                println!(
                    "JACQUARD_SRAM_DUMP: wrote {} write event(s) across {} block(s) → {}",
                    n,
                    dumper.block_count(),
                    dumper.output_path().display()
                );
            }
            Err(e) => {
                eprintln!(
                    "JACQUARD_SRAM_DUMP: failed to write {}: {}",
                    dumper.output_path().display(),
                    e
                );
            }
        }
    }

    // Warn about input commands that never fired (likely a wait that the
    // firmware didn't satisfy within --max-clock-edges).
    if let Some(d) = input_dispatcher.as_ref() {
        let remaining = d.remaining();
        if remaining > 0 && !d.stopped() {
            clilog::warn!(
                "{} input command(s) remain unconsumed at end of simulation \
                 (cursor stuck at index {} — likely a wait that never matched)",
                remaining,
                d.cursor()
            );
        }
    }

    // Print UART output as string
    if !uart_events.is_empty() {
        let uart_str: String = uart_events
            .iter()
            .map(|e| {
                if e.payload >= 32 && e.payload < 127 {
                    e.payload as char
                } else if e.payload == b'\n' {
                    '\n'
                } else if e.payload == b'\r' {
                    '\r'
                } else {
                    '.'
                }
            })
            .collect();
        println!("UART output:\n{}", uart_str);
    }

    // Print flash model stats (GPU-side state)
    if config.flash.is_some() {
        let fs = unsafe { &*(backend.flash_state_buffer.contents() as *const FlashState) };
        println!(
            "GPU Flash model: command=0x{:02X}, byte_count={}, addr=0x{:06X}, error_cmd=0x{:X}",
            fs.command, fs.byte_count, fs.addr, fs.last_error_cmd
        );
    }

    // Output events to JSON
    if let Some(ref output_path) = config.output_events {
        #[derive(serde::Serialize)]
        struct EventsOutput {
            events: Vec<UartEvent>,
        }
        let output = EventsOutput {
            events: uart_events.clone(),
        };
        let json = serde_json::to_string_pretty(&output).expect("Failed to serialize events");
        crate::sim::vcd_io::ensure_parent_dir(std::path::Path::new(output_path))
            .expect("Failed to create events output dir");
        let mut file = std::fs::File::create(output_path).expect("Failed to create events file");
        use std::io::Write;
        file.write_all(json.as_bytes())
            .expect("Failed to write events");
        clilog::info!("Wrote events to {}", output_path);
    }

    // ── Event reference comparison ───────────────────────────────────────

    let mut events_passed = true;
    if let Some(ref ref_path) = config.events_reference {
        // Parse tolerantly: the reference file may contain non-UART
        // events (e.g. SPI deselect entries from cxxrtl-generated
        // references) whose `payload` field is a string rather than a
        // u8. Skip events that don't match `UartEvent`'s schema so the
        // UART comparison can still proceed.
        let ref_file = std::fs::read_to_string(ref_path)
            .unwrap_or_else(|e| panic!("Failed to read events reference {}: {}", ref_path, e));
        let raw: serde_json::Value = serde_json::from_str(&ref_file)
            .unwrap_or_else(|e| panic!("Failed to parse events reference {}: {}", ref_path, e));
        let raw_events = raw
            .get("events")
            .and_then(|v| v.as_array())
            .unwrap_or_else(|| {
                panic!(
                    "Events reference {} has no top-level 'events' array",
                    ref_path
                )
            });
        let total_in_ref = raw_events.len();
        let ref_events: Vec<UartEvent> = raw_events
            .iter()
            .filter_map(|e| serde_json::from_value::<UartEvent>(e.clone()).ok())
            .collect();
        let skipped = total_in_ref - ref_events.len();
        if skipped > 0 {
            clilog::info!(
                "Events reference {}: skipped {} non-UART events (e.g. SPI deselect), kept {} UART events",
                ref_path,
                skipped,
                ref_events.len()
            );
        }
        let ref_events = &ref_events;
        let ref_payloads: Vec<u8> = ref_events.iter().map(|e| e.payload).collect();
        let actual_payloads: Vec<u8> = uart_events.iter().map(|e| e.payload).collect();

        println!();
        println!("=== Event Reference Check ===");
        println!("Reference: {} events from {}", ref_events.len(), ref_path);
        println!("Actual:    {} events", uart_events.len());

        if ref_payloads.len() > actual_payloads.len() {
            println!(
                "FAIL: missing {} events (got {}, expected {})",
                ref_payloads.len() - actual_payloads.len(),
                actual_payloads.len(),
                ref_payloads.len()
            );
            println!(
                "  Hint: increase --max-cycles (last reference event at timestamp {})",
                ref_events.last().map(|e| e.timestamp).unwrap_or(0)
            );
            events_passed = false;
        } else {
            let mut mismatches = 0;
            for (i, (expected, actual)) in
                ref_payloads.iter().zip(actual_payloads.iter()).enumerate()
            {
                if expected != actual {
                    if mismatches < 10 {
                        let ref_ts = ref_events[i].timestamp;
                        let act_ts = uart_events[i].timestamp;
                        println!("  MISMATCH event {}: expected 0x{:02X} (ref ts={}), got 0x{:02X} (tick={})",
                            i, expected, ref_ts, actual, act_ts);
                    }
                    mismatches += 1;
                }
            }

            if mismatches > 0 {
                println!(
                    "FAIL: {} payload mismatches out of {} events",
                    mismatches,
                    ref_payloads.len()
                );
                events_passed = false;
            } else {
                // Decode the matched message for display
                let decoded: String = actual_payloads
                    .iter()
                    .map(|&b| if b >= 32 && b < 127 { b as char } else { '.' })
                    .collect();
                println!("PASS: all {} event payloads match", ref_payloads.len());
                println!("  Decoded: \"{}\"", decoded);
            }
        }
    }

    // ── Optional CPU verification ────────────────────────────────────────

    if opts.check_with_cpu {
        if cpu_check_mismatches == 0 {
            clilog::info!(
                "CPU verification: PASSED ({} ticks checked)",
                cpu_check_max_edges.min(max_edges)
            );
        } else {
            clilog::warn!(
                "CPU verification: {} total mismatches in {} ticks",
                cpu_check_mismatches,
                cpu_check_max_edges.min(max_edges)
            );
        }
    }

    // Clean up event buffer
    unsafe {
        drop(Box::from_raw(event_buffer_ptr));
    }

    println!();
    if events_passed {
        println!("SIMULATION: PASSED");
    } else {
        println!("SIMULATION: FAILED (event mismatch)");
    }

    CosimResult {
        passed: events_passed,
        uart_events,
        edges_simulated: edges_actually_simulated,
    }
}
