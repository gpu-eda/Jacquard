// SPDX-FileCopyrightText: Copyright (c) 2026 ChipFlow Inc. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! GF180MCU cell classification.
//!
//! Each query function takes the base cell type (the result of
//! [`crate::gf180mcu::extract_cell_type`]) — never the full
//! prefixed/drive-suffixed macro name. Classification is by exact
//! match per cell type rather than prefix, mirroring the lesson from
//! `docs/adding-a-pdk.md` § Common Pitfalls (e.g. `dlygate4sd3` vs
//! latch cells in SKY130).
//!
//! Sibling of [`crate::sky130_pdk`]. AIG decomposition rules and
//! behavioural-model parsing land in Phase 4 per
//! `docs/plans/gf180mcu-enablement.md`.
//!
//! # Reset polarity
//!
//! GF180MCU uses **active-low** resets and sets, following the same
//! convention as SKY130. Reset ports are named `RN` and set ports
//! `SETN`. The "n" *prefix* in cell names like `dffnq` / `dffnrnq` /
//! `icgtn` instead indicates a **negative-edge clock** — the relevant
//! port is then `CLKN` rather than `CLK`.

#![allow(unused)]

/// Flip-flops + latches + clock-gating cells — anything whose Q output
/// captures state across a clock edge (or transparency window).
pub fn is_sequential_cell(cell_type: &str) -> bool {
    matches!(
        cell_type,
        // Positive- and negative-edge DFFs (R = active-low reset RN,
        // S = active-low set SETN, sdff = scan DFF with SE/SI pins).
        "dffq" | "dffnq"
            | "dffrnq" | "dffnrnq"
            | "dffsnq" | "dffnsnq"
            | "dffrsnq" | "dffnrsnq"
            | "sdffq" | "sdffrnq" | "sdffsnq" | "sdffrsnq"
            // Level-sensitive latches (E enable).
            | "latq" | "latrnq" | "latsnq" | "latrsnq"
            // Clock-gating cells (icgtp = positive enable, icgtn = negative).
            | "icgtp" | "icgtn"
    )
}

/// Constant-driving tie cells. Their output is always 1 (`tieh`) or 0
/// (`tiel`) and they have no functional inputs.
pub fn is_tie_cell(cell_type: &str) -> bool {
    matches!(cell_type, "tieh" | "tiel")
}

/// Physical-only cells: fillers, decap, end-cap, antenna diodes.
/// Recognised so post-P&R netlists parse, but they contribute no
/// logic to the AIG.
pub fn is_filler_cell(cell_type: &str) -> bool {
    matches!(
        cell_type,
        "antenna" | "endcap" | "fill" | "fillcap" | "filltie"
    )
}

/// Hold-time repair buffers and explicit delay cells — combinational,
/// I → Z, but inserted by P&R for timing reasons rather than logic.
pub fn is_delay_cell(cell_type: &str) -> bool {
    matches!(cell_type, "dlya" | "dlyb" | "dlyc" | "dlyd" | "hold")
}

/// Cells with more than one functional output. The AIG builder
/// processes one output pin at a time, so multi-output cells need
/// per-output decomposition rules in Phase 4.
pub fn is_multi_output_cell(cell_type: &str) -> bool {
    // Full adder (S, CO) and half adder (S, CO). All other gf180mcu
    // standard cells are single-output.
    matches!(cell_type, "addf" | "addh")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The full GF180MCU base cell-type catalogue. Both the 7t5v0 and
    /// 9t5v0 standard cell libraries ship these 69 cells; the lists
    /// are identical (verified in build.rs's dedup pass).
    const ALL_CELL_TYPES: &[&str] = &[
        "addf", "addh", "and2", "and3", "and4", "antenna", "aoi21", "aoi211", "aoi22",
        "aoi221", "aoi222", "buf", "bufz", "clkbuf", "clkinv", "dffnq", "dffnrnq",
        "dffnrsnq", "dffnsnq", "dffq", "dffrnq", "dffrsnq", "dffsnq", "dlya", "dlyb",
        "dlyc", "dlyd", "endcap", "fill", "fillcap", "filltie", "hold", "icgtn", "icgtp",
        "inv", "invz", "latq", "latrnq", "latrsnq", "latsnq", "mux2", "mux4", "nand2",
        "nand3", "nand4", "nor2", "nor3", "nor4", "oai21", "oai211", "oai22", "oai221",
        "oai222", "oai31", "oai32", "oai33", "or2", "or3", "or4", "sdffq", "sdffrnq",
        "sdffrsnq", "sdffsnq", "tieh", "tiel", "xnor2", "xnor3", "xor2", "xor3",
    ];

    #[test]
    fn catalogue_size_matches_pin_table() {
        assert_eq!(ALL_CELL_TYPES.len(), 69);
    }

    #[test]
    fn sequential_classification_is_correct() {
        // DFFs + latches + clock gating = 12 + 4 + 2 = 18 cells.
        let seq: Vec<&&str> = ALL_CELL_TYPES
            .iter()
            .filter(|c| is_sequential_cell(c))
            .collect();
        assert_eq!(
            seq.len(),
            18,
            "expected 18 sequential cells, got {:?}",
            seq
        );
    }

    #[test]
    fn tie_classification() {
        assert!(is_tie_cell("tieh"));
        assert!(is_tie_cell("tiel"));
        assert!(!is_tie_cell("dffq"));
        assert!(!is_tie_cell("inv"));
    }

    #[test]
    fn filler_classification() {
        assert!(is_filler_cell("fill"));
        assert!(is_filler_cell("fillcap"));
        assert!(is_filler_cell("filltie"));
        assert!(is_filler_cell("endcap"));
        assert!(is_filler_cell("antenna"));
        assert!(!is_filler_cell("hold"));
        assert!(!is_filler_cell("buf"));
    }

    #[test]
    fn delay_classification() {
        for c in ["dlya", "dlyb", "dlyc", "dlyd", "hold"] {
            assert!(is_delay_cell(c), "{c} should be a delay cell");
        }
        // Plain buffers aren't delay-repair cells.
        assert!(!is_delay_cell("buf"));
        assert!(!is_delay_cell("clkbuf"));
    }

    #[test]
    fn multi_output_classification() {
        assert!(is_multi_output_cell("addf"));
        assert!(is_multi_output_cell("addh"));
        // DFFs only emit Q (no QN counterpart in the gf180mcu catalogue).
        assert!(!is_multi_output_cell("dffq"));
        assert!(!is_multi_output_cell("dffrnq"));
        // Inverters are single-output.
        assert!(!is_multi_output_cell("inv"));
    }

    #[test]
    fn categories_partition_the_catalogue_disjointly() {
        // Every cell belongs to at most one of {sequential, tie, filler,
        // delay}. Multi-output is orthogonal (an adder is combinational
        // but multi-output). Catches misclassification regressions.
        for c in ALL_CELL_TYPES {
            let memberships = [
                is_sequential_cell(c),
                is_tie_cell(c),
                is_filler_cell(c),
                is_delay_cell(c),
            ];
            let count = memberships.iter().filter(|&&x| x).count();
            assert!(
                count <= 1,
                "cell {c} matches more than one classification: {:?}",
                memberships
            );
        }
    }

    /// Experiment: confirm SKY130's behavioral parser handles GF180MCU's
    /// `.functional.v` grammar without modification. The two PDKs share
    /// the same Verilog-primitive-based cell-model format; if this test
    /// passes, Phase 4 can reuse `parse_functional_model` directly
    /// rather than writing a parallel parser.
    /// The DFF cells embed a UDP primitive (`UDP_GF018hv5v_..._FF_UDP`).
    /// SKY130 expects its own `sky130_fd_sc_hd__udp_*` prefix; this test
    /// records what happens when the parser encounters a different UDP
    /// prefix. Phase 4 decisions depend on whether the parser is fully
    /// PDK-neutral on the parse side (with PDK-specific UDP *handling*
    /// downstream) or needs explicit prefix configuration.
    #[test]
    fn sky130_parser_reads_gf180mcu_dff_with_udp() {
        use crate::sky130_pdk::parse_functional_model;
        let path = "vendor/gf180mcu_fd_sc_mcu7t5v0/cells/dffq/\
                    gf180mcu_fd_sc_mcu7t5v0__dffq_1.functional.v";
        let src = std::fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("read {path}: {e}"));
        let model = parse_functional_model(&src).expect("parse should succeed");
        assert_eq!(model.module_name, "gf180mcu_fd_sc_mcu7t5v0__dffq_1");
        // dffq_1 wraps a UDP_GF018... FF UDP between two `not` gates;
        // the parser must surface all three.
        assert!(
            model.gates.len() >= 3,
            "expected at least 3 gates (not, UDP, not), got {}: {:?}",
            model.gates.len(),
            model.gates,
        );
        let has_udp = model
            .gates
            .iter()
            .any(|g| g.gate_type.contains("UDP_GF018"));
        assert!(
            has_udp,
            "expected a UDP_GF018 entry in the gate list: {:?}",
            model.gates
        );
    }

    #[test]
    fn sky130_parser_reads_gf180mcu_simple_gate() {
        use crate::sky130_pdk::parse_functional_model;
        let path = "vendor/gf180mcu_fd_sc_mcu7t5v0/cells/nand2/\
                    gf180mcu_fd_sc_mcu7t5v0__nand2_1.functional.v";
        let src = std::fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("read {path}: {e} \
                — submodule may need `git submodule update --init`"));
        let model = parse_functional_model(&src).expect("parse should succeed");
        assert_eq!(model.module_name, "gf180mcu_fd_sc_mcu7t5v0__nand2_1");
        assert_eq!(model.inputs, vec!["A1".to_string(), "A2".to_string()]);
        assert_eq!(model.outputs, vec!["ZN".to_string()]);
        // nand2_1 is implemented as De Morgan: not A1, not A2, then or →
        // 2 nots + 1 or = 3 gates.
        assert_eq!(model.gates.len(), 3, "gates: {:?}", model.gates);
    }

    #[test]
    fn combinational_cells_have_no_classification() {
        // Pure combinational gates (inv, nand2, oai21, etc.) should NOT
        // match any specialised classifier.
        for c in [
            "inv", "buf", "nand2", "nor3", "and4", "or2", "xor2", "xnor3", "aoi22",
            "oai222", "mux2", "mux4",
        ] {
            assert!(!is_sequential_cell(c), "{c} should not be sequential");
            assert!(!is_tie_cell(c), "{c} should not be a tie cell");
            assert!(!is_filler_cell(c), "{c} should not be a filler");
            assert!(!is_delay_cell(c), "{c} should not be a delay cell");
        }
    }
}
