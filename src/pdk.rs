// SPDX-FileCopyrightText: Copyright (c) 2024 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-FileCopyrightText: Copyright (c) 2026 ChipFlow Inc. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! PDK-shared types and library detection.
//!
//! Owns types and dispatch surfaces that span multiple PDKs and don't
//! belong inside any one PDK's module:
//!
//! - [`CellLibrary`] — the detected cell-library variant for a netlist
//!   (`AIGPDK`, `SKY130`, `GF180MCU`, or `Mixed`).
//! - [`detect_library`] / [`detect_library_from_file`] — dispatch
//!   helpers that classify a netlist by the cell prefixes it contains.
//!
//! Per-PDK predicates (`is_sky130_cell`, `is_aigpdk_cell`,
//! `is_gf180mcu_cell`) still live with their respective PDK modules;
//! the detection routines here aggregate them into a single verdict.

use crate::gf180mcu::is_gf180mcu_cell;
use crate::sky130::{is_aigpdk_cell, is_sky130_cell};

/// The detected cell library type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellLibrary {
    /// AIGPDK standard cells
    AIGPDK,
    /// SKY130 open-source PDK
    SKY130,
    /// GF180MCU open-source PDK (7t5v0 and/or 9t5v0 standard cell libraries)
    GF180MCU,
    /// Cells from more than one library coexist in the netlist (error case).
    Mixed,
}

impl std::fmt::Display for CellLibrary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CellLibrary::AIGPDK => write!(f, "AIGPDK"),
            CellLibrary::SKY130 => write!(f, "SKY130"),
            CellLibrary::GF180MCU => write!(f, "GF180MCU"),
            CellLibrary::Mixed => write!(f, "Mixed"),
        }
    }
}

/// Resolve detection flags into a single library verdict. `Mixed`
/// fires whenever 2+ libraries are present; absent any recognised
/// cells, `AIGPDK` is the default (consistent with prior two-library
/// behaviour).
fn resolve_library(has_aigpdk: bool, has_sky130: bool, has_gf180mcu: bool) -> CellLibrary {
    match (has_aigpdk, has_sky130, has_gf180mcu) {
        (true, true, _) | (true, _, true) | (_, true, true) => CellLibrary::Mixed,
        (_, true, _) => CellLibrary::SKY130,
        (_, _, true) => CellLibrary::GF180MCU,
        (_, false, false) => CellLibrary::AIGPDK,
    }
}

/// Detect which cell library is used in a netlist.
pub fn detect_library(cell_types: impl Iterator<Item = impl AsRef<str>>) -> CellLibrary {
    let mut has_aigpdk = false;
    let mut has_sky130 = false;
    let mut has_gf180mcu = false;

    for cell_type in cell_types {
        let name = cell_type.as_ref();
        if is_sky130_cell(name) {
            has_sky130 = true;
        } else if is_gf180mcu_cell(name) {
            has_gf180mcu = true;
        } else if is_aigpdk_cell(name) {
            has_aigpdk = true;
        }
        if (has_aigpdk as u8 + has_sky130 as u8 + has_gf180mcu as u8) >= 2 {
            return CellLibrary::Mixed;
        }
    }

    resolve_library(has_aigpdk, has_sky130, has_gf180mcu)
}

/// Detect library from a Verilog file by scanning for cell types.
///
/// This does a quick scan of the file without full parsing.
pub fn detect_library_from_file(path: &std::path::Path) -> std::io::Result<CellLibrary> {
    use std::fs::File;
    use std::io::{BufRead, BufReader};

    let file = File::open(path)?;
    let reader = BufReader::new(file);

    let mut has_aigpdk = false;
    let mut has_sky130 = false;
    let mut has_gf180mcu = false;

    for line in reader.lines() {
        let line = line?;
        if line.contains("sky130_fd_sc_") {
            has_sky130 = true;
        }
        if line.contains("gf180mcu_fd_") {
            has_gf180mcu = true;
        }
        for cell in [
            "AND2_",
            "INV ",
            "BUF ",
            "DFF ",
            "DFFSR ",
            "LATCH ",
            "CKLNQD ",
            "GEM_ASSERT ",
            "GEM_DISPLAY ",
            "$__RAMGEM_",
        ] {
            if line.contains(cell) {
                has_aigpdk = true;
                break;
            }
        }
        if (has_aigpdk as u8 + has_sky130 as u8 + has_gf180mcu as u8) >= 2 {
            return Ok(CellLibrary::Mixed);
        }
    }

    Ok(resolve_library(has_aigpdk, has_sky130, has_gf180mcu))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_library() {
        // SKY130 only
        let sky130_cells = ["sky130_fd_sc_hd__inv_2", "sky130_fd_sc_hd__nand2_1"];
        assert_eq!(detect_library(sky130_cells.iter()), CellLibrary::SKY130);

        // AIGPDK only
        let aigpdk_cells = ["AND2_00_0", "INV", "DFF"];
        assert_eq!(detect_library(aigpdk_cells.iter()), CellLibrary::AIGPDK);

        // GF180MCU only — 7t + 9t coexist within one library.
        let gf180mcu_cells = [
            "gf180mcu_fd_sc_mcu7t5v0__inv_2",
            "gf180mcu_fd_sc_mcu9t5v0__nand2_1",
        ];
        assert_eq!(detect_library(gf180mcu_cells.iter()), CellLibrary::GF180MCU);

        // Mixed — any two libraries together is an error.
        let mixed_sky_aig = ["sky130_fd_sc_hd__inv_2", "AND2_00_0"];
        assert_eq!(detect_library(mixed_sky_aig.iter()), CellLibrary::Mixed);
        let mixed_gf_sky = [
            "gf180mcu_fd_sc_mcu7t5v0__inv_2",
            "sky130_fd_sc_hd__nand2_1",
        ];
        assert_eq!(detect_library(mixed_gf_sky.iter()), CellLibrary::Mixed);
        let mixed_gf_aig = ["gf180mcu_fd_sc_mcu7t5v0__inv_2", "AND2_00_0"];
        assert_eq!(detect_library(mixed_gf_aig.iter()), CellLibrary::Mixed);

        // Empty defaults to AIGPDK
        let empty: Vec<&str> = vec![];
        assert_eq!(detect_library(empty.iter()), CellLibrary::AIGPDK);
    }
}
