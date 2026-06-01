// SPDX-FileCopyrightText: Copyright (c) 2026 ChipFlow
// SPDX-License-Identifier: Apache-2.0
//! Config-driven AHB/APB bus transaction tracing (CPU-side decode).
//!
//! The GPU observes a configured bus's pins each tick and packs a
//! compact [`RawBeat`] into a ring buffer on the protocol's gating
//! edge (see `gpu_io_step` in `csrc/kernel_v1.metal`). This module
//! holds the CPU-side half: deriving which net names to observe from a
//! [`BusTraceConfig`], and the per-protocol FSM that turns raw beats
//! into [`BusTransaction`]s.
//!
//! Keeping the protocol semantics here (rather than on the GPU) means
//! the phase-pairing / burst-tracking logic is plain, unit-testable
//! Rust — see the tests at the bottom, which exercise the decoders
//! with synthetic beat sequences and no GPU at all.
//!
//! Status: **APB3** is implemented end-to-end. **AHB-Lite / AHB5** are
//! planned (pipelined address/data pairing + burst tracking); the
//! decoder enum carries placeholders so the wiring is ready. See
//! `docs/plans/bus-transaction-tracing.md`.

use crate::testbench::{BusProtocol, BusTraceConfig};

/// Transfer direction of a decoded transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    Read,
    Write,
}

impl Dir {
    pub fn as_str(self) -> &'static str {
        match self {
            Dir::Read => "RD",
            Dir::Write => "WR",
        }
    }
}

/// Slave response status of a decoded transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BusResp {
    Ok,
    Error,
}

impl BusResp {
    pub fn as_str(self) -> &'static str {
        match self {
            BusResp::Ok => "OK",
            BusResp::Error => "ERR",
        }
    }
}

/// Burst position for AHB transactions (Phase 2). `beat` is the
/// 0-based beat index within the burst; `len` is the burst length in
/// beats (`None` for undefined-length `INCR`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BurstInfo {
    pub beat: u32,
    pub len: Option<u32>,
}

/// A fully decoded bus transaction, ready for CSV / VCD emission. The
/// owning bus is identified by the caller (the drain loop pairs each
/// transaction with its lane), so it is not stored here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BusTransaction {
    /// Cosim tick at which the transaction completed.
    pub tick: u64,
    pub protocol: BusProtocol,
    pub dir: Dir,
    pub addr: u64,
    pub data: u64,
    pub resp: BusResp,
    pub burst: Option<BurstInfo>,
}

/// One raw bus beat captured by the GPU on a gating edge. Field
/// meaning is protocol-agnostic; the decoder interprets them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RawBeat {
    pub tick: u64,
    pub bus_id: u32,
    /// Write (vs read) — APB `pwrite` / AHB `hwrite`.
    pub write: bool,
    /// Error response — APB `pslverr` / AHB `hresp`.
    pub err: bool,
    pub addr: u64,
    pub wdata: u64,
    pub rdata: u64,
}

/// Per-bus protocol decoder. Holds whatever pipeline state the
/// protocol needs to pair beats into transactions.
#[derive(Debug, Clone)]
pub enum BusTraceDecoder {
    /// APB3: each captured beat (gated on `psel & penable & pready`)
    /// is already a complete transaction, so the decoder is stateless.
    Apb3,
    /// AHB-Lite / AHB5 (Phase 2): 1-deep pending address-phase record
    /// paired with the following `hready` data beat, plus burst beat
    /// counter. Not yet implemented.
    Ahb { ahb5: bool },
}

impl BusTraceDecoder {
    /// Build a decoder for the given protocol.
    pub fn new(protocol: BusProtocol) -> Self {
        match protocol {
            BusProtocol::Apb3 => BusTraceDecoder::Apb3,
            BusProtocol::AhbLite => BusTraceDecoder::Ahb { ahb5: false },
            BusProtocol::Ahb5 => BusTraceDecoder::Ahb { ahb5: true },
        }
    }

    /// Feed one raw beat; return a completed transaction if this beat
    /// finishes one. APB3 always completes on each beat; AHB may return
    /// `None` while latching an address phase.
    pub fn push(&mut self, beat: RawBeat) -> Option<BusTransaction> {
        match self {
            BusTraceDecoder::Apb3 => Some(BusTransaction {
                tick: beat.tick,
                protocol: BusProtocol::Apb3,
                dir: if beat.write { Dir::Write } else { Dir::Read },
                addr: beat.addr,
                data: if beat.write { beat.wdata } else { beat.rdata },
                resp: if beat.err { BusResp::Error } else { BusResp::Ok },
                burst: None,
            }),
            // Phase 2: pipeline pairing + burst tracking.
            BusTraceDecoder::Ahb { .. } => None,
        }
    }
}

// ── Pin-name derivation ─────────────────────────────────────────────────────

/// Net-name base for a logical pin: the per-pin override from
/// `cfg.signals` if present, else `{prefix}{logical}`.
pub fn pin_basename(cfg: &BusTraceConfig, logical: &str) -> String {
    cfg.signals
        .get(logical)
        .cloned()
        .unwrap_or_else(|| format!("{}{}", cfg.prefix, logical))
}

/// Scalar (1-bit) logical pins for a protocol.
pub fn scalar_pins(protocol: BusProtocol) -> &'static [&'static str] {
    match protocol {
        BusProtocol::Apb3 => &["psel", "penable", "pwrite", "pready", "pslverr"],
        // Phase 2.
        BusProtocol::AhbLite | BusProtocol::Ahb5 => &["hwrite", "hready", "hresp"],
    }
}

/// Multi-bit logical bus pins for a protocol, paired with their width
/// selector (`addr` → `addr_bits`, `data` → `data_bits`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinWidth {
    Addr,
    Data,
}

pub fn bus_pins(protocol: BusProtocol) -> &'static [(&'static str, PinWidth)] {
    match protocol {
        BusProtocol::Apb3 => &[
            ("paddr", PinWidth::Addr),
            ("pwdata", PinWidth::Data),
            ("prdata", PinWidth::Data),
        ],
        // Phase 2.
        BusProtocol::AhbLite | BusProtocol::Ahb5 => &[
            ("haddr", PinWidth::Addr),
            ("hwdata", PinWidth::Data),
            ("hrdata", PinWidth::Data),
        ],
    }
}

/// All hierarchical net names this bus needs the GPU to observe, so
/// they can be registered as primary outputs (via the same path as
/// `--trace-signals`) before partitioning. Multi-bit pins are expanded
/// to one name per bit (`{base}[{i}]`).
pub fn observed_net_names(cfg: &BusTraceConfig) -> Vec<String> {
    let mut names = Vec::new();
    for pin in scalar_pins(cfg.protocol) {
        names.push(pin_basename(cfg, pin));
    }
    for (pin, width) in bus_pins(cfg.protocol) {
        let base = pin_basename(cfg, pin);
        let n = match width {
            PinWidth::Addr => cfg.addr_bits,
            PinWidth::Data => cfg.data_bits,
        };
        for i in 0..n {
            names.push(format!("{base}[{i}]"));
        }
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testbench::BusTraceConfig;
    use std::collections::HashMap;

    fn apb_cfg() -> BusTraceConfig {
        BusTraceConfig {
            name: "dmi".into(),
            protocol: BusProtocol::Apb3,
            prefix: "soc.dm.".into(),
            addr_bits: 8,
            data_bits: 32,
            signals: HashMap::new(),
        }
    }

    #[test]
    fn apb3_write_beat_decodes_to_write_transaction() {
        let mut dec = BusTraceDecoder::new(BusProtocol::Apb3);
        let t = dec
            .push(RawBeat {
                tick: 100,
                bus_id: 0,
                write: true,
                err: false,
                addr: 0x40,
                wdata: 0xDEAD_BEEF,
                rdata: 0,
                ..Default::default()
            })
            .expect("APB3 beat must complete a transaction");
        assert_eq!(t.dir, Dir::Write);
        assert_eq!(t.addr, 0x40);
        assert_eq!(t.data, 0xDEAD_BEEF);
        assert_eq!(t.resp, BusResp::Ok);
        assert_eq!(t.tick, 100);
        assert!(t.burst.is_none());
    }

    #[test]
    fn apb3_read_beat_uses_rdata() {
        let mut dec = BusTraceDecoder::new(BusProtocol::Apb3);
        let t = dec
            .push(RawBeat {
                tick: 7,
                bus_id: 1,
                write: false,
                err: false,
                addr: 0x10,
                wdata: 0xFFFF_FFFF, // must be ignored for reads
                rdata: 0x0000_0001,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(t.dir, Dir::Read);
        assert_eq!(t.data, 0x0000_0001);
    }

    #[test]
    fn apb3_error_response_propagates() {
        let mut dec = BusTraceDecoder::new(BusProtocol::Apb3);
        let t = dec
            .push(RawBeat {
                tick: 3,
                write: false,
                err: true,
                addr: 0xBAD,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(t.resp, BusResp::Error);
    }

    #[test]
    fn apb3_decodes_a_sequence_of_beats() {
        let mut dec = BusTraceDecoder::new(BusProtocol::Apb3);
        let beats = [
            RawBeat { tick: 1, write: true, addr: 0x0, wdata: 0xAA, ..Default::default() },
            RawBeat { tick: 5, write: false, addr: 0x0, rdata: 0xAA, ..Default::default() },
            RawBeat { tick: 9, write: true, addr: 0x4, wdata: 0xBB, ..Default::default() },
        ];
        let txns: Vec<_> = beats.iter().filter_map(|b| dec.push(*b)).collect();
        assert_eq!(txns.len(), 3);
        assert_eq!(txns[0].dir, Dir::Write);
        assert_eq!(txns[1].dir, Dir::Read);
        assert_eq!(txns[2].data, 0xBB);
    }

    #[test]
    fn default_pin_names_use_prefix() {
        let cfg = apb_cfg();
        assert_eq!(pin_basename(&cfg, "psel"), "soc.dm.psel");
        assert_eq!(pin_basename(&cfg, "paddr"), "soc.dm.paddr");
    }

    #[test]
    fn pin_override_replaces_default() {
        let mut cfg = apb_cfg();
        cfg.signals
            .insert("psel".into(), "top.custom_sel".into());
        assert_eq!(pin_basename(&cfg, "psel"), "top.custom_sel");
        // Non-overridden pins still derive from prefix.
        assert_eq!(pin_basename(&cfg, "penable"), "soc.dm.penable");
    }

    #[test]
    fn observed_names_expand_buses_per_bit() {
        let cfg = apb_cfg(); // addr_bits=8, data_bits=32
        let names = observed_net_names(&cfg);
        // 5 scalar + 8 addr + 32 wdata + 32 rdata = 77
        assert_eq!(names.len(), 5 + 8 + 32 + 32);
        assert!(names.contains(&"soc.dm.psel".to_string()));
        assert!(names.contains(&"soc.dm.paddr[0]".to_string()));
        assert!(names.contains(&"soc.dm.paddr[7]".to_string()));
        assert!(names.contains(&"soc.dm.prdata[31]".to_string()));
        // No bit 8 for an 8-bit address bus.
        assert!(!names.contains(&"soc.dm.paddr[8]".to_string()));
    }
}
