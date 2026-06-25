// SPDX-License-Identifier: Apache-2.0

//! Module B: a parsed Liberty library tree -> [`cell_model_ir::CellModelIr`].
//!
//! Walks each `cell` group, reads pin directions (L1), and for every output
//! pin carrying a `function` attribute compiles it to an AIG via
//! [`crate::function`] (L2, D3). The per-output single-output [`CombLogic`]s
//! are merged into ONE [`CombLogic`] per cell over a shared input numbering.
//!
//! Classification (C1 stub, D4 is C2): a cell with >=1 combinational output
//! function is [`CellKind::Comb`]; everything else (sequential, tie, filler,
//! and — for C1 — any cell whose outputs lack a Liberty `function`) is
//! [`CellKind::Other`] with `logic = None`.

use cell_model_ir::{
    AndNode, CellKind, CellModel, CellModelIr, CombLogic, Direction, LibraryMeta, OutputPin, Pin,
    Ref,
};
use liberty_parse::LibertyGroup;

use crate::function;

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
}

/// Convert a parsed `library(...)` group into a [`CellModelIr`].
///
/// `prefixes` is the D8 selection prefix set; if empty it is derived from the
/// common cell-name prefix (see [`derive_prefix`]).
pub fn convert_library(lib: &LibertyGroup, prefixes: Vec<String>) -> Conversion {
    assert_eq!(lib.group_type, "library", "expected a `library` group");
    let name = lib.first_name().unwrap_or("unnamed").to_string();

    let mut cells = Vec::new();
    let mut notes = Vec::new();

    for cell_grp in lib.groups_of_type("cell") {
        let (model, mut cnotes) = convert_cell(cell_grp);
        notes.append(&mut cnotes);
        cells.push(model);
    }

    let prefixes = if prefixes.is_empty() {
        derive_prefix(&cells).map(|p| vec![p]).unwrap_or_default()
    } else {
        prefixes
    };

    let mut ir = CellModelIr::new(LibraryMeta { name, prefixes });
    ir.cells = cells;
    Conversion { ir, notes }
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
fn convert_cell(cell: &LibertyGroup) -> (CellModel, Vec<ConvertNote>) {
    let cell_type = cell.first_name().unwrap_or("unnamed").to_string();
    let (pins, input_pins) = read_pins(cell);
    let mut notes = Vec::new();

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
    // operand that is not a declared input pin (e.g. a DFF's internal `IQ`
    // state node) cannot be combinationally modelled here — surface and skip
    // that pin. If NO output compiles, the cell is Other/None.
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
                        cell: cell_type.clone(),
                        pin: pin.clone(),
                        operands: unknown.into_iter().cloned().collect(),
                    });
                    continue;
                }
                match function::compile(pin, &expr, &input_pins) {
                    Ok(logic) => per_output.push(logic),
                    Err(e) => notes.push(ConvertNote::FunctionParseError {
                        cell: cell_type.clone(),
                        pin: pin.clone(),
                        error: e,
                    }),
                }
            }
            Err(e) => notes.push(ConvertNote::FunctionParseError {
                cell: cell_type.clone(),
                pin: pin.clone(),
                error: e,
            }),
        }
    }

    if per_output.is_empty() {
        if output_fns.is_empty() {
            notes.push(ConvertNote::SkippedNoFunction {
                cell: cell_type.clone(),
            });
        }
        return (
            CellModel {
                cell_type,
                kind: CellKind::Other,
                pins,
                logic: None,
            },
            notes,
        );
    }

    let logic = merge_outputs(&input_pins, per_output);
    (
        CellModel {
            cell_type,
            kind: CellKind::Comb,
            pins,
            logic: Some(logic),
        },
        notes,
    )
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
        assert_eq!(c.kind, CellKind::Comb);
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
        assert_eq!(c.kind, CellKind::Comb);
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
    fn sequential_output_referencing_internal_node_is_other() {
        // A DFF-like cell whose Q function references an internal node IQ
        // (not a declared input pin) — must NOT be Comb.
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
        assert_eq!(c.kind, CellKind::Other);
        assert!(c.logic.is_none());
        // Q's `function : "IQ"` references an internal state node, not an
        // input pin — a sequential cell, expected to be flagged (not an error).
        assert!(conv
            .notes
            .iter()
            .any(|n| matches!(n, ConvertNote::SequentialOutput { .. })));
    }

    #[test]
    fn cell_with_no_function_is_skipped_as_other() {
        let src = r#"
        library(demo) {
          cell(demo__fill) {
            pin(VGND) { direction : input ; }
          }
        }
        "#;
        let conv = convert_library(&parse_lib(src), vec![]);
        let c = &conv.ir.cells[0];
        assert_eq!(c.kind, CellKind::Other);
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
                kind: CellKind::Comb,
                pins: vec![],
                logic: None,
            },
            CellModel {
                cell_type: "gf180mcu_fd_sc_mcu7t5v0__nand2_1".into(),
                kind: CellKind::Comb,
                pins: vec![],
                logic: None,
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
