// SPDX-FileCopyrightText: Copyright (c) 2024 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-FileCopyrightText: Copyright (c) 2026 ChipFlow Inc. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! PDK-neutral behavioral parsing and AIG decomposition primitives.
//!
//! The `.functional.v` cell-model files shipped by open-source PDKs
//! (SKY130, GF180MCU, and others that follow the same OpenLane /
//! OpenROAD ecosystem conventions) use a small fixed grammar:
//!
//! - One `module <name>( <ports> );` declaration.
//! - `input`/`output` port-direction lines.
//! - A topologically ordered sequence of Verilog gate-primitive
//!   instantiations (`not`, `buf`, `and`, `or`, `nand`, `nor`,
//!   `xor`, `xnor`) plus optional UDP instantiations whose names
//!   are PDK-specific (e.g. `sky130_fd_sc_hd__udp_*`,
//!   `UDP_GF018hv5v_mcu_sc7_*`).
//!
//! The parser is fully prefix-agnostic; UDP entries are surfaced
//! verbatim and the PDK-specific decomposition layer routes them
//! to PDK-specific UDP handlers.
//!
//! This crate owns the PDK-neutral primitives that both `sky130_pdk`
//! and `gf180mcu_pdk` (in jacquard core) share:
//!
//! - The parser entry points (`parse_functional_model`, `parse_udp`)
//!   and their AST types (`BehavioralGate`, `BehavioralModel`,
//!   `UdpRow`, `UdpModel`).
//! - The AIG-builder helpers: `WireVal`, `GATE_MARKER`,
//!   `build_chain_gate`, `build_xor_chain`, `build_udp_aig`,
//!   `finalize_decomp_result`.
//! - The `DecompResult` type returned by every PDK's `decompose_*`
//!   entry point.
//! - The gate-level evaluators (`eval_behavioral_model`,
//!   `eval_udp_for_inputs`) used as reference oracles in tests.
//!
//! PDK-specific lookup structs (sky130's fixed-field `CellInputs`,
//! gf180's `HashMap<String, usize>`) deliberately stay in their
//! respective `*_pdk` modules — they aren't actually shared.

use std::collections::HashMap;

// ============================================================================
// Data structures
// ============================================================================

/// A single gate instantiation from a functional Verilog model.
#[derive(Debug, Clone)]
pub struct BehavioralGate {
    /// Gate type: "and", "or", "nand", "nor", "not", "xor", "xnor", "buf",
    /// or a UDP name like "sky130_fd_sc_hd__udp_mux_2to1_N"
    pub gate_type: String,
    /// Output wire name
    pub output: String,
    /// Input wire names (in port order)
    pub inputs: Vec<String>,
}

/// A parsed functional Verilog model for a cell.
#[derive(Debug, Clone)]
pub struct BehavioralModel {
    /// Module name (e.g. "sky130_fd_sc_hd__o21ai")
    pub module_name: String,
    /// Module input port names (e.g. ["A1", "A2", "B1"])
    pub inputs: Vec<String>,
    /// Module output port names (e.g. ["Y"])
    pub outputs: Vec<String>,
    /// Gate instantiations in order
    pub gates: Vec<BehavioralGate>,
}

/// A single row in a UDP truth table.
#[derive(Debug, Clone)]
pub struct UdpRow {
    /// Input values: Some(true)=1, Some(false)=0, None=don't-care (?)
    pub inputs: Vec<Option<bool>>,
    /// Output value
    pub output: bool,
}

/// A parsed Verilog UDP (User Defined Primitive).
#[derive(Debug, Clone)]
pub struct UdpModel {
    /// Primitive name
    pub name: String,
    /// Output port name
    pub output: String,
    /// Input port names in order
    pub inputs: Vec<String>,
    /// Truth table rows
    pub rows: Vec<UdpRow>,
}

// ============================================================================
// Parser: Functional Verilog models
// ============================================================================

/// Parse a functional Verilog model file (*.functional.v).
///
/// Handles:
/// - module declaration with port names and directions
/// - Gate instantiations: `gate_type name (output, input1, input2, ...);`
/// - Skips: comments, `supply`, `wire`, `timescale`, `celldefine`, etc.
pub fn parse_functional_model(verilog_src: &str) -> Option<BehavioralModel> {
    let mut module_name = String::new();
    let mut port_names: Vec<String> = Vec::new();
    let mut input_ports: Vec<String> = Vec::new();
    let mut output_ports: Vec<String> = Vec::new();
    let mut gates: Vec<BehavioralGate> = Vec::new();

    // Simple state machine
    enum State {
        LookingForModule,
        InModulePorts,
        InModuleBody,
    }
    let mut state = State::LookingForModule;

    // Accumulate multi-line tokens
    let mut accum = String::new();

    for line in verilog_src.lines() {
        let trimmed = line.trim();

        // Skip comments and preprocessor directives
        if trimmed.starts_with("//")
            || trimmed.starts_with("/*")
            || trimmed.starts_with("*/")
            || trimmed.starts_with('*')
            || trimmed.starts_with('`')
            || trimmed.is_empty()
        {
            continue;
        }

        match state {
            State::LookingForModule => {
                if trimmed.starts_with("module ") {
                    // Parse: module sky130_fd_sc_hd__o21ai (
                    accum.clear();
                    accum.push_str(trimmed);

                    if accum.contains(");") {
                        parse_module_header(&accum, &mut module_name, &mut port_names);
                        state = State::InModuleBody;
                    } else {
                        state = State::InModulePorts;
                    }
                }
            }
            State::InModulePorts => {
                accum.push(' ');
                accum.push_str(trimmed);
                if accum.contains(");") {
                    parse_module_header(&accum, &mut module_name, &mut port_names);
                    state = State::InModuleBody;
                }
            }
            State::InModuleBody => {
                if trimmed == "endmodule" {
                    break;
                }

                // Parse port direction declarations
                if trimmed.starts_with("output ") {
                    let names = parse_port_declaration(trimmed, "output");
                    output_ports.extend(names);
                    continue;
                }
                if trimmed.starts_with("input ") {
                    let names = parse_port_declaration(trimmed, "input");
                    input_ports.extend(names);
                    continue;
                }

                // Skip non-gate lines
                if trimmed.starts_with("wire ")
                    || trimmed.starts_with("supply")
                    || trimmed.starts_with("pullup")
                    || trimmed.starts_with("pulldown")
                    || trimmed.starts_with("reg ")
                {
                    continue;
                }

                // Try to parse gate instantiation
                // Could be multi-line, accumulate until we see ';'
                if !trimmed.starts_with("//") && !trimmed.is_empty() {
                    accum.clear();
                    accum.push_str(trimmed);
                    if !accum.contains(';') {
                        // Multi-line gate instantiation - keep accumulating
                        // (unusual in functional models but handle it)
                        continue;
                    }
                    if let Some(gate) = parse_gate_instantiation(&accum) {
                        gates.push(gate);
                    }
                }
            }
        }
    }

    if module_name.is_empty() {
        return None;
    }

    Some(BehavioralModel {
        module_name,
        inputs: input_ports,
        outputs: output_ports,
        gates,
    })
}

/// Parse module header: `module name ( port1, port2, ... );`
fn parse_module_header(header: &str, name: &mut String, ports: &mut Vec<String>) {
    // Find module name
    let after_module = header.strip_prefix("module ").unwrap_or(header);
    if let Some(paren_pos) = after_module.find('(') {
        *name = after_module[..paren_pos].trim().to_string();

        // Extract ports between ( and )
        let rest = &after_module[paren_pos + 1..];
        if let Some(close) = rest.find(')') {
            let port_str = &rest[..close];
            *ports = port_str
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
        }
    }
}

/// Parse port declarations like `output Y ;` or `input A1;`
fn parse_port_declaration(line: &str, keyword: &str) -> Vec<String> {
    let after_kw = line
        .strip_prefix(keyword)
        .unwrap_or(line)
        .trim()
        .trim_end_matches(';')
        .trim();
    after_kw
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Parse a gate instantiation line like:
/// `or   or0   (or0_out    , A2, A1         );`
/// Returns BehavioralGate or None if not a gate.
fn parse_gate_instantiation(line: &str) -> Option<BehavioralGate> {
    let line = line.trim().trim_end_matches(';').trim();

    // Find the parenthesized port list
    let paren_start = line.find('(')?;
    let paren_end = line.rfind(')')?;

    // Everything before '(' is: gate_type [#delay] instance_name
    let prefix = line[..paren_start].trim();
    let port_list = &line[paren_start + 1..paren_end];

    // Split prefix into tokens
    let tokens: Vec<&str> = prefix.split_whitespace().collect();
    if tokens.is_empty() {
        return None;
    }

    let gate_type = tokens[0].to_string();

    // Skip delay specifications like `UNIT_DELAY
    // (used in DFF models - we skip those cells entirely)

    // Parse port list: output is first, then inputs
    let ports: Vec<String> = port_list
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    if ports.is_empty() {
        return None;
    }

    // Skip named port connections (module instantiations like .A(wire))
    // These are hierarchical cell references, not gate primitives
    if ports[0].starts_with('.') {
        return None;
    }

    let output = ports[0].clone();
    let inputs = ports[1..].to_vec();

    Some(BehavioralGate {
        gate_type,
        output,
        inputs,
    })
}

// ============================================================================
// Parser: UDP truth tables
// ============================================================================

/// Parse a Verilog UDP primitive definition.
pub fn parse_udp(verilog_src: &str) -> Option<UdpModel> {
    let mut name = String::new();
    let mut output_port = String::new();
    let mut input_ports: Vec<String> = Vec::new();
    let mut rows: Vec<UdpRow> = Vec::new();

    enum State {
        LookingForPrimitive,
        InPrimitivePorts,
        InPrimitiveBody,
        InTable,
    }
    let mut state = State::LookingForPrimitive;
    let mut accum = String::new();

    for line in verilog_src.lines() {
        let trimmed = line.trim();

        if trimmed.starts_with("//")
            || trimmed.starts_with("/*")
            || trimmed.starts_with("*/")
            || trimmed.starts_with('*')
            || trimmed.starts_with('`')
            || trimmed.is_empty()
        {
            continue;
        }

        match state {
            State::LookingForPrimitive => {
                if trimmed.starts_with("primitive ") {
                    accum.clear();
                    accum.push_str(trimmed);
                    if accum.contains(");") {
                        parse_primitive_header(&accum, &mut name);
                        state = State::InPrimitiveBody;
                    } else {
                        state = State::InPrimitivePorts;
                    }
                }
            }
            State::InPrimitivePorts => {
                accum.push(' ');
                accum.push_str(trimmed);
                if accum.contains(");") {
                    parse_primitive_header(&accum, &mut name);
                    state = State::InPrimitiveBody;
                }
            }
            State::InPrimitiveBody => {
                if trimmed == "endprimitive" {
                    break;
                }
                if trimmed.starts_with("output ") {
                    output_port = trimmed
                        .strip_prefix("output ")
                        .unwrap()
                        .trim()
                        .trim_end_matches(';')
                        .trim()
                        .to_string();
                } else if trimmed.starts_with("input ") {
                    let ports_str = trimmed
                        .strip_prefix("input ")
                        .unwrap()
                        .trim()
                        .trim_end_matches(';')
                        .trim();
                    // Accumulate input ports (may be on separate lines)
                    for port in ports_str.split(',') {
                        let p = port.trim().to_string();
                        if !p.is_empty() {
                            input_ports.push(p);
                        }
                    }
                } else if trimmed == "table" {
                    state = State::InTable;
                }
            }
            State::InTable => {
                if trimmed == "endtable" {
                    state = State::InPrimitiveBody;
                    continue;
                }

                // Parse table row like: `0   ?   0  :  1   ;`
                let row_str = trimmed.trim_end_matches(';').trim();
                // Remove comment prefix (//  A0  A1  S  :  Y)
                if row_str.starts_with("//") {
                    continue;
                }

                if let Some(colon_pos) = row_str.find(':') {
                    let input_str = row_str[..colon_pos].trim();
                    let output_str = row_str[colon_pos + 1..].trim();

                    let input_vals: Vec<Option<bool>> = input_str
                        .split_whitespace()
                        .map(|s| match s {
                            "0" => Some(false),
                            "1" => Some(true),
                            "?" | "x" | "X" => None,
                            _ => None,
                        })
                        .collect();

                    let output_val = match output_str {
                        "1" => true,
                        "0" => false,
                        _ => continue, // skip 'x' outputs
                    };

                    if input_vals.len() == input_ports.len() {
                        rows.push(UdpRow {
                            inputs: input_vals,
                            output: output_val,
                        });
                    }
                }
            }
        }
    }

    if name.is_empty() {
        return None;
    }

    Some(UdpModel {
        name,
        output: output_port,
        inputs: input_ports,
        rows,
    })
}

fn parse_primitive_header(header: &str, name: &mut String) {
    let after_prim = header.strip_prefix("primitive ").unwrap_or(header);
    if let Some(paren_pos) = after_prim.find('(') {
        *name = after_prim[..paren_pos].trim().to_string();
    }
}

// ============================================================================
// AIG builder from behavioral models
// ============================================================================

/// Evaluate a UDP truth table for specific input values.
/// Returns the output value. Panics if no matching row found.
pub fn eval_udp_for_inputs(udp: &UdpModel, input_vals: &[bool]) -> bool {
    assert_eq!(input_vals.len(), udp.inputs.len());

    for row in &udp.rows {
        let matches = row
            .inputs
            .iter()
            .zip(input_vals.iter())
            .all(|(pattern, &actual)| match pattern {
                None => true,            // don't-care matches anything
                Some(v) => *v == actual, // must match exactly
            });
        if matches {
            return row.output;
        }
    }

    panic!(
        "UDP '{}': no matching row for inputs {:?}",
        udp.name, input_vals
    );
}

// ============================================================================
// DecompResult — the universal output type
// ============================================================================

/// Result of decomposing a cell into AIG operations.
///
/// The decomposition produces a sequence of AND gates that must be built
/// in order, where later gates can reference earlier ones.
#[derive(Debug, Clone)]
pub struct DecompResult {
    /// Sequence of AND gate operations to build.
    /// Each entry is (input_a_iv, input_b_iv) where the lower bit is inversion.
    /// References to earlier gates use negative indices (-1 = first gate output, etc.)
    pub and_gates: Vec<(i64, i64)>,
    /// Index of the final output (-1 = first gate, -2 = second gate, etc.)
    /// Positive values reference original inputs.
    pub output_idx: i64,
    /// Whether to invert the final output
    pub output_inverted: bool,
}

// ============================================================================
// WireVal — internal builder state
// ============================================================================

/// Tagged value for tracking what kind of thing a wire holds during
/// behavioral-model decomposition. Either an AIG pin (real input or
/// AND gate we built (by gate index).
///
/// `pub` so consumer PDK modules (in jacquard core) can construct AIG
/// sub-circuits through the same primitives.
#[derive(Clone, Copy, Debug)]
pub enum WireVal {
    /// An AIG pin with inversion bit (aigpin_iv). Bit 0 = inverted.
    AigPin(usize),
    /// Constant value
    Const(bool),
}

impl WireVal {
    /// Get the aigpin_iv value, creating const-0 = AigPin(0) convention.
    pub fn as_aigpin_iv(self) -> i64 {
        match self {
            WireVal::AigPin(iv) => iv as i64,
            WireVal::Const(false) => 0, // const-0
            WireVal::Const(true) => 1,  // const-1
        }
    }

    /// Invert this wire value.
    pub fn inverted(self) -> Self {
        match self {
            WireVal::AigPin(iv) => WireVal::AigPin(iv ^ 1),
            WireVal::Const(v) => WireVal::Const(!v),
        }
    }
}

// ============================================================================
// AIG construction helpers (GATE_MARKER encoding)
// ============================================================================

/// Marker bit to distinguish gate references from pin references.
/// Gate outputs use bit 30 set. This limits us to ~500M gates (more than enough).
pub(crate) const GATE_MARKER: usize = 1 << 30;

/// Check if an aigpin_iv value is a gate reference.
fn is_gate_ref(aigpin_iv: usize) -> bool {
    aigpin_iv & GATE_MARKER != 0
}

/// Extract gate index from a gate-reference aigpin_iv.
fn gate_ref_index(aigpin_iv: usize) -> usize {
    (aigpin_iv & !GATE_MARKER & !1) >> 1
}

/// Build an AND/NAND/OR/NOR chain over N inputs.
///
/// For AND/NAND: compute AND of all inputs, optionally invert at the end.
/// For OR/NOR: invert all inputs, AND them, optionally invert at the end.
///   OR(a,b,c) = NOT(AND(NOT a, NOT b, NOT c))
///   NOR(a,b,c) = AND(NOT a, NOT b, NOT c)
pub fn build_chain_gate(
    inputs: &[WireVal],
    invert_inputs: bool,
    invert_output: bool,
    and_gates: &mut Vec<(i64, i64)>,
) -> WireVal {
    assert!(inputs.len() >= 2, "Gate must have at least 2 inputs");

    let inputs: Vec<WireVal> = if invert_inputs {
        inputs.iter().map(|v| v.inverted()).collect()
    } else {
        inputs.to_vec()
    };

    // Chain 2-input AND gates
    let mut accum = inputs[0];
    for input in &inputs[1..] {
        let a_ref = accum.as_aigpin_iv();
        let b_ref = input.as_aigpin_iv();
        and_gates.push((a_ref, b_ref));
        let gate_idx = and_gates.len() - 1;
        accum = WireVal::AigPin(GATE_MARKER | (gate_idx << 1));
    }

    if invert_output {
        accum.inverted()
    } else {
        accum
    }
}

/// Build a 2-input XOR: A ^ B = !(!( A & !B) & !(!A & B))
fn build_xor_2(a: WireVal, b: WireVal, and_gates: &mut Vec<(i64, i64)>) -> WireVal {
    let a_iv = a.as_aigpin_iv();
    let b_iv = b.as_aigpin_iv();
    let a_inv_iv = a.inverted().as_aigpin_iv();
    let b_inv_iv = b.inverted().as_aigpin_iv();

    // gate0: A & !B
    and_gates.push((a_iv, b_inv_iv));
    let g0 = and_gates.len() - 1;
    let g0_val = WireVal::AigPin(GATE_MARKER | (g0 << 1));

    // gate1: !A & B
    and_gates.push((a_inv_iv, b_iv));
    let g1 = and_gates.len() - 1;
    let g1_val = WireVal::AigPin(GATE_MARKER | (g1 << 1));

    // gate2: !(A & !B) & !(!A & B)  -- this is XNOR, inverted gives XOR
    let g0_inv_iv = g0_val.inverted().as_aigpin_iv();
    let g1_inv_iv = g1_val.inverted().as_aigpin_iv();
    and_gates.push((g0_inv_iv, g1_inv_iv));
    let g2 = and_gates.len() - 1;
    // XOR = NOT(gate2), so return inverted
    WireVal::AigPin(GATE_MARKER | (g2 << 1) | 1)
}

/// Build XOR/XNOR chain for multi-input gates.
pub fn build_xor_chain(
    inputs: &[WireVal],
    invert_output: bool,
    and_gates: &mut Vec<(i64, i64)>,
) -> WireVal {
    assert!(inputs.len() >= 2);

    let mut accum = inputs[0];
    for input in &inputs[1..] {
        accum = build_xor_2(accum, *input, and_gates);
    }

    if invert_output {
        accum.inverted()
    } else {
        accum
    }
}

/// Build AIG for a UDP instantiation by converting truth table to sum-of-products.
///
/// `pub` so consumer PDK modules (e.g. `gf180mcu_pdk::decompose_with_pdk`)
/// can route their own UDP gate-type prefixes through the same SOP builder.
pub fn build_udp_aig(
    gate: &BehavioralGate,
    wires: &HashMap<String, WireVal>,
    udps: &HashMap<String, UdpModel>,
    and_gates: &mut Vec<(i64, i64)>,
) -> WireVal {
    let udp_name = &gate.gate_type;
    let udp = udps
        .get(udp_name)
        .unwrap_or_else(|| panic!("UDP '{}' not found in loaded models", udp_name));

    // Get input wire values
    let input_vals: Vec<WireVal> = gate
        .inputs
        .iter()
        .map(|name| {
            wires
                .get(name)
                .copied()
                .unwrap_or_else(|| panic!("Unknown wire '{}' in UDP '{}'", name, udp_name))
        })
        .collect();

    assert_eq!(
        input_vals.len(),
        udp.inputs.len(),
        "UDP '{}' expects {} inputs, got {}",
        udp_name,
        udp.inputs.len(),
        input_vals.len()
    );

    // Build sum-of-products from truth table rows where output=1
    // Each row with output=1 becomes a product (AND) term.
    // Product terms are ORed together.
    //
    // For rows where output=0, we don't need to do anything explicitly.
    // Don't-care (?) inputs are omitted from the product term.

    let one_rows: Vec<&UdpRow> = udp.rows.iter().filter(|r| r.output).collect();

    if one_rows.is_empty() {
        // Output is always 0
        return WireVal::Const(false);
    }

    // Build each product term
    let mut product_terms: Vec<WireVal> = Vec::new();

    for row in &one_rows {
        // Collect non-don't-care inputs for this product term
        let mut term_inputs: Vec<WireVal> = Vec::new();
        for (i, pattern) in row.inputs.iter().enumerate() {
            match pattern {
                Some(true) => term_inputs.push(input_vals[i]),
                Some(false) => term_inputs.push(input_vals[i].inverted()),
                None => {} // don't-care - omit from product
            }
        }

        if term_inputs.is_empty() {
            // All inputs are don't-care: output is unconditionally 1
            return WireVal::Const(true);
        }

        if term_inputs.len() == 1 {
            product_terms.push(term_inputs[0]);
        } else {
            // Build AND chain for this product term
            let product = build_chain_gate(&term_inputs, false, false, and_gates);
            product_terms.push(product);
        }
    }

    if product_terms.len() == 1 {
        return product_terms[0];
    }

    // OR the product terms: OR(a,b,...) = NOT(AND(NOT a, NOT b, ...))
    build_chain_gate(&product_terms, true, true, and_gates)
}

// ============================================================================
// DecompResult conversion: GATE_MARKER encoding -> standard negative-index encoding
// ============================================================================

/// Post-process a DecompResult built with GATE_MARKER encoding to use
/// standard negative-index encoding for the and_gates references.
pub fn finalize_decomp_result(and_gates: Vec<(i64, i64)>, output: WireVal) -> DecompResult {
    // Convert gate references in and_gates from GATE_MARKER to negative indices
    let converted_gates: Vec<(i64, i64)> = and_gates
        .iter()
        .map(|(a, b)| (convert_ref_to_standard(*a), convert_ref_to_standard(*b)))
        .collect();

    match output {
        WireVal::AigPin(iv) if is_gate_ref(iv) => {
            let gate_idx = gate_ref_index(iv);
            let inverted = (iv & 1) != 0;
            DecompResult {
                and_gates: converted_gates,
                output_idx: -(gate_idx as i64) - 1,
                output_inverted: inverted,
            }
        }
        WireVal::AigPin(iv) => {
            let pin_idx = iv >> 1;
            let inverted = (iv & 1) != 0;
            DecompResult {
                and_gates: converted_gates,
                output_idx: pin_idx as i64,
                output_inverted: inverted,
            }
        }
        WireVal::Const(v) => DecompResult {
            and_gates: converted_gates,
            output_idx: 0,
            output_inverted: v,
        },
    }
}

/// Convert a single reference value from GATE_MARKER encoding to standard.
fn convert_ref_to_standard(ref_val: i64) -> i64 {
    let uval = ref_val as usize;
    if is_gate_ref(uval) {
        let gate_idx = gate_ref_index(uval);
        let inverted = (uval & 1) != 0;
        let base = -((gate_idx as i64) * 2 + 1);
        if inverted {
            base ^ 1
        } else {
            base
        }
    } else {
        ref_val
    }
}

// ============================================================================
// Gate-level evaluator (for testing)
// ============================================================================

/// Directly evaluate a behavioral model's gate network for given input values.
/// This doesn't go through AIG - it directly interprets the Verilog gates.
/// Used as a reference oracle in tests.
pub fn eval_behavioral_model(
    model: &BehavioralModel,
    input_values: &HashMap<String, bool>,
    output_pin: &str,
    udps: &HashMap<String, UdpModel>,
) -> bool {
    let mut wires: HashMap<String, bool> = HashMap::new();

    // Set input values
    for (name, &val) in input_values {
        wires.insert(name.clone(), val);
    }

    // Evaluate gates in order
    for gate in &model.gates {
        let gate_type = gate.gate_type.as_str();

        if gate_type == "buf" {
            let v = wires[&gate.inputs[0]];
            wires.insert(gate.output.clone(), v);
            continue;
        }

        // PDK-neutral UDP dispatch: any gate whose type is a known UDP in
        // the model's `udps` map (regardless of vendor prefix —
        // `sky130_fd_sc_hd__udp_*`, `UDP_GF018hv5v_mcu_sc7_*`, etc.) is
        // evaluated via its truth table. Standard Verilog primitives fall
        // through to the gate match below.
        if let Some(udp) = udps.get(gate_type) {
            let input_vals: Vec<bool> = gate.inputs.iter().map(|name| wires[name]).collect();
            let result = eval_udp_for_inputs(udp, &input_vals);
            wires.insert(gate.output.clone(), result);
            continue;
        }

        let input_vals: Vec<bool> =
            gate.inputs.iter().map(|name| {
                *wires.get(name).unwrap_or_else(|| {
                    panic!(
                        "Wire '{}' not found in model '{}' at gate '{}' (type '{}'). Available wires: {:?}",
                        name, model.module_name, gate.output, gate_type,
                        wires.keys().collect::<Vec<_>>()
                    )
                })
            }).collect();

        let result = match gate_type {
            "not" => !input_vals[0],
            "and" => input_vals.iter().all(|&v| v),
            "nand" => !input_vals.iter().all(|&v| v),
            "or" => input_vals.iter().any(|&v| v),
            "nor" => !input_vals.iter().any(|&v| v),
            "xor" => input_vals.iter().fold(false, |acc, &v| acc ^ v),
            "xnor" => !input_vals.iter().fold(false, |acc, &v| acc ^ v),
            _ => panic!("Unknown gate type: {}", gate_type),
        };

        wires.insert(gate.output.clone(), result);
    }

    wires[output_pin]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sentinel: a regression catching any future change that breaks
    /// PDK-neutrality of the behavioural parser. Mirrors the recon
    /// tests in `gf180mcu_pdk::tests` but goes through the neutral
    /// crate path callers are expected to use.
    #[test]
    fn parser_reachable_via_neutral_module() {
        let src = "module tiny( A, Y );\ninput A;\noutput Y;\n\tnot u(Y, A);\nendmodule\n";
        let m: BehavioralModel = parse_functional_model(src).expect("parse");
        assert_eq!(m.module_name, "tiny");
        assert_eq!(m.gates.len(), 1);
        assert_eq!(m.gates[0].gate_type, "not");
    }
}
