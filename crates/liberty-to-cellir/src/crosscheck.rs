// SPDX-License-Identifier: Apache-2.0

//! Module C: the D6 cross-check.
//!
//! For a combinational cell that has BOTH a Liberty-derived [`CombLogic`]
//! AND a discoverable `functional.v` model, evaluate both over every input
//! assignment and surface any disagreement. This is the eval-based check
//! ADR 0019 D6 calls for — it reuses `cell_decomp`'s `.v` evaluator as the
//! oracle rather than rebuilding an AIG from the `.v`.
//!
//! Wide cells (input count above [`MAX_EXHAUSTIVE_INPUTS`]) are skipped from
//! exhaustive enumeration and reported as capped — never silently passed.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use cell_decomp::{
    eval_behavioral_model, parse_functional_model, parse_udp, BehavioralModel, UdpModel,
};
use cell_model_ir::{CellKind, CellModel};

/// Above this input count we do not enumerate all `2^n` assignments.
pub const MAX_EXHAUSTIVE_INPUTS: usize = 16;

/// A single disagreement between the Liberty AIG and the `.v` model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mismatch {
    pub cell: String,
    pub pin: String,
    /// Input pin -> value vector that disagrees.
    pub inputs: Vec<(String, bool)>,
    pub liberty: bool,
    pub functional_v: bool,
}

/// Outcome of cross-checking one cell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CellCheck {
    /// No `functional.v` model found for this cell — nothing to check.
    NoModel,
    /// Cell is not combinational (no Liberty logic) — skipped.
    NotComb,
    /// Too many inputs to enumerate exhaustively; skipped.
    Capped { cell: String, inputs: usize },
    /// The `.v` model uses a primitive the oracle cannot evaluate (e.g.
    /// `bufif0` tristate) — not comparable as pure 2-state logic. Skipped.
    UnevaluatableModel { cell: String, reason: String },
    /// Checked exhaustively; carries any mismatches found (empty == clean).
    Checked {
        cell: String,
        assignments: u64,
        mismatches: Vec<Mismatch>,
    },
}

/// Gate types the `cell_decomp` oracle ([`eval_behavioral_model`]) can
/// evaluate as pure 2-state logic. UDPs are handled separately (looked up in
/// the model's `udps` map). Anything else (tristate `bufif*`, `notif*`, …)
/// makes the model un-comparable here.
const EVALUATABLE_GATES: &[&str] = &[
    "and", "or", "nand", "nor", "not", "buf", "xor", "xnor",
];

/// An index of `.v` models discovered under a functional-v directory tree:
/// module name -> behavioural model, plus any UDP definitions found.
pub struct ModelIndex {
    pub models: HashMap<String, BehavioralModel>,
    pub udps: HashMap<String, UdpModel>,
}

impl ModelIndex {
    /// Recursively scan `dir` for `.v` files, parsing each as a behavioural
    /// model and/or a UDP. `*.functional.v` are preferred sources for cell
    /// models; UDPs are collected from anywhere (e.g. a sibling `models/`).
    pub fn scan(dir: &Path) -> std::io::Result<ModelIndex> {
        let mut models = HashMap::new();
        let mut udps = HashMap::new();
        let mut files = Vec::new();
        collect_v_files(dir, &mut files)?;
        for path in files {
            let Ok(src) = std::fs::read_to_string(&path) else {
                continue;
            };
            // A file is either a cell module or a UDP primitive. Try both.
            if let Some(udp) = parse_udp(&src) {
                udps.insert(udp.name.clone(), udp);
            }
            // Prefer the non-preprocessed `.functional.v` form for cell
            // models. `.pp.v` (preprocessed) and `.behavioral.v` are ignored
            // to avoid duplicate / timing-laden modules.
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if name.ends_with(".functional.v") && !name.ends_with(".pp.v") {
                if let Some(model) = parse_functional_model(&src) {
                    models.insert(model.module_name.clone(), model);
                }
            }
        }
        Ok(ModelIndex { models, udps })
    }
}

fn collect_v_files(dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    if !dir.is_dir() {
        return Ok(());
    }
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_v_files(&path, out)?;
        } else if path.extension().and_then(|e| e.to_str()) == Some("v") {
            out.push(path);
        }
    }
    Ok(())
}

/// Cross-check one cell against the model index.
pub fn check_cell(cell: &CellModel, index: &ModelIndex) -> CellCheck {
    let Some(logic) = &cell.logic else {
        return CellCheck::NotComb;
    };
    if cell.kind != CellKind::Comb {
        return CellCheck::NotComb;
    }
    let Some(model) = index.models.get(&cell.cell_type) else {
        return CellCheck::NoModel;
    };

    // Guard against models the oracle cannot evaluate as 2-state logic
    // (tristate gates, or UDPs not present in the index). These would make
    // `eval_behavioral_model` panic.
    for gate in &model.gates {
        let gt = gate.gate_type.as_str();
        if !EVALUATABLE_GATES.contains(&gt) && !index.udps.contains_key(gt) {
            return CellCheck::UnevaluatableModel {
                cell: cell.cell_type.clone(),
                reason: format!("model uses non-2-state primitive '{gt}'"),
            };
        }
    }

    let inputs = &logic.inputs;
    let n = inputs.len();
    if n > MAX_EXHAUSTIVE_INPUTS {
        return CellCheck::Capped {
            cell: cell.cell_type.clone(),
            inputs: n,
        };
    }

    let mut mismatches = Vec::new();
    let total = 1u64 << n;
    for mask in 0u64..total {
        let mut vals: HashMap<String, bool> = HashMap::new();
        for (i, pin) in inputs.iter().enumerate() {
            vals.insert(pin.clone(), (mask >> i) & 1 == 1);
        }
        let lib_out = logic.eval(&vals).expect("liberty eval");
        for out in &logic.outputs {
            let lib_v = lib_out[&out.pin];
            // The `.v` evaluator panics on an undriven wire or an output pin
            // the model doesn't declare. Catch that and treat the model as
            // un-evaluatable rather than crashing the whole run.
            let model = &model;
            let vals_ref = &vals;
            let pin = out.pin.clone();
            let udps = &index.udps;
            let fv = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                eval_behavioral_model(model, vals_ref, &pin, udps)
            })) {
                Ok(v) => v,
                Err(_) => {
                    return CellCheck::UnevaluatableModel {
                        cell: cell.cell_type.clone(),
                        reason: format!(
                            "model eval panicked on pin '{}' (undriven wire / missing output)",
                            out.pin
                        ),
                    };
                }
            };
            if lib_v != fv {
                mismatches.push(Mismatch {
                    cell: cell.cell_type.clone(),
                    pin: out.pin.clone(),
                    inputs: inputs
                        .iter()
                        .map(|p| (p.clone(), vals[p]))
                        .collect(),
                    liberty: lib_v,
                    functional_v: fv,
                });
            }
        }
    }

    CellCheck::Checked {
        cell: cell.cell_type.clone(),
        assignments: total,
        mismatches,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cell_model_ir::{AndNode, CombLogic, Direction, OutputPin, Pin, Ref};

    fn and2_model(name: &str) -> BehavioralModel {
        // module name( A1, A2, Z ); and u(Z, A1, A2);
        let src = format!(
            "module {name}( A1, A2, Z );\ninput A1, A2;\noutput Z;\n\tand u( Z, A1, A2 );\nendmodule\n"
        );
        parse_functional_model(&src).unwrap()
    }

    fn and2_cell(name: &str, swap_logic: bool) -> CellModel {
        // Liberty AIG for Z = A1 & A2 (or, if swap_logic, Z = A1 | A2 to
        // force a mismatch).
        let logic = if swap_logic {
            // OR via De Morgan: node3 = !A1 & !A2; Z = !node3.
            CombLogic {
                inputs: vec!["A1".into(), "A2".into()],
                and_nodes: vec![AndNode {
                    a: Ref::inv(1),
                    b: Ref::inv(2),
                }],
                outputs: vec![OutputPin {
                    pin: "Z".into(),
                    r: Ref::inv(3),
                }],
            }
        } else {
            CombLogic {
                inputs: vec!["A1".into(), "A2".into()],
                and_nodes: vec![AndNode {
                    a: Ref::node(1),
                    b: Ref::node(2),
                }],
                outputs: vec![OutputPin {
                    pin: "Z".into(),
                    r: Ref::node(3),
                }],
            }
        };
        CellModel {
            cell_type: name.into(),
            kind: CellKind::Comb,
            pins: vec![
                Pin { name: "A1".into(), direction: Direction::Input },
                Pin { name: "A2".into(), direction: Direction::Input },
                Pin { name: "Z".into(), direction: Direction::Output },
            ],
            logic: Some(logic),
            sequential: None,
            timing: None,
        }
    }

    fn index_with(name: &str) -> ModelIndex {
        let mut models = HashMap::new();
        models.insert(name.to_string(), and2_model(name));
        ModelIndex { models, udps: HashMap::new() }
    }

    #[test]
    fn agreeing_cell_is_clean() {
        let cell = and2_cell("demo__and2", false);
        let index = index_with("demo__and2");
        match check_cell(&cell, &index) {
            CellCheck::Checked { mismatches, assignments, .. } => {
                assert_eq!(assignments, 4);
                assert!(mismatches.is_empty(), "expected clean, got {mismatches:?}");
            }
            other => panic!("expected Checked, got {other:?}"),
        }
    }

    #[test]
    fn disagreeing_cell_surfaces_mismatches() {
        // Liberty says OR, .v says AND -> they differ on (1,0) and (0,1).
        let cell = and2_cell("demo__and2", true);
        let index = index_with("demo__and2");
        match check_cell(&cell, &index) {
            CellCheck::Checked { mismatches, .. } => {
                assert_eq!(mismatches.len(), 2);
                for m in &mismatches {
                    assert_eq!(m.pin, "Z");
                    // OR result (liberty) true where AND (.v) is false.
                    assert!(m.liberty && !m.functional_v);
                }
            }
            other => panic!("expected Checked, got {other:?}"),
        }
    }

    #[test]
    fn missing_model_reports_no_model() {
        let cell = and2_cell("demo__and2", false);
        let empty = ModelIndex { models: HashMap::new(), udps: HashMap::new() };
        assert_eq!(check_cell(&cell, &empty), CellCheck::NoModel);
    }

    #[test]
    fn non_comb_cell_skipped() {
        let cell = CellModel {
            cell_type: "demo__dff".into(),
            kind: CellKind::Other,
            pins: vec![],
            logic: None,
            sequential: None,
            timing: None,
        };
        let index = index_with("demo__dff");
        assert_eq!(check_cell(&cell, &index), CellCheck::NotComb);
    }

    #[test]
    fn tristate_model_is_unevaluatable_not_panic() {
        // A model using a tristate primitive must be skipped, not crash.
        let src = "module demo__bufz( A, EN, Z );\ninput A, EN;\noutput Z;\n\tbufif0 u(Z, A, EN);\nendmodule\n";
        let model = parse_functional_model(src).unwrap();
        let mut models = HashMap::new();
        models.insert("demo__bufz".to_string(), model);
        let index = ModelIndex { models, udps: HashMap::new() };
        let cell = and2_cell("demo__bufz", false); // any comb logic with a model present
        match check_cell(&cell, &index) {
            CellCheck::UnevaluatableModel { cell, .. } => assert_eq!(cell, "demo__bufz"),
            other => panic!("expected UnevaluatableModel, got {other:?}"),
        }
    }

    #[test]
    fn wide_cell_is_capped() {
        let n = MAX_EXHAUSTIVE_INPUTS + 1;
        let inputs: Vec<String> = (0..n).map(|i| format!("I{i}")).collect();
        let logic = CombLogic {
            inputs: inputs.clone(),
            and_nodes: vec![],
            outputs: vec![OutputPin { pin: "Y".into(), r: Ref::node(1) }],
        };
        let cell = CellModel {
            cell_type: "demo__wide".into(),
            kind: CellKind::Comb,
            pins: vec![],
            logic: Some(logic),
            sequential: None,
            timing: None,
        };
        // Provide a dummy model so we don't short-circuit on NoModel.
        let mut models = HashMap::new();
        let src_inputs = inputs.join(", ");
        let src = format!("module demo__wide( {src_inputs}, Y );\noutput Y;\n\tbuf u(Y, I0);\nendmodule\n");
        models.insert("demo__wide".to_string(), parse_functional_model(&src).unwrap());
        let index = ModelIndex { models, udps: HashMap::new() };
        assert_eq!(
            check_cell(&cell, &index),
            CellCheck::Capped { cell: "demo__wide".into(), inputs: n }
        );
    }
}
