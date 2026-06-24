//! Structural diff between two cell-model-IR descriptors.
//!
//! This is the [ADR 0019 D7](../../../docs/adr/0019-cell-model-ir.md) provenance
//! check in tool form: descriptors are regenerated in CI rather than committed,
//! so the gate is that regeneration is **deterministic** — a freshly generated
//! descriptor must structurally equal the previous one. It also helps debug
//! differences between descriptors produced by different tool versions.
//!
//! The diff is **structural**, not logical: two AIGs that compute the same
//! function but differ in node structure are reported as different. That is the
//! right semantics for a determinism check (we want bit-stable regeneration);
//! logical equivalence is a separate concern handled by the converter's
//! Liberty-vs-`.v` cross-check (D6) via [`crate::CombLogic::eval`].

use std::collections::BTreeSet;

use crate::CellModelIr;

/// The result of diffing two descriptors. Empty `mismatches` ⇒ identical.
#[derive(Debug, Default, Clone)]
pub struct Diff {
    /// Human-readable mismatch lines, in deterministic (sorted) order.
    pub mismatches: Vec<String>,
}

impl Diff {
    /// `true` if the two descriptors are structurally identical.
    pub fn is_clean(&self) -> bool {
        self.mismatches.is_empty()
    }
}

/// Diff `a` (expected / golden) against `b` (actual / regenerated).
pub fn diff_irs(a: &CellModelIr, b: &CellModelIr) -> Diff {
    let mut d = Diff::default();

    if (a.schema_major, a.schema_minor) != (b.schema_major, b.schema_minor) {
        d.mismatches.push(format!(
            "schema version differs: {}.{} vs {}.{}",
            a.schema_major, a.schema_minor, b.schema_major, b.schema_minor
        ));
    }
    if a.library != b.library {
        d.mismatches.push(format!(
            "library metadata differs: {:?} vs {:?}",
            a.library, b.library
        ));
    }

    // Cell-set comparison by name (order-independent, deterministic).
    let a_names: BTreeSet<&str> = a.cells.iter().map(|c| c.cell_type.as_str()).collect();
    let b_names: BTreeSet<&str> = b.cells.iter().map(|c| c.cell_type.as_str()).collect();
    for only_a in a_names.difference(&b_names) {
        d.mismatches.push(format!("cell only in A: {only_a}"));
    }
    for only_b in b_names.difference(&a_names) {
        d.mismatches.push(format!("cell only in B: {only_b}"));
    }

    // Per-cell structural comparison for the cells present in both, in sorted
    // name order so the report is deterministic.
    for name in a_names.intersection(&b_names) {
        let ca = a.cell(name).expect("name came from A");
        let cb = b.cell(name).expect("name came from B");
        if ca != cb {
            // Pinpoint the sub-field that differs for a useful message.
            if ca.kind != cb.kind {
                d.mismatches
                    .push(format!("cell {name}: kind differs ({:?} vs {:?})", ca.kind, cb.kind));
            }
            if ca.pins != cb.pins {
                d.mismatches.push(format!("cell {name}: pins differ"));
            }
            if ca.logic != cb.logic {
                d.mismatches.push(format!("cell {name}: combinational AIG differs"));
            }
        }
    }

    Diff { mismatches: d.mismatches }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CellKind, CellModel, CombLogic, Direction, LibraryMeta, OutputPin, Pin, Ref};

    fn lib() -> CellModelIr {
        let mut ir = CellModelIr::new(LibraryMeta { name: "l".into(), prefixes: vec!["l_".into()] });
        ir.cells.push(CellModel {
            cell_type: "l_inv".into(),
            kind: CellKind::Comb,
            pins: vec![
                Pin { name: "A".into(), direction: Direction::Input },
                Pin { name: "Y".into(), direction: Direction::Output },
            ],
            logic: Some(CombLogic {
                inputs: vec!["A".into()],
                and_nodes: vec![],
                outputs: vec![OutputPin { pin: "Y".into(), r: Ref::inv(1) }],
            }),
        });
        ir
    }

    #[test]
    fn identical_is_clean() {
        assert!(diff_irs(&lib(), &lib()).is_clean());
    }

    #[test]
    fn added_cell_is_reported() {
        let a = lib();
        let mut b = lib();
        b.cells.push(CellModel {
            cell_type: "l_buf".into(),
            kind: CellKind::Comb,
            pins: vec![],
            logic: None,
        });
        let d = diff_irs(&a, &b);
        assert!(!d.is_clean());
        assert!(d.mismatches.iter().any(|m| m.contains("only in B: l_buf")));
    }

    #[test]
    fn changed_logic_is_reported() {
        let a = lib();
        let mut b = lib();
        // Flip the inverter's output to a buffer: structurally different AIG.
        b.cells[0].logic.as_mut().unwrap().outputs[0].r = Ref::node(1);
        let d = diff_irs(&a, &b);
        assert!(d.mismatches.iter().any(|m| m.contains("combinational AIG differs")));
    }
}
