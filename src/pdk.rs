// SPDX-FileCopyrightText: Copyright (c) 2024 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-FileCopyrightText: Copyright (c) 2026 ChipFlow Inc. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! PDK-shared types and dispatch surfaces.
//!
//! Owns types and dispatch that span multiple PDKs and don't belong
//! inside any one PDK's module:
//!
//! - [`CellLibrary`] — the detected cell-library variant for a netlist
//!   (`AIGPDK`, `SKY130`, `GF180MCU`, or `Mixed`).
//! - [`detect_library`] / [`detect_library_from_file`] — dispatch
//!   helpers that classify a netlist by the cell prefixes it contains.
//! - [`PdkVariant`] — the dispatch enum used by `aig.rs` to collapse
//!   `if is_sky130_cell { sky130_X } else if is_gf180mcu_cell { gf180mcu_X }`
//!   shaped call sites into a single match.
//!
//! Per-PDK predicates (`is_sky130_cell`, `is_aigpdk_cell`,
//! `is_gf180mcu_cell`) still live with their respective PDK modules;
//! the detection routines here aggregate them into a single verdict.
//!
//! # Why enum dispatch (not `Box<dyn PdkHooks>`)?
//!
//! The AIG builder runs these classifiers and accessors on every cell
//! in a netlist — hundreds of thousands per design, millions for large
//! Gemmini/NVDLA dumps — and the closed set of PDKs is small (currently
//! two). A `Box<dyn>` would add a vtable dereference per call and
//! defeat inlining of the trivial predicates. An enum-with-match keeps
//! the hot path branch-only and lets the compiler inline the `matches!`
//! arms.
//!
//! Adding a third PDK is a one-arm-per-method expansion, which is
//! fine for this scale. If the PDK count ever explodes (5+ libraries)
//! it'd be worth revisiting; until then the simpler shape wins.

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

// ============================================================================
// PdkVariant — dispatch enum for standard-cell PDK predicates / pin names.
// ============================================================================

/// Standard-cell PDK identification, used by `aig.rs` to dispatch
/// per-cell preprocessing and post-processing without `if/else if`
/// ladders on `is_sky130_cell` / `is_gf180mcu_cell`.
///
/// Variants cover only the standard-cell PDKs that own behavioural
/// models loaded from vendored submodules. AIGPDK (the internal cell
/// library) is handled inline in `aig.rs` because its cells are
/// hard-coded and don't go through the `decompose_with_pdk` path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PdkVariant {
    /// google/skywater-pdk-libs-sky130_fd_sc_hd
    Sky130,
    /// gf180mcu_fd_sc_mcu7t5v0 + mcu9t5v0
    Gf180Mcu,
}

impl PdkVariant {
    /// Identify the PDK variant a vendor-prefixed cell type belongs to.
    /// Returns `None` if the cell isn't a recognised standard-cell PDK
    /// instance (i.e. it's AIGPDK, an SRAM macro, or unknown).
    pub fn classify(celltype: &str) -> Option<PdkVariant> {
        if is_sky130_cell(celltype) {
            Some(PdkVariant::Sky130)
        } else if is_gf180mcu_cell(celltype) {
            Some(PdkVariant::Gf180Mcu)
        } else {
            None
        }
    }

    /// Strip the vendor prefix from `celltype` to get the base cell
    /// type (e.g. `sky130_fd_sc_hd__inv_2` → `inv`,
    /// `gf180mcu_fd_sc_mcu7t5v0__nand2_4` → `nand2`).
    pub fn extract_cell_type<'a>(self, celltype: &'a str) -> &'a str {
        match self {
            PdkVariant::Sky130 => crate::sky130::extract_cell_type(celltype),
            PdkVariant::Gf180Mcu => crate::gf180mcu::extract_cell_type(celltype),
        }
    }

    /// True if the base cell type is a sequential element (DFF / latch /
    /// clock-gating cell).
    pub fn is_sequential(self, cell_type: &str) -> bool {
        match self {
            PdkVariant::Sky130 => crate::sky130_pdk::is_sequential_cell(cell_type),
            PdkVariant::Gf180Mcu => crate::gf180mcu_pdk::is_sequential_cell(cell_type),
        }
    }

    /// True if the base cell type is a constant-driving tie cell.
    pub fn is_tie(self, cell_type: &str) -> bool {
        match self {
            PdkVariant::Sky130 => crate::sky130_pdk::is_tie_cell(cell_type),
            PdkVariant::Gf180Mcu => crate::gf180mcu_pdk::is_tie_cell(cell_type),
        }
    }

    /// True if the cell is a physical-only filler / decap / antenna cell
    /// that contributes no logic to the AIG. SKY130 has no filler concept
    /// (its fillers are handled differently); this returns `false` for
    /// SKY130.
    pub fn is_filler(self, cell_type: &str) -> bool {
        match self {
            PdkVariant::Sky130 => false,
            PdkVariant::Gf180Mcu => crate::gf180mcu_pdk::is_filler_cell(cell_type),
        }
    }

    /// True if the cell has multiple functional outputs (full / half
    /// adders, dual-output DFFs).
    pub fn is_multi_output(self, cell_type: &str) -> bool {
        match self {
            PdkVariant::Sky130 => crate::sky130_pdk::is_multi_output_cell(cell_type),
            PdkVariant::Gf180Mcu => crate::gf180mcu_pdk::is_multi_output_cell(cell_type),
        }
    }

    /// True if the cell is a logic-bearing IO pad (input / inout buffer).
    /// SKY130 IO pads are not currently supported through the digital
    /// path; this returns `false` for SKY130.
    pub fn is_io_pad(self, cell_type: &str) -> bool {
        match self {
            PdkVariant::Sky130 => false,
            PdkVariant::Gf180Mcu => crate::gf180mcu_pdk::is_io_pad_cell(cell_type),
        }
    }

    /// Pin name for the active-low asynchronous reset on a sequential
    /// cell. SKY130: `RESET_B`; GF180MCU: `RN`.
    pub fn reset_pin(self) -> &'static str {
        match self {
            PdkVariant::Sky130 => "RESET_B",
            PdkVariant::Gf180Mcu => "RN",
        }
    }

    /// Pin name for the active-low asynchronous set on a sequential
    /// cell. SKY130: `SET_B`; GF180MCU: `SETN`.
    pub fn set_pin(self) -> &'static str {
        match self {
            PdkVariant::Sky130 => "SET_B",
            PdkVariant::Gf180Mcu => "SETN",
        }
    }
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

    #[test]
    fn pdk_variant_classify() {
        assert_eq!(
            PdkVariant::classify("sky130_fd_sc_hd__inv_2"),
            Some(PdkVariant::Sky130)
        );
        assert_eq!(
            PdkVariant::classify("gf180mcu_fd_sc_mcu7t5v0__nand2_4"),
            Some(PdkVariant::Gf180Mcu)
        );
        assert_eq!(PdkVariant::classify("AND2_00_0"), None);
        assert_eq!(PdkVariant::classify("DFF"), None);
    }

    #[test]
    fn pdk_variant_async_pin_names() {
        assert_eq!(PdkVariant::Sky130.reset_pin(), "RESET_B");
        assert_eq!(PdkVariant::Sky130.set_pin(), "SET_B");
        assert_eq!(PdkVariant::Gf180Mcu.reset_pin(), "RN");
        assert_eq!(PdkVariant::Gf180Mcu.set_pin(), "SETN");
    }
}
