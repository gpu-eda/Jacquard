// SPDX-License-Identifier: Apache-2.0

//! Module B: a parsed Liberty library tree -> [`cell_model_ir::CellModelIr`].
//!
//! Walks each `cell` group, reads pin directions (L1), and for every output
//! pin carrying a `function` attribute compiles it to an AIG via
//! [`crate::function`] (L2, D3). The per-output single-output [`CombLogic`]s
//! are merged into ONE [`CombLogic`] per cell over a shared input numbering.
//!
//! Classification (D4, C2.2): sequential cells (Liberty `ff`/`latch`) carry
//! L3 [`cell_model_ir::Sequential`] and are classified `Dff`/`Latch`;
//! integrated clock gates are `ClockGate`; a cell with a combinational output
//! cone is `Std`; tie / filler / endcap / tap cells classify by function /
//! name. L4 per-cell timing (corner-keyed) is emitted alongside. See
//! [`crate::sequential`] (L3) and [`crate::timing`] (L4).

use cell_model_ir::{
    AndNode, CellModel, CellModelIr, CombLogic, Direction, LibraryMeta, OutputPin, Pin, Ref,
};
use liberty_parse::LibertyGroup;

use crate::function;
use crate::sequential::{self, SeqNote};
use crate::timing;

/// A diagnostic emitted while converting (surfaced by the CLI / collected by
/// tests).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConvertNote {
    /// A cell with no Liberty `function` on any output — emitted as
    /// `Other`/`None`, needs a `.v` fallback (documented C1 follow-up).
    SkippedNoFunction { cell: String },
    /// An output `function` references a non-input operand (e.g. a DFF's
    /// internal `IQ` state node). This is **expected** for sequential cells,
    /// which C1 does not model combinationally (that is C2's D4 work) — it is
    /// not an error. The cell is emitted as `Other`/`None`.
    SequentialOutput {
        cell: String,
        pin: String,
        operands: Vec<String>,
    },
    /// A `function` string genuinely failed to parse or compile.
    FunctionParseError {
        cell: String,
        pin: String,
        error: String,
    },
}

/// Result of converting a whole library tree.
pub struct Conversion {
    pub ir: CellModelIr,
    pub notes: Vec<ConvertNote>,
    /// L3 (sequential) extraction diagnostics, incl. the `clear_preset_var`
    /// precedence findings.
    pub seq_notes: Vec<SeqNote>,
}

/// Convert a parsed `library(...)` group into a [`CellModelIr`].
///
/// `prefixes` is the D8 selection prefix set; if empty it is derived from the
/// common cell-name prefix (see [`derive_prefix`]).
pub fn convert_library(lib: &LibertyGroup, prefixes: Vec<String>) -> Conversion {
    assert_eq!(lib.group_type, "library", "expected a `library` group");
    let name = lib.first_name().unwrap_or("unnamed").to_string();

    // L4 corner derivation (D5, C3.1b): a single corner read from the library's
    // own PVT metadata (`operating_conditions` / `nom_*`), falling back to the
    // GF180/SKY130 filename heuristic. The per-value ps scale comes from the
    // library's `time_unit`. For a single-corner input `.lib` this is a
    // one-entry corner set; a logic-only lib (no PVT, no recognisable corner
    // name) emits no L4 timing.
    let time_unit = lib.attr("time_unit").and_then(|a| a.first_string());
    let ps_scale = timing::ps_per_time_unit(time_unit);
    let corner = timing::corner_from_library(lib);
    let corner_index = corner.as_ref().map(|_| 0u32);

    let mut cells = Vec::new();
    let mut notes = Vec::new();
    let mut seq_notes = Vec::new();

    for cell_grp in lib.groups_of_type("cell") {
        let (model, mut cnotes, mut snotes) = convert_cell(cell_grp, corner_index, ps_scale);
        notes.append(&mut cnotes);
        seq_notes.append(&mut snotes);
        cells.push(model);
    }

    let prefixes = if prefixes.is_empty() {
        derive_prefix(&cells).map(|p| vec![p]).unwrap_or_default()
    } else {
        prefixes
    };

    let mut ir = CellModelIr::new(LibraryMeta { name, prefixes });
    if let Some(c) = corner {
        ir.default_corner = c.name.clone();
        ir.corners = vec![c];
    }
    ir.cells = cells;
    Conversion {
        ir,
        notes,
        seq_notes,
    }
}

/// Read pin directions, returning `(pins, ordered_input_pin_names)`.
fn read_pins(cell: &LibertyGroup) -> (Vec<Pin>, Vec<String>) {
    let mut pins = Vec::new();
    let mut inputs = Vec::new();
    for pin_grp in cell.groups_of_type("pin") {
        let Some(name) = pin_grp.first_name() else {
            continue;
        };
        let dir = match pin_grp.attr("direction").and_then(|a| a.first_string()) {
            Some("output") => Direction::Output,
            // input / inout / internal / unspecified: treat as input
            // (its value is externally driven). Rare for stdcells.
            _ => Direction::Input,
        };
        if dir == Direction::Input {
            inputs.push(name.to_string());
        }
        pins.push(Pin {
            name: name.to_string(),
            direction: dir,
        });
    }
    (pins, inputs)
}

/// Convert one `cell` group.
///
/// `corner_index` is `Some(0)` when the library yielded an L4 corner (then
/// timing is emitted), `None` for a logic-only library. `ps_scale` converts
/// the library's native `time_unit` to picoseconds.
fn convert_cell(
    cell: &LibertyGroup,
    corner_index: Option<u32>,
    ps_scale: f64,
) -> (CellModel, Vec<ConvertNote>, Vec<SeqNote>) {
    let cell_type = cell.first_name().unwrap_or("unnamed").to_string();
    let (pins, input_pins) = read_pins(cell);
    let mut notes = Vec::new();

    // Sequential cells (Liberty `ff`/`latch`) store their data path in L3
    // next_state, NOT in L2 combinational logic — their output `function`
    // strings reference internal state vars (`IQ1`). Skip the combinational
    // compile for them so we don't emit spurious `SequentialOutput` notes.
    let is_sequential = sequential::has_ff(cell) || sequential::has_latch(cell);

    let comb_logic = if is_sequential {
        None
    } else {
        compile_comb_logic(cell, &cell_type, &input_pins, &mut notes)
    };
    let has_logic = comb_logic.is_some();

    // L3: classification + sequential pin-roles.
    let seq = sequential::build(cell, &cell_type, &input_pins, has_logic);

    // L4: per-cell timing, keyed by the single derived corner.
    let cell_timing = corner_index.and_then(|ci| timing::build_cell_timing(cell, ci, ps_scale));

    // A sequential cell carries its data path in `sequential.next_state`, so
    // `logic` is None; a combinational/physical cell carries `logic`.
    let (logic, sequential_field) = if seq.sequential.is_some() {
        (None, seq.sequential)
    } else {
        (comb_logic, None)
    };

    (
        CellModel {
            cell_type,
            kind: seq.kind,
            pins,
            logic,
            sequential: sequential_field,
            timing: cell_timing,
        },
        notes,
        seq.notes,
    )
}

/// Compile a non-sequential cell's combinational output cone into one merged
/// [`CombLogic`], or `None` if no output carries a usable Liberty `function`.
fn compile_comb_logic(
    cell: &LibertyGroup,
    cell_type: &str,
    input_pins: &[String],
    notes: &mut Vec<ConvertNote>,
) -> Option<CombLogic> {
    // Gather (output_pin, function_str) pairs, in pin order.
    let mut output_fns: Vec<(String, String)> = Vec::new();
    for pin_grp in cell.groups_of_type("pin") {
        let Some(name) = pin_grp.first_name() else {
            continue;
        };
        let is_output = matches!(
            pin_grp.attr("direction").and_then(|a| a.first_string()),
            Some("output")
        );
        if !is_output {
            continue;
        }
        if let Some(func_attr) = pin_grp.attr("function") {
            if let Some(s) = func_attr.first_string() {
                let s = s.trim();
                if !s.is_empty() {
                    output_fns.push((name.to_string(), s.to_string()));
                }
            }
        }
    }

    // Compile each output function. An output whose `function` references an
    // operand that is not a declared input pin cannot be combinationally
    // modelled here — surface and skip that pin.
    let mut per_output: Vec<CombLogic> = Vec::new();
    for (pin, func_src) in &output_fns {
        match function::parse(func_src) {
            Ok(expr) => {
                let referenced = expr.pins();
                let unknown: Vec<&String> = referenced
                    .iter()
                    .filter(|p| !input_pins.iter().any(|ip| ip == *p))
                    .collect();
                if !unknown.is_empty() {
                    notes.push(ConvertNote::SequentialOutput {
                        cell: cell_type.to_string(),
                        pin: pin.clone(),
                        operands: unknown.into_iter().cloned().collect(),
                    });
                    continue;
                }
                match function::compile(pin, &expr, input_pins) {
                    Ok(logic) => per_output.push(logic),
                    Err(e) => notes.push(ConvertNote::FunctionParseError {
                        cell: cell_type.to_string(),
                        pin: pin.clone(),
                        error: e,
                    }),
                }
            }
            Err(e) => notes.push(ConvertNote::FunctionParseError {
                cell: cell_type.to_string(),
                pin: pin.clone(),
                error: e,
            }),
        }
    }

    if per_output.is_empty() {
        if output_fns.is_empty() {
            notes.push(ConvertNote::SkippedNoFunction {
                cell: cell_type.to_string(),
            });
        }
        return None;
    }

    Some(merge_outputs(input_pins, per_output))
}

/// Merge several single-output [`CombLogic`]s (each already built over the
/// SAME shared `inputs` ordering) into one. Each output's `and_nodes` are
/// appended and its refs are remapped: input/const refs are unchanged
/// (shared numbering), and-node refs are shifted by the running and-node
/// offset.
fn merge_outputs(inputs: &[String], per_output: Vec<CombLogic>) -> CombLogic {
    let n_inputs = inputs.len();
    let mut and_nodes: Vec<AndNode> = Vec::new();
    let mut outputs: Vec<OutputPin> = Vec::new();

    let and_base = 1 + n_inputs as u32;
    for logic in per_output {
        debug_assert_eq!(
            logic.inputs, inputs,
            "merge requires identical shared input ordering"
        );
        let offset = and_nodes.len() as u32; // how many and-nodes already placed
        let remap = |r: Ref| -> Ref {
            if r.node >= and_base {
                Ref {
                    node: r.node + offset,
                    inverted: r.inverted,
                }
            } else {
                // const-0 (node 0) or an input node: unchanged.
                r
            }
        };
        for an in &logic.and_nodes {
            and_nodes.push(AndNode {
                a: remap(an.a),
                b: remap(an.b),
            });
        }
        for op in logic.outputs {
            outputs.push(OutputPin {
                pin: op.pin,
                r: remap(op.r),
            });
        }
    }

    CombLogic {
        inputs: inputs.to_vec(),
        and_nodes,
        outputs,
    }
}

/// Derive a common cell-name prefix from the cells (D8 fallback). Returns the
/// longest shared prefix trimmed to a trailing `__` (or `_`) boundary, e.g.
/// `gf180mcu_fd_sc_mcu7t5v0__`.
pub(crate) fn derive_prefix(cells: &[CellModel]) -> Option<String> {
    let mut iter = cells.iter().map(|c| c.cell_type.as_str());
    let first = iter.next()?;
    let mut common: &str = first;
    for name in iter {
        let n = common
            .bytes()
            .zip(name.bytes())
            .take_while(|(a, b)| a == b)
            .count();
        common = &common[..n];
        if common.is_empty() {
            return None;
        }
    }
    if let Some(idx) = common.rfind("__") {
        Some(common[..idx + 2].to_string())
    } else if let Some(idx) = common.rfind('_') {
        Some(common[..idx + 1].to_string())
    } else if common.is_empty() {
        None
    } else {
        Some(common.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cell_model_ir::CellKind;
    use std::collections::HashMap;

    fn parse_lib(src: &str) -> LibertyGroup {
        liberty_parse::parse(src).expect("parse liberty")
    }

    #[test]
    fn converts_and2_directions_and_logic() {
        let src = r#"
        library(demo) {
          cell(demo__and2) {
            pin(A1) { direction : input ; }
            pin(A2) { direction : input ; }
            pin(Z)  { direction : output ; function : "(A1&A2)" ; }
          }
        }
        "#;
        let conv = convert_library(&parse_lib(src), vec!["demo__".into()]);
        assert_eq!(conv.ir.cells.len(), 1);
        let c = &conv.ir.cells[0];
        assert_eq!(c.cell_type, "demo__and2");
        assert_eq!(c.kind, CellKind::Std);
        assert_eq!(c.pins.len(), 3);
        let logic = c.logic.as_ref().unwrap();
        for (a1, a2) in [(false, false), (false, true), (true, false), (true, true)] {
            let mut m = HashMap::new();
            m.insert("A1".to_string(), a1);
            m.insert("A2".to_string(), a2);
            let out = logic.eval(&m).unwrap();
            assert_eq!(out["Z"], a1 && a2);
        }
    }

    #[test]
    fn multi_output_cell_merges_into_one_logic() {
        // S = A^B, C = A&B (shared inputs A,B).
        let src = r#"
        library(demo) {
          cell(demo__ha) {
            pin(A) { direction : input ; }
            pin(B) { direction : input ; }
            pin(S) { direction : output ; function : "(A^B)" ; }
            pin(C) { direction : output ; function : "(A&B)" ; }
          }
        }
        "#;
        let conv = convert_library(&parse_lib(src), vec![]);
        let c = &conv.ir.cells[0];
        assert_eq!(c.kind, CellKind::Std);
        let logic = c.logic.as_ref().unwrap();
        assert_eq!(logic.inputs, vec!["A".to_string(), "B".to_string()]);
        assert_eq!(logic.outputs.len(), 2);
        for (a, b) in [(false, false), (false, true), (true, false), (true, true)] {
            let mut m = HashMap::new();
            m.insert("A".to_string(), a);
            m.insert("B".to_string(), b);
            let out = logic.eval(&m).unwrap();
            assert_eq!(out["S"], a ^ b, "S at {a},{b}");
            assert_eq!(out["C"], a && b, "C at {a},{b}");
        }
    }

    #[test]
    fn output_referencing_internal_node_without_ff_group_is_not_std() {
        // A cell whose Q `function` references an internal node `IQ` (not a
        // declared input pin) but with NO `ff` group: it cannot be modelled
        // combinationally, so it must NOT be `Std` and carries no logic. The
        // internal-node reference is surfaced as a `SequentialOutput` note.
        let src = r#"
        library(demo) {
          cell(demo__dff) {
            pin(CLK) { direction : input ; }
            pin(D)   { direction : input ; }
            pin(Q)   { direction : output ; function : "IQ" ; }
          }
        }
        "#;
        let conv = convert_library(&parse_lib(src), vec![]);
        let c = &conv.ir.cells[0];
        assert_ne!(c.kind, CellKind::Std);
        assert!(c.logic.is_none());
        assert!(conv
            .notes
            .iter()
            .any(|n| matches!(n, ConvertNote::SequentialOutput { .. })));
    }

    #[test]
    fn real_ff_cell_emits_l3_and_no_spurious_notes() {
        // A real flip-flop with an `ff` group: classified `Dff`, carries L3
        // sequential metadata, no `logic`, and emits NO `SequentialOutput`
        // note (the comb compile is skipped for sequential cells).
        let src = r#"
        library(demo) {
          cell(demo__dffq) {
            ff(IQ1, IQN1) { clocked_on : "CLK" ; next_state : "D" ; }
            pin(CLK) { direction : input ; clock : true ; }
            pin(D)   { direction : input ; }
            pin(Q)   { direction : output ; function : "IQ1" ; }
          }
        }
        "#;
        let conv = convert_library(&parse_lib(src), vec![]);
        let c = &conv.ir.cells[0];
        assert_eq!(c.kind, CellKind::Dff);
        assert!(c.logic.is_none());
        assert!(c.sequential.is_some());
        assert!(!conv
            .notes
            .iter()
            .any(|n| matches!(n, ConvertNote::SequentialOutput { .. })));
    }

    #[test]
    fn cell_with_no_function_is_classified_filler() {
        let src = r#"
        library(demo) {
          cell(demo__fill) {
            pin(VGND) { direction : input ; }
          }
        }
        "#;
        let conv = convert_library(&parse_lib(src), vec![]);
        let c = &conv.ir.cells[0];
        assert_eq!(c.kind, CellKind::Filler);
        assert!(c.logic.is_none());
        assert!(conv.notes.iter().any(|n| matches!(
            n,
            ConvertNote::SkippedNoFunction { cell } if cell == "demo__fill"
        )));
    }

    #[test]
    fn derives_prefix_from_cells() {
        let cells = vec![
            CellModel {
                cell_type: "gf180mcu_fd_sc_mcu7t5v0__and2_1".into(),
                kind: CellKind::Std,
                pins: vec![],
                logic: None,
                sequential: None,
                timing: None,
            },
            CellModel {
                cell_type: "gf180mcu_fd_sc_mcu7t5v0__nand2_1".into(),
                kind: CellKind::Std,
                pins: vec![],
                logic: None,
                sequential: None,
                timing: None,
            },
        ];
        assert_eq!(
            derive_prefix(&cells).as_deref(),
            Some("gf180mcu_fd_sc_mcu7t5v0__")
        );
    }

    #[test]
    fn merged_logic_round_trips_through_json() {
        let src = r#"
        library(demo) {
          cell(demo__aoi21) {
            pin(A1) { direction : input ; }
            pin(A2) { direction : input ; }
            pin(B)  { direction : input ; }
            pin(ZN) { direction : output ; function : "(((!A1)&(!B))|((!A2)&(!B)))" ; }
          }
        }
        "#;
        let conv = convert_library(&parse_lib(src), vec![]);
        let json = conv.ir.to_json().unwrap();
        let back = CellModelIr::from_json(&json).unwrap();
        assert_eq!(conv.ir, back);
    }
}
