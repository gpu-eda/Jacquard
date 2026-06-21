// SPDX-FileCopyrightText: Copyright (c) 2026 ChipFlow
// SPDX-License-Identifier: Apache-2.0
//! JTAG `remote_bitbang` peripheral model — consumes a `remote_bitbang`
//! byte stream (the protocol OpenOCD uses to drive an external JTAG
//! bridge) and drives the design's TCK/TMS/TDI/TRST input pins from it.
//!
//! Two byte sources share one FSM (see [`JtagSource`]):
//!
//! - **Replay** (`--jtag-replay <PATH>`, discussion #77 stage 1):
//!   deterministic file-replay of a recorded stream. `R` (read-TDO)
//!   commands are counted but never answered — the playback is
//!   one-directional.
//! - **Live** (`--jtag-server <PORT>`, stage 2): an accepted
//!   `remote_bitbang` TCP client drives the same FSM in lock-step, and
//!   each `R` is answered with the design's live TDO output sampled at
//!   `tdo_pos` and written back over the socket as ASCII `'0'`/`'1'`.
//!
//! Only the byte feed and the `R` response differ; the TCK/TMS/TDI/TRST
//! decode, IEEE 1149.1 TCK deferral, and override contribution are
//! identical, so replay stays byte-for-byte unchanged.
//!
//! ## `remote_bitbang` byte alphabet
//!
//! Per [OpenOCD's `remote_bitbang.c`][openocd-rb]:
//!
//! | Byte           | Meaning                                |
//! |----------------|----------------------------------------|
//! | `'0'`..=`'7'`  | Set `(TCK, TMS, TDI)` from low 3 bits  |
//! | `'r'`          | TRST off, SRST off                     |
//! | `'s'`          | TRST off, SRST on                      |
//! | `'t'`          | TRST on,  SRST off                     |
//! | `'u'`          | TRST on,  SRST on                      |
//! | `'R'`          | Read TDO (replay: counted; live: answered) |
//! | `'B'` / `'b'`  | Blink on / off (no-op)                 |
//! | `'Q'`          | Quit (treated as end-of-stream)        |
//!
//! [openocd-rb]: https://github.com/openocd-org/openocd/blob/master/src/jtag/drivers/remote_bitbang.c
//!
//! ## Pacing
//!
//! OpenOCD has no concept of simulation time. Stream bytes are
//! consumed at chip-clock pace: each byte's drive values are held
//! on the design's input pins for `hold_edges` cosim edges before
//! the next byte is consumed. Default `hold_edges = 4` — chosen
//! against the "chip-clock ≥ 2× TCK" assumption that holds by
//! construction in any chipflow design running a debug TAP.
//! Configurable per-cosim via `--jtag-hold-cycles <N>` for the
//! corner case.

use crate::sim::input_stim::QueuedAction;
use crate::sim::models::{
    read_bit, warn_unhandled, EmittedEvent, ModelOverrides, PeripheralModel,
};
use std::io::{ErrorKind, Read, Write};
use std::net::TcpStream;

/// Optional pin position + polarity for the design's TRST input.
///
/// `active_low: true` (the common case for RV32-Debug TAPs) drives
/// the configured position LOW when the stream says "TRST on".
#[derive(Debug, Clone, Copy)]
pub struct TrstPin {
    pub position: u32,
    pub active_low: bool,
}

/// Where a [`JtagReplayModel`] pulls its `remote_bitbang` bytes from.
///
/// The model's FSM (TCK/TMS/TDI/TRST decode, IEEE 1149.1 TCK deferral,
/// override contribution) is identical regardless of source — only the
/// byte feed differs. J3 adds a `Live` arm wrapping an accepted
/// `TcpStream` for the interactive `--jtag-server`; replay stays the
/// deterministic recorded path.
enum JtagSource {
    /// Deterministic replay of a recorded stream (`--jtag-replay`).
    Replay { bytes: Vec<u8>, cursor: usize },
    /// Live `remote_bitbang` socket from an accepted client
    /// (`--jtag-server`). The client (OpenOCD) paces the session, so a
    /// byte read blocks until it sends one, and `R` (read-TDO) commands
    /// are answered back over the same stream.
    Live {
        stream: TcpStream,
        /// Set once the socket reaches EOF or errors — `finished()`
        /// then lets the sim free-run instead of blocking forever.
        closed: bool,
        /// Bytes consumed so far (diagnostics; Replay derives this from
        /// its cursor).
        consumed: usize,
    },
}

impl JtagSource {
    /// Yield the next protocol byte, or `None` when the source is
    /// exhausted. Replay returns `None` at end-of-buffer; the live arm
    /// performs a *blocking* socket read (the connected client paces
    /// the session) and returns `None` on EOF or a fatal socket error.
    fn next_byte_blocking(&mut self) -> Option<u8> {
        match self {
            JtagSource::Replay { bytes, cursor } => {
                let b = bytes.get(*cursor).copied();
                if b.is_some() {
                    *cursor += 1;
                }
                b
            }
            JtagSource::Live {
                stream,
                closed,
                consumed,
            } => {
                if *closed {
                    return None;
                }
                let mut buf = [0u8; 1];
                loop {
                    match stream.read(&mut buf) {
                        Ok(0) => {
                            // Orderly client disconnect.
                            *closed = true;
                            clilog::info!("jtag server: client disconnected (EOF)");
                            return None;
                        }
                        Ok(_) => {
                            *consumed += 1;
                            return Some(buf[0]);
                        }
                        Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                        Err(e) => {
                            *closed = true;
                            clilog::warn!(
                                "jtag server: socket read error: {e}; ending session"
                            );
                            return None;
                        }
                    }
                }
            }
        }
    }

    /// Answer an `R` (read-TDO) command by writing one ASCII bit back to
    /// the client. No-op for replay (the recorded path never answers).
    fn write_tdo(&mut self, bit: u8) {
        if let JtagSource::Live { stream, closed, .. } = self {
            if *closed {
                return;
            }
            let ascii = if bit != 0 { b'1' } else { b'0' };
            if let Err(e) = stream.write_all(&[ascii]) {
                *closed = true;
                clilog::warn!("jtag server: TDO write failed: {e}; ending session");
            }
        }
    }

    /// True once the source can yield no more bytes.
    fn is_exhausted(&self) -> bool {
        match self {
            JtagSource::Replay { bytes, cursor } => *cursor >= bytes.len(),
            JtagSource::Live { closed, .. } => *closed,
        }
    }

    /// Bytes consumed so far — surfaced in end-of-run diagnostics.
    fn consumed(&self) -> usize {
        match self {
            JtagSource::Replay { cursor, .. } => *cursor,
            JtagSource::Live { consumed, .. } => *consumed,
        }
    }

    /// `true` for the interactive live socket, `false` for replay.
    fn is_live(&self) -> bool {
        matches!(self, JtagSource::Live { .. })
    }
}

/// CPU-side state for one JTAG `remote_bitbang` peripheral.
pub struct JtagReplayModel {
    /// Peripheral name (e.g. `jtag_0`).
    name: String,
    /// State-buffer position for the design's TCK input.
    tck_pos: u32,
    /// State-buffer position for the design's TMS input.
    tms_pos: u32,
    /// State-buffer position for the design's TDI input.
    tdi_pos: u32,
    /// Optional TRST input. Some chips reset the TAP via a five-cycle
    /// TMS=1 sequence instead of a dedicated pin.
    trst: Option<TrstPin>,
    /// Optional state-buffer position for the design's TDO *output*.
    /// Sampled on each `R` (read-TDO) command in live-server mode and
    /// written back to the connected client. `None` for replay (which
    /// never answers `R`) or designs whose TDO isn't mapped.
    tdo_pos: Option<u32>,
    /// Returned by `driven_positions()`. Built once at construction
    /// since `PeripheralModel::driven_positions` returns `&[u32]`.
    driven_positions_arr: Vec<u32>,
    /// Cosim edges between consuming successive stream bytes.
    hold_edges: u32,
    /// Byte feed — recorded replay or (J3) a live socket.
    source: JtagSource,
    /// Cosim edges spent on the current drive. Resets to 0 when a
    /// new byte is consumed.
    edges_held: u32,
    /// Currently-driven TCK value (0 / 1).
    tck: u8,
    /// Currently-driven TMS value (0 / 1).
    tms: u8,
    /// Currently-driven TDI value (0 / 1).
    tdi: u8,
    /// Currently-driven raw TRST value (0 / 1, pre-polarity). Combined
    /// with `trst.active_low` when contributing the override.
    trst_active: bool,
    /// TCK value deferred by one edge so TMS/TDI settle before TCK
    /// transitions. IEEE 1149.1 requires TMS/TDI stable at TCK
    /// rising edge; applying all three atomically races the TAP.
    pending_tck: Option<u8>,
    /// Total `'R'` (TDO-read) commands seen in the stream — emitted
    /// once at end-of-replay so the operator sees that stage 2 would
    /// have responded to them.
    tdo_read_requests: u64,
    /// Total `'s'` / `'t'` / `'u'` (SRST-toggling) commands — flagged
    /// at end-of-replay since SRST isn't modeled.
    srst_toggles: u64,
    /// Set true when the stream's `'Q'` (quit) command is reached.
    quit: bool,
    /// Set when the just-consumed byte was an `'R'` (read-TDO) command.
    /// The `PeripheralModel::step_edge` impl checks this after stepping
    /// and, in live-server mode, samples TDO from `output_state` and
    /// writes the ASCII bit back to the client. Cleared each edge.
    pending_tdo_read: bool,
}

impl JtagReplayModel {
    /// Build a replay model. `name` matches the configured
    /// peripheral name (typically `jtag_0`). `bytes` is the recorded
    /// `remote_bitbang` stream — captured via `scripts/capture_bitbang.py`
    /// or hand-authored for synthetic tests.
    pub fn new(
        name: String,
        tck_pos: u32,
        tms_pos: u32,
        tdi_pos: u32,
        trst: Option<TrstPin>,
        tdo_pos: Option<u32>,
        hold_edges: u32,
        bytes: Vec<u8>,
    ) -> Self {
        Self::with_source(
            name,
            tck_pos,
            tms_pos,
            tdi_pos,
            trst,
            tdo_pos,
            hold_edges,
            JtagSource::Replay { bytes, cursor: 0 },
        )
    }

    /// Build a live-server model driven by an accepted `remote_bitbang`
    /// client over `stream`. Identical to [`new`](Self::new) except the
    /// byte feed is the socket and `R` commands are answered with the
    /// design's live TDO. `tdo_pos` should be `Some` (resolved from
    /// `JtagConfig.tdo_gpio`); a `None` here means `R` always reads `0`.
    pub fn new_live(
        name: String,
        tck_pos: u32,
        tms_pos: u32,
        tdi_pos: u32,
        trst: Option<TrstPin>,
        tdo_pos: Option<u32>,
        hold_edges: u32,
        stream: TcpStream,
    ) -> Self {
        Self::with_source(
            name,
            tck_pos,
            tms_pos,
            tdi_pos,
            trst,
            tdo_pos,
            hold_edges,
            JtagSource::Live {
                stream,
                closed: false,
                consumed: 0,
            },
        )
    }

    /// Shared constructor body for both byte sources.
    fn with_source(
        name: String,
        tck_pos: u32,
        tms_pos: u32,
        tdi_pos: u32,
        trst: Option<TrstPin>,
        tdo_pos: Option<u32>,
        hold_edges: u32,
        source: JtagSource,
    ) -> Self {
        assert!(
            hold_edges > 0,
            "jtag {name}: hold_edges must be > 0 (got 0)"
        );
        // Only the *input* pins the model drives go in driven_positions;
        // `tdo_pos` is a design output the model reads, not drives.
        let mut driven = vec![tck_pos, tms_pos, tdi_pos];
        if let Some(t) = trst {
            driven.push(t.position);
        }
        Self {
            name,
            tck_pos,
            tms_pos,
            tdi_pos,
            trst,
            tdo_pos,
            driven_positions_arr: driven,
            hold_edges,
            source,
            // Pre-loaded so the first `step_edge` immediately
            // consumes the stream's first byte. Subsequent bytes
            // are spaced `hold_edges` apart.
            edges_held: hold_edges,
            // Idle drive at startup: TCK=0, TMS=1 (target Run-Test-Idle
            // entry), TDI=0, TRST deasserted.
            tck: 0,
            tms: 1,
            tdi: 0,
            trst_active: false,
            pending_tck: None,
            tdo_read_requests: 0,
            srst_toggles: 0,
            quit: false,
            pending_tdo_read: false,
        }
    }

    /// Consume one byte from the stream, updating the driven values.
    /// Public for unit testing the parser in isolation.
    pub fn consume_byte(&mut self, byte: u8) {
        match byte {
            // (TCK, TMS, TDI) packed in low 3 bits. Bit assignment per
            // OpenOCD's remote_bitbang.c: TDI=bit0, TMS=bit1, TCK=bit2.
            b'0'..=b'7' => {
                let b = byte - b'0';
                self.tdi = b & 0b001;
                self.tms = (b & 0b010) >> 1;
                self.tck = (b & 0b100) >> 2;
            }
            // TRST off (deasserted).
            b'r' => self.trst_active = false,
            // TRST off, SRST on — SRST not modeled, count it.
            b's' => {
                self.trst_active = false;
                self.srst_toggles += 1;
            }
            // TRST on (asserted), SRST off.
            b't' => self.trst_active = true,
            // TRST on, SRST on.
            b'u' => {
                self.trst_active = true;
                self.srst_toggles += 1;
            }
            // TDO read. Always counted; in live-server mode the
            // `PeripheralModel::step_edge` impl sees `pending_tdo_read`
            // and writes the sampled TDO bit back to the client.
            // Replay leaves it counted-only (no socket to answer).
            b'R' => {
                self.tdo_read_requests += 1;
                self.pending_tdo_read = true;
            }
            // Blink LED commands — no-op.
            b'B' | b'b' => {}
            // Quit — treat as end-of-stream.
            b'Q' => self.quit = true,
            // Unknown byte — ignore but warn so corrupted streams
            // surface during capture/replay. Count too noisy bytes
            // up-front would require a HashMap; one-shot at end-of-
            // replay is plenty.
            other => clilog::warn!(
                "jtag {}: unknown remote_bitbang byte 0x{:02X} ignored",
                self.name,
                other
            ),
        }
    }

    /// True after consuming the stream's final `'Q'` byte or running
    /// off the end. Cosim doesn't gate on this — replay just goes
    /// idle on its own.
    pub fn finished(&self) -> bool {
        self.quit || self.source.is_exhausted()
    }

    /// Sample the design's current TDO bit from the GPU output state.
    ///
    /// `output_state` is the *output* half of the design state
    /// (`&backend.state()[state_size..]`); `tdo_pos` indexes it
    /// directly. Returns `None` when no TDO pin is configured (replay,
    /// or a design that doesn't map TDO) — callers answering an `R`
    /// command treat that as a low (`'0'`) read.
    pub fn sample_tdo(&self, output_state: &[u32]) -> Option<u8> {
        self.tdo_pos.map(|pos| read_bit(output_state, pos))
    }

    /// Diagnostics counters surfaced at end-of-run.
    pub fn diagnostics(&self) -> JtagDiagnostics {
        JtagDiagnostics {
            bytes_consumed: self.source.consumed(),
            tdo_read_requests: self.tdo_read_requests,
            srst_toggles: self.srst_toggles,
            quit: self.quit,
        }
    }

    /// Advance one cosim edge. Drains the next byte from the stream
    /// when the current drive has been held for `hold_edges`. Public
    /// for unit testing; the `PeripheralModel` impl also calls it.
    ///
    /// Byte application is split into two phases to satisfy IEEE
    /// 1149.1's requirement that TMS/TDI are stable before TCK rises:
    ///
    ///   Phase 1 (consume edge): apply TMS/TDI immediately, defer TCK.
    ///   Phase 2 (next edge): apply the deferred TCK value.
    ///
    /// This guarantees TMS/TDI have been driven into the state buffer
    /// for at least one full cosim edge before TCK transitions,
    /// preventing the TAP from sampling a stale TMS on the rising edge.
    pub fn step_edge(&mut self) {
        if let Some(tck_val) = self.pending_tck.take() {
            self.tck = tck_val;
            return;
        }
        if self.finished() {
            return;
        }
        if self.edges_held >= self.hold_edges {
            let Some(byte) = self.source.next_byte_blocking() else {
                // Source drained between the `finished()` guard and the
                // read — for the J3 live arm this is a socket EOF mid-
                // session. Nothing to apply this edge.
                return;
            };
            let old_tck = self.tck;
            self.consume_byte(byte);
            let new_tck = self.tck;
            if new_tck != old_tck {
                self.tck = old_tck;
                self.pending_tck = Some(new_tck);
            }
            self.edges_held = 1;
        } else {
            self.edges_held += 1;
        }
    }
}

/// Counters surfaced when the run finishes — useful for confirming
/// the replay landed where expected.
#[derive(Debug, Clone, Copy)]
pub struct JtagDiagnostics {
    pub bytes_consumed: usize,
    pub tdo_read_requests: u64,
    pub srst_toggles: u64,
    pub quit: bool,
}

impl PeripheralModel for JtagReplayModel {
    fn name(&self) -> &str {
        &self.name
    }

    fn driven_positions(&self) -> &[u32] {
        &self.driven_positions_arr
    }

    fn apply_action(&mut self, action: &QueuedAction) {
        // The replay model is byte-stream driven from the file, not
        // input.json. Any action targeted at the jtag peripheral is
        // a config mistake worth warning about.
        warn_unhandled(&format!("jtag `{}`", self.name), action);
    }

    fn step_edge(
        &mut self,
        output_state: &[u32],
        overrides: &mut ModelOverrides,
        _emitted: &mut Vec<EmittedEvent>,
    ) {
        JtagReplayModel::step_edge(self);
        // Answer a just-consumed `R` (read-TDO). `output_state` is the
        // design output produced by the previous edge's `run_edges` —
        // i.e. the TDO the TAP drove in response to the TCK the client
        // just clocked. `write_tdo` is a no-op for the replay source, so
        // replay stays unaffected; only the live socket is written.
        if self.pending_tdo_read {
            self.pending_tdo_read = false;
            let bit = self.sample_tdo(output_state).unwrap_or(0);
            self.source.write_tdo(bit);
        }
        self.contribute_overrides(overrides);
    }

    fn contribute_overrides(&self, overrides: &mut ModelOverrides) {
        overrides.insert(self.tck_pos, self.tck);
        overrides.insert(self.tms_pos, self.tms);
        overrides.insert(self.tdi_pos, self.tdi);
        if let Some(trst) = self.trst {
            let driven = if trst.active_low {
                if self.trst_active {
                    0
                } else {
                    1
                }
            } else if self.trst_active {
                1
            } else {
                0
            };
            overrides.insert(trst.position, driven);
        }
    }

    fn is_active(&self) -> bool {
        !self.finished()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn driven(model: &JtagReplayModel) -> (u8, u8, u8) {
        (model.tck, model.tms, model.tdi)
    }

    #[test]
    fn consume_byte_decodes_tck_tms_tdi_packing() {
        // '0' = 0b000 → all zero. '7' = 0b111 → all one.
        // Bit assignment per OpenOCD: TDI=bit0, TMS=bit1, TCK=bit2.
        let mut m = JtagReplayModel::new(
            "jtag_0".into(),
            10, 11, 12, None, None,
            4,
            vec![],
        );
        m.consume_byte(b'0');
        assert_eq!(driven(&m), (0, 0, 0));
        m.consume_byte(b'5'); // 0b101 → TCK=1, TMS=0, TDI=1
        assert_eq!(driven(&m), (1, 0, 1));
        m.consume_byte(b'7'); // 0b111
        assert_eq!(driven(&m), (1, 1, 1));
        m.consume_byte(b'2'); // 0b010 → TCK=0, TMS=1, TDI=0
        assert_eq!(driven(&m), (0, 1, 0));
    }

    #[test]
    fn consume_byte_handles_trst_codes() {
        let mut m = JtagReplayModel::new(
            "jtag_0".into(),
            10, 11, 12,
            Some(TrstPin { position: 13, active_low: true }),
            None,
            4,
            vec![],
        );
        m.consume_byte(b'r');
        assert!(!m.trst_active);
        m.consume_byte(b't');
        assert!(m.trst_active);
        m.consume_byte(b'u');
        assert!(m.trst_active);
        assert_eq!(m.srst_toggles, 1); // 'u' toggles SRST too
        m.consume_byte(b's');
        assert!(!m.trst_active);
        assert_eq!(m.srst_toggles, 2);
    }

    #[test]
    fn consume_byte_counts_tdo_reads_and_quit() {
        let mut m = JtagReplayModel::new(
            "jtag_0".into(),
            10, 11, 12, None, None,
            4,
            vec![],
        );
        m.consume_byte(b'R');
        m.consume_byte(b'R');
        m.consume_byte(b'R');
        assert_eq!(m.tdo_read_requests, 3);
        assert!(!m.quit);
        m.consume_byte(b'Q');
        assert!(m.quit);
    }

    #[test]
    fn consume_byte_blink_is_noop() {
        let mut m = JtagReplayModel::new(
            "jtag_0".into(),
            10, 11, 12, None, None,
            4,
            vec![],
        );
        m.consume_byte(b'5');
        let before = (m.tck, m.tms, m.tdi, m.trst_active);
        m.consume_byte(b'B');
        m.consume_byte(b'b');
        assert_eq!((m.tck, m.tms, m.tdi, m.trst_active), before);
    }

    #[test]
    fn step_edge_holds_then_advances() {
        // hold_edges=2 with the stream "07": first byte drives all
        // zeros, after 2 step_edges we advance to '7' which drives
        // all ones.
        //
        // TCK deferral adds an extra edge when TCK changes: the
        // consume edge applies TMS/TDI but holds old TCK; the next
        // edge applies the new TCK.
        let mut m = JtagReplayModel::new(
            "jtag_0".into(),
            10, 11, 12, None, None,
            2,
            b"07".to_vec(),
        );
        // edges_held starts at hold_edges (2), so first step_edge
        // consumes byte '0'. TCK was 0, stays 0 → no deferral.
        m.step_edge();
        assert_eq!(driven(&m), (0, 0, 0));
        // Hold edge 2/2.
        m.step_edge();
        assert_eq!(driven(&m), (0, 0, 0));
        // Consume byte '7': TMS=1, TDI=1 applied immediately, but
        // TCK changes 0→1 so it's deferred. TCK still 0 this edge.
        m.step_edge();
        assert_eq!(driven(&m), (0, 1, 1));
        // Deferred TCK=1 lands.
        m.step_edge();
        assert_eq!(driven(&m), (1, 1, 1));
        // Stream exhausted.
        m.step_edge();
        assert!(m.finished());
    }

    #[test]
    fn step_edge_stops_at_quit() {
        let mut m = JtagReplayModel::new(
            "jtag_0".into(),
            10, 11, 12, None, None,
            1,
            b"05Q07".to_vec(),
        );
        for _ in 0..10 {
            m.step_edge();
        }
        assert!(m.finished());
        // Should have stopped before consuming the post-Q bytes.
        let consumed = m.source.consumed();
        assert!(consumed <= 3, "stopped on Q, consumed={consumed}");
    }

    #[test]
    fn step_edge_defers_tck_so_tms_settles_first() {
        // Simulates a TLR→RTI transition: TMS drops from 1→0 one byte
        // before TCK rises. Without deferral, TCK would rise in the
        // same edge as TMS changing — the TAP could sample stale TMS.
        //
        // Stream: '2' (TCK=0,TMS=1) '0' (TCK=0,TMS=0) '4' (TCK=1,TMS=0)
        let mut m = JtagReplayModel::new(
            "jtag_0".into(),
            10, 11, 12, None, None,
            1,
            b"204".to_vec(),
        );
        // Byte '2': TCK=0, TMS=1. Initial TCK=0 → no deferral.
        m.step_edge();
        assert_eq!(driven(&m), (0, 1, 0));
        // Byte '0': TCK=0, TMS=0. TCK unchanged → no deferral.
        m.step_edge();
        assert_eq!(driven(&m), (0, 0, 0));
        // Byte '4': TCK would go 0→1. TMS=0 applied immediately,
        // but TCK deferred → still 0 this edge.
        m.step_edge();
        assert_eq!(driven(&m), (0, 0, 0), "TMS must settle before TCK rises");
        // Deferred TCK=1 lands. TMS has been 0 for a full edge.
        m.step_edge();
        assert_eq!(driven(&m), (1, 0, 0), "TCK rises with TMS already stable");
    }

    #[test]
    fn step_edge_no_deferral_when_tck_unchanged() {
        // When TCK doesn't change, no deferral needed.
        // Stream: '0' (TCK=0) '2' (TCK=0,TMS=1) — TCK stays 0.
        let mut m = JtagReplayModel::new(
            "jtag_0".into(),
            10, 11, 12, None, None,
            1,
            b"02".to_vec(),
        );
        m.step_edge();
        assert_eq!(driven(&m), (0, 0, 0));
        // Byte '2': TCK stays 0, TMS changes → applied immediately.
        m.step_edge();
        assert_eq!(driven(&m), (0, 1, 0));
    }

    #[test]
    fn contribute_overrides_applies_trst_polarity() {
        let mut m = JtagReplayModel::new(
            "jtag_0".into(),
            10, 11, 12,
            Some(TrstPin { position: 13, active_low: true }),
            None,
            4,
            vec![],
        );
        m.consume_byte(b't'); // trst on (asserted)
        let mut overrides = ModelOverrides::new();
        m.contribute_overrides(&mut overrides);
        // active_low + asserted → drive LOW
        assert_eq!(overrides.get(&13), Some(&0));

        m.consume_byte(b'r'); // trst off
        overrides.clear();
        m.contribute_overrides(&mut overrides);
        // active_low + deasserted → drive HIGH
        assert_eq!(overrides.get(&13), Some(&1));
    }

    #[test]
    fn diagnostics_counts_match_stream() {
        let mut m = JtagReplayModel::new(
            "jtag_0".into(),
            10, 11, 12, None, None,
            1,
            b"05R7RsuQ".to_vec(),
        );
        for _ in 0..16 {
            m.step_edge();
        }
        let diag = m.diagnostics();
        assert_eq!(diag.tdo_read_requests, 2);
        assert_eq!(diag.srst_toggles, 2); // 's' + 'u'
        assert!(diag.quit);
    }

    #[test]
    fn sample_tdo_reads_configured_output_bit() {
        // tdo_pos = 33 → word 1, bit 1 of the output-state slice.
        let m = JtagReplayModel::new(
            "jtag_0".into(),
            10, 11, 12, None,
            Some(33),
            4,
            vec![],
        );
        // bit 33 set → sample 1.
        let mut out = vec![0u32, 0u32];
        out[1] = 1 << 1;
        assert_eq!(m.sample_tdo(&out), Some(1));
        // bit 33 clear → sample 0.
        out[1] = 0;
        assert_eq!(m.sample_tdo(&out), Some(0));
        // Out-of-range position reads as 0 (read_bit contract), not a panic.
        let short: Vec<u32> = vec![];
        assert_eq!(m.sample_tdo(&short), Some(0));
    }

    #[test]
    fn sample_tdo_none_without_configured_pin() {
        let m = JtagReplayModel::new(
            "jtag_0".into(),
            10, 11, 12, None,
            None,
            4,
            vec![],
        );
        assert_eq!(m.sample_tdo(&[0xFFFF_FFFF]), None);
    }

    /// End-to-end live-socket loopback: a real `TcpStream` client drives
    /// the FSM through `JtagSource::Live`, and an `R` is answered with
    /// the sampled TDO bit written back over the socket. This is the
    /// deterministic, GPU-free core of the V1 gate — the full-design
    /// `--jtag-server` equivalence run lives in CI (jtag-minimal-cosim
    /// server job).
    #[test]
    fn live_source_loops_back_tdo_over_socket() {
        use std::net::TcpListener;
        use std::thread;
        use std::time::Duration;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();

        // Client: drive zeros (`0`), then request TDO (`R`), then quit
        // (`Q`). Expect exactly one ASCII response byte for the `R`.
        let client = thread::spawn(move || {
            let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
            s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            s.write_all(b"0").unwrap();
            s.write_all(b"R").unwrap();
            let mut resp = [0u8; 1];
            s.read_exact(&mut resp).unwrap();
            s.write_all(b"Q").unwrap();
            resp[0]
        });

        let (stream, _) = listener.accept().unwrap();
        // Bound any wait so a logic bug fails the test instead of hanging
        // CI; the production server intentionally has no read timeout.
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut m = JtagReplayModel::new_live(
            "jtag_0".into(),
            10, 11, 12, None,
            Some(33), // TDO at output-state bit 33 (word 1, bit 1)
            1,
            stream,
        );
        // Output state with TDO high.
        let out = vec![0u32, 1u32 << 1];
        let mut ov = ModelOverrides::new();
        let mut ev = Vec::new();
        // Steps: consume '0', consume 'R' (writes TDO), consume 'Q'.
        // Extra steps are no-ops once finished.
        for _ in 0..8 {
            PeripheralModel::step_edge(&mut m, &out, &mut ov, &mut ev);
        }
        let got = client.join().unwrap();
        assert_eq!(got, b'1', "R must read back TDO=1 from output_state");
        assert_eq!(m.diagnostics().tdo_read_requests, 1);
        assert!(m.finished(), "client 'Q' ends the session");
    }
}
