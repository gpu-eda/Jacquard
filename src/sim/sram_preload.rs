// SPDX-FileCopyrightText: Copyright (c) 2026 ChipFlow
// SPDX-License-Identifier: Apache-2.0
//! ELF-driven SRAM preload at tick 0.
//!
//! `TestbenchConfig::sram_init` declares an ELF file whose loadable
//! segments are written into the design's SRAM backing storage
//! before the GPU kernel starts. Useful when the cosim run wants to
//! start from a "firmware already loaded" state — XIP-from-SRAM
//! bring-up, post-loader execution, JTAG-DM-load shortcuts.
//!
//! See ADR 0011 § "SRAM preload" and issue
//! [#80](https://github.com/ChipFlow/Jacquard/issues/80).
//!
//! ## Address mapping
//!
//! The current schema (`SramInitConfig { elf_path: String }`)
//! supports the **single-SRAM** common case: the design has exactly
//! one SRAM cell, and the ELF's lowest loadable virtual address is
//! taken as that SRAM's address 0. Each PT_LOAD segment's bytes are
//! written starting at `(vaddr - base_vaddr)`.
//!
//! Multi-SRAM designs (where the schema needs to specify which
//! ELF segment loads to which SRAM instance) are a future schema
//! extension — covered by the issue's "Possible shapes" note about
//! instance-targeted preload. Today, multi-SRAM designs error
//! cleanly with a "schema must target a specific SRAM" message.

use std::path::Path;

use elf::ElfBytes;
use elf::endian::AnyEndian;
use indexmap::IndexMap;

use crate::aigpdk::AIGPDK_SRAM_SIZE;

/// One byte chunk pulled from an ELF PT_LOAD segment, addressed
/// relative to the SRAM's base.
#[derive(Debug, Clone)]
pub struct PreloadChunk {
    /// Byte offset into the SRAM's backing storage (0 = first entry).
    pub byte_offset: usize,
    /// Segment bytes.
    pub bytes: Vec<u8>,
}

/// Errors from the preload pipeline.
#[derive(Debug)]
pub enum PreloadError {
    Io {
        path: std::path::PathBuf,
        err: std::io::Error,
    },
    ElfParse {
        path: std::path::PathBuf,
        message: String,
    },
    NoLoadableSegments {
        path: std::path::PathBuf,
    },
    MultiSramAmbiguous {
        sram_count: usize,
    },
    NoSramInDesign,
    ChunkOverflowsSram {
        byte_offset: usize,
        chunk_size: usize,
        sram_capacity_bytes: usize,
    },
}

impl std::fmt::Display for PreloadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io { path, err } => write!(f, "reading {}: {err}", path.display()),
            Self::ElfParse { path, message } => {
                write!(f, "parsing ELF {}: {message}", path.display())
            }
            Self::NoLoadableSegments { path } => write!(
                f,
                "ELF {} contains no PT_LOAD segments — nothing to preload",
                path.display()
            ),
            Self::MultiSramAmbiguous { sram_count } => write!(
                f,
                "design has {sram_count} SRAM cells but sram_init.elf_path \
                 doesn't specify which to target; multi-SRAM preload is a \
                 future schema extension (issue #80). Use a design with \
                 exactly one SRAM, or wait for the schema extension."
            ),
            Self::NoSramInDesign => write!(
                f,
                "sram_init declared but the design has no SRAM cells to load into"
            ),
            Self::ChunkOverflowsSram {
                byte_offset,
                chunk_size,
                sram_capacity_bytes,
            } => write!(
                f,
                "ELF segment chunk (offset {byte_offset}, size {chunk_size}) \
                 overflows the target SRAM's capacity ({sram_capacity_bytes} \
                 bytes). Check that the ELF's link script matches the SRAM's \
                 declared depth × width."
            ),
        }
    }
}

impl std::error::Error for PreloadError {}

/// Parse an ELF file's PT_LOAD segments into [`PreloadChunk`]s
/// addressed relative to the lowest loadable virtual address.
pub fn parse_elf_chunks(elf_path: &Path) -> Result<Vec<PreloadChunk>, PreloadError> {
    let bytes = std::fs::read(elf_path).map_err(|err| PreloadError::Io {
        path: elf_path.to_path_buf(),
        err,
    })?;
    let elf = ElfBytes::<AnyEndian>::minimal_parse(&bytes).map_err(|e| {
        PreloadError::ElfParse {
            path: elf_path.to_path_buf(),
            message: format!("{e}"),
        }
    })?;
    let segments = elf
        .segments()
        .ok_or_else(|| PreloadError::NoLoadableSegments {
            path: elf_path.to_path_buf(),
        })?;

    // PT_LOAD = 1.
    let loadable: Vec<_> = segments
        .iter()
        .filter(|s| s.p_type == elf::abi::PT_LOAD && s.p_filesz > 0)
        .collect();
    if loadable.is_empty() {
        return Err(PreloadError::NoLoadableSegments {
            path: elf_path.to_path_buf(),
        });
    }
    let base_vaddr = loadable.iter().map(|s| s.p_vaddr).min().unwrap();

    let mut chunks = Vec::with_capacity(loadable.len());
    for seg in loadable {
        let start = seg.p_offset as usize;
        let end = start + seg.p_filesz as usize;
        if end > bytes.len() {
            return Err(PreloadError::ElfParse {
                path: elf_path.to_path_buf(),
                message: format!(
                    "PT_LOAD segment claims file offset {start}..{end} but \
                     ELF is only {} bytes",
                    bytes.len()
                ),
            });
        }
        let seg_bytes = bytes[start..end].to_vec();
        let byte_offset = (seg.p_vaddr - base_vaddr) as usize;
        chunks.push(PreloadChunk {
            byte_offset,
            bytes: seg_bytes,
        });
    }
    Ok(chunks)
}

/// Resolve the single SRAM in the design to its `(cellid, storage
/// offset in u32 entries)`. Errors if the design has 0 or >1 SRAMs.
pub fn resolve_single_sram(
    sram_cell_storage_offsets: &IndexMap<usize, u32>,
) -> Result<(usize, u32), PreloadError> {
    match sram_cell_storage_offsets.len() {
        0 => Err(PreloadError::NoSramInDesign),
        1 => {
            let (&cellid, &offset) = sram_cell_storage_offsets.iter().next().unwrap();
            Ok((cellid, offset))
        }
        n => Err(PreloadError::MultiSramAmbiguous { sram_count: n }),
    }
}

/// Apply ELF preload chunks to the SRAM backing storage. Writes are
/// packed little-endian into the u32 entries — each 4 ELF bytes
/// becoming one `sram_storage[entry]`. Sub-word chunks (the last
/// few bytes when the ELF size isn't word-aligned) get the unused
/// high bits zeroed.
///
/// `storage_offset_u32` is the start of this SRAM's region in the
/// shared `sram_storage` buffer (i.e. the value
/// `FlattenedScriptV1::sram_cell_storage_offsets[cellid]`).
pub fn apply_chunks(
    sram_storage: &mut [u32],
    storage_offset_u32: u32,
    chunks: &[PreloadChunk],
) -> Result<(), PreloadError> {
    let sram_start = storage_offset_u32 as usize;
    let sram_capacity_entries = AIGPDK_SRAM_SIZE;
    let sram_capacity_bytes = sram_capacity_entries * 4;
    for chunk in chunks {
        let chunk_end = chunk.byte_offset + chunk.bytes.len();
        if chunk_end > sram_capacity_bytes {
            return Err(PreloadError::ChunkOverflowsSram {
                byte_offset: chunk.byte_offset,
                chunk_size: chunk.bytes.len(),
                sram_capacity_bytes,
            });
        }
        for (byte_idx, &b) in chunk.bytes.iter().enumerate() {
            let abs_byte = chunk.byte_offset + byte_idx;
            let entry = sram_start + (abs_byte / 4);
            let bit_shift = (abs_byte % 4) * 8;
            // Clear the target byte slot then OR in the new value
            // — handles overlapping segments + the unused-high-bits
            // case for the final word of a non-aligned segment.
            sram_storage[entry] &= !(0xFFu32 << bit_shift);
            sram_storage[entry] |= (b as u32) << bit_shift;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map_with_one(cellid: usize, offset: u32) -> IndexMap<usize, u32> {
        let mut m = IndexMap::new();
        m.insert(cellid, offset);
        m
    }

    #[test]
    fn resolve_single_sram_picks_the_only_entry() {
        let map = map_with_one(42, 100);
        assert_eq!(resolve_single_sram(&map).unwrap(), (42, 100));
    }

    #[test]
    fn resolve_single_sram_errors_on_empty() {
        let empty = IndexMap::new();
        assert!(matches!(
            resolve_single_sram(&empty),
            Err(PreloadError::NoSramInDesign)
        ));
    }

    #[test]
    fn resolve_single_sram_errors_on_multi() {
        let mut m = IndexMap::new();
        m.insert(1, 0);
        m.insert(2, 8192);
        assert!(matches!(
            resolve_single_sram(&m),
            Err(PreloadError::MultiSramAmbiguous { sram_count: 2 })
        ));
    }

    #[test]
    fn apply_chunks_packs_bytes_little_endian_into_words() {
        // 8 bytes (0x11 0x22 0x33 0x44 ; 0xAA 0xBB 0xCC 0xDD) starting at
        // chunk byte_offset 0 → two u32 entries.
        let mut storage = vec![0u32; AIGPDK_SRAM_SIZE * 2]; // two-SRAM-sized buffer
        let chunk = PreloadChunk {
            byte_offset: 0,
            bytes: vec![0x11, 0x22, 0x33, 0x44, 0xAA, 0xBB, 0xCC, 0xDD],
        };
        apply_chunks(&mut storage, 0, &[chunk]).unwrap();
        assert_eq!(storage[0], 0x44332211);
        assert_eq!(storage[1], 0xDDCCBBAA);
    }

    #[test]
    fn apply_chunks_respects_storage_offset() {
        // Write to the second SRAM's region (offset AIGPDK_SRAM_SIZE u32).
        let mut storage = vec![0u32; AIGPDK_SRAM_SIZE * 2];
        let chunk = PreloadChunk {
            byte_offset: 0,
            bytes: vec![0xDE, 0xAD, 0xBE, 0xEF],
        };
        apply_chunks(&mut storage, AIGPDK_SRAM_SIZE as u32, &[chunk]).unwrap();
        assert_eq!(storage[0], 0); // first SRAM untouched
        assert_eq!(storage[AIGPDK_SRAM_SIZE], 0xEFBEADDE);
    }

    #[test]
    fn apply_chunks_handles_subword_tail() {
        // 3 bytes → only low 24 bits of entry 0 populated.
        let mut storage = vec![0u32; AIGPDK_SRAM_SIZE];
        let chunk = PreloadChunk {
            byte_offset: 0,
            bytes: vec![0xAA, 0xBB, 0xCC],
        };
        apply_chunks(&mut storage, 0, &[chunk]).unwrap();
        assert_eq!(storage[0], 0x00CCBBAA);
    }

    #[test]
    fn apply_chunks_errors_on_overflow() {
        let mut storage = vec![0u32; AIGPDK_SRAM_SIZE];
        let chunk = PreloadChunk {
            byte_offset: (AIGPDK_SRAM_SIZE - 1) * 4,
            // 5 bytes starting at the very last u32 entry → spills past end.
            bytes: vec![0; 5],
        };
        assert!(matches!(
            apply_chunks(&mut storage, 0, &[chunk]),
            Err(PreloadError::ChunkOverflowsSram { .. })
        ));
    }

    #[test]
    fn apply_chunks_writes_at_chunk_byte_offset() {
        let mut storage = vec![0u32; AIGPDK_SRAM_SIZE];
        let chunk = PreloadChunk {
            byte_offset: 8, // start at u32 entry 2
            bytes: vec![0x55; 4],
        };
        apply_chunks(&mut storage, 0, &[chunk]).unwrap();
        assert_eq!(storage[0], 0);
        assert_eq!(storage[1], 0);
        assert_eq!(storage[2], 0x55555555);
    }
}
