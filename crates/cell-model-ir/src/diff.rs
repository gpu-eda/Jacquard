//! Structural diff between two cell-model-IR descriptors.
//!
//! This is the [Decision 0019 D7](../../../docs/architecture/decisions/0019-cell-model-ir.md) provenance
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
    // Corner set (L4, D5): order matters — a TimingValue's `corner_index` is an
    // index into this list, so a reordering is a real change.
    if a.corners != b.corners {
        d.mismatches.push(format!(
            "corner set differs: {:?} vs {:?}",
            a.corners.iter().map(|c| &c.name).collect::<Vec<_>>(),
            b.corners.iter().map(|c| &c.name).collect::<Vec<_>>(),
        ));
    }
    if a.default_corner != b.default_corner {
        d.mismatches.push(format!(
            "default corner differs: {:?} vs {:?}",
            a.default_corner, b.default_corner
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
                d.mismatches.push(format!(
                    "cell {name}: kind differs ({:?} vs {:?})",
                    ca.kind, cb.kind
                ));
            }
            if ca.pins != cb.pins {
                d.mismatches.push(format!("cell {name}: pins differ"));
            }
            if ca.logic != cb.logic {
                d.mismatches
                    .push(format!("cell {name}: combinational AIG differs"));
            }
            if ca.sequential != cb.sequential {
                d.mismatches
                    .push(format!("cell {name}: sequential (L3) block differs"));
            }
            if ca.timing != cb.timing {
                d.mismatches
                    .push(format!("cell {name}: timing (L4) block differs"));
            }
        }
    }

    Diff {
        mismatches: d.mismatches,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ActiveLevel, AsyncControl, CellKind, CellModel, CellTiming, ClockEdge, ClockPin, CombLogic,
        ConstraintArc, ConstraintKind, Corner, Direction, LibraryMeta, OutputPin, Pin, Ref,
        SeqOutput, Sequential, TimingValue,
    };

    fn lib() -> CellModelIr {
        let mut ir = CellModelIr::new(LibraryMeta {
            name: "l".into(),
            prefixes: vec!["l_".into()],
        });
        ir.cells.push(CellModel {
            cell_type: "l_inv".into(),
            kind: CellKind::Comb,
            pins: vec![
                Pin {
                    name: "A".into(),
                    direction: Direction::Input,
                },
                Pin {
                    name: "Y".into(),
                    direction: Direction::Output,
                },
            ],
            logic: Some(CombLogic {
                inputs: vec!["A".into()],
                and_nodes: vec![],
                outputs: vec![OutputPin {
                    pin: "Y".into(),
                    r: Ref::inv(1),
                }],
            }),
            sequential: None,
            timing: None,
        });
        ir
    }

    /// A minimal sequential cell with an async reset and a one-corner L4 block,
    /// for exercising the L3/L4 diff paths.
    fn seq_lib() -> CellModelIr {
        let mut ir = CellModelIr::new(LibraryMeta {
            name: "l".into(),
            prefixes: vec!["l_".into()],
        });
        ir.corners = vec![Corner {
            name: "tt".into(),
            process: "tt".into(),
            voltage: 1.8,
            temperature: 25.0,
        }];
        ir.default_corner = "tt".into();
        ir.cells.push(CellModel {
            cell_type: "l_dffrnq".into(),
            kind: CellKind::Dff,
            pins: vec![Pin {
                name: "Q".into(),
                direction: Direction::Output,
            }],
            logic: None,
            sequential: Some(Sequential {
                clock: Some(ClockPin {
                    pin: "CLK".into(),
                    edge: ClockEdge::Rising,
                }),
                enable: None,
                next_state: CombLogic {
                    inputs: vec!["D".into()],
                    and_nodes: vec![],
                    outputs: vec![OutputPin {
                        pin: "D".into(),
                        r: Ref::node(1),
                    }],
                },
                outputs: vec![SeqOutput {
                    pin: "Q".into(),
                    inverted: false,
                }],
                async_set: None,
                async_reset: Some(AsyncControl {
                    pin: "RN".into(),
                    active: ActiveLevel::Low,
                }),
                clear_preset: None,
            }),
            timing: Some(CellTiming {
                delays: vec![],
                constraints: vec![ConstraintArc {
                    data_pin: "D".into(),
                    related_pin: "CLK".into(),
                    kind: ConstraintKind::Setup,
                    edge: ClockEdge::Rising,
                    rise: vec![TimingValue {
                        corner_index: 0,
                        min: 9.0,
                        typ: 10.0,
                        max: 11.0,
                    }],
                    fall: vec![],
                }],
                sram: None,
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
            sequential: None,
            timing: None,
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
        assert!(d
            .mismatches
            .iter()
            .any(|m| m.contains("combinational AIG differs")));
    }

    #[test]
    fn seq_identical_is_clean() {
        assert!(diff_irs(&seq_lib(), &seq_lib()).is_clean());
    }

    #[test]
    fn changed_sequential_role_is_reported() {
        let a = seq_lib();
        let mut b = seq_lib();
        // Flip the async-reset polarity: an L3 change.
        b.cells[0]
            .sequential
            .as_mut()
            .unwrap()
            .async_reset
            .as_mut()
            .unwrap()
            .active = ActiveLevel::High;
        let d = diff_irs(&a, &b);
        assert!(d
            .mismatches
            .iter()
            .any(|m| m.contains("sequential (L3) block differs")));
    }

    #[test]
    fn changed_clear_preset_tie_break_is_reported() {
        // Adding a set-dominant clear_preset where there was none is an L3
        // change the diff must surface (the field lives inside the seq block).
        let a = seq_lib();
        let mut b = seq_lib();
        b.cells[0].sequential.as_mut().unwrap().clear_preset = Some(crate::ClearPreset {
            var1: crate::ClearPresetVal::High,
            var2: crate::ClearPresetVal::Low,
        });
        let d = diff_irs(&a, &b);
        assert!(d
            .mismatches
            .iter()
            .any(|m| m.contains("sequential (L3) block differs")));
    }

    #[test]
    fn changed_timing_value_is_reported() {
        let a = seq_lib();
        let mut b = seq_lib();
        // Nudge a setup constraint: an L4 change.
        b.cells[0].timing.as_mut().unwrap().constraints[0].rise[0].typ = 99.0;
        let d = diff_irs(&a, &b);
        assert!(d
            .mismatches
            .iter()
            .any(|m| m.contains("timing (L4) block differs")));
    }

    #[test]
    fn changed_corner_set_is_reported() {
        let a = seq_lib();
        let mut b = seq_lib();
        b.corners[0].name = "ss".into();
        b.default_corner = "ss".into();
        let d = diff_irs(&a, &b);
        assert!(d
            .mismatches
            .iter()
            .any(|m| m.contains("corner set differs")));
        assert!(d
            .mismatches
            .iter()
            .any(|m| m.contains("default corner differs")));
    }
}
