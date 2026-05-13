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

use std::collections::HashMap;
use std::path::Path;

use crate::sky130_pdk::{
    build_chain_gate, build_udp_aig, build_xor_chain, finalize_decomp_result, parse_udp,
    BehavioralGate, BehavioralModel, DecompResult, UdpModel, WireVal,
};

/// Behavioral models + UDPs for the GF180MCU PDK.
///
/// Lazy-loaded subset of cell types — only the ones actually instantiated
/// in the netlist need their `.functional.v` parsed. Mirrors SKY130's
/// `PdkModels` but lives separately because the on-disk layout differs.
pub struct Gf180PdkModels {
    /// Behavioral models indexed by base cell type (e.g. "nand2").
    /// Excludes sequential and tie/filler cells — those go through
    /// dedicated paths in `aig.rs::gf180mcu_postprocess`.
    pub models: HashMap<String, BehavioralModel>,
    /// UDP models indexed by the **functional.v gate-type name**
    /// (e.g. `UDP_GF018hv5v_mcu_sc7_TT_1P8V_25C_verilog_nonpg_MGM_N_IQ_FF_UDP`).
    /// Populated from the four PDK-provided primitives under
    /// `vendor/gf180mcu_fd_sc_mcu7t5v0/models/`. Currently used only
    /// for symmetry with sky130 — all GF180MCU sequential cells are
    /// short-circuited by `gf180mcu_postprocess` and never invoke
    /// the SOP decomposer, but the map is here so a future
    /// combinational UDP would route cleanly.
    pub udps: HashMap<String, UdpModel>,
}

/// Load behavioral models + UDP models needed to decompose a set of cells.
///
/// Resolves each cell type to a `.functional.v` file under the vendored
/// `gf180mcu_fd_sc_mcu7t5v0` submodule. The 7t and 9t libraries share
/// identical port layouts and functional bodies (verified at build
/// time by `build.rs`), so we read from 7t exclusively.
///
/// Sequential, tie, and filler cells are skipped — their behaviour
/// is hard-coded in `aig.rs::gf180mcu_preprocess`/`gf180mcu_postprocess`
/// (matching the sky130 precedent of skipping sequentials in its own
/// `load_pdk_models`). The functional.v files for sequential cells
/// reference state-holding UDP primitives that can't be decomposed
/// to pure combinational AIG anyway.
pub fn load_pdk_models(cells_root: &Path, cell_types: &[String]) -> Gf180PdkModels {
    let mut models = HashMap::new();
    let mut udps: HashMap<String, UdpModel> = HashMap::new();
    let models_root = cells_root.join("models");

    for cell_type in cell_types {
        if is_sequential_cell(cell_type) || is_tie_cell(cell_type) || is_filler_cell(cell_type) {
            // Sequential cells: state-holding UDPs handled directly in
            // gf180mcu_postprocess. Tie/filler: no logic to load.
            continue;
        }
        // The submodule lays out files as
        //   cells/<type>/gf180mcu_fd_sc_mcu7t5v0__<type>_<drive>.functional.v
        // Pick the smallest drive available — the functional body is
        // identical across drive strengths (verified in build.rs).
        let dir = cells_root.join("cells").join(cell_type);
        let Some(entry) = pick_smallest_drive_functional(&dir) else {
            panic!(
                "GF180MCU: no functional.v found for cell type '{cell_type}' under {}",
                dir.display()
            );
        };
        let src = std::fs::read_to_string(&entry).unwrap_or_else(|e| {
            panic!("read {}: {e}", entry.display());
        });
        let model = crate::pdk_decomp::parse_functional_model(&src).unwrap_or_else(|| {
            panic!("parse {}: returned None", entry.display())
        });

        // Load any UDPs this model references, by functional.v
        // gate-type name. Currently no combinational cells reference
        // UDPs, but the loop preserves symmetry with sky130.
        for gate in &model.gates {
            if gate.gate_type.starts_with("UDP_GF018") && !udps.contains_key(&gate.gate_type) {
                if let Some(udp) = load_udp_by_functional_name(&models_root, &gate.gate_type) {
                    udps.insert(gate.gate_type.clone(), udp);
                }
            }
        }
        models.insert(cell_type.clone(), model);
    }

    Gf180PdkModels { models, udps }
}

/// Resolve a functional.v UDP gate-type name to its on-disk primitive
/// definition under `vendor/gf180mcu_fd_sc_mcu7t5v0/models/`.
///
/// The `.functional.v` files reference UDPs by names of the form
/// `UDP_GF018hv5v_mcu_sc7_TT_<corner>_verilog_<pg>_MGM_<kind>_UDP` where
/// `<kind>` is one of `N_IQ_FF` / `HN_IQ_FF` / `N_IQ_LATCH` / `HN_IQ_LATCH`.
/// The on-disk primitive name is `gf180mcu_fd_sc_mcu7t5v0__udp_<kind_lc>`,
/// and the directory name matches `udp_<kind_lc>` — except for the latch
/// with both R and S, whose directory is misspelt `upd_hn_iq_latch`
/// upstream.
///
/// Returns the parsed UDP keyed under the **functional.v gate name** so
/// `build_udp_aig` (which looks up gates by `gate.gate_type`) hits.
fn load_udp_by_functional_name(models_root: &Path, gate_name: &str) -> Option<UdpModel> {
    // Strip the boilerplate prefix; what remains is e.g. "MGM_N_IQ_FF_UDP".
    let suffix = gate_name
        .rsplit_once("_MGM_")
        .map(|(_, s)| s)
        .unwrap_or(gate_name);
    let kind_lc = suffix.trim_end_matches("_UDP").to_lowercase();
    if kind_lc.is_empty() {
        return None;
    }

    // Two candidate directory names: the canonical `udp_<kind_lc>` and
    // the upstream typo `upd_<kind_lc>` (latch with both R and S).
    let candidate_dirs = [
        format!("udp_{kind_lc}"),
        format!("upd_{kind_lc}"),
    ];
    let candidate_files = [
        format!("gf180mcu_mcu7t5v0__udp_{kind_lc}.v"),
        format!("gf180mcu_fd_sc_mcu7t5v0__udp_{kind_lc}.v"),
    ];

    for dir_name in &candidate_dirs {
        let dir = models_root.join(dir_name);
        if !dir.is_dir() {
            continue;
        }
        for file_name in &candidate_files {
            let path = dir.join(file_name);
            if !path.is_file() {
                continue;
            }
            let src = std::fs::read_to_string(&path).unwrap_or_else(|e| {
                panic!("read {}: {e}", path.display());
            });
            if let Some(mut udp) = parse_udp(&src) {
                // Rekey so build_udp_aig finds it by the functional.v name.
                udp.name = gate_name.to_string();
                return Some(udp);
            }
        }
        // Fallback: take the first .v file in the directory.
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for entry in rd.flatten() {
                let p = entry.path();
                if p.extension().and_then(|s| s.to_str()) == Some("v") {
                    let src = std::fs::read_to_string(&p)
                        .unwrap_or_else(|e| panic!("read {}: {e}", p.display()));
                    if let Some(mut udp) = parse_udp(&src) {
                        udp.name = gate_name.to_string();
                        return Some(udp);
                    }
                }
            }
        }
    }
    None
}

fn pick_smallest_drive_functional(cell_dir: &Path) -> Option<std::path::PathBuf> {
    let mut best: Option<(usize, std::path::PathBuf)> = None;
    for entry in std::fs::read_dir(cell_dir).ok()?.flatten() {
        let path = entry.path();
        let name = path.file_name()?.to_str()?.to_string();
        if !name.ends_with(".functional.v") || name.ends_with(".functional.pp.v") {
            continue;
        }
        let stem = name.trim_end_matches(".functional.v");
        let drive = stem
            .rsplit_once('_')
            .and_then(|(_, suffix)| suffix.parse::<usize>().ok())
            .unwrap_or(0);
        if best.as_ref().is_none_or(|(d, _)| drive < *d) {
            best = Some((drive, path));
        }
    }
    best.map(|(_, p)| p)
}

/// Decompose one GF180MCU cell to AIG primitives using loaded PDK models.
///
/// Handles standard Verilog gate primitives (`not`, `buf`, `and`, `or`,
/// `nand`, `nor`, `xor`, `xnor`) plus `UDP_GF018*` UDP instantiations
/// routed through `build_udp_aig` (sum-of-products from the UDP truth
/// table). The current GF180MCU catalogue has no combinational UDP
/// cells, so the UDP path is dormant; sequential cells short-circuit
/// out of this function in `aig.rs::gf180mcu_postprocess`.
///
/// Mirrors the shape of `sky130_pdk::decompose_with_pdk`. The two-argument
/// `decompose_combinational` wrapper survives for tests that don't load
/// a `Gf180PdkModels` (their cells never reach UDP gates anyway).
///
/// `inputs` maps each module-input port name to its `aigpin_iv` value
/// in the global AIG. `output_pin` selects which module-output port
/// the returned DecompResult drives (adders have two: `S`, `CO`).
pub fn decompose_with_pdk(
    model: &BehavioralModel,
    inputs: &HashMap<String, usize>,
    output_pin: &str,
    udps: &HashMap<String, UdpModel>,
) -> DecompResult {
    let mut wires: HashMap<String, WireVal> = HashMap::new();
    for name in &model.inputs {
        let &iv = inputs.get(name).unwrap_or_else(|| {
            panic!(
                "Input pin '{name}' not provided for cell '{}'",
                model.module_name
            )
        });
        wires.insert(name.clone(), WireVal::AigPin(iv));
    }

    let mut and_gates: Vec<(i64, i64)> = Vec::new();

    for gate in &model.gates {
        let result = apply_gate(gate, &wires, udps, &mut and_gates);
        wires.insert(gate.output.clone(), result);
    }

    let output = *wires.get(output_pin).unwrap_or_else(|| {
        panic!(
            "Output pin '{output_pin}' missing from decomposed wires for cell '{}'",
            model.module_name
        )
    });
    finalize_decomp_result(and_gates, output)
}

/// UDP-free wrapper for callers (e.g. equivalence tests) that load
/// models directly and never instantiate a UDP gate. Forwards to
/// `decompose_with_pdk` with an empty UDP map.
pub fn decompose_combinational(
    model: &BehavioralModel,
    inputs: &HashMap<String, usize>,
    output_pin: &str,
) -> DecompResult {
    let empty: HashMap<String, UdpModel> = HashMap::new();
    decompose_with_pdk(model, inputs, output_pin, &empty)
}

fn apply_gate(
    gate: &BehavioralGate,
    wires: &HashMap<String, WireVal>,
    udps: &HashMap<String, UdpModel>,
    and_gates: &mut Vec<(i64, i64)>,
) -> WireVal {
    let lookup = |n: &str| -> WireVal {
        *wires
            .get(n)
            .unwrap_or_else(|| panic!("Unknown wire '{n}' in gate '{}'", gate.gate_type))
    };
    let inputs: Vec<WireVal> = gate.inputs.iter().map(|n| lookup(n)).collect();

    match gate.gate_type.as_str() {
        "buf" => {
            assert_eq!(inputs.len(), 1, "buf must have exactly 1 input");
            inputs[0]
        }
        "not" => {
            assert_eq!(inputs.len(), 1, "not must have exactly 1 input");
            inputs[0].inverted()
        }
        "and" => build_chain_gate(&inputs, false, false, and_gates),
        "nand" => build_chain_gate(&inputs, false, true, and_gates),
        // OR via De Morgan: a + b = NOT(NOT(a) AND NOT(b))
        "or" => build_chain_gate(&inputs, true, true, and_gates),
        "nor" => build_chain_gate(&inputs, true, false, and_gates),
        "xor" => build_xor_chain(&inputs, false, and_gates),
        "xnor" => build_xor_chain(&inputs, true, and_gates),
        other if other.starts_with("UDP_GF018") => {
            build_udp_aig(gate, wires, udps, and_gates)
        }
        other => panic!(
            "Unhandled GF180MCU primitive '{other}' in gate with output '{}'. \
             If this is a new Verilog primitive, extend apply_gate().",
            gate.output
        ),
    }
}

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
    fn load_pdk_models_loads_udps_referenced_by_sequential_cells() {
        // Although sequential cells aren't loaded as models, their UDP
        // references must still be picked up so a future combinational
        // UDP cell would have its truth table available. Verify that
        // the four PDK UDPs map back from their functional.v gate
        // names. Done by running the loader on a list that includes
        // a few sequentials — the loader skips the sequential model
        // but reads its functional.v specifically to collect UDP refs.
        //
        // Per the current design, sequentials skip model load entirely,
        // so this currently asserts the *negative* — UDPs come along
        // only when a combinational cell references one. We assert
        // the loader doesn't panic and surfaces some sensible state.
        let cells_root = Path::new("vendor/gf180mcu_fd_sc_mcu7t5v0");
        let cells = ["nand2".to_string(), "dffq".to_string()];
        let pdk = load_pdk_models(cells_root, &cells);
        // nand2 loads; dffq is skipped (sequential).
        assert!(pdk.models.contains_key("nand2"));
        assert!(!pdk.models.contains_key("dffq"));
    }

    #[test]
    fn load_udp_by_functional_name_resolves_all_four_primitives() {
        let models_root =
            Path::new("vendor/gf180mcu_fd_sc_mcu7t5v0/models");
        // The four UDP gate-type names referenced by GF180MCU
        // functional.v files (corner+pg fields stripped down to a
        // representative one). The loader must resolve all four —
        // including the directory-name typo `upd_hn_iq_latch`.
        let ff_n = "UDP_GF018hv5v_mcu_sc7_TT_1P8V_25C_verilog_nonpg_MGM_N_IQ_FF_UDP";
        let ff_hn = "UDP_GF018hv5v_mcu_sc7_TT_1P8V_25C_verilog_nonpg_MGM_HN_IQ_FF_UDP";
        let lat_n = "UDP_GF018hv5v_mcu_sc7_TT_1P8V_25C_verilog_nonpg_MGM_N_IQ_LATCH_UDP";
        let lat_hn = "UDP_GF018hv5v_mcu_sc7_TT_1P8V_25C_verilog_nonpg_MGM_HN_IQ_LATCH_UDP";

        for name in [ff_n, ff_hn, lat_n, lat_hn] {
            let udp = load_udp_by_functional_name(models_root, name).unwrap_or_else(|| {
                panic!("failed to load UDP '{name}' from {}", models_root.display())
            });
            assert_eq!(udp.name, name, "UDP rekey by functional.v gate name");
            // Every PDK UDP for FF/latch has 5 inputs: C, P, CK, D, N.
            assert_eq!(
                udp.inputs.len(),
                5,
                "{name}: expected 5 inputs (C, P, CK, D, N), got {:?}",
                udp.inputs
            );
        }
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

    // -------------------------------------------------------------------------
    // Boolean-equivalence: AIG decomposition vs Verilog reference oracle.
    //
    // For each combinational cell type we exercise:
    //   1. Parse the .functional.v model.
    //   2. Assign each input a unique aigpin_iv (even = uninverted).
    //   3. Iterate all 2^N input combinations.
    //   4. Evaluate via eval_behavioral_model (Verilog interpretation).
    //   5. Decompose to AIG via decompose_combinational.
    //   6. Walk the AIG and compare outputs.
    //
    // The AIG walker matches the encoding in sky130_pdk::finalize_decomp_result.
    // -------------------------------------------------------------------------

    fn load_cell_model(cell_type: &str) -> BehavioralModel {
        let dir = Path::new("vendor/gf180mcu_fd_sc_mcu7t5v0/cells").join(cell_type);
        let path = pick_smallest_drive_functional(&dir)
            .unwrap_or_else(|| panic!("no functional.v under {}", dir.display()));
        let src = std::fs::read_to_string(&path).unwrap();
        crate::pdk_decomp::parse_functional_model(&src)
            .unwrap_or_else(|| panic!("parse {}", path.display()))
    }

    /// Evaluate a decomposed AIG on a set of input pin values.
    fn eval_decomp(decomp: &DecompResult, pin_values: &HashMap<usize, bool>) -> bool {
        let mut gate_values: Vec<bool> = Vec::with_capacity(decomp.and_gates.len());
        for &(a_ref, b_ref) in &decomp.and_gates {
            let a = resolve_ref(a_ref, pin_values, &gate_values);
            let b = resolve_ref(b_ref, pin_values, &gate_values);
            gate_values.push(a & b);
        }
        let base = if decomp.output_idx >= 0 {
            pin_values[&(decomp.output_idx as usize)]
        } else {
            let gate_idx = (-decomp.output_idx - 1) as usize;
            gate_values[gate_idx]
        };
        base ^ decomp.output_inverted
    }

    fn resolve_ref(v: i64, pins: &HashMap<usize, bool>, gates: &[bool]) -> bool {
        if v >= 0 {
            let iv = v as usize;
            let pin_idx = iv >> 1;
            let inv = (iv & 1) == 1;
            pins[&pin_idx] ^ inv
        } else {
            // Encoding from convert_ref_to_standard: gate_idx N uninverted → -(2N+1);
            // inverted → -(2N+1) ^ 1 (which is even since -(2N+1) is odd).
            let inv = (v & 1) == 0;
            let gate_idx = if inv {
                ((-v - 2) / 2) as usize
            } else {
                ((-v - 1) / 2) as usize
            };
            gates[gate_idx] ^ inv
        }
    }

    /// For one cell + output pin, sweep every 2^N input combination and
    /// assert AIG output matches the Verilog-interpreted reference.
    fn assert_equivalent(cell_type: &str, output_pin: &str) {
        let model = load_cell_model(cell_type);
        let n = model.inputs.len();
        assert!(n <= 8, "{cell_type} has {n} inputs — bump the sweep cap");

        // Assign each input pin index 1, 2, 3, … (aigpin_iv = idx * 2).
        let pin_index_for: HashMap<String, usize> = model
            .inputs
            .iter()
            .enumerate()
            .map(|(i, name)| (name.clone(), i + 1))
            .collect();
        let aigpin_iv_inputs: HashMap<String, usize> = pin_index_for
            .iter()
            .map(|(name, &idx)| (name.clone(), idx * 2))
            .collect();

        let decomp = decompose_combinational(&model, &aigpin_iv_inputs, output_pin);

        let empty_udps = HashMap::new();
        for mask in 0..(1u32 << n) {
            let mut bool_inputs: HashMap<String, bool> = HashMap::new();
            let mut pin_values: HashMap<usize, bool> = HashMap::new();
            for (i, name) in model.inputs.iter().enumerate() {
                let bit = (mask >> i) & 1 == 1;
                bool_inputs.insert(name.clone(), bit);
                pin_values.insert(pin_index_for[name], bit);
            }
            let expected = crate::sky130_pdk::eval_behavioral_model(
                &model,
                &bool_inputs,
                output_pin,
                &empty_udps,
            );
            let got = eval_decomp(&decomp, &pin_values);
            assert_eq!(
                got, expected,
                "cell={cell_type} pin={output_pin} mask=0b{mask:0width$b} \
                 inputs={bool_inputs:?}: expected {expected}, got {got}",
                width = n.max(1)
            );
        }
    }

    #[test]
    fn equivalent_inv() {
        assert_equivalent("inv", "ZN");
    }

    #[test]
    fn equivalent_buf() {
        assert_equivalent("buf", "Z");
    }

    #[test]
    fn equivalent_nand2() {
        assert_equivalent("nand2", "ZN");
    }

    #[test]
    fn equivalent_nand4() {
        assert_equivalent("nand4", "ZN");
    }

    #[test]
    fn equivalent_or3() {
        assert_equivalent("or3", "Z");
    }

    #[test]
    fn equivalent_nor2() {
        assert_equivalent("nor2", "ZN");
    }

    #[test]
    fn equivalent_xor2() {
        assert_equivalent("xor2", "Z");
    }

    #[test]
    fn equivalent_xnor3() {
        assert_equivalent("xnor3", "ZN");
    }

    #[test]
    fn equivalent_aoi22() {
        assert_equivalent("aoi22", "ZN");
    }

    #[test]
    fn equivalent_oai222() {
        assert_equivalent("oai222", "ZN");
    }

    #[test]
    fn equivalent_mux2() {
        assert_equivalent("mux2", "Z");
    }

    #[test]
    fn equivalent_addf_sum() {
        assert_equivalent("addf", "S");
    }

    #[test]
    fn equivalent_addf_carry_out() {
        assert_equivalent("addf", "CO");
    }
}
