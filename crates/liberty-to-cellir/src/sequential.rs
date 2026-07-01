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
//! ## The `clear_preset_var` tie-break (C4)
//!
//! When both `clear` and `preset` are asserted at once, Liberty's
//! `clear_preset_var1` / `clear_preset_var2` state what the two state
//! variables (`Q`, `QN`) take. This module reads them into the schema's
//! [`cell_model_ir::ClearPreset`] ([`Sequential::clear_preset`]) so the
//! consumer overlay can honour set-dominant cells rather than assuming
//! reset-dominant. It ALSO keeps emitting the
//! [`SeqNote::ClearPresetNotResetDominant`] generation diagnostic for any cell
//! that is not reset-dominant (`var1 = L`, `var2 = H`) — a useful signal for
//! spotting cells whose behaviour differs from Jacquard's historical default.

use cell_model_ir::{
    ActiveLevel, CellKind, ClearPreset, ClearPresetVal, ClockEdge, ClockPin, CombLogic, EnablePin,
    SeqOutput, Sequential,
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
        &[q_var.as_str(), qn_var.as_str()],
        notes,
    );

    let async_reset = build_async(ff.attr("clear"), "clear", cell_type, notes);
    let async_set = build_async(ff.attr("preset"), "preset", cell_type, notes);

    // clear_preset tie-break, only meaningful when BOTH controls exist.
    let clear_preset = if async_reset.is_some() && async_set.is_some() {
        extract_clear_preset(ff, cell_type, notes)
    } else {
        None
    };

    let outputs = state_outputs(cell, &q_var, &qn_var);

    let seq = Sequential {
        clock,
        enable: None,
        next_state,
        outputs,
        async_set,
        async_reset,
        clear_preset,
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
        &[q_var.as_str(), qn_var.as_str()],
        notes,
    );

    let async_reset = build_async(latch.attr("clear"), "clear", cell_type, notes);
    let async_set = build_async(latch.attr("preset"), "preset", cell_type, notes);
    let clear_preset = if async_reset.is_some() && async_set.is_some() {
        extract_clear_preset(latch, cell_type, notes)
    } else {
        None
    };

    let outputs = state_outputs(cell, &q_var, &qn_var);

    let seq = Sequential {
        clock: None,
        enable,
        next_state,
        outputs,
        async_set,
        async_reset,
        clear_preset,
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
///
/// `state_vars` are the flip-flop/latch state variable names from the
/// `ff(IQ, IQN)` / `latch(IQ, IQN)` header: `[Q_var, QN_var]`. Commercial
/// flops write their next_state over their own state node — e.g. GF130
/// `SEDFF_X4` → `((SE&SI)|((!SE)&((E&D)|((!E)&IQ))))`, where `IQ` is the ff's
/// current state. The Q state variable is admitted as an extra **self-feedback
/// input** of the next-state cone (appended after the real input pins) so these
/// cells compile to a structurally-valid next_state instead of erroring. The
/// inverted state variable (`IQN`) is folded to `!IQ` first, so at most one
/// feedback input is introduced.
///
/// **Consumer implication:** the resulting `next_state.inputs` may contain a
/// name that is NOT a cell pin (the `IQ` state var). A descriptor consumer must
/// wire that input to the flop's own current output (Q self-feedback). The
/// GF180/SKY130 built-ins never reference their state var in `next_state`
/// (their `next_state` is `D` / a scan-mux over real pins), so no consumed path
/// is affected; this only arises for commercial libraries (GF130) not yet on
/// the descriptor consumer path.
fn build_next_state(
    expr_str: Option<&str>,
    cell_type: &str,
    input_pins: &[String],
    state_vars: &[&str],
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
    let q_var = state_vars.first().copied().filter(|s| !s.is_empty());
    let qn_var = state_vars.get(1).copied().filter(|s| !s.is_empty());
    match function::parse(src) {
        Ok(mut expr) => {
            // Fold the inverted state variable (`IQN`) to `!IQ` so only the Q
            // state var can appear as a feedback input.
            if let (Some(q), Some(qn)) = (q_var, qn_var) {
                expr = expr.substitute_pin(
                    qn,
                    &function::Expr::Not(Box::new(function::Expr::Pin(q.into()))),
                );
            }
            let referenced = expr.pins();
            // Real cell input pins referenced, in cell input order.
            let mut inputs: Vec<String> = input_pins
                .iter()
                .filter(|p| referenced.iter().any(|r| r == *p))
                .cloned()
                .collect();
            // Append the Q state variable as a self-feedback input when the
            // next_state references it (and it is not already an input pin).
            if let Some(q) = q_var {
                if referenced.iter().any(|r| r == q) && !inputs.iter().any(|p| p == q) {
                    inputs.push(q.to_string());
                }
            }
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

/// Read `clear_preset_var1/var2` into a [`ClearPreset`] tie-break and, as a
/// side effect, surface a [`SeqNote::ClearPresetNotResetDominant`] diagnostic
/// when the pair is not reset-dominant (`var1 = L`, `var2 = H`). Called only
/// when the cell has BOTH `clear` and `preset`.
///
/// Returns `None` (and no note) when neither attribute is present — Liberty
/// omits the pair for cells where the tie-break is degenerate/unspecified, and
/// an absent field means "reset-dominant default" to the consumer, exactly
/// matching the historical hardcoded behaviour.
fn extract_clear_preset(
    group: &LibertyGroup,
    cell_type: &str,
    notes: &mut Vec<SeqNote>,
) -> Option<ClearPreset> {
    let raw = |attr: &str| -> Option<String> {
        group
            .attr(attr)
            .and_then(|a| a.first_string())
            .map(|s| s.trim_matches('"').to_uppercase())
    };
    let var1_s = raw("clear_preset_var1");
    let var2_s = raw("clear_preset_var2");
    if var1_s.is_none() && var2_s.is_none() {
        // Liberty specifies no tie-break at all — reset-dominant default.
        return None;
    }
    let var1_s = var1_s.unwrap_or_default();
    let var2_s = var2_s.unwrap_or_default();

    // Reset-dominant: Q (var1) → L, QN (var2) → H. Anything else — including an
    // absent var1/var2, `H` (set-dominant), or `N`/`X`/`T` — is not provably
    // reset-dominant, so surface the diagnostic (kept from C2).
    let reset_dominant = var1_s == "L" && var2_s == "H";
    if !reset_dominant {
        notes.push(SeqNote::ClearPresetNotResetDominant {
            cell: cell_type.to_string(),
            var1: var1_s.clone(),
            var2: var2_s.clone(),
        });
    }

    Some(ClearPreset {
        var1: parse_clear_preset_val(&var1_s),
        var2: parse_clear_preset_val(&var2_s),
    })
}

/// Map a Liberty `clear_preset_var*` token to a [`ClearPresetVal`]. An
/// unrecognized/absent token defaults to [`ClearPresetVal::Unknown`] (`X`).
fn parse_clear_preset_val(s: &str) -> ClearPresetVal {
    match s {
        "L" => ClearPresetVal::Low,
        "H" => ClearPresetVal::High,
        "N" => ClearPresetVal::NoChange,
        "T" => ClearPresetVal::Toggle,
        _ => ClearPresetVal::Unknown,
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

    /// GF130-shaped scan/enable flop whose `next_state` writes over its own
    /// `ff(IQ, IQN)` state node: `((SE&SI)|((!SE)&((E&D)|((!E)&IQ))))`. The `IQ`
    /// state var must be admitted as a self-feedback input rather than erroring.
    #[test]
    fn next_state_references_ff_state_var() {
        let src = r#"
        library(demo) {
          cell(demo__sedff) {
            ff(IQ, IQN) {
              clocked_on : "CLK" ;
              next_state : "((SE&SI)|((!SE)&((E&D)|((!E)&IQ))))" ;
            }
            pin(CLK){ direction : input ; clock : true ; }
            pin(D)  { direction : input ; }
            pin(E)  { direction : input ; }
            pin(SE) { direction : input ; }
            pin(SI) { direction : input ; }
            pin(Q)  { direction : output ; function : "IQ" ; }
          }
        }
        "#;
        let cell = parse_cell(src);
        let ins = input_pins(&cell);
        let r = build(&cell, "demo__sedff", &ins, false);
        // No next_state compile error.
        assert!(
            r.notes.is_empty(),
            "unexpected next_state notes: {:?}",
            r.notes
        );
        let ns = &r.sequential.unwrap().next_state;
        // The Q state var IQ is a self-feedback input, appended after the pins.
        assert!(
            ns.inputs.contains(&"IQ".to_string()),
            "inputs={:?}",
            ns.inputs
        );
        assert_eq!(ns.inputs.last().unwrap(), "IQ");
        // Truth table: SE selects SI; else E selects D, else holds IQ.
        use std::collections::HashMap;
        for se in [false, true] {
            for si in [false, true] {
                for e in [false, true] {
                    for d in [false, true] {
                        for iq in [false, true] {
                            let m = HashMap::from([
                                ("SE".to_string(), se),
                                ("SI".to_string(), si),
                                ("E".to_string(), e),
                                ("D".to_string(), d),
                                ("IQ".to_string(), iq),
                            ]);
                            let out = ns.eval(&m).unwrap()["next_state"];
                            let expected = if se {
                                si
                            } else if e {
                                d
                            } else {
                                iq
                            };
                            assert_eq!(out, expected, "SE={se} SI={si} E={e} D={d} IQ={iq}");
                        }
                    }
                }
            }
        }
    }

    /// When `next_state` references the inverted state var `IQN`, it folds to
    /// `!IQ` — only the single Q self-feedback input is introduced.
    #[test]
    fn next_state_references_inverted_state_var() {
        let src = r#"
        library(demo) {
          cell(demo__tdff) {
            ff(IQ, IQN) {
              clocked_on : "CLK" ;
              next_state : "(T&IQN)|((!T)&IQ)" ;
            }
            pin(CLK){ direction : input ; clock : true ; }
            pin(T)  { direction : input ; }
            pin(Q)  { direction : output ; function : "IQ" ; }
          }
        }
        "#;
        let cell = parse_cell(src);
        let ins = input_pins(&cell);
        let r = build(&cell, "demo__tdff", &ins, false);
        assert!(r.notes.is_empty(), "unexpected notes: {:?}", r.notes);
        let ns = &r.sequential.unwrap().next_state;
        // IQN folded to !IQ → IQ is the only state input; IQN absent.
        assert!(
            ns.inputs.contains(&"IQ".to_string()),
            "inputs={:?}",
            ns.inputs
        );
        assert!(
            !ns.inputs.contains(&"IQN".to_string()),
            "inputs={:?}",
            ns.inputs
        );
        use std::collections::HashMap;
        for t in [false, true] {
            for iq in [false, true] {
                let m = HashMap::from([("T".to_string(), t), ("IQ".to_string(), iq)]);
                let out = ns.eval(&m).unwrap()["next_state"];
                // T toggles: next = T ? !IQ : IQ.
                let expected = if t { !iq } else { iq };
                assert_eq!(out, expected, "T={t} IQ={iq}");
            }
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
        // The tie-break is emitted faithfully (L, H) and reduces to reset-dominant.
        let cp = r
            .sequential
            .unwrap()
            .clear_preset
            .expect("clear_preset emitted");
        assert_eq!(cp.var1, ClearPresetVal::Low);
        assert_eq!(cp.var2, ClearPresetVal::High);
        assert_eq!(cp.dominance(), cell_model_ir::Dominance::ResetDominant);
    }

    #[test]
    fn clear_preset_set_dominant_is_emitted_and_flagged() {
        // Same as DFFRSNQ but var1=H (set wins) — must emit var1=H and also
        // surface the diagnostic (both remain useful).
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
        let cp = r
            .sequential
            .unwrap()
            .clear_preset
            .expect("set-dominant clear_preset emitted");
        assert_eq!(cp.var1, ClearPresetVal::High);
        assert_eq!(cp.var2, ClearPresetVal::Low);
        assert_eq!(cp.dominance(), cell_model_ir::Dominance::SetDominant);
    }

    #[test]
    fn clear_preset_absent_when_no_set_preset_pair() {
        // A plain reset-only DFF (no preset) must not carry a clear_preset.
        let src = r#"
        library(demo) {
          cell(demo__dffrnq) {
            ff(IQ1, IQN1) {
              clocked_on : "CLK" ;
              next_state : "D" ;
              clear : "(!RN)" ;
            }
            pin(CLK) { direction : input ; clock : true ; }
            pin(D)   { direction : input ; }
            pin(RN)  { direction : input ; }
            pin(Q)   { direction : output ; function : "IQ1" ; }
          }
        }
        "#;
        let cell = parse_cell(src);
        let ins = input_pins(&cell);
        let r = build(&cell, "demo__dffrnq", &ins, false);
        assert!(r.sequential.unwrap().clear_preset.is_none());
    }

    #[test]
    fn clear_preset_set_dominant_latch_is_emitted() {
        // A GF180 `latrsnq`-shaped set-dominant latch (var1=H).
        let src = r#"
        library(demo) {
          cell(demo__latrsnq) {
            latch(IQ1, IQN1) {
              enable : "E" ;
              data_in : "D" ;
              clear : "(!RN)" ;
              preset : "(!SETN)" ;
              clear_preset_var1 : H ;
              clear_preset_var2 : L ;
            }
            pin(E)   { direction : input ; }
            pin(D)   { direction : input ; }
            pin(RN)  { direction : input ; }
            pin(SETN){ direction : input ; }
            pin(Q)   { direction : output ; function : "IQ1" ; }
          }
        }
        "#;
        let cell = parse_cell(src);
        let ins = input_pins(&cell);
        let r = build(&cell, "demo__latrsnq", &ins, false);
        assert_eq!(r.kind, CellKind::Latch);
        let cp = r
            .sequential
            .unwrap()
            .clear_preset
            .expect("latch clear_preset");
        assert_eq!(cp.var1, ClearPresetVal::High);
        assert_eq!(cp.dominance(), cell_model_ir::Dominance::SetDominant);
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
