// SPDX-License-Identifier: Apache-2.0

//! L4 — timing characterization, corner-keyed (ADR 0019 D5).
//!
//! Projects a cell's Liberty `timing()` groups onto
//! [`cell_model_ir::CellTiming`]: combinational + clock→output delay arcs
//! ([`cell_model_ir::DelayArc`]), setup/hold/recovery/removal constraint arcs
//! ([`cell_model_ir::ConstraintArc`]), and SRAM/macro timing
//! ([`cell_model_ir::SramTiming`]). Every number is a
//! [`cell_model_ir::TimingValue`] naming the corner by index — for a
//! single-corner input `.lib` there is exactly one corner and `min`/`typ`/`max`
//! are equal (no within-corner derate is available from one library).
//!
//! ## Units
//!
//! Liberty values are in the library's `time_unit` (GF180 / SKY130 use
//! `1ns`); the IR carries true **picoseconds** as `f64`, so this module scales
//! by the `time_unit` factor and keeps full `f64` precision. (The legacy
//! runtime `liberty_parser::TimingLibrary` ignored `time_unit` and rounded to
//! integer ps — carrying scaled `f64` ps here is strictly more faithful, which
//! the C2.3 consumer rewrite will consume directly.)
//!
//! ## Scalar extraction
//!
//! Real `.lib` timing tables are 2-D `cell_rise (template) { values(...) }`
//! LUTs. The schema's `TimingValue` is a single representative scalar, so this
//! module takes the first table entry — matching what
//! `liberty_parser::extract_scalar_ps` consumes today.

use cell_model_ir::{
    CellTiming, ClockEdge, ConstraintArc, ConstraintKind, Corner, DelayArc, DelayKind, TimingValue,
};
use liberty_parse::LibertyGroup;

/// Picoseconds-per-`time_unit` for the named Liberty time unit. Defaults to
/// `1.0` (assume the value is already ps) for an unrecognised / absent unit.
pub fn ps_per_time_unit(time_unit: Option<&str>) -> f64 {
    let Some(u) = time_unit else { return 1.0 };
    let u = u.trim().trim_matches('"').to_lowercase();
    // Split into a leading numeric scale and a unit suffix (e.g. `10ps`).
    let split = u.find(|c: char| c.is_alphabetic()).unwrap_or(u.len());
    let (num, suffix) = u.split_at(split);
    let scale: f64 = if num.trim().is_empty() {
        1.0
    } else {
        num.trim().parse().unwrap_or(1.0)
    };
    let unit_ps = match suffix.trim() {
        "s" => 1.0e12,
        "ms" => 1.0e9,
        "us" => 1.0e6,
        "ns" => 1.0e3,
        "ps" => 1.0,
        "fs" => 1.0e-3,
        _ => 1.0,
    };
    scale * unit_ps
}

/// Derive a single [`Corner`] from a Liberty library name such as
/// `gf180mcu_fd_sc_mcu7t5v0__tt_025C_5v00` (or a SKY130
/// `sky130_fd_sc_hd__ss_100C_1v40`). Returns `None` if no `__<process>_...`
/// suffix is recognisable.
pub fn corner_from_library_name(lib_name: &str) -> Option<Corner> {
    // The corner suffix is the trailing `<process>_<temp>_<volt>` after the
    // last `__`.
    let suffix = lib_name.rsplit("__").next()?;
    let parts: Vec<&str> = suffix.split('_').collect();
    if parts.len() < 3 {
        return None;
    }
    let process = parts[0].to_string();
    // Only accept known process labels to avoid misreading a cell name.
    if !matches!(process.as_str(), "ss" | "tt" | "ff" | "sf" | "fs") {
        return None;
    }
    let temperature = parse_temp(parts[1])?;
    let voltage = parse_voltage(parts[2])?;
    Some(Corner {
        name: suffix.to_string(),
        process,
        voltage,
        temperature,
    })
}

/// Parse a Liberty corner temperature token: `025C` ⇒ 25.0, `n40C` ⇒ -40.0,
/// `125C` ⇒ 125.0.
fn parse_temp(tok: &str) -> Option<f32> {
    let t = tok.trim_end_matches(['C', 'c']);
    let (neg, digits) = if let Some(rest) = t.strip_prefix('n') {
        (true, rest)
    } else if let Some(rest) = t.strip_prefix('m') {
        (true, rest)
    } else {
        (false, t)
    };
    let v: f32 = digits.parse().ok()?;
    Some(if neg { -v } else { v })
}

/// Parse a Liberty corner voltage token: `5v00` ⇒ 5.0, `1v80` ⇒ 1.8,
/// `1v62` ⇒ 1.62.
fn parse_voltage(tok: &str) -> Option<f32> {
    let replaced = tok.replacen('v', ".", 1).replacen('V', ".", 1);
    replaced.parse().ok()
}

/// Extract the first scalar value (in the library's native unit) from a timing
/// table group such as `cell_rise (tmpl) { values("1.0, 2.0", ...) }`.
fn first_table_value(table: &LibertyGroup) -> Option<f64> {
    let values = table.attr("values")?;
    let first = values.first_string()?;
    let token = first
        .trim_start()
        .trim_start_matches('"')
        .split([',', '"', ')', ' ', '\n', '\\'])
        .find(|s| !s.trim().is_empty())?
        .trim();
    token.parse::<f64>().ok()
}

/// Build a single-corner [`TimingValue`] (min = typ = max) for a table, scaled
/// to ps. `None` if the table carries no readable scalar.
fn timing_value(table: &LibertyGroup, corner_index: u32, ps_scale: f64) -> Option<TimingValue> {
    let v = first_table_value(table)? * ps_scale;
    Some(TimingValue {
        corner_index,
        min: v,
        typ: v,
        max: v,
    })
}

/// Classify a Liberty `timing_type` string into a constraint kind + edge, if it
/// is a constraint arc (setup/hold/recovery/removal).
fn constraint_kind(timing_type: &str) -> Option<(ConstraintKind, ClockEdge)> {
    let (kind, edge) = match timing_type {
        "setup_rising" => (ConstraintKind::Setup, ClockEdge::Rising),
        "setup_falling" => (ConstraintKind::Setup, ClockEdge::Falling),
        "hold_rising" => (ConstraintKind::Hold, ClockEdge::Rising),
        "hold_falling" => (ConstraintKind::Hold, ClockEdge::Falling),
        "recovery_rising" => (ConstraintKind::Recovery, ClockEdge::Rising),
        "recovery_falling" => (ConstraintKind::Recovery, ClockEdge::Falling),
        "removal_rising" => (ConstraintKind::Removal, ClockEdge::Rising),
        "removal_falling" => (ConstraintKind::Removal, ClockEdge::Falling),
        _ => return None,
    };
    Some((kind, edge))
}

/// Whether a `timing_type` denotes a sequential (edge / async-control) delay
/// arc as opposed to a combinational one.
fn is_sequential_delay(timing_type: Option<&str>) -> bool {
    matches!(
        timing_type,
        Some("rising_edge") | Some("falling_edge") | Some("clear") | Some("preset")
    )
}

/// Build the L4 [`CellTiming`] for one cell at `corner_index`. Returns `None`
/// if the cell carries no readable timing arcs.
pub fn build_cell_timing(
    cell: &LibertyGroup,
    corner_index: u32,
    ps_scale: f64,
) -> Option<CellTiming> {
    let mut delays: Vec<DelayArc> = Vec::new();
    let mut constraints: Vec<ConstraintArc> = Vec::new();

    for pin in cell.groups_of_type("pin") {
        let Some(pin_name) = pin.first_name() else {
            continue;
        };
        let direction = pin
            .attr("direction")
            .and_then(|a| a.first_string())
            .map(|d| d.trim_matches('"').to_string())
            .unwrap_or_default();

        for timing in pin.groups_of_type("timing") {
            let related = timing
                .attr("related_pin")
                .and_then(|a| a.first_string())
                .map(|s| s.trim_matches('"').to_string());
            let timing_type = timing
                .attr("timing_type")
                .and_then(|a| a.first_string())
                .map(|s| s.trim_matches('"').to_string());

            let Some(related_pin) = related else { continue };

            // Constraint arc (setup/hold/recovery/removal) on an input/data pin.
            if let Some(tt) = timing_type.as_deref() {
                if let Some((kind, edge)) = constraint_kind(tt) {
                    let rise = collect(timing, "rise_constraint", corner_index, ps_scale);
                    let fall = collect(timing, "fall_constraint", corner_index, ps_scale);
                    if !rise.is_empty() || !fall.is_empty() {
                        constraints.push(ConstraintArc {
                            data_pin: pin_name.to_string(),
                            related_pin,
                            kind,
                            edge,
                            rise,
                            fall,
                        });
                    }
                    continue;
                }
            }

            // Delay arc — only meaningful for output pins (input pins carry
            // constraints, not delays). Skip width/period checks (no
            // cell_rise/cell_fall).
            if direction != "output" {
                continue;
            }
            let rise = collect(timing, "cell_rise", corner_index, ps_scale);
            let fall = collect(timing, "cell_fall", corner_index, ps_scale);
            if rise.is_empty() && fall.is_empty() {
                continue;
            }
            let kind = if is_sequential_delay(timing_type.as_deref()) {
                DelayKind::ClockToOutput
            } else {
                DelayKind::Combinational
            };
            delays.push(DelayArc {
                from_pin: related_pin,
                to_pin: pin_name.to_string(),
                kind,
                rise,
                fall,
            });
        }
    }

    if delays.is_empty() && constraints.is_empty() {
        return None;
    }
    Some(CellTiming {
        delays,
        constraints,
        sram: None,
    })
}

/// Collect a single-corner `TimingValue` for the named child table, returned as
/// a one-element vec (or empty if absent).
fn collect(
    timing: &LibertyGroup,
    table_type: &str,
    corner_index: u32,
    ps_scale: f64,
) -> Vec<TimingValue> {
    timing
        .group_of_type(table_type)
        .and_then(|t| timing_value(t, corner_index, ps_scale))
        .into_iter()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ps_scaling() {
        assert_eq!(ps_per_time_unit(Some("1ns")), 1000.0);
        assert_eq!(ps_per_time_unit(Some("1ps")), 1.0);
        assert_eq!(ps_per_time_unit(Some("10ps")), 10.0);
        assert_eq!(ps_per_time_unit(Some("\"1ns\"")), 1000.0);
        assert_eq!(ps_per_time_unit(None), 1.0);
    }

    #[test]
    fn corner_parse_gf180() {
        let c = corner_from_library_name("gf180mcu_fd_sc_mcu7t5v0__tt_025C_5v00").unwrap();
        assert_eq!(c.name, "tt_025C_5v00");
        assert_eq!(c.process, "tt");
        assert_eq!(c.temperature, 25.0);
        assert_eq!(c.voltage, 5.0);
    }

    #[test]
    fn corner_parse_negative_temp_and_sub_volt() {
        let c = corner_from_library_name("gf180mcu_fd_sc_mcu9t5v0__ss_n40C_1v62").unwrap();
        assert_eq!(c.process, "ss");
        assert_eq!(c.temperature, -40.0);
        assert!((c.voltage - 1.62).abs() < 1e-5);
    }

    #[test]
    fn corner_parse_sky130() {
        let c = corner_from_library_name("sky130_fd_sc_hd__ss_100C_1v40").unwrap();
        assert_eq!(c.process, "ss");
        assert_eq!(c.temperature, 100.0);
        assert!((c.voltage - 1.40).abs() < 1e-5);
    }

    #[test]
    fn non_corner_name_is_none() {
        assert!(corner_from_library_name("some_random_lib").is_none());
    }

    fn parse_cell(src: &str) -> LibertyGroup {
        let lib = liberty_parse::parse(src).expect("parse");
        let cell = lib.groups_of_type("cell").next().unwrap().clone();
        cell
    }

    #[test]
    fn combinational_delay_arc_extracted() {
        let src = r#"
        library(demo) {
          cell(demo__inv) {
            pin(A) { direction : input ; }
            pin(Y) {
              direction : output ;
              timing() {
                related_pin : "A" ;
                cell_rise(scalar) { values("0.05"); }
                cell_fall(scalar) { values("0.04"); }
              }
            }
          }
        }
        "#;
        let cell = parse_cell(src);
        let t = build_cell_timing(&cell, 0, 1000.0).unwrap();
        assert_eq!(t.delays.len(), 1);
        let arc = &t.delays[0];
        assert_eq!(arc.from_pin, "A");
        assert_eq!(arc.to_pin, "Y");
        assert_eq!(arc.kind, DelayKind::Combinational);
        // 0.05 ns -> 50 ps.
        assert!((arc.rise[0].typ - 50.0).abs() < 1e-9);
        assert_eq!(arc.rise[0].corner_index, 0);
        assert_eq!(arc.rise[0].min, arc.rise[0].max);
    }

    #[test]
    fn dff_setup_hold_clk_to_q_extracted() {
        let src = r#"
        library(demo) {
          cell(demo__dff) {
            pin(CLK) { direction : input ; clock : true ; }
            pin(D) {
              direction : input ;
              timing() {
                related_pin : "CLK" ; timing_type : setup_rising ;
                rise_constraint(scalar) { values("0.08"); }
                fall_constraint(scalar) { values("0.075"); }
              }
              timing() {
                related_pin : "CLK" ; timing_type : hold_rising ;
                rise_constraint(scalar) { values("0.02"); }
                fall_constraint(scalar) { values("0.02"); }
              }
            }
            pin(Q) {
              direction : output ;
              timing() {
                related_pin : "CLK" ; timing_type : rising_edge ;
                cell_rise(scalar) { values("0.15"); }
                cell_fall(scalar) { values("0.14"); }
              }
            }
          }
        }
        "#;
        let cell = parse_cell(src);
        let t = build_cell_timing(&cell, 0, 1000.0).unwrap();
        // setup + hold constraints on D.
        assert_eq!(t.constraints.len(), 2);
        let setup = t
            .constraints
            .iter()
            .find(|c| c.kind == ConstraintKind::Setup)
            .unwrap();
        assert_eq!(setup.data_pin, "D");
        assert_eq!(setup.related_pin, "CLK");
        assert_eq!(setup.edge, ClockEdge::Rising);
        assert!((setup.rise[0].typ - 80.0).abs() < 1e-9);
        let hold = t
            .constraints
            .iter()
            .find(|c| c.kind == ConstraintKind::Hold)
            .unwrap();
        assert!((hold.rise[0].typ - 20.0).abs() < 1e-9);
        // clock→Q delay.
        assert_eq!(t.delays.len(), 1);
        assert_eq!(t.delays[0].kind, DelayKind::ClockToOutput);
        assert!((t.delays[0].rise[0].typ - 150.0).abs() < 1e-9);
    }

    #[test]
    fn width_and_period_checks_are_skipped() {
        let src = r#"
        library(demo) {
          cell(demo__dff) {
            pin(CLK) {
              direction : input ; clock : true ;
              timing() {
                related_pin : "CLK" ; timing_type : min_pulse_width ;
                rise_constraint(scalar) { values("0.5"); }
                fall_constraint(scalar) { values("0.5"); }
              }
            }
          }
        }
        "#;
        let cell = parse_cell(src);
        // No setup/hold/delay → no timing.
        assert!(build_cell_timing(&cell, 0, 1000.0).is_none());
    }
}
