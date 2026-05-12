// SPDX-FileCopyrightText: Copyright (c) 2026 ChipFlow Inc. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! GF180MCU standard-cell library support.
//!
//! Sibling of [`crate::sky130`]; together with [`crate::gf180mcu_pdk`] this
//! module provides cell detection, pin direction, and library identification
//! for GlobalFoundries' open-source 180 nm MCU PDK across both
//! `gf180mcu_fd_sc_mcu7t5v0` (7-track, 5 V) and `gf180mcu_fd_sc_mcu9t5v0`
//! (9-track, 5 V) standard-cell libraries.
//!
//! Cell-name convention:
//! `gf180mcu_fd_sc_<track>__<celltype>_<drive>`
//! e.g. `gf180mcu_fd_sc_mcu7t5v0__nand2_1`.
//!
//! IO pads (`gf180mcu_fd_io__*`), primitives (`gf180mcu_fd_pr__*`), and IP
//! blocks (`gf180mcu_fd_ip_*__*`) all share the `gf180mcu_fd_` prefix and
//! are detected as GF180MCU here so the netlist parser doesn't reject them;
//! cell-type extraction is conservative and only strips the standard-cell
//! prefixes. Decomposition for non-standard-cell families lands in later
//! phases per `docs/plans/gf180mcu-enablement.md`.

/// All GF180MCU cell instantiations share this prefix.
const GF180MCU_PREFIX: &str = "gf180mcu_fd_";

const SC_7T_PREFIX: &str = "gf180mcu_fd_sc_mcu7t5v0__";
const SC_9T_PREFIX: &str = "gf180mcu_fd_sc_mcu9t5v0__";

/// Is `name` any GF180MCU cell — standard cell, IO pad, primitive, or IP?
pub fn is_gf180mcu_cell(name: &str) -> bool {
    name.starts_with(GF180MCU_PREFIX)
}

/// Strip the GF180MCU library prefix and the trailing drive-strength
/// suffix to recover the base cell type.
///
/// `gf180mcu_fd_sc_mcu7t5v0__nand2_1` → `nand2`.
/// `gf180mcu_fd_sc_mcu9t5v0__inv_20` → `inv`.
///
/// Non-standard-cell families (IO, PR, IP) return the suffix after the
/// library prefix verbatim — decomposition for those lands in later
/// phases.
pub fn extract_cell_type(name: &str) -> &str {
    let stripped = name
        .strip_prefix(SC_7T_PREFIX)
        .or_else(|| name.strip_prefix(SC_9T_PREFIX))
        .unwrap_or(name);

    // Drive strength is always a numeric tail after the final underscore
    // — matches the SKY130 convention.
    if let Some(last_underscore) = stripped.rfind('_') {
        let suffix = &stripped[last_underscore + 1..];
        if !suffix.is_empty() && suffix.chars().all(|c| c.is_ascii_digit()) {
            return &stripped[..last_underscore];
        }
    }

    stripped
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_standard_cells_in_both_variants() {
        assert!(is_gf180mcu_cell("gf180mcu_fd_sc_mcu7t5v0__nand2_1"));
        assert!(is_gf180mcu_cell("gf180mcu_fd_sc_mcu9t5v0__nand2_1"));
    }

    #[test]
    fn detects_io_pr_and_ip_families() {
        assert!(is_gf180mcu_cell("gf180mcu_fd_io__bi_24t"));
        assert!(is_gf180mcu_cell("gf180mcu_fd_pr__nmos_3p3"));
        assert!(is_gf180mcu_cell("gf180mcu_fd_ip_sram__sram64x16m8wm1"));
    }

    #[test]
    fn rejects_non_gf180mcu_names() {
        assert!(!is_gf180mcu_cell("sky130_fd_sc_hd__nand2_1"));
        assert!(!is_gf180mcu_cell("AND2_00_0"));
        assert!(!is_gf180mcu_cell(""));
        assert!(!is_gf180mcu_cell("gf180mcu"));
    }

    #[test]
    fn extracts_base_cell_type_across_variants() {
        assert_eq!(extract_cell_type("gf180mcu_fd_sc_mcu7t5v0__inv_1"), "inv");
        assert_eq!(extract_cell_type("gf180mcu_fd_sc_mcu7t5v0__inv_20"), "inv");
        assert_eq!(extract_cell_type("gf180mcu_fd_sc_mcu9t5v0__nand2_4"), "nand2");
        assert_eq!(extract_cell_type("gf180mcu_fd_sc_mcu9t5v0__dffq_1"), "dffq");
        assert_eq!(
            extract_cell_type("gf180mcu_fd_sc_mcu7t5v0__aoi222_1"),
            "aoi222"
        );
    }

    #[test]
    fn extraction_handles_dff_with_polarity_in_name() {
        // The 'n' in dffnq / dffnrnq is part of the cell type, not a
        // drive-strength suffix — the drive number is the trailing _1.
        assert_eq!(extract_cell_type("gf180mcu_fd_sc_mcu7t5v0__dffnq_1"), "dffnq");
        assert_eq!(extract_cell_type("gf180mcu_fd_sc_mcu7t5v0__dffnrsnq_2"), "dffnrsnq");
    }

    #[test]
    fn extraction_passes_through_unprefixed_names() {
        // Unprefixed names still get numeric-suffix stripping.
        assert_eq!(extract_cell_type("inv_1"), "inv");
        // Non-numeric tails are left intact — no false trim.
        assert_eq!(extract_cell_type("not_a_cell"), "not_a_cell");
        assert_eq!(extract_cell_type("plain"), "plain");
    }
}
