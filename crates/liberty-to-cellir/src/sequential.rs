// SPDX-License-Identifier: Apache-2.0

//! L3 — sequential pin-roles + classification (ADR 0019 D4).
//!
//! Reads a cell's Liberty `ff` / `latch` group (and the `clock_gate_*` pin
//! attributes) and projects it onto [`cell_model_ir::Sequential`] plus a
//! [`cell_model_ir::CellKind`]. Liberty is the authoritative source for L3 —
//! `clocked_on` / `next_state` / `data_in` / `clear` / `preset` state the
//! sequential semantics directly, whereas the behavioural `.v` buries them in
//! a vendor UDP.
//!
//! The synchronous next-state function — including any scan mux
//! (`next_state : "((D&!SE)|(SE&SI))"`) — is compiled to the same pre-built
//! AIG as L2 (D3) via [`crate::function`]: it is just a boolean function, so
//! it needs no special handling beyond the boolean compiler. Only the
//! genuinely-special roles (clock edge, level enable, async set/reset
//! polarity, state outputs) get named fields.
//!
//! ## The `clear_preset_var` precedence check (C2.1 flagged risk)
//!
//! Jacquard's `wire_dff_reset_set_overlay` bakes a FIXED reset-dominant
//! tie-break: `d_in = AND(OR(D, !SETN), RN)` — when both async controls are
//! asserted the state goes to 0 (clear wins). The schema cannot express a
//! per-cell override. So for every cell with BOTH `clear` and `preset` this
//! module reads Liberty's `clear_preset_var1` / `clear_preset_var2` and
//! verifies it matches reset-dominant (`var1 = L`, the Q state var goes low
//! when both are asserted). Any divergence is surfaced as a
//! [`SeqNote::ClearPresetNotResetDominant`] generation-time diagnostic rather
//! than silently emitted — that is the signal the schema needs a per-cell
//! `clear_preset` field.

use cell_model_ir::{
    ActiveLevel, CellKind, ClockEdge, ClockPin, CombLogic, EnablePin, SeqOutput, Sequential,
};
use liberty_parse::LibertyGroup;

use crate::function;

/// A diagnostic emitted while extracting L3 sequential roles.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SeqNote {
    /// A cell with BOTH `clear` and `preset` whose `clear_preset_var1/var2`
    /// is NOT reset-dominant (`var1 = L`, `var2 = H`). Jacquard's overlay is
    /// hardcoded reset-dominant, so this cell would be mis-simulated — the
    /// schema needs a per-cell `clear_preset` tie-break field. See the module
    /// docs.
    ClearPresetNotResetDominant {
        cell: String,
        var1: String,
        var2: String,
    },
    /// A `clocked_on` / `clear` / `preset` / `enable` expression that is not
    /// the simple `pin` / `!pin` form this extractor understands. Surfaced
    /// rather than guessed.
    UnparsedControl {
        cell: String,
        role: String,
        expr: String,
    },
    /// A sequential cell whose `next_state` / `data_in` function failed to
    /// compile to an AIG.
    NextStateCompileError {
        cell: String,
        expr: String,
        error: String,
    },
}

/// The L3 result for one cell: its classification, optional sequential
/// metadata, and any diagnostics.
pub struct SeqResult {
    pub kind: CellKind,
    pub sequential: Option<Sequential>,
    pub notes: Vec<SeqNote>,
}

/// Whether a cell is sequential / clock-gating from its Liberty groups+pins.
pub fn has_ff(cell: &LibertyGroup) -> bool {
    cell.group_of_type("ff").is_some()
}

pub fn has_latch(cell: &LibertyGroup) -> bool {
    cell.group_of_type("latch").is_some()
}

/// True if any pin carries a `clock_gate_*` attribute (integrated clock gate).
pub fn is_clock_gate(cell: &LibertyGroup) -> bool {
    for pin in cell.groups_of_type("pin") {
        for attr in &[
            "clock_gate_clock_pin",
            "clock_gate_enable_pin",
            "clock_gate_out_pin",
        ] {
            if pin
                .attr(attr)
                .and_then(|a| a.first_string())
                .map(|v| v.trim_matches('"') == "true")
                .unwrap_or(false)
            {
                return true;
            }
        }
    }
    false
}

/// Parse a Liberty control expression of the form `pin`, `!pin`, `(pin)`, or
/// `(!pin)` into `(pin_name, asserted_when_low)`. Returns `None` for any more
/// complex expression (the caller surfaces an [`SeqNote::UnparsedControl`]).
fn parse_pin_polarity(expr_str: &str) -> Option<(String, bool)> {
    use crate::function::Expr;
    let expr = function::parse(expr_str).ok()?;
    match expr {
        Expr::Pin(p) => Some((p, false)),
        Expr::Not(inner) => match *inner {
            Expr::Pin(p) => Some((p, true)),
            _ => None,
        },
        _ => None,
    }
}

/// Build the L3 result for one cell.
///
/// `input_pins` is the cell's declared input-pin ordering (shared with L2).
/// `has_logic` indicates a combinational output cone was compiled (used to
/// distinguish `std` from a physical cell).
pub fn build(
    cell: &LibertyGroup,
    cell_type: &str,
    input_pins: &[String],
    has_logic: bool,
) -> SeqResult {
    let mut notes = Vec::new();

    if let Some(ff) = cell.group_of_type("ff") {
        let (kind, seq) = build_ff(cell, ff, cell_type, input_pins, &mut notes);
        return SeqResult {
            kind,
            sequential: seq,
            notes,
        };
    }
    if let Some(latch) = cell.group_of_type("latch") {
        let (kind, seq) = build_latch(cell, latch, cell_type, input_pins, &mut notes);
        return SeqResult {
            kind,
            sequential: seq,
            notes,
        };
    }
    if is_clock_gate(cell) {
        // Integrated clock gates are modelled by a `statetable` in GF180, not
        // an `ff`/`latch`, so there is no next-state function to fold here.
        // Classify the kind; the consumer keeps its existing clock-gate path
        // (out of C2.2 scope). L4 timing is still emitted.
        return SeqResult {
            kind: CellKind::ClockGate,
            sequential: None,
            notes,
        };
    }

    // Not sequential. Classify the kind from logic presence / name / function.
    let kind = classify_non_sequential(cell, cell_type, input_pins, has_logic);
    SeqResult {
        kind,
        sequential: None,
        notes,
    }
}

/// Build a flip-flop's [`Sequential`] from its `ff` group.
fn build_ff(
    cell: &LibertyGroup,
    ff: &LibertyGroup,
    cell_type: &str,
    input_pins: &[String],
    notes: &mut Vec<SeqNote>,
) -> (CellKind, Option<Sequential>) {
    // ff(IQ, IQN) { clocked_on; next_state; clear; preset; clear_preset_var* }
    let q_var = ff.names.first().cloned().unwrap_or_default();
    let qn_var = ff.names.get(1).cloned().unwrap_or_default();

    let clock = ff
        .attr("clocked_on")
        .and_then(|a| a.first_string())
        .and_then(|s| parse_clock(s, cell_type, notes));

    let next_state = build_next_state(
        ff.attr("next_state").and_then(|a| a.first_string()),
        cell_type,
        input_pins,
        notes,
    );

    let async_reset = build_async(ff.attr("clear"), "clear", cell_type, notes);
    let async_set = build_async(ff.attr("preset"), "preset", cell_type, notes);

    // clear_preset_var precedence check when BOTH controls exist.
    if async_reset.is_some() && async_set.is_some() {
        check_clear_preset(ff, cell_type, notes);
    }

    let outputs = state_outputs(cell, &q_var, &qn_var);

    let seq = Sequential {
        clock,
        enable: None,
        next_state,
        outputs,
        async_set,
        async_reset,
    };
    (CellKind::Dff, Some(seq))
}

/// Build a latch's [`Sequential`] from its `latch` group.
fn build_latch(
    cell: &LibertyGroup,
    latch: &LibertyGroup,
    cell_type: &str,
    input_pins: &[String],
    notes: &mut Vec<SeqNote>,
) -> (CellKind, Option<Sequential>) {
    let q_var = latch.names.first().cloned().unwrap_or_default();
    let qn_var = latch.names.get(1).cloned().unwrap_or_default();

    // Level-sensitive enable (transparency control).
    let enable = latch
        .attr("enable")
        .and_then(|a| a.first_string())
        .and_then(parse_pin_polarity)
        .map(|(pin, low)| EnablePin {
            pin,
            active: if low {
                ActiveLevel::Low
            } else {
                ActiveLevel::High
            },
        });

    let next_state = build_next_state(
        latch.attr("data_in").and_then(|a| a.first_string()),
        cell_type,
        input_pins,
        notes,
    );

    let async_reset = build_async(latch.attr("clear"), "clear", cell_type, notes);
    let async_set = build_async(latch.attr("preset"), "preset", cell_type, notes);
    if async_reset.is_some() && async_set.is_some() {
        check_clear_preset(latch, cell_type, notes);
    }

    let outputs = state_outputs(cell, &q_var, &qn_var);

    let seq = Sequential {
        clock: None,
        enable,
        next_state,
        outputs,
        async_set,
        async_reset,
    };
    (CellKind::Latch, Some(seq))
}

/// Parse `clocked_on` into a clock pin + edge. `pin` ⇒ rising, `!pin` ⇒
/// falling (negedge GF180 `CLKN` cells).
fn parse_clock(expr: &str, cell_type: &str, notes: &mut Vec<SeqNote>) -> Option<ClockPin> {
    match parse_pin_polarity(expr) {
        Some((pin, false)) => Some(ClockPin {
            pin,
            edge: ClockEdge::Rising,
        }),
        Some((pin, true)) => Some(ClockPin {
            pin,
            edge: ClockEdge::Falling,
        }),
        None => {
            notes.push(SeqNote::UnparsedControl {
                cell: cell_type.to_string(),
                role: "clocked_on".to_string(),
                expr: expr.to_string(),
            });
            None
        }
    }
}

/// Build the next-state AIG from a Liberty function string, restricting the
/// AIG inputs to the pins the expression actually references (in cell
/// input-pin order).
fn build_next_state(
    expr_str: Option<&str>,
    cell_type: &str,
    input_pins: &[String],
    notes: &mut Vec<SeqNote>,
) -> CombLogic {
    let Some(src) = expr_str else {
        // No next_state — emit a constant-0 placeholder so the schema's
        // non-optional field is populated. (Should not happen for real flops.)
        return CombLogic {
            inputs: vec![],
            and_nodes: vec![],
            outputs: vec![cell_model_ir::OutputPin {
                pin: "next_state".to_string(),
                r: cell_model_ir::Ref::node(0),
            }],
        };
    };
    match function::parse(src) {
        Ok(expr) => {
            // Restrict inputs to referenced pins, preserving cell input order.
            let referenced = expr.pins();
            let inputs: Vec<String> = input_pins
                .iter()
                .filter(|p| referenced.iter().any(|r| r == *p))
                .cloned()
                .collect();
            match function::compile("next_state", &expr, &inputs) {
                Ok(logic) => logic,
                Err(error) => {
                    notes.push(SeqNote::NextStateCompileError {
                        cell: cell_type.to_string(),
                        expr: src.to_string(),
                        error,
                    });
                    fallback_next_state()
                }
            }
        }
        Err(error) => {
            notes.push(SeqNote::NextStateCompileError {
                cell: cell_type.to_string(),
                expr: src.to_string(),
                error,
            });
            fallback_next_state()
        }
    }
}

fn fallback_next_state() -> CombLogic {
    CombLogic {
        inputs: vec![],
        and_nodes: vec![],
        outputs: vec![cell_model_ir::OutputPin {
            pin: "next_state".to_string(),
            r: cell_model_ir::Ref::node(0),
        }],
    }
}

/// Parse a `clear` / `preset` attribute into an [`cell_model_ir::AsyncControl`].
fn build_async(
    attr: Option<&liberty_parse::Attribute>,
    role: &str,
    cell_type: &str,
    notes: &mut Vec<SeqNote>,
) -> Option<cell_model_ir::AsyncControl> {
    let s = attr?.first_string()?;
    match parse_pin_polarity(s) {
        Some((pin, low)) => Some(cell_model_ir::AsyncControl {
            pin,
            active: if low {
                ActiveLevel::Low
            } else {
                ActiveLevel::High
            },
        }),
        None => {
            notes.push(SeqNote::UnparsedControl {
                cell: cell_type.to_string(),
                role: role.to_string(),
                expr: s.to_string(),
            });
            None
        }
    }
}

/// Map output pins whose `function` references the state variables to
/// [`SeqOutput`]s. A pin driving the Q state var ⇒ non-inverted; driving the
/// QN var (or `!Q`) ⇒ inverted.
fn state_outputs(cell: &LibertyGroup, q_var: &str, qn_var: &str) -> Vec<SeqOutput> {
    let mut outputs = Vec::new();
    for pin in cell.groups_of_type("pin") {
        let Some(name) = pin.first_name() else {
            continue;
        };
        let is_output = pin
            .attr("direction")
            .and_then(|a| a.first_string())
            .map(|d| d.trim_matches('"') == "output")
            .unwrap_or(false);
        if !is_output {
            continue;
        }
        let Some(func) = pin.attr("function").and_then(|a| a.first_string()) else {
            continue;
        };
        let Ok(expr) = function::parse(func) else {
            continue;
        };
        let inverted = match &expr {
            crate::function::Expr::Pin(p) if p == q_var => false,
            crate::function::Expr::Pin(p) if p == qn_var => true,
            crate::function::Expr::Not(inner) => {
                matches!(inner.as_ref(), crate::function::Expr::Pin(p) if p == q_var)
            }
            _ => continue,
        };
        outputs.push(SeqOutput {
            pin: name.to_string(),
            inverted,
        });
    }
    outputs
}

/// Verify `clear_preset_var1/var2` is reset-dominant; surface a diagnostic if
/// not. Reset-dominant ⇒ when both clear and preset are asserted, the Q state
/// var goes to `L` (and QN to `H`).
fn check_clear_preset(group: &LibertyGroup, cell_type: &str, notes: &mut Vec<SeqNote>) {
    let var1 = group
        .attr("clear_preset_var1")
        .and_then(|a| a.first_string())
        .map(|s| s.trim_matches('"').to_uppercase())
        .unwrap_or_default();
    let var2 = group
        .attr("clear_preset_var2")
        .and_then(|a| a.first_string())
        .map(|s| s.trim_matches('"').to_uppercase())
        .unwrap_or_default();
    // Reset-dominant: Q (var1) → L, QN (var2) → H. If var1 is absent Liberty
    // leaves the tie-break unspecified, which is also not provably
    // reset-dominant — surface it.
    let reset_dominant = var1 == "L" && var2 == "H";
    if !reset_dominant {
        notes.push(SeqNote::ClearPresetNotResetDominant {
            cell: cell_type.to_string(),
            var1,
            var2,
        });
    }
}

/// Classify a non-sequential, non-clock-gate cell.
fn classify_non_sequential(
    cell: &LibertyGroup,
    cell_type: &str,
    input_pins: &[String],
    has_logic: bool,
) -> CellKind {
    // Constant tie cells: an output whose function is the literal `1`/`0`
    // with no data inputs.
    if input_pins.is_empty() {
        for pin in cell.groups_of_type("pin") {
            if let Some(func) = pin.attr("function").and_then(|a| a.first_string()) {
                match func.trim().trim_matches(['"', '(', ')'].as_ref()).trim() {
                    "1" => return CellKind::TieHigh,
                    "0" => return CellKind::TieLow,
                    _ => {}
                }
            }
        }
    }

    if has_logic {
        return CellKind::Std;
    }

    // Physical / fill / tap cells: classify by name.
    let name = cell_type.to_lowercase();
    let base = name.rsplit("__").next().unwrap_or(&name);
    if base.contains("endcap") {
        CellKind::Endcap
    } else if base.contains("tap") {
        CellKind::Tap
    } else if base.contains("fill") || base.contains("decap") || base.contains("dcap") {
        CellKind::Filler
    } else if base.contains("antenna") || base.contains("diode") {
        // Antenna diodes are physical-only; no dedicated kind, treat as filler.
        CellKind::Filler
    } else {
        // Unknown non-logic cell: the safest physical default.
        CellKind::Filler
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_cell(src: &str) -> LibertyGroup {
        let lib = liberty_parse::parse(src).expect("parse liberty");
        let cell = lib.groups_of_type("cell").next().expect("a cell").clone();
        cell
    }

    fn input_pins(cell: &LibertyGroup) -> Vec<String> {
        cell.groups_of_type("pin")
            .filter(|p| {
                p.attr("direction")
                    .and_then(|a| a.first_string())
                    .map(|d| d == "input")
                    .unwrap_or(false)
            })
            .filter_map(|p| p.first_name().map(String::from))
            .collect()
    }

    // A GF180-shaped rising-edge DFF with active-low async reset+set.
    const DFFRSNQ: &str = r#"
    library(demo) {
      cell(demo__dffrsnq) {
        ff(IQ1, IQN1) {
          clocked_on : "CLK" ;
          next_state : "D" ;
          clear : "(!RN)" ;
          preset : "(!SETN)" ;
          clear_preset_var1 : L ;
          clear_preset_var2 : H ;
        }
        pin(CLK) { direction : input ; clock : true ; }
        pin(D)   { direction : input ; }
        pin(RN)  { direction : input ; }
        pin(SETN){ direction : input ; }
        pin(Q)   { direction : output ; function : "IQ1" ; }
      }
    }
    "#;

    #[test]
    fn dff_roles_extracted() {
        let cell = parse_cell(DFFRSNQ);
        let ins = input_pins(&cell);
        let r = build(&cell, "demo__dffrsnq", &ins, false);
        assert_eq!(r.kind, CellKind::Dff);
        let seq = r.sequential.unwrap();
        let clk = seq.clock.unwrap();
        assert_eq!(clk.pin, "CLK");
        assert_eq!(clk.edge, ClockEdge::Rising);
        let reset = seq.async_reset.unwrap();
        assert_eq!(reset.pin, "RN");
        assert_eq!(reset.active, ActiveLevel::Low);
        let set = seq.async_set.unwrap();
        assert_eq!(set.pin, "SETN");
        assert_eq!(set.active, ActiveLevel::Low);
        assert_eq!(
            seq.outputs,
            vec![SeqOutput {
                pin: "Q".into(),
                inverted: false
            }]
        );
        // next_state is just D.
        assert_eq!(seq.next_state.inputs, vec!["D".to_string()]);
        // reset-dominant ⇒ no diagnostic.
        assert!(r.notes.is_empty(), "unexpected notes: {:?}", r.notes);
    }

    #[test]
    fn negedge_clock_detected() {
        let src = r#"
        library(demo) {
          cell(demo__dffnrnq) {
            ff(IQ1, IQN1) {
              clocked_on : "(!CLKN)" ;
              next_state : "D" ;
              clear : "(!RN)" ;
            }
            pin(CLKN){ direction : input ; clock : true ; }
            pin(D)   { direction : input ; }
            pin(RN)  { direction : input ; }
            pin(Q)   { direction : output ; function : "IQ1" ; }
          }
        }
        "#;
        let cell = parse_cell(src);
        let ins = input_pins(&cell);
        let r = build(&cell, "demo__dffnrnq", &ins, false);
        let clk = r.sequential.unwrap().clock.unwrap();
        assert_eq!(clk.pin, "CLKN");
        assert_eq!(clk.edge, ClockEdge::Falling);
    }

    #[test]
    fn scan_mux_folded_into_next_state() {
        let src = r#"
        library(demo) {
          cell(demo__sdffrnq) {
            ff(IQ1, IQN1) {
              clocked_on : "CLK" ;
              next_state : "((D&(!SE))|(SE&SI))" ;
              clear : "(!RN)" ;
            }
            pin(CLK){ direction : input ; clock : true ; }
            pin(D)  { direction : input ; }
            pin(SE) { direction : input ; }
            pin(SI) { direction : input ; }
            pin(RN) { direction : input ; }
            pin(Q)  { direction : output ; function : "IQ1" ; }
          }
        }
        "#;
        let cell = parse_cell(src);
        let ins = input_pins(&cell);
        let r = build(&cell, "demo__sdffrnq", &ins, false);
        let seq = r.sequential.unwrap();
        let ns = &seq.next_state;
        assert_eq!(
            ns.inputs,
            vec!["D".to_string(), "SE".to_string(), "SI".to_string()]
        );
        // Verify the folded mux truth table: D when !SE, SI when SE.
        use std::collections::HashMap;
        for (d, se, si) in [
            (false, false, false),
            (true, false, false),
            (false, true, true),
            (true, true, false),
            (false, false, true),
            (true, true, true),
        ] {
            let m = HashMap::from([
                ("D".to_string(), d),
                ("SE".to_string(), se),
                ("SI".to_string(), si),
            ]);
            let out = ns.eval(&m).unwrap();
            let expected = if se { si } else { d };
            assert_eq!(out["next_state"], expected, "mux D={d} SE={se} SI={si}");
        }
    }

    #[test]
    fn latch_roles_extracted() {
        let src = r#"
        library(demo) {
          cell(demo__latrnq) {
            latch(IQ2, IQN2) {
              enable : "E" ;
              data_in : "D" ;
              clear : "(!RN)" ;
            }
            pin(E)  { direction : input ; }
            pin(D)  { direction : input ; }
            pin(RN) { direction : input ; }
            pin(Q)  { direction : output ; function : "IQ2" ; }
          }
        }
        "#;
        let cell = parse_cell(src);
        let ins = input_pins(&cell);
        let r = build(&cell, "demo__latrnq", &ins, false);
        assert_eq!(r.kind, CellKind::Latch);
        let seq = r.sequential.unwrap();
        assert!(seq.clock.is_none());
        let en = seq.enable.unwrap();
        assert_eq!(en.pin, "E");
        assert_eq!(en.active, ActiveLevel::High);
        assert_eq!(seq.async_reset.unwrap().pin, "RN");
        assert_eq!(seq.next_state.inputs, vec!["D".to_string()]);
    }

    #[test]
    fn qn_output_marked_inverted() {
        let src = r#"
        library(demo) {
          cell(demo__dffnq) {
            ff(IQ1, IQN1) {
              clocked_on : "CLK" ;
              next_state : "D" ;
            }
            pin(CLK){ direction : input ; clock : true ; }
            pin(D)  { direction : input ; }
            pin(QN) { direction : output ; function : "IQN1" ; }
          }
        }
        "#;
        let cell = parse_cell(src);
        let ins = input_pins(&cell);
        let r = build(&cell, "demo__dffnq", &ins, false);
        let seq = r.sequential.unwrap();
        assert_eq!(
            seq.outputs,
            vec![SeqOutput {
                pin: "QN".into(),
                inverted: true
            }]
        );
    }

    #[test]
    fn clear_preset_reset_dominant_is_clean() {
        let cell = parse_cell(DFFRSNQ);
        let ins = input_pins(&cell);
        let r = build(&cell, "demo__dffrsnq", &ins, false);
        assert!(
            !r.notes
                .iter()
                .any(|n| matches!(n, SeqNote::ClearPresetNotResetDominant { .. })),
            "reset-dominant var1=L must not flag: {:?}",
            r.notes
        );
    }

    #[test]
    fn clear_preset_set_dominant_is_flagged() {
        // Same as DFFRSNQ but var1=H (set wins) — must surface a diagnostic.
        let src = DFFRSNQ.replace("clear_preset_var1 : L", "clear_preset_var1 : H");
        let src = src.replace("clear_preset_var2 : H", "clear_preset_var2 : L");
        let cell = parse_cell(&src);
        let ins = input_pins(&cell);
        let r = build(&cell, "demo__dffsetdom", &ins, false);
        assert!(
            r.notes.iter().any(|n| matches!(
                n,
                SeqNote::ClearPresetNotResetDominant { var1, .. } if var1 == "H"
            )),
            "set-dominant must flag, got: {:?}",
            r.notes
        );
    }

    #[test]
    fn clock_gate_classified() {
        let src = r#"
        library(demo) {
          cell(demo__icgtp) {
            statetable("CLK E TE", "IQ2 IQN2") { table : " - - - : - : - "; }
            pin(CLK){ direction : input ; clock_gate_clock_pin : true ; }
            pin(E)  { direction : input ; clock_gate_enable_pin : true ; }
            pin(TE) { direction : input ; clock_gate_test_pin : true ; }
            pin(Q)  { direction : output ; clock_gate_out_pin : true ; state_function : "(CLK&IQ2)" ; }
          }
        }
        "#;
        let cell = parse_cell(src);
        let ins = input_pins(&cell);
        let r = build(&cell, "demo__icgtp", &ins, false);
        assert_eq!(r.kind, CellKind::ClockGate);
    }

    #[test]
    fn physical_and_tie_classification() {
        let fill =
            parse_cell(r#"library(demo){ cell(demo__fill_1){ pin(VGND){direction:input;} } }"#);
        assert_eq!(
            build(&fill, "demo__fill_1", &[], false).kind,
            CellKind::Filler
        );

        let endcap = parse_cell(r#"library(demo){ cell(demo__endcap){ } }"#);
        assert_eq!(
            build(&endcap, "demo__endcap", &[], false).kind,
            CellKind::Endcap
        );

        let tie = parse_cell(
            r#"library(demo){ cell(demo__tiehi){ pin(Q){direction:output;function:"1";} } }"#,
        );
        assert_eq!(
            build(&tie, "demo__tiehi", &[], false).kind,
            CellKind::TieHigh
        );
    }

    #[test]
    fn std_cell_when_has_logic() {
        let src = r#"library(demo){ cell(demo__and2){
            pin(A){direction:input;} pin(B){direction:input;}
            pin(Z){direction:output;function:"(A&B)";} } }"#;
        let cell = parse_cell(src);
        let ins = input_pins(&cell);
        assert_eq!(build(&cell, "demo__and2", &ins, true).kind, CellKind::Std);
    }
}
