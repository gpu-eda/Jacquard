// SPDX-License-Identifier: Apache-2.0
//! Static enumeration of a design's X-sources for `cosim --xprop` debugging.
//!
//! Under `cosim --xprop` an X value originates at a known X-source — an
//! unreset DFF Q (undefined at power-up), an SRAM read port (memory undefined
//! until written), or a primary input the testbench leaves undriven — and
//! propagates forward through combinational logic. Finding *why* a given
//! signal reads X is otherwise a manual trace→guess→re-run loop.
//!
//! This module computes the X-source set statically from the netlist (+ the
//! cosim config, for the undriven-input class) so the `jacquard xsources`
//! subcommand can dump it as JSON. `netlist-graph xroots` then intersects a
//! signal's backward cone with this set to answer "which X-sources make
//! signal S read X?" — see issue #98 and `docs/x-debugging.md`.
//!
//! Everything here is plain CPU logic (no GPU/Metal feature): X-sources are a
//! static property of the netlist + config, independent of simulation.

use std::collections::{HashMap, HashSet};

use netlistdb::{Direction, GeneralHierName, GeneralPinName, NetlistDB};
use serde::Serialize;

use crate::aig::{DriverType, AIG};
use crate::testbench::TestbenchConfig;

/// Schema version of the emitted manifest. Additive-only: new fields and new
/// `XSourceKind` variants may be added without a major bump.
///
/// 1.1: added the optional per-source `src` (RTL `file:line` provenance, WS-B).
pub const X_SOURCE_SCHEMA_VERSION: &str = "1.1";

/// Classification of an X-source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum XSourceKind {
    /// DFF Q with no reset/set pin connected — X at power-up and never reset,
    /// so a persistent root unless its data cone resolves to a known value.
    UnresetDff,
    /// DFF Q *with* a reset/set pin — X at power-up but defined once reset
    /// asserts. Reported so a backward query can see it, but it is usually not
    /// the culprit when reset has been applied.
    ResetDff,
    /// SRAM read-data port — X until the addressed cell has been written.
    SramRead,
    /// Primary input the cosim config leaves undriven — no clock/reset/
    /// constant/peripheral drives it, so it reads X every tick.
    UndrivenInput,
}

/// One X-source: the net that carries the X, its classification, and (for
/// DFF/SRAM) the driving cell instance.
#[derive(Debug, Clone, Serialize)]
pub struct XSource {
    /// Hierarchical net name, in the same convention `--trace-signals`
    /// resolves and `netlist-graph` matches against.
    pub net: String,
    pub kind: XSourceKind,
    /// Driving cell instance name (DFF/SRAM sources only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cell: Option<String>,
    /// RTL source location (`file:line`) of the driving cell, from Yosys
    /// `(* src *)` provenance (WS-B). Present for DFF/SRAM sources when the
    /// netlist carried provenance; `None` for undriven inputs (no cell) and for
    /// `-noattr`/non-provenance netlists. Lets a query report `spiflash.v:88`
    /// instead of the flattened cell name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub src: Option<String>,
}

/// The RTL source location of a netlist cell, from its `(* src *)` provenance
/// (`NetlistDB.cell_src`, WS-B B1), as an owned `String` for serialization.
fn cell_src_string(netlistdb: &NetlistDB, cell_id: usize) -> Option<String> {
    netlistdb
        .cell_src
        .get(cell_id)
        .and_then(|o| o.as_ref())
        .map(|s| s.to_string())
}

/// The full manifest emitted by `jacquard xsources`.
#[derive(Debug, Clone, Serialize)]
pub struct XSourceManifest {
    pub schema_version: String,
    /// The netlist path the manifest was computed from.
    pub netlist: String,
    /// Whether undriven-input classification was performed. False when no
    /// config was supplied (then only DFF/SRAM sources are present).
    pub undriven_inputs_classified: bool,
    pub x_sources: Vec<XSource>,
}

/// Pin-name leaves that mark a DFF reset or set input. Matched case-
/// insensitively against the cell's pin port names across sky130 / gf180mcu /
/// generic libraries.
const RESET_SET_PIN_LEAVES: &[&str] = &[
    "reset_b", "set_b", "resetn", "setn", "reset", "set", "clrz", "clr", "clrn", "pre", "pren",
    "rn", "sn", "rb", "sb", "r", "s",
];

/// Parse a GPIO index from a port name like `gpio_in[5]` or `gpio_in:5`.
///
/// Shared by cosim's GPIO mapping (`build_gpio_mapping`) and `xsources`'
/// driven-input set so the two stay consistent — see issue #98.
pub fn parse_gpio_index(pin_name: &str, prefix: &str) -> Option<usize> {
    // Try "gpio_in[N]" format.
    if let Some(start) = pin_name.find(&format!("{}[", prefix)) {
        let after = &pin_name[start + prefix.len() + 1..];
        if let Some(end) = after.find(']') {
            return after[..end].parse().ok();
        }
    }
    // Try "gpio_in:N" format.
    if let Some(start) = pin_name.find(&format!("{}:", prefix)) {
        let after = &pin_name[start + prefix.len() + 1..];
        let num_str: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
        return num_str.parse().ok();
    }
    None
}

/// Build aigpin → representative netlistdb net-id. A net's driver pin and all
/// its load pins share the same aigpin, so any pin on the net resolves the
/// name; we keep the first seen.
fn build_aigpin_to_net(netlistdb: &NetlistDB, aig: &AIG) -> HashMap<usize, usize> {
    let mut map: HashMap<usize, usize> = HashMap::new();
    for pinid in 0..netlistdb.num_pins {
        let aigpin = aig.pin2aigpin_iv[pinid] >> 1;
        let netid = netlistdb.pin2net[pinid];
        map.entry(aigpin).or_insert(netid);
    }
    map
}

/// Name an aigpin by its net (falling back to a synthetic label if unmapped).
fn net_name_of(aigpin: usize, aigpin2net: &HashMap<usize, usize>, netlistdb: &NetlistDB) -> String {
    match aigpin2net.get(&aigpin) {
        Some(&netid) => netlistdb.netnames[netid].dbg_fmt_pin(),
        None => format!("<aigpin {aigpin}>"),
    }
}

/// Name a cell's primary output net (the net driven by its first output pin).
///
/// Used to name DFF Q and SRAM read-data sources directly from the netlist,
/// which is robust where an AIG-pin → net lookup is not: a reset DFF's
/// `dff.q` aigpin is the post-reset-mux internal node, not the node bound to
/// the netlist Q pin, so it has no entry in the aigpin→net map.
fn cell_output_net_name(netlistdb: &NetlistDB, cell_id: usize) -> Option<String> {
    for pinid in netlistdb.cell2pin.iter_set(cell_id) {
        if netlistdb.pindirect[pinid] == Direction::O {
            let netid = netlistdb.pin2net[pinid];
            return Some(netlistdb.netnames[netid].dbg_fmt_pin());
        }
    }
    None
}

/// Whether a DFF cell instance has a reset or set pin wired up.
fn dff_has_reset(netlistdb: &NetlistDB, cell_id: usize) -> bool {
    for pinid in netlistdb.cell2pin.iter_set(cell_id) {
        // pinnames[pinid].1 is the port leaf name (e.g. "RESET_B").
        let leaf = netlistdb.pinnames[pinid].1.to_ascii_lowercase();
        if RESET_SET_PIN_LEAVES.contains(&leaf.as_str()) {
            return true;
        }
    }
    false
}

/// The set of GPIO indices and explicit port names the config drives.
///
/// Mirrors the driver wiring `cosim` performs: clock(s), reset, constant
/// inputs/ports, and every peripheral's pins. A primary input outside this set
/// is left undriven and reads X under `--xprop`. Keep in sync with the cosim
/// driving sites in `cosim_metal.rs` (reset/constant application, `build_edge_ops`,
/// `set_flash_din`, and each `PeripheralModel`'s driven pins) — a peripheral
/// that starts driving an input there must be reflected here, or that input is
/// mis-reported as an undriven X-source.
fn driven_gpios_and_ports(config: &TestbenchConfig) -> (HashSet<usize>, HashSet<String>) {
    let mut gpios: HashSet<usize> = HashSet::new();
    let mut ports: HashSet<String> = HashSet::new();

    gpios.insert(config.reset_gpio);
    for clk in config.effective_clocks() {
        gpios.insert(clk.gpio);
    }
    for key in config.constant_inputs.keys() {
        if let Ok(idx) = key.parse::<usize>() {
            gpios.insert(idx);
        }
    }
    for name in config.constant_ports.keys() {
        ports.insert(name.clone());
    }
    for flash in config.effective_qspi_memory() {
        gpios.extend([flash.clk_gpio, flash.csn_gpio]);
        // `set_flash_din` drives 4 data lanes (d0..d0+3) for dual/quad SPI.
        gpios.extend((0..4).map(|i| flash.d0_gpio + i));
    }
    for uart in config.effective_uarts() {
        // rx is a design input the model drives; tx is a design output —
        // harmless to include, it is not a primary input.
        gpios.insert(uart.rx_gpio);
        gpios.insert(uart.tx_gpio);
    }
    for gpio in &config.gpios {
        gpios.extend(gpio.pins.iter().copied());
    }
    if let Some(jtag) = &config.jtag {
        gpios.extend([jtag.tck_gpio, jtag.tms_gpio, jtag.tdi_gpio]);
        if let Some(trst) = jtag.trst_gpio {
            gpios.insert(trst);
        }
    }
    (gpios, ports)
}

/// Compute the X-source manifest for a design.
///
/// `config` is required only for the `undriven-input` class; without it those
/// sources are omitted and `undriven_inputs_classified` is false.
pub fn compute_x_source_manifest(
    netlistdb: &NetlistDB,
    aig: &AIG,
    config: Option<&TestbenchConfig>,
    netlist_path: &str,
) -> XSourceManifest {
    let aigpin2net = build_aigpin_to_net(netlistdb, aig);
    let mut x_sources: Vec<XSource> = Vec::new();

    // DFF Q outputs (cell_id → DFF). Name via the netlist Q output pin
    // (robust for reset DFFs, whose `dff.q` aigpin is the post-mux node).
    for &cell_id in aig.dffs.keys() {
        let net = cell_output_net_name(netlistdb, cell_id)
            .unwrap_or_else(|| format!("<dff cell {cell_id}>"));
        let kind = if dff_has_reset(netlistdb, cell_id) {
            XSourceKind::ResetDff
        } else {
            XSourceKind::UnresetDff
        };
        x_sources.push(XSource {
            net,
            kind,
            cell: Some(netlistdb.cellnames[cell_id].dbg_fmt_hier()),
            src: cell_src_string(netlistdb, cell_id),
        });
    }

    // SRAM read-data ports (cell_id → RAMBlock). Slot 0 = unused, skip.
    for (&cell_id, sram) in &aig.srams {
        let cell = netlistdb.cellnames[cell_id].dbg_fmt_hier();
        for &rd_pin in &sram.port_r_rd_data {
            if rd_pin == 0 {
                continue;
            }
            x_sources.push(XSource {
                net: net_name_of(rd_pin, &aigpin2net, netlistdb),
                kind: XSourceKind::SramRead,
                cell: Some(cell.clone()),
                src: cell_src_string(netlistdb, cell_id),
            });
        }
    }

    // Undriven primary inputs — needs the config to know the driven set.
    let undriven_inputs_classified = config.is_some();
    if let Some(config) = config {
        let (driven_gpios, driven_ports) = driven_gpios_and_ports(config);
        let input_name_to_gpio: HashMap<String, usize> = config
            .port_mapping
            .as_ref()
            .map(|pm| {
                pm.inputs
                    .iter()
                    .filter_map(|(k, v)| k.parse::<usize>().ok().map(|idx| (v.clone(), idx)))
                    .collect()
            })
            .unwrap_or_default();

        for (aigpin, driv) in aig.drivers.iter().enumerate() {
            let DriverType::InputPort(pinid) = driv else {
                continue;
            };
            let pin_name = netlistdb.pinnames[*pinid].dbg_fmt_pin();
            let gpio_idx = input_name_to_gpio
                .get(&pin_name)
                .copied()
                .or_else(|| parse_gpio_index(&pin_name, "gpio_in"));
            let driven = gpio_idx.is_some_and(|idx| driven_gpios.contains(&idx))
                || driven_ports.contains(&pin_name);
            if !driven {
                x_sources.push(XSource {
                    net: net_name_of(aigpin, &aigpin2net, netlistdb),
                    kind: XSourceKind::UndrivenInput,
                    cell: None,
                    // Undriven inputs are ports, not cells — no `(* src *)`.
                    src: None,
                });
            }
        }
    }

    XSourceManifest {
        schema_version: X_SOURCE_SCHEMA_VERSION.to_string(),
        netlist: netlist_path.to_string(),
        undriven_inputs_classified,
        x_sources,
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::sim::setup::build_netlist_and_aig;

    const DEMO_NETLIST: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/xprop_cosim/xprop_demo_synth.gv"
    );
    const DEMO_CONFIG: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/xprop_cosim/sim_config.json"
    );

    fn demo_manifest(with_config: bool) -> XSourceManifest {
        let (netlistdb, aig) =
            build_netlist_and_aig(Path::new(DEMO_NETLIST), None, &[], None, None);
        let config: Option<TestbenchConfig> = if with_config {
            let f = std::fs::File::open(DEMO_CONFIG).unwrap();
            Some(serde_json::from_reader(std::io::BufReader::new(f)).unwrap())
        } else {
            None
        };
        compute_x_source_manifest(&netlistdb, &aig, config.as_ref(), DEMO_NETLIST)
    }

    fn kind_of<'a>(m: &'a XSourceManifest, net: &str) -> Option<&'a XSourceKind> {
        m.x_sources.iter().find(|s| s.net == net).map(|s| &s.kind)
    }

    /// Look up a source by its driving cell instance. Cell names are stable;
    /// net names go through netlistdb canonicalisation (assign-merge), so the
    /// counter flops' Q nets are emitted as `_10_[*]` not `unreset_count[*]`.
    fn kind_of_cell<'a>(m: &'a XSourceManifest, cell: &str) -> Option<&'a XSourceKind> {
        m.x_sources
            .iter()
            .find(|s| s.cell.as_deref() == Some(cell))
            .map(|s| &s.kind)
    }

    #[test]
    fn dff_sources_classified_by_reset_connection() {
        let m = demo_manifest(false);
        // Self-holding register with no reset → unreset (named output port).
        assert_eq!(kind_of(&m, "q_unreset"), Some(&XSourceKind::UnresetDff));
        // The counter flops (cells _30_/_31_/_32_) have no reset either.
        assert_eq!(kind_of_cell(&m, "_30_"), Some(&XSourceKind::UnresetDff));
        assert_eq!(kind_of_cell(&m, "_32_"), Some(&XSourceKind::UnresetDff));
        // DFFSR-based regs carry a reset/set pin → reset-dff.
        assert_eq!(kind_of(&m, "q_reset"), Some(&XSourceKind::ResetDff));
        assert_eq!(kind_of(&m, "q_undriven_reg"), Some(&XSourceKind::ResetDff));
    }

    #[test]
    fn dff_nets_named_not_synthetic() {
        // Reset DFFs must resolve to real net names, not `<aigpin N>` /
        // `<dff cell N>` fallbacks (regression for the post-reset-mux aigpin).
        let m = demo_manifest(false);
        for s in &m.x_sources {
            assert!(
                !s.net.starts_with('<'),
                "x-source {s:?} fell back to a synthetic name"
            );
        }
    }

    #[test]
    fn no_config_omits_undriven_inputs() {
        let m = demo_manifest(false);
        assert!(!m.undriven_inputs_classified);
        assert!(
            !m.x_sources
                .iter()
                .any(|s| s.kind == XSourceKind::UndrivenInput),
            "undriven inputs must not be classified without a config"
        );
    }

    #[test]
    fn config_classifies_undriven_but_not_driven_inputs() {
        let m = demo_manifest(true);
        assert!(m.undriven_inputs_classified);
        // The genuinely-unconnected input reads X.
        assert_eq!(
            kind_of(&m, "unconnected"),
            Some(&XSourceKind::UndrivenInput)
        );
        // clk / rst_n are driven (clock / reset gpio via port_mapping), so
        // they must NOT be reported as undriven X-sources.
        assert!(kind_of(&m, "clk").is_none());
        assert!(kind_of(&m, "rst_n").is_none());
    }

    const ANNOTATED_NETLIST: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/xprop_cosim/xprov_annotated.gv"
    );

    #[test]
    fn x_source_carries_src_provenance() {
        // WS-B B3: an annotated netlist's X-source reports its RTL `src`.
        let (netlistdb, aig) =
            build_netlist_and_aig(Path::new(ANNOTATED_NETLIST), None, &[], None, None);
        let m = compute_x_source_manifest(&netlistdb, &aig, None, ANNOTATED_NETLIST);

        let s = m
            .x_sources
            .iter()
            .find(|s| s.cell.as_deref() == Some("reg0"))
            .expect("no reg0 x-source in manifest");
        assert_eq!(s.kind, XSourceKind::UnresetDff);
        assert_eq!(s.src.as_deref(), Some("xprov.v:9.3-9.44"));
        // ...and it round-trips into the emitted JSON.
        let json = serde_json::to_string(&m).unwrap();
        assert!(json.contains("\"src\":\"xprov.v:9.3-9.44\""));

        // A source without provenance omits `src` from the JSON entirely
        // (skip_serializing_if), so un-annotated `-noattr` manifests are
        // byte-identical to before this field existed.
        let bare = XSource {
            net: "n".to_string(),
            kind: XSourceKind::UndrivenInput,
            cell: None,
            src: None,
        };
        assert!(!serde_json::to_string(&bare).unwrap().contains("src"));
    }

    #[test]
    fn manifest_is_serializable() {
        let m = demo_manifest(true);
        let json = serde_json::to_string(&m).unwrap();
        assert!(json.contains("\"schema_version\":\"1.1\""));
        assert!(json.contains("\"unreset-dff\""));
        assert!(json.contains("\"undriven-input\""));
    }
}
