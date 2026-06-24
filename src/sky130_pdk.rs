// SPDX-FileCopyrightText: Copyright (c) 2024 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0
//! SKY130 PDK behavioral model parser and AIG decomposition.
//!
//! Parses gate-level functional Verilog models from the official SKY130 PDK
//! (google/skywater-pdk-libs-sky130_fd_sc_hd) and converts them to AIG
//! decompositions. This replaces hand-coded decompositions with vendor-verified
//! reference models.
//!
//! The functional models use only Verilog gate primitives:
//!   and, or, nand, nor, not, xor, xnor, buf
//! Some cells (mux2, mux2i, mux4) use UDP truth tables instead.

use std::collections::HashMap;
use std::path::Path;

// PDK-neutral behavioural parsing + AIG-decomposition primitives now live
// in the standalone `cell_decomp` crate. Bring in everything the staying
// `decompose_*` / `load_*` code (and the test suite) references so their
// existing call shape is preserved.
use cell_decomp::{
    build_chain_gate, build_udp_aig, build_xor_chain, finalize_decomp_result,
    parse_functional_model, parse_udp, BehavioralModel, UdpModel, WireVal,
};

// ============================================================================
// Core types (formerly in sky130_decomp.rs)
// ============================================================================

/// Fixed-size struct for collecting input pins during SKY130 decomposition.
/// Most SKY130 cells have at most 5 inputs. Using a fixed struct avoids heap allocation.
#[derive(Default, Clone, Copy, Debug)]
pub struct CellInputs {
    pub a: usize,
    pub a_n: usize,
    pub a0: usize,
    pub a1: usize,
    pub a1_n: usize,
    pub a2: usize,
    pub a2_n: usize,
    pub a3: usize,
    pub a4: usize,
    pub b: usize,
    pub b_n: usize,
    pub b1: usize,
    pub b1_n: usize,
    pub b2: usize,
    pub c: usize,
    pub c_n: usize,
    pub c1: usize,
    pub c2: usize,
    pub d: usize,
    pub d_n: usize,
    pub d1: usize,
    pub s: usize,
    pub s0: usize,
    pub s1: usize,
    pub cin: usize,
    pub set_b: usize,
    pub reset_b: usize,
    pub sleep: usize,
    pub sleep_b: usize,
}

impl CellInputs {
    /// Create a new CellInputs with all pins set to MAX (unset).
    #[inline]
    pub fn new() -> Self {
        Self {
            a: usize::MAX,
            a_n: usize::MAX,
            a0: usize::MAX,
            a1: usize::MAX,
            a1_n: usize::MAX,
            a2: usize::MAX,
            a2_n: usize::MAX,
            a3: usize::MAX,
            a4: usize::MAX,
            b: usize::MAX,
            b_n: usize::MAX,
            b1: usize::MAX,
            b1_n: usize::MAX,
            b2: usize::MAX,
            c: usize::MAX,
            c_n: usize::MAX,
            c1: usize::MAX,
            c2: usize::MAX,
            d: usize::MAX,
            d_n: usize::MAX,
            d1: usize::MAX,
            s: usize::MAX,
            s0: usize::MAX,
            s1: usize::MAX,
            cin: usize::MAX,
            set_b: usize::MAX,
            reset_b: usize::MAX,
            sleep: usize::MAX,
            sleep_b: usize::MAX,
        }
    }

    /// Set a pin value by name. Returns true if the pin was recognized.
    #[inline]
    pub fn set_pin(&mut self, pin_name: &str, value: usize) -> bool {
        match pin_name {
            "A" => self.a = value,
            "A_N" => self.a_n = value,
            "A0" => self.a0 = value,
            "A1" => self.a1 = value,
            "A1_N" => self.a1_n = value,
            "A2" => self.a2 = value,
            "A2_N" => self.a2_n = value,
            "A3" => self.a3 = value,
            "A4" => self.a4 = value,
            "B" => self.b = value,
            "B_N" => self.b_n = value,
            "B1" => self.b1 = value,
            "B1_N" => self.b1_n = value,
            "B2" => self.b2 = value,
            "C" => self.c = value,
            "C_N" => self.c_n = value,
            "C1" => self.c1 = value,
            "C2" => self.c2 = value,
            "D" => self.d = value,
            "D_N" => self.d_n = value,
            "D1" => self.d1 = value,
            "S" => self.s = value,
            "S0" => self.s0 = value,
            "S1" => self.s1 = value,
            "CIN" => self.cin = value,
            "SET_B" => self.set_b = value,
            "RESET_B" => self.reset_b = value,
            "SLEEP" => self.sleep = value,
            "SLEEP_B" => self.sleep_b = value,
            _ => return false,
        }
        true
    }
}

// `DecompResult` is now defined in the `cell_decomp` crate; re-export it
// here so existing `crate::sky130_pdk::DecompResult` call sites continue to
// work unchanged.
pub use cell_decomp::DecompResult;

/// Check if a cell type is a sequential element (DFF or latch).
///
/// This is the exhaustive list of sky130_fd_sc_hd cells containing DFF or
/// latch UDPs in their behavioral Verilog models. Derived from the PDK by
/// grepping for `udp_dff` and `udp_dlatch` primitives in behavioral.v files.
///
/// IMPORTANT: Do NOT use prefix matching here — `dlygate*` and `dlymetal*`
/// are combinational delay buffers that happen to start with "dl".
const SKY130_SEQUENTIAL_CELLS: &[&str] = &[
    // D flip-flops
    "dfbbn",
    "dfbbp",
    "dfrbp",
    "dfrtn",
    "dfrtp",
    "dfsbp",
    "dfstp",
    "dfxbp",
    "dfxtp",
    // Latches and clock-gating latches
    "dlclkp",
    "dlrbn",
    "dlrbp",
    "dlrtn",
    "dlrtp",
    "dlxbn",
    "dlxbp",
    "dlxtn",
    "dlxtp",
    // Enable D flip-flops
    "edfxbp",
    "edfxtp",
    // Low-power isolation latch
    "lpflow_inputisolatch",
    // Scan D flip-flops
    "sdfbbn",
    "sdfbbp",
    "sdfrbp",
    "sdfrtn",
    "sdfrtp",
    "sdfsbp",
    "sdfstp",
    "sdfxbp",
    "sdfxtp",
    // Scan clock-gating latch
    "sdlclkp",
    // Scan enable D flip-flops
    "sedfxbp",
    "sedfxtp",
];

pub fn is_sequential_cell(cell_type: &str) -> bool {
    SKY130_SEQUENTIAL_CELLS.contains(&cell_type)
}

/// Check if a cell is a tie cell (constant generator).
pub fn is_tie_cell(cell_type: &str) -> bool {
    cell_type == "conb"
}

/// Check if a cell has multiple outputs (like adders).
pub fn is_multi_output_cell(cell_type: &str) -> bool {
    matches!(cell_type, "ha" | "fa" | "dfbbp")
}

// ============================================================================
// Data structures
// ============================================================================

// `BehavioralGate`, `BehavioralModel`, `UdpRow`, and `UdpModel` are now
// defined in the `cell_decomp` crate; they are imported above so existing
// call sites continue to work unchanged.

/// Collection of loaded PDK models for a cell library.
pub struct PdkModels {
    /// Behavioral models indexed by cell type (e.g. "o21ai" -> model)
    pub models: HashMap<String, BehavioralModel>,
    /// UDP models indexed by primitive name
    pub udps: HashMap<String, UdpModel>,
}

// ============================================================================
// AIG builder from behavioral models
// ============================================================================

// The functional-model and UDP parsers (`parse_functional_model`,
// `parse_udp`, and their helpers), the `eval_udp_for_inputs` truth-table
// evaluator, and the AIG-builder primitives (`WireVal`, `build_chain_gate`,
// `build_xor_chain`, `build_udp_aig`, `finalize_decomp_result`) all now
// live in the `cell_decomp` crate; they are imported at the top of this
// module so the local `decompose_*` builders keep their existing call shape.

/// Convert a parsed behavioral model to an AIG decomposition for a specific output pin.
///
/// # Arguments
/// * `model` - The parsed behavioral model
/// * `cell_inputs` - CellInputs with aigpin_iv values for each module input
/// * `output_pin` - Which output pin to build the logic cone for (e.g. "Y", "SUM", "COUT")
/// * `udps` - Available UDP models for resolving UDP instantiations
///
/// # Returns
/// A `DecompResult` describing the AIG for this output
pub fn decompose_from_behavioral(
    model: &BehavioralModel,
    cell_inputs: &CellInputs,
    output_pin: &str,
    udps: &HashMap<String, UdpModel>,
) -> DecompResult {
    // Map wire names to their values
    let mut wires: HashMap<String, WireVal> = HashMap::new();

    // Map module input port names to their aigpin_iv values from CellInputs
    for input_name in &model.inputs {
        let aigpin_iv = get_cell_input_by_name(cell_inputs, input_name);
        assert_ne!(
            aigpin_iv,
            usize::MAX,
            "Input pin '{}' not set in CellInputs for cell '{}'",
            input_name,
            model.module_name
        );
        wires.insert(input_name.clone(), WireVal::AigPin(aigpin_iv));
    }

    // Process gates in order (they're topologically sorted in the Verilog)
    let mut and_gates: Vec<(i64, i64)> = Vec::new();
    // Map from gate output index to AND gate index in and_gates
    // gate_idx_for_wire[wire_name] = index into and_gates (0-based)

    for gate in &model.gates {
        let gate_type = gate.gate_type.as_str();

        // Skip buf gates - they're just passthroughs
        if gate_type == "buf" {
            assert_eq!(gate.inputs.len(), 1, "buf must have exactly 1 input");
            let input_val = wires
                .get(&gate.inputs[0])
                .copied()
                .unwrap_or_else(|| panic!("Unknown wire '{}' in buf gate", gate.inputs[0]));
            wires.insert(gate.output.clone(), input_val);
            continue;
        }

        // Handle UDP instantiations
        if gate_type.starts_with("sky130_fd_sc_hd__udp_") {
            let result = build_udp_aig(gate, &wires, udps, &mut and_gates);
            wires.insert(gate.output.clone(), result);
            continue;
        }

        // Resolve all input wires
        let input_vals: Vec<WireVal> = gate
            .inputs
            .iter()
            .map(|name| {
                wires
                    .get(name)
                    .copied()
                    .unwrap_or_else(|| panic!("Unknown wire '{}' in {} gate", name, gate_type))
            })
            .collect();

        let result = match gate_type {
            "not" => {
                assert_eq!(input_vals.len(), 1);
                input_vals[0].inverted()
            }
            "and" => build_chain_gate(&input_vals, false, false, &mut and_gates),
            "nand" => build_chain_gate(&input_vals, false, true, &mut and_gates),
            "or" => build_chain_gate(&input_vals, true, true, &mut and_gates),
            "nor" => build_chain_gate(&input_vals, true, false, &mut and_gates),
            "xor" => build_xor_chain(&input_vals, false, &mut and_gates),
            "xnor" => build_xor_chain(&input_vals, true, &mut and_gates),
            _ => panic!(
                "Unknown gate type '{}' in model '{}'",
                gate_type, model.module_name
            ),
        };

        wires.insert(gate.output.clone(), result);
    }

    // Get the output wire value
    let output_val = wires
        .get(output_pin)
        .copied()
        .unwrap_or_else(|| panic!("Output pin '{}' not found in model", output_pin));

    // Convert to DecompResult
    match output_val {
        WireVal::AigPin(iv) => {
            let pin_idx = iv >> 1;
            let inverted = (iv & 1) != 0;
            DecompResult {
                and_gates,
                output_idx: pin_idx as i64,
                output_inverted: inverted,
            }
        }
        WireVal::Const(v) => {
            // Constant output: use pin index 0 which is const-0
            DecompResult {
                and_gates,
                output_idx: 0,
                output_inverted: v,
            }
        }
    }
}

// `build_chain_gate`, `GATE_MARKER`, `build_xor_chain`, `build_udp_aig`
// (and their internal helpers `is_gate_ref`, `gate_ref_index`,
// `build_xor_2`) now live in the `cell_decomp` crate.

/// Map a CellInputs pin name to its aigpin_iv value.
fn get_cell_input_by_name(inputs: &CellInputs, name: &str) -> usize {
    match name {
        "A" => inputs.a,
        "A_N" => inputs.a_n,
        "A0" => inputs.a0,
        "A1" => inputs.a1,
        "A1_N" => inputs.a1_n,
        "A2" => inputs.a2,
        "A2_N" => inputs.a2_n,
        "A3" => inputs.a3,
        "A4" => inputs.a4,
        "B" => inputs.b,
        "B_N" => inputs.b_n,
        "B1" => inputs.b1,
        "B1_N" => inputs.b1_n,
        "B2" => inputs.b2,
        "C" => inputs.c,
        "C_N" => inputs.c_n,
        "C1" => inputs.c1,
        "C2" => inputs.c2,
        "D" => inputs.d,
        "D_N" => inputs.d_n,
        "D1" => inputs.d1,
        "S" => inputs.s,
        "S0" => inputs.s0,
        "S1" => inputs.s1,
        "CIN" => inputs.cin,
        "SET_B" => inputs.set_b,
        "RESET_B" => inputs.reset_b,
        "SLEEP" => inputs.sleep,
        "SLEEP_B" => inputs.sleep_b,
        _ => usize::MAX,
    }
}

// `finalize_decomp_result` and the internal `convert_ref_to_standard`
// helper now live in the `cell_decomp` crate.

// ============================================================================
// Model loading
// ============================================================================

/// Validate that all gate inputs in a model reference known wires.
/// Returns false if any gate references a wire that isn't a module input
/// or the output of a previous gate.
fn validate_model(model: &BehavioralModel) -> bool {
    let mut known_wires: std::collections::HashSet<&str> = std::collections::HashSet::new();

    // Module inputs are known
    for name in &model.inputs {
        known_wires.insert(name.as_str());
    }

    for gate in &model.gates {
        // Check all inputs are known
        for input_name in &gate.inputs {
            if !known_wires.contains(input_name.as_str()) {
                return false;
            }
        }
        // Gate output becomes known
        known_wires.insert(gate.output.as_str());
    }

    // Check that all declared outputs are produced
    for output_name in &model.outputs {
        if !known_wires.contains(output_name.as_str()) {
            return false;
        }
    }

    true
}

/// Load all PDK models needed for a set of cell types.
///
/// # Arguments
/// * `pdk_cells_path` - Path to the `cells/` directory of the PDK
/// * `cell_types` - Set of cell type names that need models (e.g. ["o21ai", "nand2", ...])
///
/// # Returns
/// A PdkModels containing all loaded models and UDPs.
pub fn load_pdk_models(pdk_cells_path: &Path, cell_types: &[String]) -> PdkModels {
    let mut models = HashMap::new();
    let mut udps: HashMap<String, UdpModel> = HashMap::new();

    // Derive models_path from cells_path: ../models/
    let models_path = pdk_cells_path.parent().unwrap().join("models");

    for cell_type in cell_types {
        // Skip sequential and tie cells - handled elsewhere
        if is_sequential_cell(cell_type) || is_tie_cell(cell_type) {
            continue;
        }

        let cell_dir = pdk_cells_path.join(cell_type);
        let func_file = cell_dir.join(format!("sky130_fd_sc_hd__{}.functional.v", cell_type));

        if !func_file.exists() {
            clilog::warn!(
                "PDK functional model not found for cell '{}': {}",
                cell_type,
                func_file.display()
            );
            continue;
        }

        let src = std::fs::read_to_string(&func_file)
            .unwrap_or_else(|e| panic!("Failed to read PDK model {}: {}", func_file.display(), e));

        if let Some(model) = parse_functional_model(&src) {
            // Validate: check that all gate inputs reference known wires
            // (module inputs, or outputs of earlier gates)
            let valid = validate_model(&model);
            if !valid {
                clilog::debug!(
                    "Skipping PDK model '{}': has unresolvable wire references (macro cell?)",
                    cell_type
                );
                continue;
            }

            // Check if the model uses any UDPs and load them
            for gate in &model.gates {
                if gate.gate_type.starts_with("sky130_fd_sc_hd__udp_") {
                    let udp_name = &gate.gate_type;
                    if !udps.contains_key(udp_name) {
                        if let Some(udp) = load_udp(&models_path, udp_name) {
                            udps.insert(udp_name.clone(), udp);
                        }
                    }
                }
            }
            models.insert(cell_type.clone(), model);
        } else {
            clilog::warn!(
                "Failed to parse PDK functional model for cell '{}'",
                cell_type
            );
        }
    }

    clilog::info!(
        "Loaded {} PDK cell models and {} UDP models",
        models.len(),
        udps.len()
    );

    PdkModels { models, udps }
}

/// Load a single UDP model from the models directory.
fn load_udp(models_path: &Path, udp_name: &str) -> Option<UdpModel> {
    // UDP name format: sky130_fd_sc_hd__udp_mux_2to1_N
    // Directory format: udp_mux_2to1_n (lowercase)
    // File: sky130_fd_sc_hd__udp_mux_2to1_n.v (note: lowercase in filename sometimes differs)

    // Extract the udp part after "sky130_fd_sc_hd__"
    let udp_suffix = udp_name
        .strip_prefix("sky130_fd_sc_hd__")
        .unwrap_or(udp_name);

    // The directory name is lowercase version of the suffix
    let dir_name = udp_suffix.to_lowercase();
    let udp_dir = models_path.join(&dir_name);

    if !udp_dir.exists() {
        clilog::warn!("UDP directory not found: {}", udp_dir.display());
        return None;
    }

    // Find the .v file (not .blackbox.v or .tb.v)
    let udp_file = udp_dir.join(format!("sky130_fd_sc_hd__{}.v", dir_name));

    if !udp_file.exists() {
        clilog::warn!("UDP file not found: {}", udp_file.display());
        return None;
    }

    let src = std::fs::read_to_string(&udp_file)
        .unwrap_or_else(|e| panic!("Failed to read UDP file {}: {}", udp_file.display(), e));

    parse_udp(&src)
}

/// Decompose a cell using PDK models.
///
/// This is the main entry point for PDK-based decomposition. Panics if no
/// PDK model is available for the given cell type.
pub fn decompose_with_pdk(
    cell_type: &str,
    inputs: &CellInputs,
    output_pin: &str,
    pdk: &PdkModels,
) -> DecompResult {
    let model = pdk.models.get(cell_type).unwrap_or_else(|| {
        panic!(
            "No PDK model found for cell type '{}'. Ensure the sky130_fd_sc_hd submodule is \
             initialized (git submodule update --init) and the cell model exists.",
            cell_type
        )
    });

    let mut and_gates = Vec::new();
    let mut wires: HashMap<String, WireVal> = HashMap::new();

    // Map module input port names to their aigpin_iv values
    for input_name in &model.inputs {
        let aigpin_iv = get_cell_input_by_name(inputs, input_name);
        if aigpin_iv == usize::MAX {
            panic!(
                "Input pin '{}' not set in CellInputs for cell type '{}'",
                input_name, cell_type
            );
        }
        wires.insert(input_name.clone(), WireVal::AigPin(aigpin_iv));
    }

    // Process gates in order
    for gate in &model.gates {
        let gate_type = gate.gate_type.as_str();

        if gate_type == "buf" {
            assert_eq!(gate.inputs.len(), 1);
            let input_val = wires
                .get(&gate.inputs[0])
                .copied()
                .unwrap_or_else(|| panic!("Unknown wire '{}' in buf gate", gate.inputs[0]));
            wires.insert(gate.output.clone(), input_val);
            continue;
        }

        if gate_type.starts_with("sky130_fd_sc_hd__udp_") {
            let result = build_udp_aig(gate, &wires, &pdk.udps, &mut and_gates);
            wires.insert(gate.output.clone(), result);
            continue;
        }

        let input_vals: Vec<WireVal> = gate
            .inputs
            .iter()
            .map(|name| {
                wires
                    .get(name)
                    .copied()
                    .unwrap_or_else(|| panic!("Unknown wire '{}' in {} gate", name, gate_type))
            })
            .collect();

        let result = match gate_type {
            "not" => {
                assert_eq!(input_vals.len(), 1);
                input_vals[0].inverted()
            }
            "and" => build_chain_gate(&input_vals, false, false, &mut and_gates),
            "nand" => build_chain_gate(&input_vals, false, true, &mut and_gates),
            "or" => build_chain_gate(&input_vals, true, true, &mut and_gates),
            "nor" => build_chain_gate(&input_vals, true, false, &mut and_gates),
            "xor" => build_xor_chain(&input_vals, false, &mut and_gates),
            "xnor" => build_xor_chain(&input_vals, true, &mut and_gates),
            _ => panic!(
                "Unknown gate type '{}' in model '{}'",
                gate_type, model.module_name
            ),
        };

        wires.insert(gate.output.clone(), result);
    }

    // Get the requested output
    let output_val = wires.get(output_pin).copied().unwrap_or_else(|| {
        panic!(
            "Output pin '{}' not found in model '{}'. Available: {:?}",
            output_pin, model.module_name, model.outputs
        )
    });

    finalize_decomp_result(and_gates, output_val)
}

// `eval_behavioral_model` (the gate-level reference oracle used in tests)
// now lives in the `cell_decomp` crate; it is imported inside the
// `#[cfg(test)]` block below so the test suite keeps its existing call shape.

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    // Reference-oracle evaluators used only by the test suite; they live in
    // the `cell_decomp` crate alongside the parsers they exercise.
    use cell_decomp::{eval_behavioral_model, eval_udp_for_inputs};

    #[test]
    fn test_parse_o21ai() {
        let src = std::fs::read_to_string(
            "vendor/sky130_fd_sc_hd/cells/o21ai/sky130_fd_sc_hd__o21ai.functional.v",
        )
        .unwrap();
        let model = parse_functional_model(&src).unwrap();
        assert_eq!(model.module_name, "sky130_fd_sc_hd__o21ai");
        assert_eq!(model.inputs, vec!["A1", "A2", "B1"]);
        assert_eq!(model.outputs, vec!["Y"]);
        assert_eq!(model.gates.len(), 3); // or, nand, buf
        assert_eq!(model.gates[0].gate_type, "or");
        assert_eq!(model.gates[1].gate_type, "nand");
        assert_eq!(model.gates[2].gate_type, "buf");
    }

    #[test]
    fn test_parse_ha() {
        let src = std::fs::read_to_string(
            "vendor/sky130_fd_sc_hd/cells/ha/sky130_fd_sc_hd__ha.functional.v",
        )
        .unwrap();
        let model = parse_functional_model(&src).unwrap();
        assert_eq!(model.module_name, "sky130_fd_sc_hd__ha");
        assert_eq!(model.inputs, vec!["A", "B"]);
        assert_eq!(model.outputs, vec!["COUT", "SUM"]);
        assert_eq!(model.gates.len(), 4); // and, buf, xor, buf
    }

    #[test]
    fn test_parse_mux2i_udp() {
        let src = std::fs::read_to_string(
            "vendor/sky130_fd_sc_hd/cells/mux2i/sky130_fd_sc_hd__mux2i.functional.v",
        )
        .unwrap();
        let model = parse_functional_model(&src).unwrap();
        assert_eq!(model.module_name, "sky130_fd_sc_hd__mux2i");
        assert_eq!(model.inputs, vec!["A0", "A1", "S"]);
        assert_eq!(model.outputs, vec!["Y"]);
        // Should have UDP instantiation + buf
        assert_eq!(model.gates.len(), 2);
        assert!(model.gates[0]
            .gate_type
            .starts_with("sky130_fd_sc_hd__udp_mux_2to1"));
    }

    #[test]
    fn test_parse_udp_mux_2to1_n() {
        let src = std::fs::read_to_string(
            "vendor/sky130_fd_sc_hd/models/udp_mux_2to1_n/sky130_fd_sc_hd__udp_mux_2to1_n.v",
        )
        .unwrap();
        let udp = parse_udp(&src).unwrap();
        assert_eq!(udp.inputs, vec!["A0", "A1", "S"]);
        assert_eq!(udp.output, "Y");
        assert_eq!(udp.rows.len(), 6);

        // Verify: A0=0, S=0 -> Y=1 (inverted mux)
        assert!(eval_udp_for_inputs(&udp, &[false, false, false]));
        // A0=1, S=0 -> Y=0
        assert!(!eval_udp_for_inputs(&udp, &[true, false, false]));
        // A1=0, S=1 -> Y=1
        assert!(eval_udp_for_inputs(&udp, &[false, false, true]));
        // A1=1, S=1 -> Y=0
        assert!(!eval_udp_for_inputs(&udp, &[false, true, true]));
    }

    /// Helper: set up CellInputs and a bool->aigpin mapping for testing.
    /// Returns (CellInputs, input_name_to_bool_map) for the given model.
    fn setup_test_inputs(
        model: &BehavioralModel,
        values: &[bool],
    ) -> (CellInputs, HashMap<String, bool>) {
        assert_eq!(model.inputs.len(), values.len());
        let mut cell_inputs = CellInputs::new();
        let mut bool_map = HashMap::new();

        for (i, (name, &val)) in model.inputs.iter().zip(values.iter()).enumerate() {
            // Assign aigpin (i+1) to each input
            // aigpin_iv = ((i+1) << 1) | 0  for non-inverted
            let aigpin_iv = (i + 1) << 1;
            cell_inputs.set_pin(name, aigpin_iv);
            bool_map.insert(name.clone(), val);
        }

        (cell_inputs, bool_map)
    }

    /// Evaluate a DecompResult with concrete boolean inputs.
    /// `pin_values` maps aigpin (not aigpin_iv) to bool.
    fn eval_decomp_bool(decomp: &DecompResult, pin_values: &HashMap<usize, bool>) -> bool {
        let mut gate_outputs: Vec<bool> = Vec::new();

        for (a_ref, b_ref) in &decomp.and_gates {
            let a_val = resolve_decomp_ref_bool(*a_ref, pin_values, &gate_outputs);
            let b_val = resolve_decomp_ref_bool(*b_ref, pin_values, &gate_outputs);
            gate_outputs.push(a_val && b_val);
        }

        let output = if decomp.output_idx < 0 {
            let gate_idx = (-decomp.output_idx - 1) as usize;
            gate_outputs[gate_idx]
        } else {
            let aigpin = decomp.output_idx as usize;
            *pin_values
                .get(&aigpin)
                .unwrap_or_else(|| panic!("Pin {} not found in values map", aigpin))
        };

        if decomp.output_inverted {
            !output
        } else {
            output
        }
    }

    fn resolve_decomp_ref_bool(
        ref_val: i64,
        pin_values: &HashMap<usize, bool>,
        gate_outputs: &[bool],
    ) -> bool {
        if ref_val < 0 {
            let abs_ref = -ref_val;
            let gate_idx = ((abs_ref - 1) / 2) as usize;
            let inverted = (abs_ref % 2) == 0;
            let val = gate_outputs[gate_idx];
            if inverted {
                !val
            } else {
                val
            }
        } else {
            // ref_val is aigpin_iv: pin = ref_val >> 1, inverted = ref_val & 1
            let aigpin = (ref_val >> 1) as usize;
            let inverted = (ref_val & 1) != 0;
            let val = *pin_values.get(&aigpin).unwrap_or_else(|| {
                panic!(
                    "Pin {} not found in values map (ref_val={})",
                    aigpin, ref_val
                )
            });
            if inverted {
                !val
            } else {
                val
            }
        }
    }

    /// Load all PDK models and UDPs from the submodule.
    fn load_test_pdk() -> PdkModels {
        let pdk_path = Path::new("vendor/sky130_fd_sc_hd/cells");
        if !pdk_path.exists() {
            panic!("sky130_fd_sc_hd submodule not initialized. Run: git submodule update --init");
        }

        // Get all cell types
        let mut cell_types: Vec<String> = Vec::new();
        for entry in std::fs::read_dir(pdk_path).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                cell_types.push(entry.file_name().to_string_lossy().to_string());
            }
        }
        cell_types.sort();

        load_pdk_models(pdk_path, &cell_types)
    }

    /// Check if all input pins of a model are recognized by CellInputs.
    fn all_inputs_supported(model: &BehavioralModel) -> bool {
        let supported = [
            "A", "A_N", "A0", "A1", "A1_N", "A2", "A2_N", "A3", "A4", "B", "B_N", "B1", "B1_N",
            "B2", "C", "C_N", "C1", "C2", "D", "D_N", "D1", "S", "S0", "S1", "CIN", "SET_B",
            "RESET_B", "SLEEP", "SLEEP_B",
        ];
        model
            .inputs
            .iter()
            .all(|name| supported.contains(&name.as_str()))
    }

    /// Exhaustive truth-table test for all combinational cells with supported pins.
    /// For each cell: evaluate all 2^N input combinations through both
    /// the AIG decomposition and the direct gate evaluator, verify they match.
    #[test]
    fn test_all_cells_vs_pdk() {
        let pdk = load_test_pdk();

        let mut tested = 0;
        let mut skipped_inputs = 0;
        let mut skipped_size = 0;

        // Sort keys for deterministic output
        let mut cell_types: Vec<&String> = pdk.models.keys().collect();
        cell_types.sort();

        for cell_type in cell_types {
            let model = &pdk.models[cell_type];
            let num_inputs = model.inputs.len();

            // Skip cells with unsupported input pin names
            if !all_inputs_supported(model) {
                skipped_inputs += 1;
                continue;
            }

            // Skip cells with too many inputs for exhaustive testing (> 8 inputs)
            if num_inputs > 8 {
                skipped_size += 1;
                continue;
            }

            let num_combos = 1u32 << num_inputs;

            for output_pin in &model.outputs {
                for combo in 0..num_combos {
                    // Convert combo to input booleans
                    let input_bools: Vec<bool> =
                        (0..num_inputs).map(|i| ((combo >> i) & 1) != 0).collect();

                    // Set up CellInputs
                    let (cell_inputs, bool_map) = setup_test_inputs(model, &input_bools);

                    // Evaluate via direct gate interpretation (reference)
                    let expected = eval_behavioral_model(model, &bool_map, output_pin, &pdk.udps);

                    // Evaluate via AIG decomposition
                    let decomp = decompose_with_pdk(cell_type, &cell_inputs, output_pin, &pdk);

                    // Build pin_values map for decomp evaluator
                    let mut pin_values: HashMap<usize, bool> = HashMap::new();
                    pin_values.insert(0, false); // const-0
                    for (i, &val) in input_bools.iter().enumerate() {
                        pin_values.insert(i + 1, val);
                    }

                    let actual = eval_decomp_bool(&decomp, &pin_values);

                    assert_eq!(
                        actual, expected,
                        "Mismatch for cell '{}' output '{}' with inputs {:?}: AIG={}, expected={}",
                        cell_type, output_pin, input_bools, actual, expected
                    );
                }
            }
            tested += 1;
        }

        println!(
            "Tested {} cell types exhaustively, skipped {} (unsupported pins), {} (too many inputs)",
            tested, skipped_inputs, skipped_size
        );
        assert!(
            tested > 50,
            "Expected to test at least 50 cell types, but only tested {}",
            tested
        );
    }
}
