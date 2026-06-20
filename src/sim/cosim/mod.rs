// SPDX-FileCopyrightText: Copyright (c) 2024 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0
//! Backend-agnostic co-simulation engine: the `CosimBackend` trait, the public
//! `CosimOpts`/`CosimResult` API, the generic `run_cosim_generic<B>` driver, the
//! multi-clock scheduler, the CPU baseline helpers, and the free-function ops
//! patchers. The GPU/Metal backend lives in the gated `metal` submodule.
//!
//! This module compiles with **no GPU feature** — it must contain zero `metal::`
//! / `MTLResourceOptions` dependencies. Anything GPU-specific belongs in
//! `metal.rs` behind `#[cfg(feature = "metal")]`.

use std::collections::HashMap;

use crate::aig::{DriverType, AIG};
use crate::flatten::FlattenedScriptV1;
use crate::sim::setup::LoadedDesign;
// Shared with the `xsources` driven-input computation so cosim's GPIO
// mapping and the static X-source query stay consistent (issue #98).
use crate::sim::x_sources::parse_gpio_index;
use crate::testbench::{CppSpiFlash, PortMapping, TestbenchConfig, UartEvent};
use netlistdb::{Direction, GeneralPinName, NetlistDB};

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

/// Bit set/clear operation (must match Metal shader BitOp struct).
#[repr(C)]
#[derive(Clone, Copy)]
struct BitOp {
    position: u32, // bit position in state buffer
    value: u32,    // 0 = clear, 1 = set
}

/// Maximum number of UART channels the cosim engine supports. Bounds the
/// agnostic per-run UART config sanity check and (in the Metal backend) the
/// GPU-side `UartParams`/`UartChannel` arrays.
const MAX_UARTS: usize = 4;

/// Batch size for backend dispatch: number of consecutive scheduler edges run
/// in one `run_edges` call (no per-tick CPU interaction within a batch).
const BATCH_SIZE: usize = 1024;

/// CPU-side per-bus lane: pairs a GPU capture slot with its name and
/// protocol decoder. Vec index == GPU `bus_id` packed into entry flags.
pub(crate) struct BusTraceLane {
    name: String,
    decoder: crate::sim::models::bus_trace::BusTraceDecoder,
}

// ── Bus-trace pin-position resolution (ADR 0013) ────────────────────────────
//
// Backend-agnostic counterparts of the Metal `BusTraceParams` widths. The Metal
// GPU struct (`metal::BusTraceParams`, `#[repr(C)]`) reuses these via `super::`
// so the agnostic resolution here and the GPU struct packing stay in lockstep.

/// Max buses a single cosim run may trace (bounds both backends' per-bus arrays).
pub(crate) const MAX_BUS_TRACES: usize = 4;
/// Max address-bus width captured per beat.
pub(crate) const BUS_TRACE_MAX_ADR_BITS: usize = 32;
/// Max data-bus width captured per beat.
pub(crate) const BUS_TRACE_MAX_DAT_BITS: usize = 32;

// ── Shared GPU peripheral `#[repr(C)]` ABI (Metal / CUDA / HIP) ──────────────
//
// These mirror the device structs in `csrc/kernel_v1.metal` (Metal) and
// `csrc/kernel_v1_impl.cuh` (CUDA/HIP) field-for-field. They were originally
// private to `metal.rs`; lifted here (Stage B B0) so all three GPU backends
// share one ABI definition instead of triple-maintaining it. Items are
// module-private to `mod.rs` (fields too) — the backend submodules are
// descendants and can read them via `super::`. Gated to GPU builds; the
// `CpuBackend` reference uses its own agnostic types (`CpuUartConfig`,
// `BusTracePositions`).
//
// The `size_of` asserts below pin the Rust layout; matching `static_assert`s in
// the device headers pin the C/Metal layout to the same byte counts. ABI drift
// (field order/padding) silently corrupts cross-FFI buffers — these are the
// compile-time guard. Keep both sides in lockstep.

/// UART ring-buffer byte capacity (per channel).
#[cfg(any(feature = "metal", feature = "cuda", feature = "hip"))]
const UART_CHANNEL_CAP: usize = 4096;

/// GPU-side UART decoder state (per channel, persistent across ticks).
#[cfg(any(feature = "metal", feature = "cuda", feature = "hip"))]
#[repr(C)]
struct UartDecoderState {
    state: u32, // 0=IDLE, 1=START, 2=DATA, 3=STOP
    last_tx: u32,
    start_cycle: u32,
    bits_received: u32,
    value: u32,
    current_cycle: u32,
}

/// Per-channel config within `UartParams`.
#[cfg(any(feature = "metal", feature = "cuda", feature = "hip"))]
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct UartPerChannelConfig {
    tx_out_pos: u32,
    cycles_per_bit: u32,
}

/// Parameters for UART in the `gpu_io_step` kernel.
#[cfg(any(feature = "metal", feature = "cuda", feature = "hip"))]
#[repr(C)]
struct UartParams {
    state_size: u32,
    n_uarts: u32,
    _pad: [u32; 2],
    channels: [UartPerChannelConfig; MAX_UARTS],
}

/// GPU→CPU UART byte ring buffer (one per channel).
#[cfg(any(feature = "metal", feature = "cuda", feature = "hip"))]
#[repr(C)]
struct UartChannel {
    write_head: u32,
    capacity: u32,
    _pad: [u32; 2],
    data: [u8; UART_CHANNEL_CAP],
}

#[cfg(any(feature = "metal", feature = "cuda", feature = "hip"))]
const WB_TRACE_MAX_ADR_BITS: usize = 30;
#[cfg(any(feature = "metal", feature = "cuda", feature = "hip"))]
const WB_TRACE_MAX_DAT_BITS: usize = 32;
#[cfg(any(feature = "metal", feature = "cuda", feature = "hip"))]
const WB_TRACE_CHANNEL_CAP: usize = 16384;

/// Parameters for the legacy Wishbone bus trace (`gpu_io_step`).
#[cfg(any(feature = "metal", feature = "cuda", feature = "hip"))]
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

/// Per-tick Wishbone bus snapshot.
#[cfg(any(feature = "metal", feature = "cuda", feature = "hip"))]
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct WbTraceEntry {
    tick: u32,
    flags: u32,
    ibus_adr: u32,
    ibus_rdata: u32,
    dbus_adr: u32,
}

/// GPU→CPU Wishbone trace ring buffer header (entries follow at byte 16).
#[cfg(any(feature = "metal", feature = "cuda", feature = "hip"))]
#[repr(C)]
struct WbTraceChannel {
    write_head: u32,
    capacity: u32,
    current_tick: u32,
    prev_flags: u32,
    // entries[capacity] follow in memory
}

/// AHB/APB bus-trace ring-buffer entry capacity (ADR 0013).
#[cfg(any(feature = "metal", feature = "cuda", feature = "hip"))]
const BUS_TRACE_CHANNEL_CAP: usize = 16384;

/// Per-bus signal positions (config-driven AHB/APB trace, ADR 0013).
#[cfg(any(feature = "metal", feature = "cuda", feature = "hip"))]
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

#[cfg(any(feature = "metal", feature = "cuda", feature = "hip"))]
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

/// All-bus params block bound to the `gpu_io_step` kernel.
#[cfg(any(feature = "metal", feature = "cuda", feature = "hip"))]
#[repr(C)]
struct BusTraceParamsAll {
    n_buses: u32,
    _pad: [u32; 3],
    buses: [BusTraceParams; MAX_BUS_TRACES],
}

/// Compact raw beat captured by the GPU on a bus's gating edge.
#[cfg(any(feature = "metal", feature = "cuda", feature = "hip"))]
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct BusTraceEntry {
    tick: u32,
    flags: u32,
    addr: u32,
    wdata: u32,
    rdata: u32,
}

/// GPU→CPU bus-trace ring buffer header (entries follow at byte 16).
#[cfg(any(feature = "metal", feature = "cuda", feature = "hip"))]
#[repr(C)]
struct BusTraceChannel {
    write_head: u32,
    capacity: u32,
    current_tick: u32,
    prev_gate: u32,
    // entries[capacity] follow in memory
}

// ── ABI size guards: Rust side (matching `static_assert`s in the device
// headers). Recompute if any struct changes; the device-side asserts must use
// the same literals.
#[cfg(any(feature = "metal", feature = "cuda", feature = "hip"))]
const _: () = {
    use std::mem::size_of;
    assert!(size_of::<UartDecoderState>() == 24);
    assert!(size_of::<UartPerChannelConfig>() == 8);
    assert!(size_of::<UartParams>() == 48);
    assert!(size_of::<UartChannel>() == 4112);
    assert!(size_of::<WbTraceParams>() == 404);
    assert!(size_of::<WbTraceEntry>() == 20);
    assert!(size_of::<WbTraceChannel>() == 16);
    assert!(size_of::<BusTraceParams>() == 416);
    assert!(size_of::<BusTraceParamsAll>() == 1680);
    assert!(size_of::<BusTraceEntry>() == 20);
    assert!(size_of::<BusTraceChannel>() == 16);
};

/// Backend-agnostic per-bus pin positions resolved from the netlist. Plain
/// Rust (no `repr(C)`, no `metal::`): the Metal backend packs these field-for-
/// field into the GPU `BusTraceParams`; the CPU backend reads them directly in
/// its bus-trace FSM. `0xFFFFFFFF` is the "unresolved / absent" sentinel,
/// matching the GPU `READ_OUT_BIT` short-circuit.
pub(crate) struct BusTracePositions {
    /// True iff this bus decodes APB3 (the only protocol gated in Phase 1).
    pub protocol_apb3: bool,
    pub addr_bits: usize,
    pub data_bits: usize,
    pub sel_pos: u32,
    pub enable_pos: u32,
    pub ready_pos: u32,
    pub write_pos: u32,
    pub resp_pos: u32,
    pub addr_pos: [u32; BUS_TRACE_MAX_ADR_BITS],
    pub wdata_pos: [u32; BUS_TRACE_MAX_DAT_BITS],
    pub rdata_pos: [u32; BUS_TRACE_MAX_DAT_BITS],
}

impl Default for BusTracePositions {
    fn default() -> Self {
        BusTracePositions {
            protocol_apb3: true, // APB3 == GPU `protocol: 0`
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

/// Resolve every configured bus's pin positions + build its CPU-side decoder
/// lane. Backend-agnostic (uses the agnostic `resolve_to_state_pos`); shared by
/// the Metal backend (which packs the positions into the GPU `BusTraceParams`)
/// and the CPU backend (which runs the beat FSM directly). Protocol-gated to
/// APB3 — other protocols are skipped with a warning (Phase 2). The returned
/// `Vec<BusTracePositions>` and `Vec<BusTraceLane>` are index-aligned: vec index
/// == GPU `bus_id` (packed into the beat `flags>>8`).
pub(crate) fn build_bus_trace(
    aig: &AIG,
    netlistdb: &NetlistDB,
    script: &FlattenedScriptV1,
    configs: &[crate::testbench::BusTraceConfig],
) -> (Vec<BusTracePositions>, Vec<BusTraceLane>) {
    use crate::sim::models::bus_trace::{pin_basename, BusTraceDecoder};
    use crate::sim::trace_signals::resolve_to_state_pos;
    use crate::testbench::BusProtocol;

    let mut positions: Vec<BusTracePositions> = Vec::new();
    let mut lanes: Vec<BusTraceLane> = Vec::new();

    let resolve =
        |name: &str| resolve_to_state_pos(aig, netlistdb, script, name).unwrap_or(0xFFFFFFFF);

    for cfg in configs.iter() {
        if positions.len() >= MAX_BUS_TRACES {
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

        let mut p = BusTracePositions {
            protocol_apb3: true,
            addr_bits,
            data_bits,
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

        positions.push(p);
        lanes.push(BusTraceLane {
            name: cfg.name.clone(),
            decoder: BusTraceDecoder::new(cfg.protocol),
        });
    }

    (positions, lanes)
}


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

// ── Shared GPU param builders (WB / APB bus trace) ──────────────────────────
//
// Resolve netlist pin positions into the GPU `WbTraceParams` / `BusTraceParamsAll`
// structs bound by `gpu_io_step`. Lifted from `metal.rs` (Stage B B2) so Metal
// and CUDA/HIP share one resolution path; the GPU struct bytes are unchanged
// from the pre-extraction Metal version. Gated to GPU builds (the structs are).

/// Resolve a bus signal with bit index, e.g. "ibus__adr" with bit 5 → "ibus__adr[5]".
#[cfg(any(feature = "metal", feature = "cuda", feature = "hip"))]
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
#[cfg(any(feature = "metal", feature = "cuda", feature = "hip"))]
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
#[cfg(any(feature = "metal", feature = "cuda", feature = "hip"))]
fn build_bus_trace_params(
    aig: &AIG,
    netlistdb: &NetlistDB,
    script: &FlattenedScriptV1,
    configs: &[crate::testbench::BusTraceConfig],
) -> (BusTraceParamsAll, Vec<BusTraceLane>) {
    // Resolve pin positions + build the CPU decoder lanes agnostically (shared
    // with the CPU backend), then pack the positions field-for-field into the
    // GPU `BusTraceParams`. The packing below is the *only* GPU-specific step;
    // the resolve/warn/lane logic lives in `build_bus_trace`, so the GPU struct
    // bytes are unchanged from the pre-extraction inline version.
    let (positions, lanes) = build_bus_trace(aig, netlistdb, script, configs);

    let mut all = BusTraceParamsAll {
        n_buses: 0,
        _pad: [0; 3],
        buses: [BusTraceParams::default(); MAX_BUS_TRACES],
    };
    for pos in positions.iter() {
        // Only APB3 buses reach here (others are skipped in build_bus_trace),
        // so the GPU `protocol` field is always 0 == BUS_PROTO_APB3.
        debug_assert!(pos.protocol_apb3);
        let p = BusTraceParams {
            protocol: 0,
            addr_bits: pos.addr_bits as u32,
            data_bits: pos.data_bits as u32,
            sel_pos: pos.sel_pos,
            enable_pos: pos.enable_pos,
            ready_pos: pos.ready_pos,
            write_pos: pos.write_pos,
            resp_pos: pos.resp_pos,
            addr_pos: pos.addr_pos,
            wdata_pos: pos.wdata_pos,
            rdata_pos: pos.rdata_pos,
        };
        let idx = all.n_buses as usize;
        all.buses[idx] = p;
        all.n_buses += 1;
    }

    (all, lanes)
}

// ── Shared CUDA/HIP device-byte-buffer helpers ──────────────────────────────
//
// The GPU IO `#[repr(C)]` structs cross the FFI as untyped `UVec<u8>` byte
// buffers (the event-buffer pattern): ulib `UVec<T>` requires `T: UniversalCopy`,
// which the IO structs are not. These helpers build/read those buffers and are
// identical for both `CudaBackend` and `HipBackend`, so they live here (gated to
// the two CUDA/HIP backends; Metal uses `metal::Buffer` directly, not `UVec<u8>`).

/// Read a native-endian u32 from a byte slice at `off` (alignment-safe — the
/// `UVec<u8>` host backing is byte-aligned, so a `*const T` cast would be UB).
#[cfg(any(feature = "cuda", feature = "hip"))]
pub(crate) fn rd_u32(b: &[u8], off: usize) -> u32 {
    u32::from_ne_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}

/// Upload a fully-initialised host byte buffer to a device-resident `UVec<u8>`.
#[cfg(any(feature = "cuda", feature = "hip"))]
pub(crate) fn host_bytes_to_dev(bytes: Vec<u8>, device: ulib::Device) -> ulib::UVec<u8> {
    use ulib::AsUPtrMut;
    let mut u: ulib::UVec<u8> = bytes.into();
    u.as_mut_uptr(device);
    u
}

/// Copy a slice of `#[repr(C)]` POD values to a `Vec<u8>` of their raw bytes.
#[cfg(any(feature = "cuda", feature = "hip"))]
fn pod_slice_bytes<T>(v: &[T]) -> Vec<u8> {
    // SAFETY: `T` is a `#[repr(C)]` POD struct; reading the slice bytes is sound.
    unsafe { std::slice::from_raw_parts(v.as_ptr() as *const u8, std::mem::size_of_val(v)) }.to_vec()
}

/// Serialize a host `#[repr(C)]` value's raw bytes into a device `UVec<u8>`.
#[cfg(any(feature = "cuda", feature = "hip"))]
pub(crate) fn struct_to_dev<T>(v: &T, device: ulib::Device) -> ulib::UVec<u8> {
    slice_to_dev(std::slice::from_ref(v), device)
}

/// Serialize a host slice of `#[repr(C)]` values into a device `UVec<u8>`.
#[cfg(any(feature = "cuda", feature = "hip"))]
pub(crate) fn slice_to_dev<T>(v: &[T], device: ulib::Device) -> ulib::UVec<u8> {
    host_bytes_to_dev(pod_slice_bytes(v), device)
}

/// Build a zeroed host byte buffer for `n_slots` GPU ring-buffer channels of
/// `slot_size` bytes each, writing `cap` into each channel's `capacity` field
/// (the u32 at byte offset 4 in `UartChannel` / `WbTraceChannel` /
/// `BusTraceChannel`). Shared by the UART/WB/bus channel allocations in `new`.
#[cfg(any(feature = "cuda", feature = "hip"))]
pub(crate) fn channel_buf(slot_size: usize, n_slots: usize, cap: u32) -> Vec<u8> {
    let mut b = vec![0u8; n_slots * slot_size];
    for i in 0..n_slots {
        b[i * slot_size + 4..i * slot_size + 8].copy_from_slice(&cap.to_ne_bytes());
    }
    b
}

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

/// Backend-agnostic snapshot of the SPI-flash decode FSM's printable fields
/// (ADR 0017, Layer 3: output decode is backend-internal, the orchestration
/// only sees decoded records). Mirrors the GPU `FlashState` struct's fields the
/// cosim diagnostics print, but carries no Metal-ABI layout — each backend
/// copies its native state into this on demand.
///
/// `curr_byte`/`out_buffer`/`prev_d_out` round out the FSM record for the
/// future CpuBackend decoder + full-snapshot diagnostics; the Metal tick-0
/// hexdump reads them straight from `FlashState` via `debug_flash_raw_tick0`,
/// so they're carried but not yet read on this path.
#[allow(dead_code)]
struct FlashDebug {
    d_i: u8,
    data_width: u32,
    prev_csn: u32,
    in_reset: u32,
    bit_count: i32,
    byte_count: i32,
    addr: u32,
    curr_byte: u8,
    command: u8,
    out_buffer: u8,
    prev_clk: u32,
    prev_d_out: u8,
    last_error_cmd: u32,
    model_prev_csn: u32,
}

trait CosimBackend {
    /// Fat constructor (ADR 0017 Layer 1/2): the backend allocates + initialises
    /// all its own storage from the agnostic descriptions. Returns the backend
    /// plus the CPU-side bus-trace decoder lanes the orchestration owns (Layer 1).
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
    ) -> (Self, Vec<BusTraceLane>)
    where
        Self: Sized;

    /// Profile the backend's per-tick kernels (Metal: GPU pipelines). Default
    /// no-op — a CPU backend has no GPU kernels to profile.
    fn profile_kernels(&self, _num_ticks: usize) {}

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

    /// Full design state `[input_state | output_state]`, `2 × state_size`
    /// words. Backend-owned storage (Metal: a slice over the shared MTLBuffer;
    /// CpuBackend: a `Vec<u32>`). Read accessor for VCD/diagnostic paths.
    fn state(&self) -> &[u32];
    /// Mutable view of the full design state (pre-loop stimulus deposits /
    /// CPU-side state_prep). First exercised in step 3b; on the trait now so
    /// the seam is complete.
    fn state_mut(&mut self) -> &mut [u32];
    /// SRAM backing store, `sram_storage_size` words (may be empty for a
    /// SRAM-less design). For final dump / equivalence compare.
    fn sram(&self) -> &[u32];

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

    // ── Peripheral output decode (ADR 0017, Layer 3) ──────────────────────
    //
    // Output decode is backend-internal: the GPU mirrors run the FSMs as
    // kernels and stash results in shared ring buffers; a CpuBackend would run
    // the equivalent CPU decoders. The orchestration never touches the native
    // structs — it reads decoded records (bytes / beats / snapshots) through
    // these methods.

    /// Current SPI-flash MISO nibble (`FlashState.d_i`). Functional read for
    /// the `--check-with-cpu` replay (the value `gpu_apply_flash_din` injects
    /// into the input state before the edge).
    fn flash_d_i(&self) -> u8;

    /// Snapshot the flash decode FSM's printable fields for diagnostics. No
    /// default — each backend copies from its native state.
    fn flash_debug_snapshot(&self) -> FlashDebug;

    /// Backend-specific tick-0 debug dump of the raw flash-state bytes (Metal:
    /// the `FlashState` ABI hexdump + `offsetof d_i`). Pure debug artifact;
    /// non-Metal backends are no-ops.
    fn debug_flash_raw_tick0(&self) {}

    /// Drain the UART TX decoder's decoded bytes, advancing the backend's read
    /// cursor. Returns `(channel_idx, byte)` pairs in capture order; the
    /// orchestration maps idx → name, records `UartEvent`s and dispatches.
    fn drain_uart_tx(&mut self) -> Vec<(usize, u8)>;

    /// Drain the AHB/APB bus-trace decoder's raw beats, advancing the read
    /// cursor. The orchestration routes each beat to its protocol decoder.
    fn drain_bus_beats(&mut self) -> Vec<crate::sim::models::bus_trace::RawBeat>;

    /// Drain the legacy Wishbone-trace channel, emitting its debug `eprintln!`s
    /// internally and advancing the read cursor. Debug-only; default no-op
    /// (CpuBackend leaves `write_head=0`, so this never fires).
    fn drain_wb_trace_debug(&mut self) {}

    /// Diagnostic read of a UART channel's decoder FSM: `(state, current_cycle)`
    /// for channel `ch`. Default `(0, 0)` for backends without the GPU mirror.
    fn uart_decoder_debug(&self, _ch: usize) -> (u32, u32) {
        (0, 0)
    }

    /// Run `batch` consecutive scheduler edges starting at `schedule_offset`
    /// in one dispatch, snapshotting each output slot into the VCD ring when
    /// enabled. Returns the completion token to pass to [`CosimBackend::wait`].
    fn run_edges(&self, batch: usize, schedule_offset: usize) -> u64;
    /// Block until the dispatch identified by `token` has completed.
    fn wait(&self, token: u64);
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

/// Backend-generic co-simulation driver (ADR 0017). Drives the entire run
/// through the [`CosimBackend`] trait — `B::new` constructs the backend, every
/// per-tick interaction is a trait method, and no concrete `MetalBackend` /
/// `metal::` token appears in this body. The public [`run_cosim`] shim picks
/// `MetalBackend`; a future CPU backend monomorphises the same code path.
fn run_cosim_generic<B: CosimBackend>(
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

    // ── Agnostic flash const-params (P1.3b-i) ────────────────────────────
    //
    // The flash MISO bit positions and clk/csn output positions are
    // const-after-init in the backend's flash-buffer setup
    // (FlashModelParams / FlashDinParams). Recompute them here as
    // backend-agnostic locals so the cosim loop's flash diagnostics don't
    // read the GPU param buffers. Bit-identical to the buffer init formulas.
    let flash_clk_out_pos: u32 = config
        .flash
        .as_ref()
        .and_then(|f| gpio_map.output_bits.get(&f.clk_gpio).copied())
        .unwrap_or(0);
    let flash_csn_out_pos: u32 = config
        .flash
        .as_ref()
        .and_then(|f| gpio_map.output_bits.get(&f.csn_gpio).copied())
        .unwrap_or(0);
    let flash_has: bool = config.flash.is_some();
    let mut flash_d_in_pos: [u32; 4] = [0xFFFFFFFF; 4];
    for (i, slot) in flash_d_in_pos.iter_mut().enumerate() {
        *slot = config
            .flash
            .as_ref()
            .and_then(|f| gpio_map.input_bits.get(&(f.d0_gpio + i)).copied())
            .unwrap_or(0xFFFFFFFF);
    }
    let mut flash_d_out_pos: [u32; 4] = [0xFFFFFFFF; 4];
    for (i, slot) in flash_d_out_pos.iter_mut().enumerate() {
        *slot = config
            .flash
            .as_ref()
            .and_then(|f| gpio_map.output_bits.get(&(f.d0_gpio + i)).copied())
            .unwrap_or(0xFFFFFFFF);
    }

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

    // ── Backend construction is deferred to B::new ───────────────────────
    //
    // The native simulator, the state/SRAM/blocks/event buffers, the flash +
    // IO peripheral buffers, the timing-constraints buffer, and the struct
    // literal are all encapsulated in `B::new` below (called after the
    // scheduler + per_edge_ops are built, since `new` needs
    // `sched_ticks_per_sys_clk_cycle`). The backend-agnostic stimulus deposits
    // (reg_init / reset / constant_ports / set_flash_din) run *after*
    // construction, through `state_mut()`.

    // SRAM write dumper (JACQUARD_SRAM_DUMP=<path>). Opt-in
    // diagnostic that snapshots `sram_storage` per batch and emits
    // a per-block write log at end of simulation. See
    // `src/sim/sram_dump.rs` for the report format. (Borrows aig/netlist/
    // script only — no `states` dependency, so it stays here.)
    let mut sram_dumper =
        crate::sim::sram_dump::SramDumper::from_env(aig, netlistdb, script);

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

    // ── Assemble the backend (fat constructor, ADR 0017 Layer 1/2) ───────
    //
    // `B::new` encapsulates the native simulator, the state/SRAM/blocks/event
    // buffers, the flash + IO peripheral buffers, the timing-constraints
    // buffer, the struct literal, and the per-stage param pre-write — in the
    // exact pre-refactor order (bit-identical). It returns the backend plus
    // the agnostic CPU bus decoders (`bus_lanes`); the orchestration (ADR 0017
    // Layer 1) owns those. `run_cosim_generic` is parameterised over the
    // backend; the public `run_cosim` shim picks `MetalBackend`.
    let (mut backend, mut bus_lanes) = B::new(
        aig,
        netlistdb,
        script,
        config,
        &gpio_map,
        &uart_configs,
        sched_ticks_per_sys_clk_cycle,
        state_size,
        num_blocks,
        num_major_stages,
        arrival_state_offset,
        timing_constraints,
    );
    let n_uarts = uart_configs.len();
    // Accumulated decoded transactions (bus_id, transaction), drained
    // each batch. The name is looked up from `bus_lanes` at CSV-write
    // time, so the hot drain path stays allocation-free.
    let mut bus_transactions: Vec<(u32, crate::sim::models::bus_trace::BusTransaction)> =
        Vec::new();

    // ── Stimulus deposits (backend-agnostic) ─────────────────────────────
    //
    // These were inline before construction; they now run *after* `B::new`
    // through `state_mut()`, since the backend owns its state storage.
    // Bit-identical: the backend's state-buffer setup already did `fill(0)` +
    // the xprop X-mask seed + SRAM preload, and the flash/IO buffer setup
    // allocates independent buffers that don't read the state, so depositing
    // here (rather than before those builds) is order-equivalent.
    {
        let states = backend.state_mut();

        // Register value-injection (issue #108, ADR 0016). `reg_init` deposits
        // a definite value into chosen registers at tick 0 with `$deposit`
        // semantics — the seed clears the power-up X-mask, then the design
        // drives the register normally. The register sibling of `sram_init`;
        // the fix path for X-poisoned unreset CDC launch registers (#102). NOT
        // `force`: applied once here, not re-driven each edge.
        //
        // A DFF Q is sequential state, not a primary input in the per-edge
        // BitOp schedule, so a deposit must survive `state_prep`'s
        // output→input copy (kernel_v1.metal): write the value AND clear the
        // X-mask in BOTH slots, mirroring the xprop seed. The output slot is
        // the source of truth the first `simulate` reads; the input slot
        // covers any pre-copy read.
        if !config.reg_init.is_empty() {
            let rio = script.reg_io_state_size as usize;
            let mut deposited_bits = 0usize;
            for entry in &config.reg_init {
                let width = entry.width.unwrap_or(1);
                for b in 0..width {
                    // A scalar register (width 1, however spelled) resolves by
                    // its bare name; a wider one resolves each bit `name[b]` (a
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

        // Set flash D_IN defaults (high = no data) — initial state before GPU
        // flash takes over.
        if let Some(ref flash_cfg) = config.flash {
            set_flash_din(
                &mut states[..state_size],
                &gpio_map,
                flash_cfg.d0_gpio,
                0x0F,
            );
        }
    }

    clilog::finish!(timer_prep);

    // ── Materialise the per-edge schedule ────────────────────────────────
    //
    // `init_schedule` builds the backend's native per-edge buffers once from
    // the backend-agnostic `per_edge_ops` description built above. After this
    // point the cosim loop drives the design through `backend`.
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

        // Backend resources (e.g. the Metal event buffer) freed when `backend`
        // drops at this early return (all backend work has completed).
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
    {
        let fs = backend.flash_debug_snapshot();
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
    let uart_names: Vec<String> = uart_configs.iter().map(|(name, _, _, _)| name.clone()).collect();

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
            let gpu_states: &[u32] = backend.state();
            cpu_states.copy_from_slice(gpu_states);
            let gpu_sram: &[u32] = backend.sram();
            cpu_sram.copy_from_slice(gpu_sram);
            // Save flash d_i before GPU modifies it (apply_flash_din reads this)
            if tick == 0 {
                // Metal-ABI raw-bytes hexdump + field dump + offsetof
                // (backend-internal debug artifact).
                backend.debug_flash_raw_tick0();
            }
            saved_flash_d_i = backend.flash_d_i();
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
            let input_state: &[u32] = &backend.state()[..state_size];
            let output_state_dff: &[u32] = &backend.state()[state_size..];
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
            let output_state: &[u32] = &backend.state()[state_size..];
            let flash_clk_pos = flash_clk_out_pos;
            let flash_csn_pos = flash_csn_out_pos;
            let flash_clk =
                (output_state[(flash_clk_pos >> 5) as usize] >> (flash_clk_pos & 31)) & 1;
            let flash_csn =
                (output_state[(flash_csn_pos >> 5) as usize] >> (flash_csn_pos & 31)) & 1;
            let fs = backend.flash_debug_snapshot();
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
                if tick == 0 {
                    eprintln!(
                        "  CPU flash_din: has_flash={}, d_i=0x{:02X}, d_in_pos={:?}",
                        flash_has as u32, saved_flash_d_i, flash_d_in_pos
                    );
                }
                if flash_has {
                    for i in 0..4usize {
                        let pos = flash_d_in_pos[i];
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
            let gpu_states: &[u32] = backend.state();
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
            let gpu_sram: &[u32] = backend.sram();
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

        // Drain UART channels: the backend decodes TX bytes into its ring;
        // the orchestration maps channel idx → name and records events.
        let t_drain = std::time::Instant::now();
        for (i, byte) in backend.drain_uart_tx() {
            let name = &uart_names[i];
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
        }
        // Drain WB trace channel (legacy debug-only; backend-internal eprintln).
        backend.drain_wb_trace_debug();
        // Drain AHB/APB bus trace channel: route each raw beat to its
        // bus's protocol decoder, collecting completed transactions.
        if !bus_lanes.is_empty() {
            for beat in backend.drain_bus_beats() {
                let bus_id = beat.bus_id;
                if let Some(lane) = bus_lanes.get_mut(bus_id as usize) {
                    if let Some(txn) = lane.decoder.push(beat) {
                        bus_transactions.push((bus_id, txn));
                    }
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
            let storage: &[u32] = backend.sram();
            dumper.snapshot_and_diff(storage, tick as u64);
        }

        // Deep diagnostics: SRAM activity + flash transaction tracking
        if deep_diag && tick > reset_edges && batch == 1 {
            let fs = backend.flash_debug_snapshot();
            let st = backend.state();
            let read_out_bit = |pos: u32| -> u32 {
                let word_idx = state_size + (pos as usize >> 5);
                let bit_idx = pos & 31;
                (st[word_idx] >> bit_idx) & 1
            };
            let csn_val = read_out_bit(flash_csn_out_pos);

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
                let sram = backend.sram();
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

        // Diagnostic: dump flash-related signals
        if trace_ticks > 0 && tick <= reset_edges + trace_ticks && tick > reset_edges {
            {
                let st = backend.state();
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
                let fs = backend.flash_debug_snapshot();
                let clk_val = read_out_bit(flash_clk_out_pos);
                let csn_val = read_out_bit(flash_csn_out_pos);
                let mut d_out_vals = [0u32; 4];
                for i in 0..4 {
                    if flash_d_out_pos[i] != 0xFFFFFFFF {
                        d_out_vals[i] = read_out_bit(flash_d_out_pos[i]);
                    }
                }
                let mut d_in_vals = [0u32; 4];
                for i in 0..4 {
                    if flash_d_in_pos[i] != 0xFFFFFFFF {
                        d_in_vals[i] = read_in_bit(flash_d_in_pos[i]);
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
            // Read first UART's TX bit and decoder state for diagnostics. The
            // TX bit position is the agnostic UartParams.channels[0].tx_out_pos
            // formula (build_io_buffers): output_bits[tx_gpio], default 0.
            let (uart_tx_val, uart_dec_state, uart_dec_cycle) = if n_uarts > 0 {
                let tx_pos = uart_configs
                    .first()
                    .and_then(|(_, tx_gpio, _, _)| gpio_map.output_bits.get(tx_gpio).copied())
                    .unwrap_or(0);
                let st = backend.state();
                let tx_word = state_size + (tx_pos as usize >> 5);
                let tx_bit = tx_pos & 31;
                let tx_val = (st[tx_word] >> tx_bit) & 1;
                let (dec_state, dec_cycle) = backend.uart_decoder_debug(0);
                (tx_val, dec_state, dec_cycle)
            } else {
                (0, 0, 0)
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
                let st = backend.state();
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

    {
        let st = backend.state();
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

    let last_error_cmd = backend.flash_debug_snapshot().last_error_cmd;
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
        let fs = backend.flash_debug_snapshot();
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

    // Backend resources (e.g. the Metal event buffer) freed when `backend`
    // drops at end-of-scope (all backend work has completed). No manual
    // cleanup needed.

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

// ── CpuBackend (non-gated CPU reference backend, Phase 1 step 5a) ───────────
//
// The CPU reference backend (ADR 0017 Layer 2): all design state lives in plain
// `Vec<u32>`s, and `run_edges` steps the design through
// `cpu_reference::simulate_block_v1[_xprop]` — the same partition executor the
// `--check-with-cpu` path uses. It is the cosim oracle: byte-identical to the
// Metal golden on the logic fixtures (no timing, no SRAM-xprop — both asserted
// off in `new`). Phase 1 step 5a implements the LOGIC subset; the UART-TX
// decoder (5b) and bus-trace beat extraction (5c) are stubbed (the `drain_*`
// methods return empty, `new` returns empty `bus_lanes`).
//
// Interior mutability: the trait's `run_edges(&self)` and `vcd_snapshot(&self)`
// take a shared ref (Metal's buffers are interior-mutable shared memory). The
// CpuBackend mirrors that with `UnsafeCell` over its `Vec`s — sound because
// cosim dispatch is strictly sequential (no concurrent access to a backend),
// and `state_mut(&mut self)` keeps the compile-time exclusivity the seam
// promises for the pre-loop stimulus deposits.
use std::cell::UnsafeCell;

/// Per-channel UART-TX decoder config, mirroring Metal `UartPerChannelConfig`
/// (`tx_out_pos`, `cycles_per_bit`) built identically to
/// `MetalBackend::build_io_buffers`. `cycles_per_bit` is already scaled to
/// scheduler edges (`cpb * sched_ticks_per_sys_clk_cycle`).
struct CpuUartConfig {
    tx_out_pos: u32,
    cycles_per_bit: u32,
}

/// Per-channel UART-TX decoder FSM state, mirroring Metal `UartDecoderState`.
/// Persistent across edges/batches (like the GPU `uart_state` buffer). Ported
/// verbatim from `gpu_io_step` (kernel_v1.metal:1189-1249): a 4-state decoder
/// (0=idle, 1=start, 2=data, 3=stop) advancing `current_cycle` by 1 per edge.
struct CpuUartDecoderState {
    state: u32,
    last_tx: u32,
    start_cycle: u32,
    bits_received: u32,
    value: u32,
    current_cycle: u32,
}

impl CpuUartDecoderState {
    /// Init matching `build_io_buffers`: idle, TX line idle-high, counters zero.
    fn new() -> Self {
        CpuUartDecoderState {
            state: 0,
            last_tx: 1, // TX line idle high
            start_cycle: 0,
            bits_received: 0,
            value: 0,
            current_cycle: 0,
        }
    }
}

struct CpuBackend {
    /// Full design state `[input_state | output_state]`, `2 × state_size` words.
    state: UnsafeCell<Vec<u32>>,
    /// SRAM backing store, sized `max(1)` like Metal (a SRAM-less design has
    /// `sram_storage_size == 0`); `sram()` returns the `[..sram_len]` view.
    sram: UnsafeCell<Vec<u32>>,
    /// SRAM X-mask shadow (xprop); sized like `sram` when xprop is on, else 1.
    sram_xmask: UnsafeCell<Vec<u32>>,
    /// Per-edge schedule: one `Vec<BitOp>` per scheduler edge.
    schedule: Vec<Vec<BitOp>>,
    edges_per_period: usize,
    gcd_ps: u64,
    /// Words per state slot (`effective_state_size()`).
    state_size: usize,
    /// True SRAM word count (`sram_storage_size`); `sram()` view length.
    sram_len: usize,
    num_blocks: usize,
    num_major_stages: usize,
    xprop_enabled: bool,
    /// X-mask word offset within a slot (`reg_io_state_size` when xprop on,
    /// else 0). Mirrors the GPU `state_prep` `xmask_state_offset` sentinel.
    xmask_state_offset: usize,
    /// Reg/IO words per slot (`reg_io_state_size`); the X-mask lane width.
    reg_io_state_size: usize,
    /// Flash reset line (logic fixtures have no flash; carried for the contract).
    flash_in_reset: bool,
    /// Block program, copied from `script` so the backend owns its storage
    /// (no lifetime entanglement with `LoadedDesign`).
    blocks_start: Vec<usize>,
    blocks_data: Vec<u32>,
    /// Per-edge `[input | output]` snapshots for the most recent `run_edges`,
    /// one slot per edge in the batch (N=1 per edge). `None` until enabled.
    vcd_ring: UnsafeCell<Option<Vec<Vec<u32>>>>,
    enable_vcd: bool,
    /// Per-channel UART-TX decoder config (`tx_out_pos`, `cycles_per_bit`),
    /// derived in `new` exactly as `MetalBackend::build_io_buffers` builds
    /// `UartParams`. Length `n_uarts`; empty for designs with no UART.
    uart_configs: Vec<CpuUartConfig>,
    /// Per-channel UART-TX decoder FSM state, persistent across edges/batches
    /// (mirrors the GPU `uart_state` buffer). Stepped per edge in `run_edges`.
    uart_states: UnsafeCell<Vec<CpuUartDecoderState>>,
    /// Completed `(channel_idx, byte)` pairs awaiting drain (mirrors the GPU
    /// `UartChannel` ring; `drain_uart_tx` empties it). Interior-mutable so the
    /// per-edge FSM in `run_edges(&self)` can push.
    uart_decoded: UnsafeCell<Vec<(usize, u8)>>,
    /// Per-bus APB3 pin positions (Phase 1 step 5c), resolved in `new` via the
    /// shared `build_bus_trace`. Index == GPU `bus_id` (packed into beat flags).
    /// Empty for designs with no `bus_traces`.
    bus_positions: Vec<BusTracePositions>,
    /// Per-bus gate bits from the previous edge (rising-edge detection); mirrors
    /// the GPU `BusTraceChannel.prev_gate`. Bit `b` == bus `b`'s gate last edge.
    bus_prev_gate: UnsafeCell<u32>,
    /// Monotonic edge counter stamped into each beat's `tick`; mirrors the GPU
    /// `BusTraceChannel.current_tick`, incremented once per edge from 0.
    bus_current_tick: UnsafeCell<u32>,
    /// Captured raw beats awaiting drain (mirrors the GPU `BusTraceEntry` ring;
    /// `drain_bus_beats` empties it). Interior-mutable for the per-edge FSM.
    bus_beats: UnsafeCell<Vec<crate::sim::models::bus_trace::RawBeat>>,
}

impl CpuBackend {
    #[inline]
    fn state_ref(&self) -> &[u32] {
        // SAFETY: cosim dispatch is sequential; no other live borrow exists.
        unsafe { &*self.state.get() }
    }
    #[inline]
    #[allow(clippy::mut_from_ref)]
    fn state_inner_mut(&self) -> &mut Vec<u32> {
        // SAFETY: only called from `run_edges`/`state_mut`, never concurrently.
        unsafe { &mut *self.state.get() }
    }
}

impl CosimBackend for CpuBackend {
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
        // Phase 1 scope guards (plan Risks): the CPU stepper has no arrival
        // readback, and `simulate_block_v1` carries no SRAM X-mask, so SRAM
        // under xprop would read as always-known.
        assert!(
            !script.timing_arrivals_enabled,
            "CpuBackend: timed cosim not supported (Phase 1) — arrival readback \
             rides the GPU ring; use the Metal backend for --timing-vcd cosim."
        );
        assert!(
            !(script.xprop_enabled && script.sram_storage_size > 0),
            "CpuBackend: --xprop with SRAM not supported (Phase 1) — \
             cpu_reference::simulate_block_v1 has no SRAM X-mask, so SRAM cells \
             would read as always-known; use the Metal backend."
        );

        // Design state: [input | output], zeroed, with the xprop X-mask seed
        // mirroring MetalBackend::build_state_buffers (both slots seeded so
        // edge-0 state_prep's output→input copy doesn't wipe the seed).
        let mut state = vec![0u32; 2 * state_size];
        if script.xprop_enabled {
            let rio = script.reg_io_state_size as usize;
            let xmask = crate::sim::vcd_io::xprop_xmask_template_cosim(script);
            state[rio..2 * rio].copy_from_slice(&xmask);
            state[state_size + rio..state_size + 2 * rio].copy_from_slice(&xmask);
            clilog::info!(
                "cosim X-propagation enabled (CpuBackend): {} reg/io words, \
                 X-mask seeded (both slots) for uninitialised state",
                rio
            );
        }

        // SRAM backing store (sized max(1) like Metal; sliced to the real len).
        let sram_alloc_len = (script.sram_storage_size as usize).max(1);
        let mut sram = vec![0u32; sram_alloc_len];

        // SRAM X-mask shadow (all-X when xprop, sized 1 dummy otherwise). The
        // (xprop && sram>0) assert above means xprop with real SRAM never
        // reaches here; this stays for layout parity.
        let sram_xmask_len = if script.xprop_enabled {
            (script.sram_storage_size as usize).max(1)
        } else {
            1
        };
        let mut sram_xmask =
            vec![if script.xprop_enabled { 0xFFFF_FFFFu32 } else { 0u32 }; sram_xmask_len];

        // SRAM preload (issue #80) — same path as build_state_buffers. The
        // logic fixtures declare no `sram_init`, so this is untaken; wired for
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
                "SRAM preload (CpuBackend): {} bytes from {} → cell {} at storage offset {}",
                total_bytes,
                elf_path.display(),
                cellid,
                storage_offset
            );
        }

        // Per-channel UART-TX decoder config + FSM state (Phase 1 step 5b).
        // Built EXACTLY as MetalBackend::build_io_buffers builds UartParams:
        //   tx_out_pos   = gpio_map.output_bits[tx_gpio] (or 0 if absent)
        //   cycles_per_bit = cpb * sched_ticks_per_sys_clk_cycle (edges, not
        //                    clock cycles — the decoder counts scheduler edges)
        // The uart_configs tuple is (name, tx_gpio, _, cpb).
        let cpu_uart_configs: Vec<CpuUartConfig> = uart_configs
            .iter()
            .map(|(_, tx_gpio, _, cpb)| CpuUartConfig {
                tx_out_pos: gpio_map.output_bits.get(tx_gpio).copied().unwrap_or(0),
                cycles_per_bit: cpb * sched_ticks_per_sys_clk_cycle as u32,
            })
            .collect();
        let cpu_uart_states: Vec<CpuUartDecoderState> = (0..uart_configs.len())
            .map(|_| CpuUartDecoderState::new())
            .collect();

        // Per-bus APB3 pin positions + CPU decoder lanes (Phase 1 step 5c),
        // built EXACTLY as MetalBackend's build_bus_trace_params (which packs
        // the same positions into the GPU struct) via the shared builder.
        let (bus_positions, bus_lanes) =
            build_bus_trace(aig, netlistdb, script, config.effective_bus_traces());

        let backend = CpuBackend {
            state: UnsafeCell::new(state),
            sram: UnsafeCell::new(sram),
            sram_xmask: UnsafeCell::new(sram_xmask),
            schedule: Vec::new(),
            edges_per_period: 0,
            gcd_ps: 0,
            state_size,
            sram_len: script.sram_storage_size as usize,
            num_blocks,
            num_major_stages,
            xprop_enabled: script.xprop_enabled,
            xmask_state_offset: 0,
            reg_io_state_size: script.reg_io_state_size as usize,
            flash_in_reset: false,
            blocks_start: script.blocks_start.iter().copied().collect(),
            blocks_data: script.blocks_data.iter().copied().collect(),
            vcd_ring: UnsafeCell::new(None),
            enable_vcd: false,
            uart_configs: cpu_uart_configs,
            uart_states: UnsafeCell::new(cpu_uart_states),
            uart_decoded: UnsafeCell::new(Vec::new()),
            bus_positions,
            bus_prev_gate: UnsafeCell::new(0),
            bus_current_tick: UnsafeCell::new(0),
            bus_beats: UnsafeCell::new(Vec::new()),
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
        self.state_ref()
    }
    fn state_mut(&mut self) -> &mut [u32] {
        self.state.get_mut()
    }
    fn sram(&self) -> &[u32] {
        // SAFETY: sequential dispatch — no concurrent borrow.
        let sram: &Vec<u32> = unsafe { &*self.sram.get() };
        &sram[..self.sram_len]
    }

    fn flash_set_in_reset(&mut self, in_reset: bool) {
        self.flash_in_reset = in_reset;
    }

    fn enable_vcd_ring(&mut self) {
        self.enable_vcd = true;
        // Lazily allocate on first run_edges; N=1 → one slot per edge.
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
        // Logic fixtures have no flash; idle MISO nibble is all-high.
        0x0F
    }

    fn flash_debug_snapshot(&self) -> FlashDebug {
        // No flash FSM on the CPU logic path (5a); report a zeroed snapshot.
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
        // Drain semantics (called once per batch by run_cosim_generic, which
        // stamps each byte with the batch `tick` + maps idx→name): return the
        // bytes the per-edge FSM accumulated this batch, then clear.
        std::mem::take(self.uart_decoded.get_mut())
    }

    fn drain_bus_beats(&mut self) -> Vec<crate::sim::models::bus_trace::RawBeat> {
        // Return the beats the per-edge FSM accumulated this batch, then clear
        // (mirrors the GPU ring drain in MetalBackend::drain_bus_beats; here the
        // beats are already in RawBeat form, captured in run_edges).
        std::mem::take(self.bus_beats.get_mut())
    }

    fn run_edges(&self, batch: usize, schedule_offset: usize) -> u64 {
        let state_size = self.state_size;
        let state = self.state_inner_mut();
        // SAFETY: sequential dispatch — sram/sram_xmask/vcd_ring are only
        // touched here and in &mut-self methods, never concurrently.
        let sram = unsafe { &mut *self.sram.get() };
        let sram_xmask = unsafe { &mut *self.sram_xmask.get() };
        let ring = unsafe { &mut *self.vcd_ring.get() };
        // UART-TX decoder state + accumulator (Phase 1 step 5b). The FSM state
        // persists across edges/batches (mirrors the GPU `uart_state` buffer);
        // completed bytes accumulate until `drain_uart_tx`.
        let uart_states = unsafe { &mut *self.uart_states.get() };
        let uart_decoded = unsafe { &mut *self.uart_decoded.get() };
        // Bus-trace FSM state + accumulator (Phase 1 step 5c). `prev_gate` /
        // `current_tick` persist across edges/batches (mirror the GPU
        // `BusTraceChannel` header); beats accumulate until `drain_bus_beats`.
        let bus_prev_gate = unsafe { &mut *self.bus_prev_gate.get() };
        let bus_current_tick = unsafe { &mut *self.bus_current_tick.get() };
        let bus_beats = unsafe { &mut *self.bus_beats.get() };

        if let Some(r) = ring.as_mut() {
            r.clear();
        }

        let xmask_off = self.xmask_state_offset;
        let rio = self.reg_io_state_size;

        for e in 0..batch {
            let sched_idx = (schedule_offset + e) % self.edges_per_period;

            // ── CPU state_prep (mirrors kernel_v1.metal `state_prep`) ──────
            // Step 1: copy output slot → input slot.
            state.copy_within(state_size..2 * state_size, 0);
            // Step 2: apply this edge's BitOps to the input slot; driving a
            // bit clears its X-mask (xmask_off != 0 ⇒ xprop on).
            for op in &self.schedule[sched_idx] {
                let word_idx = (op.position >> 5) as usize;
                let bit_mask = 1u32 << (op.position & 31);
                if op.value != 0 {
                    state[word_idx] |= bit_mask;
                } else {
                    state[word_idx] &= !bit_mask;
                }
                if xmask_off != 0 {
                    state[xmask_off + word_idx] &= !bit_mask;
                }
            }
            // (No flash_din injection: logic fixtures have no SPI flash. 5a.)

            // ── Simulate every partition for this edge ─────────────────────
            for stage_i in 0..self.num_major_stages {
                for block_i in 0..self.num_blocks {
                    let start = self.blocks_start[stage_i * self.num_blocks + block_i];
                    let end = self.blocks_start[stage_i * self.num_blocks + block_i + 1];
                    if start == end {
                        continue;
                    }
                    let script_slice = &self.blocks_data[start..end];
                    let (input_half, output_half) = state.split_at_mut(state_size);
                    if self.xprop_enabled {
                        // Value + X-mask lanes are packed in the same slot at
                        // [0..rio] and [xmask_off..xmask_off+rio]. The xprop
                        // executor takes them as separate slices (it indexes
                        // xmasks from 0, not from xmask_off).
                        let (in_val, in_x) = input_half.split_at(xmask_off);
                        let (out_val, out_x) = output_half.split_at_mut(xmask_off);
                        crate::sim::cpu_reference::simulate_block_v1_xprop(
                            script_slice,
                            in_val,
                            out_val,
                            &in_x[..rio],
                            &mut out_x[..rio],
                            sram,
                            sram_xmask,
                            false,
                        );
                    } else {
                        crate::sim::cpu_reference::simulate_block_v1(
                            script_slice,
                            input_half,
                            output_half,
                            sram,
                            false,
                        );
                    }
                }
            }

            // ── UART-TX decoder (ported from gpu_io_step, kernel_v1.metal ──
            // 1189-1249). Runs AFTER `simulate` — same point the Metal
            // `encode_io_step` runs, with the output slot fresh — on EVERY
            // edge from tick 0 (including reset edges), `current_cycle` += 1
            // per edge, matching the GPU. READ_OUT_BIT reads the OUTPUT slot:
            // states[state_size + (pos>>5)] >> (pos&31) & 1; pos==u32::MAX → 0.
            for (ui, cfg) in self.uart_configs.iter().enumerate() {
                let cycles_per_bit = cfg.cycles_per_bit;
                let tx = if cfg.tx_out_pos != u32::MAX {
                    (state[state_size + (cfg.tx_out_pos >> 5) as usize]
                        >> (cfg.tx_out_pos & 31))
                        & 1
                } else {
                    0
                };

                let us = &mut uart_states[ui];
                let cycle = us.current_cycle;
                let mut st = us.state;
                let last_tx = us.last_tx;
                let mut start_cycle = us.start_cycle;
                let mut bits_received = us.bits_received;
                let mut value = us.value;

                if st == 0 {
                    if last_tx == 1 && tx == 0 {
                        st = 1;
                        start_cycle = cycle;
                    }
                } else if st == 1 {
                    if cycle >= start_cycle + cycles_per_bit / 2 {
                        if tx == 0 {
                            st = 2;
                            start_cycle += cycles_per_bit;
                            bits_received = 0;
                            value = 0;
                        } else {
                            st = 0;
                        }
                    }
                } else if st == 2 {
                    let bit_center =
                        start_cycle + bits_received * cycles_per_bit + cycles_per_bit / 2;
                    if cycle >= bit_center {
                        value |= tx << bits_received;
                        if bits_received >= 7 {
                            st = 3;
                            start_cycle += 8 * cycles_per_bit;
                        } else {
                            bits_received += 1;
                        }
                    }
                } else if st == 3 {
                    if cycle >= start_cycle + cycles_per_bit / 2 {
                        if tx == 1 {
                            uart_decoded.push((ui, (value & 0xFF) as u8));
                        }
                        st = 0;
                    }
                }

                us.state = st;
                us.last_tx = tx;
                us.start_cycle = start_cycle;
                us.bits_received = bits_received;
                us.value = value;
                us.current_cycle = cycle + 1;
            }

            // ── Bus transaction trace (APB3, ported from gpu_io_step,
            // kernel_v1.metal:1305-1352). Runs AFTER `simulate`, same point as
            // the GPU block, reading the OUTPUT slot. Per bus: gate = sel & en &
            // rdy (rdy=1 when pready absent); on a rising edge of the gate emit
            // one RawBeat stamped with `current_tick`. `prev_gate` /
            // `current_tick` persist; `current_tick` += 1 once per edge from 0,
            // regardless of whether any bus fired. READ_OUT_BIT reads the OUTPUT
            // slot; pos==u32::MAX → 0 (sentinel short-circuit).
            if !self.bus_positions.is_empty() {
                let btick = *bus_current_tick;
                let prev = *bus_prev_gate;
                let mut new_gate = 0u32;
                let read_out = |pos: u32| -> u32 {
                    if pos != u32::MAX {
                        (state[state_size + (pos >> 5) as usize] >> (pos & 31)) & 1
                    } else {
                        0
                    }
                };
                for (b, bp) in self.bus_positions.iter().enumerate() {
                    let gate = if bp.protocol_apb3 {
                        let sel = read_out(bp.sel_pos);
                        let en = read_out(bp.enable_pos);
                        let rdy = if bp.ready_pos == 0xFFFFFFFF {
                            1
                        } else {
                            read_out(bp.ready_pos)
                        };
                        sel & en & rdy
                    } else {
                        0
                    };
                    new_gate |= gate << b;

                    let rising = gate != 0 && ((prev >> b) & 1) == 0;
                    if rising {
                        let write = read_out(bp.write_pos);
                        let err = read_out(bp.resp_pos);
                        let mut addr = 0u32;
                        for i in 0..bp.addr_bits.min(BUS_TRACE_MAX_ADR_BITS) {
                            addr |= read_out(bp.addr_pos[i]) << i;
                        }
                        let mut wdata = 0u32;
                        let mut rdata = 0u32;
                        for i in 0..bp.data_bits.min(BUS_TRACE_MAX_DAT_BITS) {
                            wdata |= read_out(bp.wdata_pos[i]) << i;
                            rdata |= read_out(bp.rdata_pos[i]) << i;
                        }
                        // Pack the same flags the GPU writes, then unpack into a
                        // RawBeat identically to MetalBackend::drain_bus_beats.
                        let flags = (write & 1) | ((err & 1) << 1) | ((b as u32) << 8);
                        bus_beats.push(crate::sim::models::bus_trace::RawBeat {
                            tick: btick as u64,
                            bus_id: (flags >> 8) & 0xFF,
                            write: (flags & 1) != 0,
                            err: (flags >> 1) & 1 != 0,
                            addr: addr as u64,
                            wdata: wdata as u64,
                            rdata: rdata as u64,
                        });
                    }
                }
                *bus_prev_gate = new_gate;
                *bus_current_tick = btick + 1;
            }

            // ── VCD snapshot: full [input | output] slot for this edge ─────
            if self.enable_vcd {
                if let Some(r) = ring.as_mut() {
                    r.push(state.clone());
                }
            }
        }

        // CpuBackend runs synchronously; the token is unused (wait is a no-op).
        0
    }

    fn wait(&self, _token: u64) {
        // Synchronous CPU backend — run_edges already completed.
    }
}

/// Public CPU-backend cosim entry point (Phase 1 step 5a). Drives the same
/// agnostic `run_cosim_generic<B>` orchestration as the Metal shim, with the
/// CPU reference backend. Available with no GPU feature; the cosim oracle.
pub fn run_cosim_cpu(
    design: &mut LoadedDesign,
    config: &TestbenchConfig,
    opts: &CosimOpts,
    timing_constraints: &Option<Vec<u32>>,
) -> CosimResult {
    run_cosim_generic::<CpuBackend>(design, config, opts, timing_constraints)
}

#[cfg(feature = "metal")]
mod metal;
#[cfg(feature = "metal")]
pub use metal::run_cosim;

#[cfg(feature = "cuda")]
mod cuda;
#[cfg(feature = "cuda")]
pub use cuda::run_cosim_cuda;

#[cfg(feature = "hip")]
mod hip;
#[cfg(feature = "hip")]
pub use hip::run_cosim_hip;
