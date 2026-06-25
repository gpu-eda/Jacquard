//! Cell-model IR — a generated, JSON-first, per-cell-type library descriptor.
//!
//! Realises [ADR 0019](../../../docs/adr/0019-cell-model-ir.md). One
//! descriptor carries *everything per-cell-type about a library*; Jacquard
//! core consumes it as its only source of cell semantics, so the per-PDK Rust
//! classifiers, the `build.rs` pin-table generation, the runtime
//! `functional.v` parse, and the runtime `.lib` parse can all retire.
//!
//! ## Scope of this crate at C1
//!
//! Plan checkpoint C1 (`docs/plans/cell-model-ir.md`) defines only the corner
//! of the schema needed to build the AIG:
//!
//! - **L1 — pin directions** ([`Pin`] / [`Direction`]).
//! - **L2 — combinational logic** as a pre-built and-inverter graph
//!   ([`CombLogic`], decision D3): the runtime splices the cell's AIG straight
//!   into the design AIG with no decomposition work.
//!
//! L3 (sequential pin-roles + classification beyond the [`CellKind`] stub) and
//! L4 (timing characterization) arrive in C2 — the schema is explicitly
//! versioned ([`SCHEMA_MAJOR`] / [`SCHEMA_MINOR`]) so they extend it
//! compatibly.
//!
//! ## Identifier alignment (decision D1)
//!
//! Cells are keyed by their **full netlist cell-type name** (e.g.
//! `gf180mcu_fd_sc_mcu9t5v0__nand2_1`) and pins by their **netlistdb pin-name
//! string** (`A1`, `Y`, `ZN`). This is the same join the timing IR (ADR 0002)
//! uses, so the two IRs co-reference a design purely through the netlist with
//! no shared-schema join to maintain.
//!
//! ## The AIG encoding (L2)
//!
//! [`CombLogic`] is a flat, AIGER-like node list. Nodes are numbered:
//!
//! - node `0` is the **constant-0** node (constant-1 is `Ref { node: 0,
//!   inverted: true }`);
//! - nodes `1 ..= inputs.len()` are the **input pins**, in [`CombLogic::inputs`]
//!   order;
//! - the remaining nodes are the **AND gates** in [`CombLogic::and_nodes`]
//!   order (`and_nodes[k]` is node `1 + inputs.len() + k`).
//!
//! A [`Ref`] names any node plus an inversion bit. Each AND node is the
//! conjunction of two refs to *earlier* nodes; each output pin maps to a ref.
//! This is the same and-inverter shape Jacquard's design AIG uses
//! ([ADR 0014](../../../docs/adr/0014-aig-as-simulation-ir.md)), so splicing is
//! a direct node-id remap at load.

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

pub mod diff;

/// Schema major version. Bumped on a breaking change to the on-disk shape.
pub const SCHEMA_MAJOR: u16 = 0;
/// Schema minor version. Bumped on a backward-compatible addition (e.g. the
/// C2 sequential/timing blocks added alongside the C1 L1+L2 fields).
pub const SCHEMA_MINOR: u16 = 1;

/// A whole library's worth of per-cell-type facts: one descriptor per library.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CellModelIr {
    /// Schema major version the document was written against ([`SCHEMA_MAJOR`]).
    pub schema_major: u16,
    /// Schema minor version the document was written against ([`SCHEMA_MINOR`]).
    pub schema_minor: u16,
    /// Library-level metadata, including the selection prefixes (D8).
    pub library: LibraryMeta,
    /// Per-cell-type models, keyed by [`CellModel::cell_type`].
    pub cells: Vec<CellModel>,
}

impl CellModelIr {
    /// Create an empty descriptor stamped with the current schema version.
    pub fn new(library: LibraryMeta) -> Self {
        Self {
            schema_major: SCHEMA_MAJOR,
            schema_minor: SCHEMA_MINOR,
            library,
            cells: Vec::new(),
        }
    }

    /// Serialize to pretty JSON (the canonical on-disk form — D2).
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }

    /// Parse from JSON.
    pub fn from_json(s: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(s)
    }

    /// Write the descriptor to a path as pretty JSON.
    pub fn write_to(&self, path: impl AsRef<Path>) -> std::io::Result<()> {
        let json = self
            .to_json()
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        std::fs::write(path, json)
    }

    /// Read a descriptor from a JSON file.
    pub fn read_from(path: impl AsRef<Path>) -> std::io::Result<Self> {
        let s = std::fs::read_to_string(path)?;
        Self::from_json(&s).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }

    /// Look up a cell by its full netlist cell-type name.
    pub fn cell(&self, cell_type: &str) -> Option<&CellModel> {
        self.cells.iter().find(|c| c.cell_type == cell_type)
    }
}

/// Library-level metadata.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LibraryMeta {
    /// Library name, e.g. `gf180mcu_fd_sc_mcu9t5v0`.
    pub name: String,
    /// Cell-name prefix(es) this descriptor covers, for netlist auto-matching
    /// (D8). A netlist whose cell types start with one of these is served by
    /// this descriptor unless `--cell-descriptor` overrides.
    pub prefixes: Vec<String>,
}

/// Everything per-cell-type for one cell.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CellModel {
    /// Full netlist cell-type name — the join key (D1).
    pub cell_type: String,
    /// Classification. C1 only distinguishes combinational from
    /// not-yet-modelled; the full L3 kind set (dff/latch/clock_gate/ram/…)
    /// lands in C2.
    pub kind: CellKind,
    /// Pin directions (L1).
    pub pins: Vec<Pin>,
    /// Combinational logic as a pre-built AIG (L2). `None` for cells C1 does
    /// not model combinationally (sequential, tie, filler, …); those gain
    /// their own blocks in C2.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logic: Option<CombLogic>,
}

/// Cell classification (C1 stub; extended in C2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CellKind {
    /// Purely combinational standard cell (has [`CellModel::logic`]).
    Comb,
    /// Recognised but not combinationally modelled at C1 (sequential, tie,
    /// filler, IO pad, …). Refined into specific kinds in C2.
    Other,
}

/// One pin's direction (L1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pin {
    /// Pin name as it appears in the netlist (`A1`, `Y`, `ZN`).
    pub name: String,
    /// Input or output.
    pub direction: Direction,
}

/// Pin direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Input,
    Output,
}

/// Combinational logic of a cell as a pre-built and-inverter graph (L2, D3).
///
/// See the crate-level docs for the node numbering. The graph is acyclic with
/// nodes in topological order: every [`AndNode`] references only earlier nodes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CombLogic {
    /// Input pin names, in input-node order. Input pin `inputs[i]` is node
    /// `1 + i` (node `0` is the constant-0 node).
    pub inputs: Vec<String>,
    /// AND gates in topological order. `and_nodes[k]` is node
    /// `1 + inputs.len() + k` and references only earlier nodes.
    pub and_nodes: Vec<AndNode>,
    /// Output pins, each mapping to the node (with inversion) that drives it.
    pub outputs: Vec<OutputPin>,
}

impl CombLogic {
    /// Total number of nodes: const-0 + inputs + AND gates.
    pub fn num_nodes(&self) -> usize {
        1 + self.inputs.len() + self.and_nodes.len()
    }

    /// Evaluate every output pin for a given assignment of input pin -> bool.
    ///
    /// Pure 2-state evaluation over the AIG; used by round-trip tests and by
    /// the converter's Liberty-vs-`.v` cross-check (ADR 0019 D6). Returns a
    /// map from output pin name to value. Returns `Err` if an input pin named
    /// in [`Self::inputs`] is missing from `input_values`.
    pub fn eval(
        &self,
        input_values: &HashMap<String, bool>,
    ) -> Result<HashMap<String, bool>, String> {
        // node_vals[i] = value of node i. node 0 is constant 0.
        let mut node_vals = vec![false; self.num_nodes()];
        for (i, pin) in self.inputs.iter().enumerate() {
            node_vals[1 + i] = *input_values
                .get(pin)
                .ok_or_else(|| format!("missing value for input pin '{pin}'"))?;
        }
        let and_base = 1 + self.inputs.len();
        for (k, gate) in self.and_nodes.iter().enumerate() {
            let a = gate.a.eval(&node_vals);
            let b = gate.b.eval(&node_vals);
            node_vals[and_base + k] = a && b;
        }
        Ok(self
            .outputs
            .iter()
            .map(|o| (o.pin.clone(), o.r.eval(&node_vals)))
            .collect())
    }
}

/// A two-input AND gate. Output is `a.eval() & b.eval()`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AndNode {
    pub a: Ref,
    pub b: Ref,
}

/// An output pin and the node (with inversion) that drives it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputPin {
    /// Output pin name.
    pub pin: String,
    /// The node that drives this pin.
    pub r: Ref,
}

/// A reference to a node, optionally inverted.
///
/// `node` indexes the flat node list `[const0, inputs…, and_nodes…]`;
/// `inverted` negates that node's value. Constant-1 is
/// `Ref { node: 0, inverted: true }`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ref {
    pub node: u32,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub inverted: bool,
}

impl Ref {
    /// A non-inverted reference to `node`.
    pub fn node(node: u32) -> Self {
        Self {
            node,
            inverted: false,
        }
    }

    /// An inverted reference to `node`.
    pub fn inv(node: u32) -> Self {
        Self {
            node,
            inverted: true,
        }
    }

    /// Evaluate this ref against a node-value table.
    fn eval(&self, node_vals: &[bool]) -> bool {
        node_vals[self.node as usize] ^ self.inverted
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a NAND2 (`Y = !(A & B)`) descriptor by hand and check the schema,
    /// JSON round-trip, and AIG evaluation.
    fn nand2() -> CellModelIr {
        let mut ir = CellModelIr::new(LibraryMeta {
            name: "demo_lib".into(),
            prefixes: vec!["demo_".into()],
        });
        // node 0 = const0, node 1 = A, node 2 = B, node 3 = A & B.
        let logic = CombLogic {
            inputs: vec!["A".into(), "B".into()],
            and_nodes: vec![AndNode {
                a: Ref::node(1),
                b: Ref::node(2),
            }],
            outputs: vec![OutputPin {
                pin: "Y".into(),
                r: Ref::inv(3),
            }],
        };
        ir.cells.push(CellModel {
            cell_type: "demo_nand2".into(),
            kind: CellKind::Comb,
            pins: vec![
                Pin {
                    name: "A".into(),
                    direction: Direction::Input,
                },
                Pin {
                    name: "B".into(),
                    direction: Direction::Input,
                },
                Pin {
                    name: "Y".into(),
                    direction: Direction::Output,
                },
            ],
            logic: Some(logic),
        });
        ir
    }

    #[test]
    fn json_round_trip_is_lossless() {
        let ir = nand2();
        let json = ir.to_json().unwrap();
        let back = CellModelIr::from_json(&json).unwrap();
        assert_eq!(ir, back);
    }

    #[test]
    fn aig_evaluates_nand2_truth_table() {
        let ir = nand2();
        let logic = ir.cell("demo_nand2").unwrap().logic.as_ref().unwrap();
        for (a, b) in [(false, false), (false, true), (true, false), (true, true)] {
            let inputs = HashMap::from([("A".to_string(), a), ("B".to_string(), b)]);
            let out = logic.eval(&inputs).unwrap();
            assert_eq!(out["Y"], !(a && b), "NAND2 mismatch for A={a} B={b}");
        }
    }

    #[test]
    fn eval_reports_missing_input() {
        let ir = nand2();
        let logic = ir.cell("demo_nand2").unwrap().logic.as_ref().unwrap();
        let inputs = HashMap::from([("A".to_string(), true)]); // B missing
        assert!(logic.eval(&inputs).is_err());
    }

    #[test]
    fn ref_inverted_default_omitted_from_json() {
        // A non-inverted ref should serialize without an `inverted` field
        // (serde default-skip), keeping the AIG payload compact.
        let json = serde_json::to_string(&Ref::node(5)).unwrap();
        assert_eq!(json, r#"{"node":5}"#);
        let json_inv = serde_json::to_string(&Ref::inv(5)).unwrap();
        assert_eq!(json_inv, r#"{"node":5,"inverted":true}"#);
    }
}
