// SPDX-FileCopyrightText: Copyright (c) 2026
// SPDX-License-Identifier: Apache-2.0
//! Live signal-streaming tap for cosim (observe-only).
//!
//! Samples a configured bundle of design output nets per triggered edge
//! and streams packed bytes out a UNIX-domain socket to an external
//! renderer (first use: a live C64 video debug view). Reuses the per-edge
//! `vcd_snapshot` ring — no GPU code — and never drives design inputs, so
//! it stays out of the reactive path and imposes no `batch=1` penalty.
//!
//! Design: `docs/spikes/video-streaming-tap.md`.
//!
//! ## Wire format
//!
//! Cosim listens on the socket; a renderer connects. After each GPU batch,
//! the tap writes one frame per batch that produced samples:
//!
//! ```text
//! [ u64 first_tick LE ][ u32 sample_count LE ][ sample_count × bytes_per_sample payload ]
//! ```
//!
//! Each sample packs the bundle's signals LSB-first in config order
//! (`signals[0]` → bit 0 of byte 0). `bytes_per_sample = ceil(n_signals / 8)`.
//! The `first_tick` header lets a renderer resync after a dropped batch.
//! Backpressure or a gone client drops the batch and the connection; the
//! renderer is expected to reconnect.

use std::io::Write;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;

use crate::sim::models::{read_bit, Edge, EdgeDetector};
use crate::testbench::{SignalStreamConfig, StreamTrigger};

/// Bytes in the per-batch frame header: `u64 first_tick` + `u32 sample_count`.
const FRAME_HEADER_LEN: usize = 12;

/// When the tap emits a sample.
enum TriggerState {
    /// Emit every `n`-th edge seen.
    Every { n: u32, counter: u32 },
    /// Emit on the rising edge of the net at `pos` in the output state.
    Strobe { pos: u32, edge: EdgeDetector },
}

/// Trigger + packing, split out from the socket so it is unit-testable
/// without binding a listener.
struct Sampler {
    /// Output-state bit positions for each bundle signal, in pack order.
    positions: Vec<u32>,
    bytes_per_sample: usize,
    trigger: TriggerState,
}

impl Sampler {
    fn new(positions: Vec<u32>, trigger: TriggerState) -> Self {
        let bytes_per_sample = positions.len().div_ceil(8).max(1);
        Self {
            positions,
            bytes_per_sample,
            trigger,
        }
    }

    /// Evaluate the trigger for one edge, advancing its state. If it fires,
    /// append one packed sample (`bytes_per_sample` bytes, LSB-first) to
    /// `out` and return `true`. `output_state` is the design-output half of
    /// the per-edge snapshot (same frame as `resolve_to_state_pos`). Packs
    /// in place — no per-edge allocation in the drain loop's hot path.
    fn sample_into(&mut self, output_state: &[u32], out: &mut Vec<u8>) -> bool {
        let fire = match &mut self.trigger {
            TriggerState::Every { n, counter } => {
                let f = *counter == 0;
                *counter = (*counter + 1) % (*n).max(1);
                f
            }
            TriggerState::Strobe { pos, edge } => {
                matches!(edge.update(read_bit(output_state, *pos) != 0), Edge::Rising)
            }
        };
        if !fire {
            return false;
        }
        let base = out.len();
        out.resize(base + self.bytes_per_sample, 0);
        for (i, &pos) in self.positions.iter().enumerate() {
            if read_bit(output_state, pos) != 0 {
                out[base + i / 8] |= 1 << (i % 8);
            }
        }
        true
    }
}

/// A resolved, ready-to-run streaming tap. Constructed once per
/// [`SignalStreamConfig`] after its signals resolve to state positions.
pub struct SignalStreamTap {
    name: String,
    socket_path: PathBuf,
    listener: UnixListener,
    client: Option<UnixStream>,
    sampler: Sampler,
    /// Samples accumulated within the current batch.
    batch_buf: Vec<u8>,
    /// Tick of the first sample in `batch_buf`; meaningful only while
    /// `batch_buf` is non-empty.
    batch_first_tick: u64,
}

impl SignalStreamTap {
    /// Bind the socket and build the tap. `positions` are the resolved
    /// output-state positions of `cfg.signals` (pack order); `strobe_pos`
    /// is the resolved position of the strobe net, required iff the trigger
    /// is [`StreamTrigger::Strobe`]. The caller resolves these (it holds the
    /// AIG / netlist / script) and skips the tap if any signal is unresolved.
    pub fn new(
        cfg: &SignalStreamConfig,
        positions: Vec<u32>,
        strobe_pos: Option<u32>,
    ) -> std::io::Result<Self> {
        let socket_path = PathBuf::from(&cfg.socket);

        // Remove a stale socket from a previous run so bind() succeeds, but
        // refuse to clobber a non-socket file the user may have named by
        // mistake.
        if let Ok(meta) = std::fs::symlink_metadata(&socket_path) {
            use std::os::unix::fs::FileTypeExt;
            if meta.file_type().is_socket() {
                let _ = std::fs::remove_file(&socket_path);
            } else {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    format!("path {} exists and is not a socket", cfg.socket),
                ));
            }
        }

        let listener = UnixListener::bind(&socket_path)?;
        listener.set_nonblocking(true)?;

        let trigger = match &cfg.trigger {
            StreamTrigger::Every(n) => TriggerState::Every {
                n: (*n).max(1),
                counter: 0,
            },
            StreamTrigger::Strobe(_) => TriggerState::Strobe {
                pos: strobe_pos.expect("strobe trigger requires a resolved strobe position"),
                edge: EdgeDetector::new(false),
            },
        };

        Ok(Self {
            name: cfg.name.clone(),
            socket_path,
            listener,
            client: None,
            sampler: Sampler::new(positions, trigger),
            batch_buf: Vec::new(),
            batch_first_tick: 0,
        })
    }

    /// Sample one edge. Appends a packed sample to the batch buffer if the
    /// trigger fires, stamping the batch's first-sample tick.
    pub fn on_edge(&mut self, output_state: &[u32], tick: u64) {
        let was_empty = self.batch_buf.is_empty();
        if self.sampler.sample_into(output_state, &mut self.batch_buf) && was_empty {
            self.batch_first_tick = tick;
        }
    }

    /// Flush the batch's samples to the connected client. Non-blocking:
    /// accepts a pending connection, and drops the batch + connection on any
    /// write error (backpressure or a gone renderer).
    pub fn flush_batch(&mut self) {
        if self.client.is_none() {
            if let Ok((stream, _)) = self.listener.accept() {
                let _ = stream.set_nonblocking(true);
                self.client = Some(stream);
                clilog::info!("signal-stream `{}`: client connected", self.name);
            }
        }

        if self.batch_buf.is_empty() {
            return;
        }

        if let Some(stream) = self.client.as_mut() {
            let n_samples = (self.batch_buf.len() / self.sampler.bytes_per_sample) as u32;
            let mut header = [0u8; FRAME_HEADER_LEN];
            header[..8].copy_from_slice(&self.batch_first_tick.to_le_bytes());
            header[8..].copy_from_slice(&n_samples.to_le_bytes());
            let wrote = stream
                .write_all(&header)
                .and_then(|_| stream.write_all(&self.batch_buf));
            if wrote.is_err() {
                clilog::debug!("signal-stream `{}`: client write failed, dropping", self.name);
                self.client = None;
            }
        }

        self.batch_buf.clear();
    }
}

impl Drop for SignalStreamTap {
    fn drop(&mut self) {
        // Best-effort cleanup of the socket file we created.
        let _ = std::fs::remove_file(&self.socket_path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a small output state with the given bit positions set.
    fn state(bits: &[u32]) -> Vec<u32> {
        let mut s = vec![0u32; 4];
        for &b in bits {
            s[(b >> 5) as usize] |= 1 << (b & 31);
        }
        s
    }

    /// Run one edge through the sampler; return the packed sample if it fired.
    fn fire(s: &mut Sampler, st: &[u32]) -> Option<Vec<u8>> {
        let mut buf = Vec::new();
        s.sample_into(st, &mut buf).then_some(buf)
    }

    #[test]
    fn every_fires_on_multiples() {
        let mut s = Sampler::new(vec![0], TriggerState::Every { n: 3, counter: 0 });
        let st = state(&[0]);
        // Fires on edge 0, 3, 6, …
        assert!(fire(&mut s, &st).is_some()); // 0
        assert!(fire(&mut s, &st).is_none()); // 1
        assert!(fire(&mut s, &st).is_none()); // 2
        assert!(fire(&mut s, &st).is_some()); // 3
    }

    #[test]
    fn every_one_fires_each_edge() {
        let mut s = Sampler::new(vec![0], TriggerState::Every { n: 1, counter: 0 });
        let st = state(&[]);
        assert!(fire(&mut s, &st).is_some());
        assert!(fire(&mut s, &st).is_some());
    }

    #[test]
    fn strobe_fires_only_on_rising_edge() {
        // Strobe net at pos 10; sampled net at pos 0.
        let mut s = Sampler::new(
            vec![0],
            TriggerState::Strobe {
                pos: 10,
                edge: EdgeDetector::new(false),
            },
        );
        let low = state(&[0]); // strobe=0
        let high = state(&[0, 10]); // strobe=1
        assert!(fire(&mut s, &low).is_none()); // 0→0
        assert!(fire(&mut s, &high).is_some()); // 0→1 rising
        assert!(fire(&mut s, &high).is_none()); // 1→1 held
        assert!(fire(&mut s, &low).is_none()); // 1→0 falling
        assert!(fire(&mut s, &high).is_some()); // 0→1 rising again
    }

    #[test]
    fn packs_lsb_first_in_signal_order() {
        // 6 signals → 1 byte. positions map signal i → some state bit.
        // signals: [pos2, pos5, pos0, pos1, pos7, pos3]
        let mut s = Sampler::new(
            vec![2, 5, 0, 1, 7, 3],
            TriggerState::Every { n: 1, counter: 0 },
        );
        // Drive state bits 2,0,7 high → signals index 0,2,4 high.
        let sample = fire(&mut s, &state(&[2, 0, 7])).unwrap();
        assert_eq!(sample.len(), 1);
        // bits 0,2,4 set → 0b0001_0101 = 0x15
        assert_eq!(sample[0], 0b0001_0101);
    }

    #[test]
    fn packs_multibyte_bundle() {
        // 10 signals → 2 bytes.
        let positions: Vec<u32> = (0..10).collect();
        let mut s = Sampler::new(positions, TriggerState::Every { n: 1, counter: 0 });
        // Set state bits 0,1,8,9 → signals 0,1,8,9.
        let sample = fire(&mut s, &state(&[0, 1, 8, 9])).unwrap();
        assert_eq!(sample.len(), 2);
        assert_eq!(sample[0], 0b0000_0011); // signals 0,1 in byte 0
        assert_eq!(sample[1], 0b0000_0011); // signals 8,9 in byte 1
    }
}
