// SPDX-FileCopyrightText: Copyright (c) 2024 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0
//! Liberty (.lib) **timing** extraction (L4 — per-cell-type
//! characterization).
//!
//! The low-level tokenizer and generic group/attribute parser now live in
//! the [`liberty_parse`] crate — the single Liberty front-end in the repo
//! (ADR 0019 D6). This module no longer parses bytes: it parses with
//! `liberty_parse::parse` and **walks** the resulting `LibertyGroup` tree
//! (see the private `walk` module) to project it onto the timing types
//! below. It extracts the scalar timing values used in `aigpdk.lib` and
//! SKY130-style libraries:
//! - Cell delays (cell_rise, cell_fall) for combinational cells
//! - Setup/hold constraints for sequential cells (DFF, DFFSR)
//! - Clock-to-Q delays for sequential cells
//! - SRAM timing (read delays, setup/hold)
//!
//! The public `TimingLibrary` / `CellTiming` / `PinTiming` / `TimingArc`
//! API is unchanged; only the parsing internals moved.

use indexmap::IndexMap;
use std::fs;
use std::path::Path;

/// Timing arc from an input pin to an output pin.
#[derive(Debug, Clone, Default)]
pub struct TimingArc {
    /// Related (input) pin name
    pub related_pin: String,
    /// Timing type (e.g., "setup_rising", "hold_rising", "rising_edge")
    pub timing_type: Option<String>,
    /// Cell rise delay in picoseconds
    pub cell_rise_ps: Option<u64>,
    /// Cell fall delay in picoseconds
    pub cell_fall_ps: Option<u64>,
    /// Rise constraint (for setup/hold) in picoseconds
    pub rise_constraint_ps: Option<u64>,
    /// Fall constraint (for setup/hold) in picoseconds
    pub fall_constraint_ps: Option<u64>,
}

/// Timing information for a single pin.
#[derive(Debug, Clone, Default)]
pub struct PinTiming {
    /// Pin name
    pub name: String,
    /// Pin direction ("input", "output", "internal")
    pub direction: String,
    /// Whether this pin is a clock
    pub is_clock: bool,
    /// Timing arcs from/to this pin
    pub timing_arcs: Vec<TimingArc>,
}

/// Timing information for a cell.
#[derive(Debug, Clone, Default)]
pub struct CellTiming {
    /// Cell name
    pub name: String,
    /// Pin timing information, keyed by pin name
    pub pins: IndexMap<String, PinTiming>,
}

impl CellTiming {
    /// Get the maximum propagation delay through this cell (for combinational cells).
    /// Returns (rise_delay_ps, fall_delay_ps).
    pub fn max_combinational_delay(&self) -> (u64, u64) {
        let mut max_rise = 0u64;
        let mut max_fall = 0u64;

        for pin in self.pins.values() {
            if pin.direction == "output" {
                for arc in &pin.timing_arcs {
                    if arc.timing_type.is_none()
                        || arc.timing_type.as_deref() == Some("combinational")
                    {
                        if let Some(rise) = arc.cell_rise_ps {
                            max_rise = max_rise.max(rise);
                        }
                        if let Some(fall) = arc.cell_fall_ps {
                            max_fall = max_fall.max(fall);
                        }
                    }
                }
            }
        }

        (max_rise, max_fall)
    }

    /// Get setup time for data pin relative to clock (for sequential cells).
    /// Returns (rise_setup_ps, fall_setup_ps) for rising edge of clock.
    pub fn setup_time(&self, data_pin: &str) -> Option<(u64, u64)> {
        let pin = self.pins.get(data_pin)?;
        for arc in &pin.timing_arcs {
            if arc.timing_type.as_deref() == Some("setup_rising") {
                return Some((
                    arc.rise_constraint_ps.unwrap_or(0),
                    arc.fall_constraint_ps.unwrap_or(0),
                ));
            }
        }
        None
    }

    /// Get hold time for data pin relative to clock (for sequential cells).
    /// Returns (rise_hold_ps, fall_hold_ps) for rising edge of clock.
    pub fn hold_time(&self, data_pin: &str) -> Option<(u64, u64)> {
        let pin = self.pins.get(data_pin)?;
        for arc in &pin.timing_arcs {
            if arc.timing_type.as_deref() == Some("hold_rising") {
                return Some((
                    arc.rise_constraint_ps.unwrap_or(0),
                    arc.fall_constraint_ps.unwrap_or(0),
                ));
            }
        }
        None
    }

    /// Get clock-to-Q delay for output pin (for sequential cells).
    /// Returns (rise_delay_ps, fall_delay_ps).
    pub fn clock_to_q(&self, output_pin: &str) -> Option<(u64, u64)> {
        let pin = self.pins.get(output_pin)?;
        for arc in &pin.timing_arcs {
            if arc.timing_type.as_deref() == Some("rising_edge") {
                return Some((arc.cell_rise_ps.unwrap_or(0), arc.cell_fall_ps.unwrap_or(0)));
            }
        }
        None
    }
}

/// A Liberty timing library containing cell timing data.
#[derive(Debug, Clone, Default)]
pub struct TimingLibrary {
    /// Library name
    pub name: String,
    /// Time unit (e.g., "1ps")
    pub time_unit: String,
    /// Cells in the library, keyed by cell name
    pub cells: IndexMap<String, CellTiming>,
}

impl TimingLibrary {
    /// Load a Liberty library from a file path.
    pub fn from_file(path: impl AsRef<Path>) -> Result<Self, String> {
        let content =
            fs::read_to_string(path.as_ref()).map_err(|e| format!("Failed to read file: {}", e))?;
        Self::parse(&content)
    }

    /// Load the default AIGPDK library from the standard location.
    pub fn load_aigpdk() -> Result<Self, String> {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("aigpdk/aigpdk.lib");
        Self::from_file(path)
    }

    /// Create a TimingLibrary with default SKY130 timing values.
    ///
    /// These are approximate values based on typical SKY130 HD cell characteristics.
    /// For accurate timing, provide a proper Liberty file from the Skywater PDK.
    pub fn default_sky130() -> Self {
        // SKY130 typical timing values (in picoseconds):
        // - Inverter: ~30ps
        // - 2-input gates: ~50ps
        // - Complex gates (AOI/OAI): ~60-80ps
        // - DFF clk-to-Q: ~150ps
        // - DFF setup: ~80ps
        // - DFF hold: ~20ps
        // - SRAM read: ~500ps (conservative estimate)

        let mut lib = TimingLibrary {
            name: "sky130_default".to_string(),
            time_unit: "1ps".to_string(),
            cells: IndexMap::new(),
        };

        // Add generic AND gate timing (used for all combinational logic)
        let mut and_cell = CellTiming {
            name: "AND2".to_string(),
            pins: IndexMap::new(),
        };
        let and_out = PinTiming {
            name: "X".to_string(),
            direction: "output".to_string(),
            is_clock: false,
            timing_arcs: vec![TimingArc {
                related_pin: "A".to_string(),
                timing_type: None,
                cell_rise_ps: Some(50),
                cell_fall_ps: Some(50),
                rise_constraint_ps: None,
                fall_constraint_ps: None,
            }],
        };
        and_cell.pins.insert("X".to_string(), and_out);
        lib.cells.insert("AND2".to_string(), and_cell);

        // Also add as AND2_00_0 for compatibility with existing lookup
        let mut and_compat = CellTiming {
            name: "AND2_00_0".to_string(),
            pins: IndexMap::new(),
        };
        let and_compat_out = PinTiming {
            name: "Y".to_string(),
            direction: "output".to_string(),
            is_clock: false,
            timing_arcs: vec![TimingArc {
                related_pin: "A".to_string(),
                timing_type: None,
                cell_rise_ps: Some(50),
                cell_fall_ps: Some(50),
                rise_constraint_ps: None,
                fall_constraint_ps: None,
            }],
        };
        and_compat.pins.insert("Y".to_string(), and_compat_out);
        lib.cells.insert("AND2_00_0".to_string(), and_compat);

        // Add DFF timing
        let mut dff_cell = CellTiming {
            name: "DFF".to_string(),
            pins: IndexMap::new(),
        };
        // D pin with setup/hold
        let d_pin = PinTiming {
            name: "D".to_string(),
            direction: "input".to_string(),
            is_clock: false,
            timing_arcs: vec![
                TimingArc {
                    related_pin: "CLK".to_string(),
                    timing_type: Some("setup_rising".to_string()),
                    cell_rise_ps: None,
                    cell_fall_ps: None,
                    rise_constraint_ps: Some(80),
                    fall_constraint_ps: Some(80),
                },
                TimingArc {
                    related_pin: "CLK".to_string(),
                    timing_type: Some("hold_rising".to_string()),
                    cell_rise_ps: None,
                    cell_fall_ps: None,
                    rise_constraint_ps: Some(20),
                    fall_constraint_ps: Some(20),
                },
            ],
        };
        dff_cell.pins.insert("D".to_string(), d_pin);
        // Q pin with clk-to-Q
        let q_pin = PinTiming {
            name: "Q".to_string(),
            direction: "output".to_string(),
            is_clock: false,
            timing_arcs: vec![TimingArc {
                related_pin: "CLK".to_string(),
                timing_type: Some("rising_edge".to_string()),
                cell_rise_ps: Some(150),
                cell_fall_ps: Some(150),
                rise_constraint_ps: None,
                fall_constraint_ps: None,
            }],
        };
        dff_cell.pins.insert("Q".to_string(), q_pin);
        lib.cells.insert("DFF".to_string(), dff_cell);

        // Add SRAM timing (for CF_SRAM_* cells)
        let mut sram_cell = CellTiming {
            name: "$__RAMGEM_SYNC_".to_string(),
            pins: IndexMap::new(),
        };
        let rd_data_pin = PinTiming {
            name: "PORT_R_RD_DATA".to_string(),
            direction: "output".to_string(),
            is_clock: false,
            timing_arcs: vec![TimingArc {
                related_pin: "PORT_R_CLK".to_string(),
                timing_type: Some("rising_edge".to_string()),
                cell_rise_ps: Some(500),
                cell_fall_ps: Some(500),
                rise_constraint_ps: None,
                fall_constraint_ps: None,
            }],
        };
        sram_cell
            .pins
            .insert("PORT_R_RD_DATA".to_string(), rd_data_pin);
        lib.cells.insert("$__RAMGEM_SYNC_".to_string(), sram_cell);

        lib
    }

    /// Parse Liberty content from a string.
    ///
    /// Errors if the parser succeeded structurally but extracted zero
    /// cells — this is the silent-failure mode that issue #2 of the
    /// timing-correctness review called out, and the WS5 deliverable in
    /// `docs/plans/phase-0-ir-and-oracle.md`. The structural parser
    /// already rejects input without a `library` block, so this check
    /// only fires when the input was real-shaped Liberty that yielded no
    /// usable cells.
    ///
    /// Test fixtures that intentionally provide skeletal Liberty content
    /// can use [`Self::parse_unchecked`] to bypass the check.
    pub fn parse(content: &str) -> Result<Self, String> {
        let lib = Self::parse_unchecked(content)?;
        if lib.cells.is_empty() {
            return Err(format!(
                "Liberty input ({} bytes) parsed structurally but yielded no cells. \
                 This indicates a parser issue or unrecognised dialect. Investigate \
                 the input, or call TimingLibrary::parse_unchecked if zero-cell parses \
                 are intentional here.",
                content.len()
            ));
        }
        Ok(lib)
    }

    /// Parse Liberty content without the non-empty-cells sanity check.
    ///
    /// For test fixtures that intentionally provide minimal Liberty input,
    /// or for callers that have already validated the input by other means.
    /// Production paths should prefer [`Self::parse`].
    pub fn parse_unchecked(content: &str) -> Result<Self, String> {
        let root = liberty_parse::parse(content)?;
        Ok(walk::walk_library(&root))
    }

    /// Get timing for a cell by name.
    pub fn get_cell(&self, name: &str) -> Option<&CellTiming> {
        self.cells.get(name)
    }

    /// Get combinational delay for an AND gate variant.
    /// The AIGPDK uses AND2_XY_Z naming where X,Y are inversion flags.
    pub fn and_gate_delay(&self, cell_name: &str) -> Option<(u64, u64)> {
        self.cells
            .get(cell_name)
            .map(|c| c.max_combinational_delay())
    }

    /// Get delay for the inverter cell (INV).
    pub fn inv_delay(&self) -> Option<(u64, u64)> {
        self.cells.get("INV").map(|c| c.max_combinational_delay())
    }

    /// Get delay for the buffer cell (BUF).
    pub fn buf_delay(&self) -> Option<(u64, u64)> {
        self.cells.get("BUF").map(|c| c.max_combinational_delay())
    }

    /// Get DFF timing information.
    pub fn dff_timing(&self) -> Option<DFFTiming> {
        let cell = self.cells.get("DFF")?;
        Some(DFFTiming {
            setup_rise_ps: cell.setup_time("D").map(|(r, _)| r).unwrap_or(0),
            setup_fall_ps: cell.setup_time("D").map(|(_, f)| f).unwrap_or(0),
            hold_rise_ps: cell.hold_time("D").map(|(r, _)| r).unwrap_or(0),
            hold_fall_ps: cell.hold_time("D").map(|(_, f)| f).unwrap_or(0),
            clk_to_q_rise_ps: cell.clock_to_q("Q").map(|(r, _)| r).unwrap_or(0),
            clk_to_q_fall_ps: cell.clock_to_q("Q").map(|(_, f)| f).unwrap_or(0),
        })
    }

    /// Get DFFSR (DFF with set/reset) timing information.
    pub fn dffsr_timing(&self) -> Option<DFFTiming> {
        let cell = self.cells.get("DFFSR")?;
        Some(DFFTiming {
            setup_rise_ps: cell.setup_time("D").map(|(r, _)| r).unwrap_or(0),
            setup_fall_ps: cell.setup_time("D").map(|(_, f)| f).unwrap_or(0),
            hold_rise_ps: cell.hold_time("D").map(|(r, _)| r).unwrap_or(0),
            hold_fall_ps: cell.hold_time("D").map(|(_, f)| f).unwrap_or(0),
            clk_to_q_rise_ps: cell.clock_to_q("Q").map(|(r, _)| r).unwrap_or(0),
            clk_to_q_fall_ps: cell.clock_to_q("Q").map(|(_, f)| f).unwrap_or(0),
        })
    }

    /// Get SRAM timing information.
    pub fn sram_timing(&self) -> Option<SRAMTiming> {
        let cell = self.cells.get("$__RAMGEM_SYNC_")?;

        // Get read data output timing (clock to Q)
        let read_delay = cell
            .pins
            .get("PORT_R_RD_DATA")
            .and_then(|p| {
                p.timing_arcs.iter().find_map(|arc| {
                    if arc.timing_type.as_deref() == Some("rising_edge") {
                        Some((arc.cell_rise_ps.unwrap_or(0), arc.cell_fall_ps.unwrap_or(0)))
                    } else {
                        None
                    }
                })
            })
            .unwrap_or((0, 0));

        Some(SRAMTiming {
            read_clk_to_data_rise_ps: read_delay.0,
            read_clk_to_data_fall_ps: read_delay.1,
            // Setup/hold for address and data inputs (simplified)
            addr_setup_ps: 0, // These use falling edge timing which is 0.0001ps
            addr_hold_ps: 0,
            write_data_setup_ps: 0,
            write_data_hold_ps: 0,
        })
    }
}

/// DFF timing parameters.
#[derive(Debug, Clone, Default)]
pub struct DFFTiming {
    /// Setup time for rising data transition (ps)
    pub setup_rise_ps: u64,
    /// Setup time for falling data transition (ps)
    pub setup_fall_ps: u64,
    /// Hold time for rising data transition (ps)
    pub hold_rise_ps: u64,
    /// Hold time for falling data transition (ps)
    pub hold_fall_ps: u64,
    /// Clock-to-Q delay for rising output (ps)
    pub clk_to_q_rise_ps: u64,
    /// Clock-to-Q delay for falling output (ps)
    pub clk_to_q_fall_ps: u64,
}

impl DFFTiming {
    /// Get maximum setup time (ps).
    pub fn max_setup(&self) -> u64 {
        self.setup_rise_ps.max(self.setup_fall_ps)
    }

    /// Get maximum hold time (ps).
    pub fn max_hold(&self) -> u64 {
        self.hold_rise_ps.max(self.hold_fall_ps)
    }

    /// Get maximum clock-to-Q delay (ps).
    pub fn max_clk_to_q(&self) -> u64 {
        self.clk_to_q_rise_ps.max(self.clk_to_q_fall_ps)
    }
}

/// SRAM timing parameters.
#[derive(Debug, Clone, Default)]
pub struct SRAMTiming {
    /// Read clock to data output rise delay (ps)
    pub read_clk_to_data_rise_ps: u64,
    /// Read clock to data output fall delay (ps)
    pub read_clk_to_data_fall_ps: u64,
    /// Address setup time (ps)
    pub addr_setup_ps: u64,
    /// Address hold time (ps)
    pub addr_hold_ps: u64,
    /// Write data setup time (ps)
    pub write_data_setup_ps: u64,
    /// Write data hold time (ps)
    pub write_data_hold_ps: u64,
}

/// Tree-walking extraction from the generic Liberty AST.
///
/// The low-level tokenizer and group/attribute parser now live in the
/// `liberty-parse` crate (the single Liberty front-end — ADR 0019 D6).
/// This module's job is purely to *walk* the resulting [`LibertyGroup`]
/// tree and project it onto the timing types (`TimingLibrary`,
/// `CellTiming`, `PinTiming`, `TimingArc`). No timing semantics changed in
/// the refactor: the same groups/attributes are read, the same scalar
/// extraction and ps conversion are applied.
mod walk {
    use super::{CellTiming, PinTiming, TimingArc, TimingLibrary};
    use liberty_parse::LibertyGroup;

    /// Parse a floating point value and convert to picoseconds (integer).
    ///
    /// The Liberty file uses `time_unit : "1ps"`, so values are already in
    /// ps; we only round to the nearest integer. Ported verbatim from the
    /// former `LibertyParser::parse_float_to_ps`.
    fn parse_float_to_ps(value: &str) -> u64 {
        let value = value.trim().trim_matches('"');
        if let Ok(f) = value.parse::<f64>() {
            // time_unit is 1ps, so no scale factor — just round to integer ps.
            f.round() as u64
        } else {
            0
        }
    }

    /// Extract the first scalar from a timing-table group such as
    /// `cell_rise (scalar) { values ("1.0"); }`.
    ///
    /// Mirrors the former parser: find the `values` attribute and take the
    /// first numeric token, splitting on commas/quotes so a list-valued
    /// `values ("a, b, c")` yields its first entry. 2-D lookup tables (no
    /// scalar leading value, or absent `values`) yield `None`, matching the
    /// old "skip and use default timing" behaviour.
    fn extract_scalar_ps(table: &LibertyGroup) -> Option<u64> {
        let values = table.attr("values")?;
        let first = values.first_string()?;
        // `first_string` already returns the de-quoted text of the first
        // value. A list packed in one quoted string ("a, b, c") still needs
        // splitting on the separators the old parser split on.
        let token = first
            .trim_start()
            .trim_start_matches('"')
            .split([',', '"', ')'])
            .next()
            .unwrap_or("")
            .trim();
        if token.is_empty() {
            None
        } else {
            Some(parse_float_to_ps(token))
        }
    }

    /// Walk a `timing () { ... }` group into a [`TimingArc`].
    fn walk_timing_arc(timing: &LibertyGroup) -> TimingArc {
        let mut arc = TimingArc::default();

        if let Some(rp) = timing.attr("related_pin").and_then(|a| a.first_string()) {
            arc.related_pin = rp.trim_matches('"').to_string();
        }
        if let Some(tt) = timing.attr("timing_type").and_then(|a| a.first_string()) {
            arc.timing_type = Some(tt.trim_matches('"').to_string());
        }

        // Timing tables are nested groups: cell_rise / cell_fall /
        // rise_constraint / fall_constraint. Use the FIRST of each (the old
        // parser overwrote on repeats too, so first-wins vs last-wins only
        // differs for malformed duplicate tables; first is what extraction
        // would observe walking in order).
        for table in &timing.groups {
            let slot = match table.group_type.as_str() {
                "cell_rise" => &mut arc.cell_rise_ps,
                "cell_fall" => &mut arc.cell_fall_ps,
                "rise_constraint" => &mut arc.rise_constraint_ps,
                "fall_constraint" => &mut arc.fall_constraint_ps,
                _ => continue,
            };
            if slot.is_none() {
                if let Some(ps) = extract_scalar_ps(table) {
                    *slot = Some(ps);
                }
            }
        }

        arc
    }

    /// Walk a `pin (...) { ... }` or `bus (...) { ... }` group into a
    /// [`PinTiming`]. Buses are treated like pins for timing purposes
    /// (their direction + timing arcs), matching the former `parse_bus`.
    fn walk_pin(pin_group: &LibertyGroup) -> PinTiming {
        let mut pin = PinTiming {
            name: pin_group.first_name().unwrap_or("").to_string(),
            ..Default::default()
        };

        if let Some(dir) = pin_group.attr("direction").and_then(|a| a.first_string()) {
            pin.direction = dir.trim_matches('"').to_string();
        }
        if let Some(clk) = pin_group.attr("clock").and_then(|a| a.first_string()) {
            pin.is_clock = clk.trim_matches('"') == "true";
        }

        for timing in pin_group.groups_of_type("timing") {
            pin.timing_arcs.push(walk_timing_arc(timing));
        }

        pin
    }

    /// Walk a `cell (...) { ... }` group into a [`CellTiming`].
    fn walk_cell(cell_group: &LibertyGroup) -> CellTiming {
        let mut cell = CellTiming {
            name: cell_group.first_name().unwrap_or("").to_string(),
            ..Default::default()
        };

        // `pin` and `bus` groups both carry pin-level timing. Walk them in
        // source order; later definitions overwrite earlier ones by name,
        // matching the former insert-by-name behaviour.
        for child in &cell_group.groups {
            match child.group_type.as_str() {
                "pin" | "bus" => {
                    let pin = walk_pin(child);
                    cell.pins.insert(pin.name.clone(), pin);
                }
                _ => {}
            }
        }

        cell
    }

    /// Project a parsed Liberty `library` group onto a [`TimingLibrary`].
    pub(super) fn walk_library(root: &LibertyGroup) -> TimingLibrary {
        let mut lib = TimingLibrary {
            name: root.first_name().unwrap_or("").to_string(),
            ..Default::default()
        };

        if let Some(tu) = root.attr("time_unit").and_then(|a| a.first_string()) {
            lib.time_unit = tu.trim_matches('"').to_string();
        }

        for cell_group in root.groups_of_type("cell") {
            let cell = walk_cell(cell_group);
            lib.cells.insert(cell.name.clone(), cell);
        }

        lib
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_aigpdk() {
        let lib = TimingLibrary::load_aigpdk().expect("Failed to load AIGPDK library");

        assert_eq!(lib.name, "aigpdk");
        assert_eq!(lib.time_unit, "1ps");

        // Check AND gate timing
        let and_timing = lib.and_gate_delay("AND2_00_0");
        assert!(and_timing.is_some());
        let (rise, fall) = and_timing.unwrap();
        assert_eq!(rise, 1); // 1.0ps
        assert_eq!(fall, 1);

        // Check INV timing (near-zero delay)
        let inv_timing = lib.inv_delay();
        assert!(inv_timing.is_some());
        let (rise, fall) = inv_timing.unwrap();
        assert_eq!(rise, 0); // 0.0001ps rounds to 0
        assert_eq!(fall, 0);

        // Check DFF timing
        let dff = lib.dff_timing();
        assert!(dff.is_some());
        let dff = dff.unwrap();
        assert_eq!(dff.max_setup(), 0); // 0.0001ps rounds to 0
        assert_eq!(dff.max_hold(), 0);
        assert_eq!(dff.max_clk_to_q(), 0);

        // Check SRAM timing
        let sram = lib.sram_timing();
        assert!(sram.is_some());
        let sram = sram.unwrap();
        assert_eq!(sram.read_clk_to_data_rise_ps, 1); // 1.0ps
    }

    #[test]
    fn test_all_and_gates() {
        let lib = TimingLibrary::load_aigpdk().expect("Failed to load AIGPDK library");

        let and_variants = [
            "AND2_00_0",
            "AND2_01_0",
            "AND2_10_0",
            "AND2_11_0",
            "AND2_11_1",
        ];

        for name in and_variants {
            let timing = lib.and_gate_delay(name);
            assert!(timing.is_some(), "Missing timing for {}", name);
            let (rise, fall) = timing.unwrap();
            assert_eq!(rise, 1, "Wrong rise delay for {}", name);
            assert_eq!(fall, 1, "Wrong fall delay for {}", name);
        }
    }

    #[test]
    fn test_parse_sky130_format() {
        // Test SKY130-style liberty format with quoted names
        let sky130_lib = r#"
library ("sky130_fd_sc_hd__tt_025C_1v80") {
  time_unit : "1ns";
  cell ("sky130_fd_sc_hd__inv_1") {
    pin ("A") {
      direction : input;
    }
    pin ("Y") {
      direction : output;
      timing () {
        related_pin : "A";
        cell_rise (scalar) {
          values ("50.0");
        }
        cell_fall (scalar) {
          values ("45.0");
        }
      }
    }
  }
  cell ("sky130_fd_sc_hd__dfxtp_1") {
    pin ("CLK") {
      direction : input;
      clock : true;
    }
    pin ("D") {
      direction : input;
      timing () {
        related_pin : "CLK";
        timing_type : setup_rising;
        rise_constraint (scalar) {
          values ("80.0");
        }
        fall_constraint (scalar) {
          values ("75.0");
        }
      }
    }
    pin ("Q") {
      direction : output;
      timing () {
        related_pin : "CLK";
        timing_type : rising_edge;
        cell_rise (scalar) {
          values ("150.0");
        }
        cell_fall (scalar) {
          values ("140.0");
        }
      }
    }
  }
}
"#;

        let lib = TimingLibrary::parse(sky130_lib).expect("Failed to parse SKY130-style library");

        // Check library name
        assert_eq!(lib.name, "sky130_fd_sc_hd__tt_025C_1v80");
        assert_eq!(lib.time_unit, "1ns");

        // Check inverter cell
        let inv_cell = lib.get_cell("sky130_fd_sc_hd__inv_1");
        assert!(inv_cell.is_some(), "Missing inverter cell");
        let inv = inv_cell.unwrap();
        let (rise, fall) = inv.max_combinational_delay();
        assert_eq!(rise, 50); // 50ns
        assert_eq!(fall, 45); // 45ns

        // Check DFF cell
        let dff_cell = lib.get_cell("sky130_fd_sc_hd__dfxtp_1");
        assert!(dff_cell.is_some(), "Missing DFF cell");
        let dff = dff_cell.unwrap();

        // Check setup time
        let setup = dff.setup_time("D");
        assert!(setup.is_some(), "Missing setup time");
        let (rise_setup, fall_setup) = setup.unwrap();
        assert_eq!(rise_setup, 80);
        assert_eq!(fall_setup, 75);

        // Check clock-to-Q
        let clk_q = dff.clock_to_q("Q");
        assert!(clk_q.is_some(), "Missing clock-to-Q");
        let (rise_q, fall_q) = clk_q.unwrap();
        assert_eq!(rise_q, 150);
        assert_eq!(fall_q, 140);
    }

    /// Liberty input that looks real but contains no cells. `parse` should
    /// fail loudly rather than silently succeed with an empty library.
    #[test]
    fn parse_rejects_library_input_with_zero_cells() {
        let no_cells = r#"library(test) {
            technology(cmos);
            time_unit : "1ps";
        }"#;
        let err = TimingLibrary::parse(no_cells)
            .expect_err("expected parse() to reject zero-cell Liberty input");
        assert!(
            err.contains("yielded no cells"),
            "unexpected error message: {err}"
        );
    }

    /// `parse_unchecked` is the explicit opt-out for fixtures that
    /// intentionally provide skeletal Liberty content.
    #[test]
    fn parse_unchecked_accepts_zero_cell_library() {
        let no_cells = r#"library(test) {
            technology(cmos);
            time_unit : "1ps";
        }"#;
        let lib = TimingLibrary::parse_unchecked(no_cells)
            .expect("parse_unchecked should accept zero-cell Liberty");
        assert!(lib.cells.is_empty());
    }
}
