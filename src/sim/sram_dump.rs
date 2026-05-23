// SPDX-License-Identifier: Apache-2.0
//! Per-block SRAM write logger for diagnosing the explicit-port
//! RAMBlock runtime path (ADR 0011 / PR #81).
//!
//! Opt-in via `JACQUARD_SRAM_DUMP=<path>`. When enabled, the dumper
//! snapshots `sram_storage` after each batch commit, diffs against
//! the previous snapshot, and accumulates per-word write events
//! keyed back to the originating SRAM cell. At end-of-simulation it
//! writes a structured text report grouped by block.
//!
//! Use when the GPU-side SRAM machinery appears to be receiving no
//! writes despite the design driving its OCD-SRAM control signals —
//! the per-block sectioning surfaces whether the gap is at the
//! composition layer (all blocks zero across all ticks), the address
//! routing (writes landing on the wrong word), or the kernel write
//! path (no writes anywhere). Reads are NOT logged in v1 — the read
//! address is transient kernel state, and the primary diagnostic
//! question is "are writes happening at all?"

use std::io::Write;
use std::path::PathBuf;

use crate::aig::AIG;
use crate::aigpdk::AIGPDK_SRAM_ADDR_WIDTH;
use crate::flatten::FlattenedScriptV1;
use netlistdb::NetlistDB;

const STORAGE_WORDS_PER_BLOCK: u32 = 1u32 << AIGPDK_SRAM_ADDR_WIDTH;

/// Per-block metadata, cached once at simulator init.
#[derive(Debug, Clone)]
pub struct BlockMeta {
    pub cellid: usize,
    pub name: String,
    /// First u32-word index in `sram_storage` owned by this block.
    pub storage_offset: u32,
    /// Address bits actually wired through the explicit-port
    /// composition (10 for the OCD `sram1024x8m8wm1` shape; 13 for
    /// full-bus AIGPDK SRAM). Used by the dump consumer to know
    /// which subset of the 8192-word slot is meaningful.
    pub used_addr_width: u32,
    /// Data bits actually wired (8 for OCD; 32 for full-bus).
    pub data_width_bits: u32,
    /// Composed write-enable AIG-pin iv values per data bit.
    /// `iv = (aigpin << 1) | inv`. `iv == 0` means the composition
    /// collapsed to literal-0 (a constant zero → writes can never
    /// fire); `iv == 1` means literal-1 (writes always fire — also
    /// suspicious); anything else is a real composed AIG pin and
    /// the write-enable can flip at runtime.
    ///
    /// Populated only for slots 0..`data_width_bits` — entries
    /// beyond that are deliberately left at 0 by the AIG-side
    /// composition and would be redundant to surface.
    pub wr_en_iv: Vec<usize>,
}

#[derive(Debug, Clone, Copy)]
pub struct WriteEvent {
    pub tick: u64,
    pub block_idx: u32,
    pub word_offset_in_block: u32,
    pub old: u32,
    pub new: u32,
}

pub struct SramDumper {
    output_path: PathBuf,
    blocks: Vec<BlockMeta>,
    /// Sorted by `storage_offset` for fast O(log N) reverse lookup
    /// from absolute word index to owning block.
    offset_index: Vec<(u32, u32)>, // (offset, block_idx)
    snapshot: Vec<u32>,
    events: Vec<WriteEvent>,
    max_events: usize,
    truncated: bool,
    snapshots_taken: u64,
}

impl SramDumper {
    /// Construct a dumper if `JACQUARD_SRAM_DUMP=<path>` is set in
    /// the environment. Returns `None` to skip the entire mechanism
    /// (zero per-batch overhead) when the env var is absent.
    pub fn from_env(
        aig: &AIG,
        netlistdb: &NetlistDB,
        script: &FlattenedScriptV1,
    ) -> Option<Self> {
        let path_str = std::env::var("JACQUARD_SRAM_DUMP").ok()?;
        let output_path = PathBuf::from(path_str);
        let max_events: usize = std::env::var("JACQUARD_SRAM_DUMP_MAX_EVENTS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(1_000_000);

        let mut blocks: Vec<BlockMeta> = Vec::new();
        for (&cellid, &storage_offset) in script.sram_cell_storage_offsets.iter() {
            let name = format!("{}", netlistdb.cellnames[cellid]);
            let (used_addr_width, data_width_bits) = derive_widths(aig, cellid);
            let wr_en_iv: Vec<usize> = aig
                .srams
                .get(&cellid)
                .map(|ram| ram.port_w_wr_en_iv[..data_width_bits as usize].to_vec())
                .unwrap_or_default();
            blocks.push(BlockMeta {
                cellid,
                name,
                storage_offset,
                used_addr_width,
                data_width_bits,
                wr_en_iv,
            });
        }
        blocks.sort_by_key(|b| b.storage_offset);

        let mut offset_index: Vec<(u32, u32)> = blocks
            .iter()
            .enumerate()
            .map(|(i, b)| (b.storage_offset, i as u32))
            .collect();
        offset_index.sort_by_key(|&(off, _)| off);

        let snapshot = vec![0u32; script.sram_storage_size as usize];

        eprintln!(
            "JACQUARD_SRAM_DUMP enabled: {} blocks, {} u32 words, output={}, max_events={}",
            blocks.len(),
            script.sram_storage_size,
            output_path.display(),
            max_events
        );
        for b in &blocks {
            eprintln!(
                "  block {:>3}: cellid={} name={} offset={} addr_w={} data_w={}",
                b.cellid, b.cellid, b.name, b.storage_offset, b.used_addr_width, b.data_width_bits
            );
        }

        Some(Self {
            output_path,
            blocks,
            offset_index,
            snapshot,
            events: Vec::new(),
            max_events,
            truncated: false,
            snapshots_taken: 0,
        })
    }

    /// Compare current `sram_storage` against the previous snapshot;
    /// append a `WriteEvent` for each changed word. Updates the
    /// snapshot in-place. O(N) per call where N is total u32 words
    /// in `sram_storage`. Caller should invoke after each batch
    /// commit (or every K batches if cost matters).
    pub fn snapshot_and_diff(&mut self, sram_storage: &[u32], tick: u64) {
        debug_assert_eq!(sram_storage.len(), self.snapshot.len());
        if self.truncated {
            return;
        }
        self.snapshots_taken += 1;

        let n = sram_storage.len();
        let mut i = 0;
        while i < n {
            let new_val = sram_storage[i];
            let old_val = self.snapshot[i];
            if new_val != old_val {
                if self.events.len() >= self.max_events {
                    self.truncated = true;
                    eprintln!(
                        "JACQUARD_SRAM_DUMP: event cap {} reached at tick {}, dropping further events",
                        self.max_events, tick
                    );
                    self.snapshot[i] = new_val;
                    return;
                }
                let abs = i as u32;
                let block_idx = self.lookup_block(abs);
                let word_offset_in_block = abs - self.blocks[block_idx as usize].storage_offset;
                self.events.push(WriteEvent {
                    tick,
                    block_idx,
                    word_offset_in_block,
                    old: old_val,
                    new: new_val,
                });
                self.snapshot[i] = new_val;
            }
            i += 1;
        }
    }

    /// Reverse-lookup: given an absolute u32-word index into
    /// `sram_storage`, find which block owns it. Returns the block's
    /// position in `self.blocks`. Each block owns `STORAGE_WORDS_PER_BLOCK`
    /// consecutive words from its `storage_offset`. Binary search by
    /// upper-bound on offset.
    fn lookup_block(&self, abs_word: u32) -> u32 {
        // Find the rightmost block whose storage_offset <= abs_word.
        // partition_point returns index of first element NOT
        // satisfying the predicate.
        let pp = self.offset_index.partition_point(|&(off, _)| off <= abs_word);
        debug_assert!(pp > 0, "abs_word {} below all block offsets", abs_word);
        let (_off, block_idx) = self.offset_index[pp - 1];
        block_idx
    }

    /// Write the structured dump file. Sections per block: meta
    /// header, write events (chronological), per-block summary.
    /// Trailing global summary. Returns the number of events
    /// written.
    pub fn write_dump(&self) -> std::io::Result<usize> {
        let file = std::fs::File::create(&self.output_path)?;
        let mut w = std::io::BufWriter::new(file);

        writeln!(w, "# Jacquard SRAM write dump")?;
        writeln!(
            w,
            "# blocks={} snapshots_taken={} total_events={}{}",
            self.blocks.len(),
            self.snapshots_taken,
            self.events.len(),
            if self.truncated { " (TRUNCATED)" } else { "" }
        )?;
        writeln!(
            w,
            "# storage_words_per_block={} (= 1 << AIGPDK_SRAM_ADDR_WIDTH)",
            STORAGE_WORDS_PER_BLOCK
        )?;
        writeln!(w)?;

        // Bucket events by block.
        let mut events_by_block: Vec<Vec<&WriteEvent>> =
            (0..self.blocks.len()).map(|_| Vec::new()).collect();
        for e in &self.events {
            events_by_block[e.block_idx as usize].push(e);
        }

        for (bi, b) in self.blocks.iter().enumerate() {
            let block_events = &events_by_block[bi];
            writeln!(w, "## block {} (cellid={})", b.name, b.cellid)?;
            writeln!(
                w,
                "#   storage_offset={} storage_words={}",
                b.storage_offset, STORAGE_WORDS_PER_BLOCK
            )?;
            writeln!(
                w,
                "#   used_addr_width={} (covers {} entries) data_width_bits={}",
                b.used_addr_width,
                1u32 << b.used_addr_width,
                b.data_width_bits
            )?;
            // Composed write-enable: surface each bit's iv. A row
            // of literal-0 across all bits is the signature of a
            // dead composition (clken or one of the polarised
            // control signals collapsed). Real composed pins have
            // iv >= 2 (aigpin index 1 means iv 2 or 3 — anything
            // below 2 is a constant). We tag the diagnostic up-front
            // so a grep for "wr_en_status=dead" pulls the failing
            // blocks straight out of the dump.
            let wr_en_str: Vec<String> = b.wr_en_iv.iter().map(|iv| iv.to_string()).collect();
            let dead = !b.wr_en_iv.is_empty() && b.wr_en_iv.iter().all(|&iv| iv == 0);
            let always_on = !b.wr_en_iv.is_empty() && b.wr_en_iv.iter().all(|&iv| iv == 1);
            // "Mask-collapsed": every bit's wr_en is the same composed
            // aigpin (>= 2). Signature of `we_mask_active_iv[i] = 1`
            // (literal-1) for all i, which means `add_and_gate(X, 1)`
            // returns X identically per bit. Composition treated the
            // write-mask as either absent or single-bit. The bit-
            // granular mask is therefore ineffective; either no byte
            // writes (if X is constantly 0 at runtime) or all-or-none
            // bytes write together.
            let mask_collapsed = b.wr_en_iv.len() >= 2
                && b.wr_en_iv.iter().all(|&iv| iv >= 2)
                && b.wr_en_iv.iter().all(|&iv| iv == b.wr_en_iv[0]);
            let status = if dead {
                "dead (all wr_en collapsed to literal 0 — writes will never fire)"
            } else if always_on {
                "always-on (all wr_en literal 1 — every clock edge writes)"
            } else if mask_collapsed {
                "mask-collapsed (all wr_en bits share one aigpin — \
                 per-bit WEN pin loop never bound, granularity ineffective)"
            } else {
                "live"
            };
            writeln!(w, "#   port_w_wr_en_iv=[{}]", wr_en_str.join(","))?;
            writeln!(w, "#   wr_en_status={}", status)?;
            writeln!(w, "#   write_events={}", block_events.len())?;

            if block_events.is_empty() {
                writeln!(w, "#   (no writes observed)")?;
                writeln!(w)?;
                continue;
            }

            let mut distinct: std::collections::HashSet<u32> = std::collections::HashSet::new();
            for e in block_events {
                distinct.insert(e.word_offset_in_block);
            }
            writeln!(w, "#   distinct_words_touched={}", distinct.len())?;
            writeln!(w, "# tick word_offset old new")?;
            for e in block_events {
                writeln!(
                    w,
                    "{} {} 0x{:08X} 0x{:08X}",
                    e.tick, e.word_offset_in_block, e.old, e.new
                )?;
            }
            writeln!(w)?;
        }

        w.flush()?;
        Ok(self.events.len())
    }

    /// How many write events have we recorded? For end-of-sim
    /// summary line printed by the caller.
    pub fn event_count(&self) -> usize {
        self.events.len()
    }

    /// How many blocks have we registered? For diagnostic output.
    pub fn block_count(&self) -> usize {
        self.blocks.len()
    }

    /// Where will the dump be written?
    pub fn output_path(&self) -> &std::path::Path {
        &self.output_path
    }
}

#[cfg(test)]
impl SramDumper {
    /// Test-only constructor: skip the AIG/NetlistDB/script path
    /// and inject pre-built block metadata directly. Output path is
    /// arbitrary (never written by the test). `max_events` defaults
    /// to 1000.
    fn with_blocks_for_test(blocks: Vec<BlockMeta>, storage_size: u32) -> Self {
        let mut offset_index: Vec<(u32, u32)> = blocks
            .iter()
            .enumerate()
            .map(|(i, b)| (b.storage_offset, i as u32))
            .collect();
        offset_index.sort_by_key(|&(off, _)| off);
        Self {
            output_path: PathBuf::from("/dev/null"),
            blocks,
            offset_index,
            snapshot: vec![0u32; storage_size as usize],
            events: Vec::new(),
            max_events: 1000,
            truncated: false,
            snapshots_taken: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk_block(cellid: usize, name: &str, offset: u32) -> BlockMeta {
        BlockMeta {
            cellid,
            name: name.into(),
            storage_offset: offset,
            used_addr_width: 10,
            data_width_bits: 8,
            wr_en_iv: vec![42; 8],
        }
    }

    /// Two contiguous blocks at offsets 0 and 8192 (the standard
    /// AIGPDK layout): writes inside each block are attributed to
    /// the correct block, with the right `word_offset_in_block`.
    #[test]
    fn block_attribution_two_contiguous_blocks() {
        let blocks = vec![mk_block(7, "u_sram_a", 0), mk_block(13, "u_sram_b", 8192)];
        let mut d = SramDumper::with_blocks_for_test(blocks, 16384);

        // Simulate tick 100: word 5 of block A and word 3 of block B
        // both flipped to non-zero. Snapshot was all-zero.
        let mut storage = vec![0u32; 16384];
        storage[5] = 0xDEAD_BEEF;
        storage[8192 + 3] = 0xCAFE_BABE;
        d.snapshot_and_diff(&storage, 100);

        assert_eq!(d.events.len(), 2);
        assert_eq!(d.events[0].block_idx, 0);
        assert_eq!(d.events[0].word_offset_in_block, 5);
        assert_eq!(d.events[0].old, 0);
        assert_eq!(d.events[0].new, 0xDEAD_BEEF);
        assert_eq!(d.events[1].block_idx, 1);
        assert_eq!(d.events[1].word_offset_in_block, 3);
        assert_eq!(d.events[1].new, 0xCAFE_BABE);

        // Next batch: no further changes — no new events emitted.
        d.snapshot_and_diff(&storage, 101);
        assert_eq!(d.events.len(), 2);

        // Tick 110: word 5 of block A overwritten back to 0.
        // Tracks old/new correctly.
        storage[5] = 0;
        d.snapshot_and_diff(&storage, 110);
        assert_eq!(d.events.len(), 3);
        assert_eq!(d.events[2].tick, 110);
        assert_eq!(d.events[2].old, 0xDEAD_BEEF);
        assert_eq!(d.events[2].new, 0);
    }

    /// The `max_events` cap stops further event accumulation but
    /// still advances the snapshot so the dump count is a true
    /// "we observed N writes before cap" rather than "we capped
    /// then re-observed the same words next batch".
    #[test]
    fn max_events_cap_terminates_diff() {
        let blocks = vec![mk_block(1, "u_sram", 0)];
        let mut d = SramDumper::with_blocks_for_test(blocks, 100);
        d.max_events = 3;

        // 5 distinct words flipped; only the first 3 should be
        // recorded, but snapshot must still reflect the new state
        // up to where we stopped.
        let mut storage = vec![0u32; 100];
        for i in 0..5 {
            storage[i] = 0x1000_0000 + i as u32;
        }
        d.snapshot_and_diff(&storage, 1);

        assert_eq!(d.events.len(), 3);
        assert!(d.truncated);

        // Next batch: same storage, no NEW changes from the
        // post-cap snapshot — no additional events.
        d.snapshot_and_diff(&storage, 2);
        assert_eq!(d.events.len(), 3);
    }

    /// Block ordering by `storage_offset` survives an out-of-order
    /// caller (input `blocks` may be in cellid order, not offset
    /// order). After construction, attribution lookup must still
    /// land on the right block.
    #[test]
    fn block_lookup_is_offset_keyed_not_cellid_keyed() {
        // High-cellid block at the low offset; low-cellid block at
        // the high offset. Demonstrates the offset_index sort.
        let blocks = vec![
            mk_block(99, "u_first", 0),
            mk_block(7, "u_second", 8192),
        ];
        let d = SramDumper::with_blocks_for_test(blocks, 16384);

        assert_eq!(d.lookup_block(0), 0);
        assert_eq!(d.lookup_block(8191), 0);
        assert_eq!(d.lookup_block(8192), 1);
        assert_eq!(d.lookup_block(16383), 1);
    }
}

/// Derive the address-bus and data-bus widths actually wired through
/// the explicit-port composition by counting non-default entries in
/// the RAMBlock's `port_w_*` arrays. For opaque-mode cells (no
/// `[cells.NAME.ram]`) both come back as 0; the diagnostic header
/// then surfaces that the cell wasn't promoted out of opaque mode.
fn derive_widths(aig: &AIG, cellid: usize) -> (u32, u32) {
    let Some(ram) = aig.srams.get(&cellid) else {
        return (0, 0);
    };
    let used_addr_width = ram
        .port_w_addr_iv
        .iter()
        .rposition(|&iv| iv != 0)
        .map(|i| (i + 1) as u32)
        .unwrap_or(0);
    let data_width_bits = ram
        .port_w_wr_data_iv
        .iter()
        .rposition(|&iv| iv != 0)
        .map(|i| (i + 1) as u32)
        .unwrap_or(0);
    (used_addr_width, data_width_bits)
}
