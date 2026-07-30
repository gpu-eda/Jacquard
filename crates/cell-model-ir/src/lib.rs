//! Cell-model IR — a generated, JSON-first, per-cell-type library descriptor.
//!
//! Realises [Decision 0019](../../../docs/architecture/decisions/0019-cell-model-ir.md). One
//! descriptor carries *everything per-cell-type about a library*; Jacquard
//! core consumes it as its only source of cell semantics, so the per-PDK Rust
//! classifiers, the `build.rs` pin-table generation, the runtime
//! `functional.v` parse, and the runtime `.lib` parse can all retire.
//!
//! ## Scope of this crate
//!
//! - **L1 — pin directions** ([`Pin`] / [`Direction`]).
//! - **L2 — combinational logic** as a pre-built and-inverter graph
//!   ([`CombLogic`], decision D3): the runtime splices the cell's AIG straight
//!   into the design AIG with no decomposition work.
//! - **L3 — sequential pin-roles + classification** ([`Sequential`],
//!   [`CellKind`], decision D4): clock pin + edge, level enable, next-state as
//!   the same pre-built AIG (D3), Q/QN outputs, async set/reset with polarity.
//! - **L4 — timing characterization** ([`CellTiming`], decision D5):
//!   per-cell-type combinational / clock→Q delays and setup/hold constraints,
//!   plus SRAM/macro timing, **keyed by corner** ([`Corner`] / [`TimingValue`])
//!   exactly as the timing IR (Decision 0002, `crates/timing-ir`) does — corner is
//!   the outer key, `min`/`typ`/`max` is the orthogonal within-corner derate.
//!
//! The schema is explicitly versioned ([`SCHEMA_MAJOR`] / [`SCHEMA_MINOR`]).
//! L1/L2 were defined at plan checkpoint C1; L3/L4 are the additive C2 minor
//! bump and are skip-serialized when absent, so a C1-shaped combinational
//! descriptor round-trips byte-identically under the newer schema.
//!
//! ## L4 corner-keying (decision D5)
//!
//! Multi-corner lives in the one descriptor, keyed by corner — the same shape
//! the timing IR uses: a descriptor-level ordered [`CellModelIr::corners`] set
//! plus a [`CellModelIr::default_corner`], and every timing number is a
//! [`TimingValue`] carrying a `corner_index` into that set with `min`/`typ`/`max`
//! as the within-corner derate triple. L1–L3 (directions, function, sequential
//! roles, and especially the D3 AIG — the bulk of the descriptor) are
//! corner-invariant; only L4's numbers vary across `ss`/`tt`/`ff`, so the AIG is
//! stored once and only [`TimingValue`]s repeat per corner. The single-corner
//! common flow degrades to a one-entry corner set.
//!
//! ## Identifier alignment (decision D1)
//!
//! Cells are keyed by their **full netlist cell-type name** (e.g.
//! `gf180mcu_fd_sc_mcu9t5v0__nand2_1`) and pins by their **netlistdb pin-name
//! string** (`A1`, `Y`, `ZN`). This is the same join the timing IR (Decision 0002)
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
//! ([Decision 0014](../../../docs/architecture/decisions/0014-aig-as-simulation-ir.md)), so splicing is
//! a direct node-id remap at load.

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

pub mod diff;

/// Schema major version. Bumped on a breaking change to the on-disk shape.
pub const SCHEMA_MAJOR: u16 = 0;
/// Schema minor version. Bumped on a backward-compatible addition. `2` adds
/// the C2 L3 sequential/classification block ([`Sequential`], the extended
/// [`CellKind`]) and the L4 corner-keyed timing block ([`CellTiming`],
/// [`Corner`], [`CellModelIr::corners`] / [`CellModelIr::default_corner`])
/// alongside the C1 L1+L2 fields, which are unchanged. `3` adds the C4
/// [`Sequential::clear_preset`] tie-break ([`ClearPreset`]) capturing the
/// state when async clear and preset are asserted simultaneously — additive,
/// `skip_serializing_if` absent, so `2`-era documents still read back.
pub const SCHEMA_MINOR: u16 = 3;

/// A whole library's worth of per-cell-type facts: one descriptor per library.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CellModelIr {
    /// Schema major version the document was written against ([`SCHEMA_MAJOR`]).
    pub schema_major: u16,
    /// Schema minor version the document was written against ([`SCHEMA_MINOR`]).
    pub schema_minor: u16,
    /// Library-level metadata, including the selection prefixes (D8).
    pub library: LibraryMeta,
    /// Ordered PVT corner set (L4, D5). Every [`TimingValue`] in the
    /// descriptor names a corner by its index into this list. Empty for a
    /// descriptor that carries no L4 timing (e.g. a logic-only C1 descriptor);
    /// the single-corner common flow has exactly one entry. Mirrors the timing
    /// IR's `corners` (Decision 0002).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub corners: Vec<Corner>,
    /// Name of the corner selected when the user passes no `--corner` (D5).
    /// Must match one [`Corner::name`] in [`Self::corners`] when timing is
    /// present; empty when the descriptor carries no L4 timing.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub default_corner: String,
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
            corners: Vec::new(),
            default_corner: String::new(),
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

    /// Index of the named corner in [`Self::corners`], for resolving a
    /// `--corner <name>` selection (or the [`Self::default_corner`]) to the
    /// `corner_index` stamped on every [`TimingValue`]. `None` if absent.
    pub fn corner_index(&self, name: &str) -> Option<u32> {
        self.corners
            .iter()
            .position(|c| c.name == name)
            .map(|i| i as u32)
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
    /// Classification (L3, D4). See [`CellKind`].
    pub kind: CellKind,
    /// Pin directions (L1).
    pub pins: Vec<Pin>,
    /// Combinational logic as a pre-built AIG (L2). `None` for cells with no
    /// purely-combinational output cone (sequential cells store their data
    /// path in [`Self::sequential`] instead; tie/filler/physical cells have
    /// no logic).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logic: Option<CombLogic>,
    /// Sequential pin-role metadata (L3, D4). `Some` for `dff` / `latch` /
    /// `clock_gate` cells; `None` for combinational and physical cells.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sequential: Option<Sequential>,
    /// Per-cell-type timing characterization (L4, D5), keyed by corner.
    /// `None` for a logic-only descriptor or a cell with no characterized
    /// arcs (e.g. filler / tap).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timing: Option<CellTiming>,
}

/// Cell classification (L3, D4).
///
/// The declared kind set aligns variant-for-variant (same `snake_case`
/// strings) with the hand-override manifest kind in `src/cell_library.rs`, so
/// the ADR-0010 `.cells.toml` is the *same data model* over the generated IR
/// (D8). The IO-pad family `io_pad_*` is flattened to the concrete
/// [`Self::IoPadInput`] / [`Self::IoPadOutput`] / [`Self::IoPadBidir`]
/// variants, matching that manifest.
///
/// `ram` keeps its ADR-0011 port schema (carried in the hand-override manifest
/// / future descriptor RAM block); this enum only *classifies* a cell as RAM,
/// it does not redesign the port mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CellKind {
    /// Combinational standard cell (has [`CellModel::logic`]). This is the C1
    /// name for what D4 calls `std`; **transitional** — the C1 converter still
    /// emits it, C2.2 migrates the converter to [`Self::Std`], after which
    /// `Comb`/`Other` retire. Kept now so the C1 converter keeps compiling.
    Comb,
    /// Recognised but unclassified at C1 (a sequential/tie/filler/pad cell the
    /// C1 converter did not refine). **Transitional**, as [`Self::Comb`].
    Other,
    /// Combinational standard cell (D4 `std`). Has [`CellModel::logic`].
    Std,
    /// Edge-triggered flip-flop. Carries [`CellModel::sequential`] with a
    /// [`Sequential::clock`].
    Dff,
    /// Level-sensitive latch. Carries [`CellModel::sequential`] with a level
    /// [`Sequential::enable`] (and typically no edge clock).
    Latch,
    /// Integrated clock-gating cell.
    ClockGate,
    /// Synchronous RAM macro — keeps its ADR-0011 port schema; this is only
    /// the classification tag.
    Ram,
    /// Constant-1 driver.
    TieHigh,
    /// Constant-0 driver.
    TieLow,
    /// Physical-only filler / decap. No logic.
    Filler,
    /// Row-end / boundary cap. Physical-only.
    Endcap,
    /// Substrate tap. Physical-only.
    Tap,
    /// IO pad — input-only (`io_pad_*`).
    IoPadInput,
    /// IO pad — output-only (`io_pad_*`).
    IoPadOutput,
    /// IO pad — bidirectional, a/oe/y triple (`io_pad_*`).
    IoPadBidir,
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
    /// the converter's Liberty-vs-`.v` cross-check (Decision 0019 D6). Returns a
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

// ---------------------------------------------------------------------------
// L3 — sequential pin-roles + classification (decision D4)
// ---------------------------------------------------------------------------

/// Sequential pin-role metadata for a `dff` / `latch` / `clock_gate` cell
/// (L3, D4).
///
/// This replaces the hardcoded pin-name matches in `src/aig.rs:2080-2260`: the
/// consumer reads roles from here instead of matching `"CLK"` / `"RN"` /
/// `"SETN"` / `"SE"` / … by string. The *synchronous* combinational data path
/// — including any scan mux (`D_eff = SE ? SI : D`) and clock-gate `E | TE` —
/// is folded into [`Self::next_state`] as the same pre-built AIG as L2 (D3), so
/// only the genuinely-special roles (clock edge, level enable, async
/// set/reset polarity, state outputs) need named fields. The *asynchronous*
/// set/reset are **not** part of `next_state`: they force the state directly
/// and also gate the clock-enable, so they are modelled separately
/// ([`Self::async_set`] / [`Self::async_reset`]).
///
/// Role coverage cross-checked against every case the consumer hardcodes:
/// AIGPDK `DFF`/`DFFSR` (active-low `S`/`R`), SKY130 `dfxtp`/`dfrtp`/`edfxtp`
/// (`RESET_B`/`SET_B`/`DE`), GF180 `dffq`/`dffnq`/`dffrnq`/`dffsnq`, scan
/// `sdff*` (`SE`/`SI`), latches `lat*` (`E`), and clock gates `icgt*`
/// (`E`/`TE`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Sequential {
    /// Edge-triggered clock pin + active edge. `Some` for flip-flops and
    /// clock gates; `None` for a level-sensitive latch (which uses
    /// [`Self::enable`] as its transparency control).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clock: Option<ClockPin>,
    /// Level-sensitive enable. For a `latch` this is the transparency control
    /// (state follows `next_state` while asserted); for a gated `dff` (e.g.
    /// SKY130 `edfxtp`) this is the clock-enable ANDed with the edge clock.
    /// `None` when the cell has no enable pin.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enable: Option<EnablePin>,
    /// Synchronous next-state function as a pre-built AIG (D3). Its
    /// [`CombLogic::inputs`] are cell input-pin names (`D`, and any `SE`/`SI`,
    /// `E`/`TE`, …); it has exactly one output — the value captured on the
    /// active clock edge / while the enable is asserted. The consumer splices
    /// it like L2 logic.
    pub next_state: CombLogic,
    /// State outputs — `Q` (state) and/or `QN` (inverted state).
    pub outputs: Vec<SeqOutput>,
    /// Asynchronous set (force state to 1), with its assertion polarity.
    /// `None` if the cell has no async set. (Liberty `ff` `preset`.)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub async_set: Option<AsyncControl>,
    /// Asynchronous reset (force state to 0), with its assertion polarity.
    /// `None` if the cell has no async reset. (Liberty `ff` `clear`.)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub async_reset: Option<AsyncControl>,
    /// The simultaneous-assertion tie-break for a cell with BOTH async set
    /// (`preset`) and reset (`clear`): what the state variables take when both
    /// are asserted at once (Liberty `clear_preset_var1` / `clear_preset_var2`).
    /// `None` when the cell lacks a clear+preset pair, or when the Liberty group
    /// omits the attributes. See [`ClearPreset`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clear_preset: Option<ClearPreset>,
}

/// The simultaneous-assertion tie-break for a flop/latch that has BOTH an
/// async clear (reset) and an async preset (set): the value each of the two
/// state variables (`Q` = `var1`, `QN` = `var2`) takes when clear AND preset
/// are asserted at the same time. This is Liberty's `clear_preset_var1` /
/// `clear_preset_var2`.
///
/// Jacquard's consumer overlay (`wire_dff_reset_set_overlay` in `src/aig.rs`)
/// historically hardcoded reset-dominant (`var1 = L`). Most cells match, but
/// set-dominant cells (`var1 = H`) exist across every foundry library tested
/// (GF180 `latrsnq`, SKY130, GF130 `TLATSR`/`TLATNSR`, IHP `sg13g2_sdfbbp_1`);
/// carrying the true tie-break here lets the consumer honour it rather than
/// silently mis-simulating. Use [`ClearPreset::dominance`] to reduce the pair
/// to the tie-break the overlay needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClearPreset {
    /// `clear_preset_var1` — the value the first state variable (`Q`) takes
    /// when clear and preset are asserted simultaneously.
    pub var1: ClearPresetVal,
    /// `clear_preset_var2` — the value the second state variable (`QN`) takes
    /// when clear and preset are asserted simultaneously. Usually the
    /// complement of [`Self::var1`], but Liberty permits any combination.
    pub var2: ClearPresetVal,
}

/// Which async control wins when clear and preset are asserted together, as
/// derived from a [`ClearPreset`]'s `var1` (the `Q` state variable).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dominance {
    /// `var1 = L` — the state goes to 0; clear (reset) wins. This is Jacquard's
    /// legacy hardcoded behaviour and the default when `clear_preset` is absent.
    ResetDominant,
    /// `var1 = H` — the state goes to 1; preset (set) wins.
    SetDominant,
    /// `var1` is `N` / `X` / `T` (hold / unknown / toggle): not a simple
    /// dominance. The consumer falls back to reset-dominant (the safe default).
    Other,
}

impl ClearPreset {
    /// Reduce the `var1`/`var2` pair to the simultaneous-assertion dominance
    /// the consumer overlay needs, keyed on `var1` (the `Q` state variable).
    pub fn dominance(&self) -> Dominance {
        match self.var1 {
            ClearPresetVal::Low => Dominance::ResetDominant,
            ClearPresetVal::High => Dominance::SetDominant,
            ClearPresetVal::NoChange | ClearPresetVal::Unknown | ClearPresetVal::Toggle => {
                Dominance::Other
            }
        }
    }
}

/// A Liberty `clear_preset_var*` value: the state a variable takes when clear
/// and preset are asserted simultaneously. Liberty's five permitted values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClearPresetVal {
    /// `L` — the state variable goes low.
    Low,
    /// `H` — the state variable goes high.
    High,
    /// `N` — no change (holds its previous value).
    NoChange,
    /// `X` — unknown.
    Unknown,
    /// `T` — toggles (inverts its previous value).
    Toggle,
}

/// Clock pin + the edge it triggers on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClockPin {
    /// Clock pin name (`CLK`, `CLKN`, `PORT_R_CLK`, …).
    pub pin: String,
    /// Edge the cell captures on.
    pub edge: ClockEdge,
}

/// Active clock edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClockEdge {
    /// Positive edge (`CLK`).
    Rising,
    /// Negative edge (GF180 `CLKN` cells).
    Falling,
}

/// A level-sensitive enable pin + the level at which it is asserted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnablePin {
    /// Enable pin name (latch `E`, gated-DFF `DE`).
    pub pin: String,
    /// Level (high/low) at which the enable is asserted.
    pub active: ActiveLevel,
}

/// An asynchronous set or reset control: which pin, and the level that
/// asserts it.
///
/// Active-low controls (GF180 `RN`/`SETN`, SKY130 `RESET_B`/`SET_B`, AIGPDK
/// `R`/`S`) are [`ActiveLevel::Low`]; an active-high reset is
/// [`ActiveLevel::High`]. This is the fact the consumer's async overlay needs
/// to build `d_in = AND(OR(D, !SETN), RN)` / `en = OR(clk, !RN, !SETN)`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AsyncControl {
    /// The set/reset pin name.
    pub pin: String,
    /// Level at which the control is asserted.
    pub active: ActiveLevel,
}

/// Active level for an enable or async control.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActiveLevel {
    /// Asserted when the pin is logic 1.
    High,
    /// Asserted when the pin is logic 0 (the common `*N` / `*_B` convention).
    Low,
}

/// A sequential cell's state output pin.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeqOutput {
    /// Output pin name (`Q`, `QN`).
    pub pin: String,
    /// `false` ⇒ drives the stored state (`Q`); `true` ⇒ drives the inverted
    /// state (`QN`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub inverted: bool,
}

// ---------------------------------------------------------------------------
// L4 — timing characterization, corner-keyed (decision D5)
// ---------------------------------------------------------------------------

/// A PVT corner. Mirrors the timing IR's `Corner` (Decision 0002): every
/// [`TimingValue`] names one of these by its index into
/// [`CellModelIr::corners`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Corner {
    /// Corner name, e.g. `tt_025C_1v80`, `ss_125C_1v08`. The `--corner`
    /// selector and [`CellModelIr::default_corner`] match on this.
    pub name: String,
    /// Process label, e.g. `ss`, `tt`, `ff`.
    pub process: String,
    /// Supply voltage in volts.
    pub voltage: f32,
    /// Temperature in degrees Celsius.
    pub temperature: f32,
}

/// A single timing number at one corner, with the min/typ/max within-corner
/// derate triple. Mirrors the timing IR's `TimingValue`: corner is the **outer**
/// key (`corner_index`), `min`/`typ`/`max` is the orthogonal **inner** derate
/// — not a reinterpretation of the triple. Picoseconds, `f64` (the consumer
/// rounds to its internal `u16` raw-ps at consumption, as the timing IR
/// consumer does).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TimingValue {
    /// Index into [`CellModelIr::corners`].
    pub corner_index: u32,
    /// Fast/min-derate value (ps).
    pub min: f64,
    /// Typical value (ps); may equal `min` or `max` when unavailable.
    pub typ: f64,
    /// Slow/max-derate value (ps).
    pub max: f64,
}

/// Per-cell-type timing characterization (L4, D5). Carries everything the
/// runtime consumes today from `liberty_parser::TimingLibrary` — combinational
/// and clock→Q delays ([`Self::delays`]), setup/hold constraints
/// ([`Self::constraints`]), and SRAM/macro timing ([`Self::sram`]) — but
/// pre-extracted and keyed by corner. The per-cell aggregate views
/// (`DFFTiming`, `max_combinational_delay`, …) are reconstructable from these
/// arcs by the consumer.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CellTiming {
    /// Delay arcs: combinational input→output and sequential clock→output.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub delays: Vec<DelayArc>,
    /// Setup/hold (and recovery/removal) constraint arcs on sequential pins.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub constraints: Vec<ConstraintArc>,
    /// SRAM/macro timing, when this cell is a memory (`kind = ram`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sram: Option<SramTiming>,
}

/// A propagation-delay arc through the cell. Corresponds to a Liberty
/// `timing()` group with `cell_rise`/`cell_fall` tables.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DelayArc {
    /// Driving (related) pin — the arc's source.
    pub from_pin: String,
    /// Output pin the arc drives.
    pub to_pin: String,
    /// Whether this is a combinational arc or a clock→output (sequential)
    /// arc.
    pub kind: DelayKind,
    /// Per-corner rising-output delay (`cell_rise`).
    pub rise: Vec<TimingValue>,
    /// Per-corner falling-output delay (`cell_fall`).
    pub fall: Vec<TimingValue>,
}

/// Whether a [`DelayArc`] is combinational or a sequential clock→output delay.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DelayKind {
    /// Input pin → output pin through combinational logic.
    Combinational,
    /// Clock pin → state output (`Q`/`QN`); Liberty `rising_edge` /
    /// `falling_edge` timing type.
    ClockToOutput,
}

/// A setup/hold (or recovery/removal) constraint arc on a sequential pin.
/// Corresponds to a Liberty `timing()` group whose `timing_type` is a
/// constraint, with `rise_constraint`/`fall_constraint` tables.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConstraintArc {
    /// Constrained (data/control) pin, e.g. `D`, `SI`, `RN`.
    pub data_pin: String,
    /// Reference pin the constraint is relative to — the clock.
    pub related_pin: String,
    /// Which constraint this arc expresses.
    pub kind: ConstraintKind,
    /// Reference clock edge the constraint applies to.
    pub edge: ClockEdge,
    /// Per-corner constraint for a rising data transition (`rise_constraint`).
    pub rise: Vec<TimingValue>,
    /// Per-corner constraint for a falling data transition (`fall_constraint`).
    pub fall: Vec<TimingValue>,
}

/// Constraint kind for a [`ConstraintArc`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConstraintKind {
    /// Data-before-clock setup (`setup_rising`/`setup_falling`).
    Setup,
    /// Data-after-clock hold (`hold_rising`/`hold_falling`).
    Hold,
    /// Async-deassert recovery (`recovery_*`).
    Recovery,
    /// Async-deassert removal (`removal_*`).
    Removal,
}

/// SRAM/macro timing — the data Jacquard's SRAM path consumes today
/// (`liberty_parser::SRAMTiming`), corner-keyed. RAM keeps its ADR-0011 port
/// schema; this carries only the *timing* numbers.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SramTiming {
    /// Read clock → data output, rising (per corner).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub read_clk_to_data_rise: Vec<TimingValue>,
    /// Read clock → data output, falling (per corner).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub read_clk_to_data_fall: Vec<TimingValue>,
    /// Address setup before the active clock edge (per corner).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub addr_setup: Vec<TimingValue>,
    /// Address hold after the active clock edge (per corner).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub addr_hold: Vec<TimingValue>,
    /// Write-data setup before the active clock edge (per corner).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub write_data_setup: Vec<TimingValue>,
    /// Write-data hold after the active clock edge (per corner).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub write_data_hold: Vec<TimingValue>,
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
            sequential: None,
            timing: None,
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

    /// A rising-edge DFF with an active-low async reset (`RN`) and an active-low
    /// async set (`SETN`) — the GF180 `dffsrnq` shape — characterized across
    /// three corners (`ss`/`tt`/`ff`). Exercises every L3 role plus a
    /// multi-corner L4 block.
    fn dff_with_async_reset() -> CellModelIr {
        let mut ir = CellModelIr::new(LibraryMeta {
            name: "demo_lib".into(),
            prefixes: vec!["demo_".into()],
        });
        ir.corners = vec![
            Corner {
                name: "ss_125C_1v08".into(),
                process: "ss".into(),
                voltage: 1.08,
                temperature: 125.0,
            },
            Corner {
                name: "tt_025C_1v20".into(),
                process: "tt".into(),
                voltage: 1.20,
                temperature: 25.0,
            },
            Corner {
                name: "ff_m40C_1v32".into(),
                process: "ff".into(),
                voltage: 1.32,
                temperature: -40.0,
            },
        ];
        ir.default_corner = "tt_025C_1v20".into();

        // next-state is just D (node 1) — single input, single output.
        let next_state = CombLogic {
            inputs: vec!["D".into()],
            and_nodes: vec![],
            outputs: vec![OutputPin {
                pin: "D".into(),
                r: Ref::node(1),
            }],
        };

        // A per-corner derate triple for each of the three corners. Values are
        // exactly-representable f64s (and vary by corner) so the round-trip
        // assertion tests the schema, not float formatting.
        let per_corner = |base: f64| -> Vec<TimingValue> {
            (0..3)
                .map(|i| {
                    let slow = (i as f64) * 4.0; // ss slower than ff
                    TimingValue {
                        corner_index: i,
                        min: base - 5.0 + slow,
                        typ: base + slow,
                        max: base + 5.0 + slow,
                    }
                })
                .collect()
        };

        let timing = CellTiming {
            delays: vec![DelayArc {
                from_pin: "CLK".into(),
                to_pin: "Q".into(),
                kind: DelayKind::ClockToOutput,
                rise: per_corner(120.0),
                fall: per_corner(130.0),
            }],
            constraints: vec![
                ConstraintArc {
                    data_pin: "D".into(),
                    related_pin: "CLK".into(),
                    kind: ConstraintKind::Setup,
                    edge: ClockEdge::Rising,
                    rise: per_corner(40.0),
                    fall: per_corner(45.0),
                },
                ConstraintArc {
                    data_pin: "D".into(),
                    related_pin: "CLK".into(),
                    kind: ConstraintKind::Hold,
                    edge: ClockEdge::Rising,
                    rise: per_corner(10.0),
                    fall: per_corner(12.0),
                },
            ],
            sram: None,
        };

        ir.cells.push(CellModel {
            cell_type: "demo_dffsrnq".into(),
            kind: CellKind::Dff,
            pins: vec![
                Pin {
                    name: "D".into(),
                    direction: Direction::Input,
                },
                Pin {
                    name: "CLK".into(),
                    direction: Direction::Input,
                },
                Pin {
                    name: "RN".into(),
                    direction: Direction::Input,
                },
                Pin {
                    name: "SETN".into(),
                    direction: Direction::Input,
                },
                Pin {
                    name: "Q".into(),
                    direction: Direction::Output,
                },
            ],
            logic: None,
            sequential: Some(Sequential {
                clock: Some(ClockPin {
                    pin: "CLK".into(),
                    edge: ClockEdge::Rising,
                }),
                enable: None,
                next_state,
                outputs: vec![SeqOutput {
                    pin: "Q".into(),
                    inverted: false,
                }],
                async_set: Some(AsyncControl {
                    pin: "SETN".into(),
                    active: ActiveLevel::Low,
                }),
                async_reset: Some(AsyncControl {
                    pin: "RN".into(),
                    active: ActiveLevel::Low,
                }),
                clear_preset: None,
            }),
            timing: Some(timing),
        });
        ir
    }

    #[test]
    fn sequential_multicorner_round_trips() {
        let ir = dff_with_async_reset();
        let json = ir.to_json().unwrap();
        let back = CellModelIr::from_json(&json).unwrap();
        assert_eq!(ir, back);

        // Schema version stamped at the current minor.
        assert_eq!(
            (back.schema_major, back.schema_minor),
            (SCHEMA_MAJOR, SCHEMA_MINOR)
        );

        // L3 roles survived the round-trip.
        let seq = back
            .cell("demo_dffsrnq")
            .unwrap()
            .sequential
            .as_ref()
            .unwrap();
        assert_eq!(seq.clock.as_ref().unwrap().edge, ClockEdge::Rising);
        assert_eq!(seq.async_reset.as_ref().unwrap().pin, "RN");
        assert_eq!(seq.async_reset.as_ref().unwrap().active, ActiveLevel::Low);
        assert_eq!(seq.async_set.as_ref().unwrap().active, ActiveLevel::Low);
    }

    #[test]
    fn set_dominant_clear_preset_round_trips() {
        // Take the reset+set flop and stamp it set-dominant (var1=H): the field
        // must survive a JSON round-trip and reduce to SetDominant, while an
        // absent field defaults to ResetDominant.
        let mut ir = dff_with_async_reset();
        {
            let seq = ir.cells[0].sequential.as_mut().unwrap();
            assert!(seq.clear_preset.is_none(), "baseline has no clear_preset");
            seq.clear_preset = Some(ClearPreset {
                var1: ClearPresetVal::High,
                var2: ClearPresetVal::Low,
            });
        }
        let json = ir.to_json().unwrap();
        assert!(
            json.contains("clear_preset"),
            "set-dominant field must serialize:\n{json}"
        );
        let back = CellModelIr::from_json(&json).unwrap();
        assert_eq!(ir, back);

        let cp = back.cells[0]
            .sequential
            .as_ref()
            .unwrap()
            .clear_preset
            .unwrap();
        assert_eq!(cp.var1, ClearPresetVal::High);
        assert_eq!(cp.var2, ClearPresetVal::Low);
        assert_eq!(cp.dominance(), Dominance::SetDominant);

        // Absent field is reset-dominant by omission (default = today's behavior).
        assert!(dff_with_async_reset().cells[0]
            .sequential
            .as_ref()
            .unwrap()
            .clear_preset
            .is_none());

        // The other var1 values map to Other (consumer → reset-dominant fallback).
        assert_eq!(
            ClearPreset {
                var1: ClearPresetVal::Low,
                var2: ClearPresetVal::High
            }
            .dominance(),
            Dominance::ResetDominant
        );
        for v in [
            ClearPresetVal::NoChange,
            ClearPresetVal::Unknown,
            ClearPresetVal::Toggle,
        ] {
            assert_eq!(
                ClearPreset {
                    var1: v,
                    var2: ClearPresetVal::Unknown
                }
                .dominance(),
                Dominance::Other
            );
        }
    }

    #[test]
    fn c2_sequential_without_clear_preset_is_backward_readable() {
        // A schema-2 document (async_set + async_reset but no clear_preset key)
        // must still deserialize under schema 3 with clear_preset == None.
        let c2_json = r#"{
            "schema_major": 0,
            "schema_minor": 2,
            "library": { "name": "demo_lib", "prefixes": ["demo_"] },
            "cells": [
                {
                    "cell_type": "demo_dffsrnq",
                    "kind": "dff",
                    "pins": [
                        { "name": "D",   "direction": "input"  },
                        { "name": "CLK", "direction": "input"  },
                        { "name": "RN",  "direction": "input"  },
                        { "name": "SETN","direction": "input"  },
                        { "name": "Q",   "direction": "output" }
                    ],
                    "sequential": {
                        "clock": { "pin": "CLK", "edge": "rising" },
                        "next_state": {
                            "inputs": ["D"], "and_nodes": [],
                            "outputs": [{ "pin": "D", "r": { "node": 1, "inverted": false } }]
                        },
                        "outputs": [{ "pin": "Q" }],
                        "async_set":   { "pin": "SETN", "active": "low" },
                        "async_reset": { "pin": "RN",   "active": "low" }
                    }
                }
            ]
        }"#;
        let ir = CellModelIr::from_json(c2_json).unwrap();
        let seq = ir
            .cell("demo_dffsrnq")
            .unwrap()
            .sequential
            .as_ref()
            .unwrap();
        assert!(seq.clear_preset.is_none());
        assert!(seq.async_set.is_some() && seq.async_reset.is_some());
    }

    #[test]
    fn corner_index_resolves_default_and_named() {
        let ir = dff_with_async_reset();
        assert_eq!(ir.corner_index("ss_125C_1v08"), Some(0));
        assert_eq!(ir.corner_index(&ir.default_corner), Some(1));
        assert_eq!(ir.corner_index("ff_m40C_1v32"), Some(2));
        assert_eq!(ir.corner_index("no_such_corner"), None);

        // Every L4 value names a corner inside the declared set.
        let timing = ir.cell("demo_dffsrnq").unwrap().timing.as_ref().unwrap();
        for arc in &timing.delays {
            for v in arc.rise.iter().chain(arc.fall.iter()) {
                assert!((v.corner_index as usize) < ir.corners.len());
            }
        }
    }

    #[test]
    fn c1_combinational_shape_is_backward_compatible() {
        // A C1-shaped (L1+L2 only) descriptor must (a) still serialize without
        // any of the new L3/L4 keys, and (b) parse from JSON that predates
        // them — the additive minor-bump contract.
        let ir = nand2();
        let json = ir.to_json().unwrap();
        assert!(
            !json.contains("sequential")
                && !json.contains("timing")
                && !json.contains("corners")
                && !json.contains("default_corner"),
            "absent L3/L4 fields must be skip-serialized:\n{json}"
        );

        // A hand-written C1-era document (no corners / sequential / timing
        // keys at all) still deserializes under the C2 schema.
        let c1_json = r#"{
            "schema_major": 0,
            "schema_minor": 1,
            "library": { "name": "demo_lib", "prefixes": ["demo_"] },
            "cells": [
                {
                    "cell_type": "demo_inv",
                    "kind": "comb",
                    "pins": [
                        { "name": "A", "direction": "input" },
                        { "name": "Y", "direction": "output" }
                    ],
                    "logic": {
                        "inputs": ["A"],
                        "and_nodes": [],
                        "outputs": [ { "pin": "Y", "r": { "node": 1, "inverted": true } } ]
                    }
                }
            ]
        }"#;
        let parsed = CellModelIr::from_json(c1_json).unwrap();
        assert!(parsed.corners.is_empty());
        assert_eq!(parsed.default_corner, "");
        let cell = parsed.cell("demo_inv").unwrap();
        assert_eq!(cell.kind, CellKind::Comb);
        assert!(cell.sequential.is_none());
        assert!(cell.timing.is_none());
    }
}
