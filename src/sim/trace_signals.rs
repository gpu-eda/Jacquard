// SPDX-License-Identifier: Apache-2.0
//! User-controlled signal tracing for in-design observability.
//!
//! Resolves a list of hierarchical signal names (one per line, file-
//! supplied) into AIG pins and registers each into
//! `aig.extra_observable_names`, piggybacking on the same VCD-emission
//! path that the GF180MCU `bi_24t` bidir-pad split already uses
//! (`emit_extra_observables` in `src/sim/vcd_io.rs`). Each traced
//! signal becomes a primary output, gets a state-buffer slot at
//! partition time, and lands in the output VCD alongside top-level
//! IO. Works uniformly for sequential (DFF Q) and combinational nets
//! — anything that has a name in netlistdb's `netname2id` is
//! traceable.
//!
//! Trace list registration happens at AIG construction time (before
//! partitioning), so the file must be supplied via the
//! `--trace-signals` CLI flag rather than a runtime env var.
//!
//! File format: one signal per line. Blank lines and lines whose
//! first non-whitespace character is `#` are skipped. Hierarchical
//! names use `.` as a separator; bit indices use trailing `[N]`.
//!
//! ```text
//! # JTAG-DM internal state
//! chip_core.dm.haltreq_q[0]
//! chip_core.dm.haltreq_q[1]
//!
//! # Yosys-internal nets — same syntax works
//! chip_core.sram_u._00147_
//! ```
//!
//! Unresolved names are warned, not fatal — the rest of the trace
//! list still registers. A trailing summary line reports how many
//! signals registered vs. how many were dropped, so a bad name list
//! surfaces clearly at startup.

use std::path::Path;

use compact_str::CompactString;
use netlistdb::{HierName, NetlistDB};

use crate::aig::AIG;
use crate::flatten::FlattenedScriptV1;

/// Parsed shape of a single signal name from the trace file.
/// `hier` is the cell-instance hierarchy (everything before the leaf
/// segment); `leaf` is the final segment name; `bit` is an optional
/// trailing `[N]` bus index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedSignalName {
    pub hier: HierName,
    pub leaf: CompactString,
    pub bit: Option<isize>,
}

#[derive(Debug)]
pub enum TraceError {
    Io {
        path: std::path::PathBuf,
        err: std::io::Error,
    },
    InvalidName {
        line: usize,
        raw: String,
        reason: String,
    },
}

impl std::fmt::Display for TraceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io { path, err } => write!(f, "trace file {}: {}", path.display(), err),
            Self::InvalidName { line, raw, reason } => {
                write!(f, "trace file line {line}: `{raw}` — {reason}")
            }
        }
    }
}

impl std::error::Error for TraceError {}

/// Read a trace file, returning the list of non-empty, non-comment
/// signal name strings (raw, not yet parsed). Caller invokes
/// `parse_signal_name` to convert each into a `ParsedSignalName`.
pub fn read_trace_file(path: &Path) -> Result<Vec<String>, TraceError> {
    let contents = std::fs::read_to_string(path).map_err(|err| TraceError::Io {
        path: path.to_path_buf(),
        err,
    })?;
    let mut names = Vec::new();
    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        names.push(trimmed.to_string());
    }
    Ok(names)
}

/// Parse a signal name into one or more candidate
/// `ParsedSignalName` shapes for netlistdb lookup. Dotted names are
/// ambiguous in synthesized netlists: Yosys often emits flattened
/// designs where the entire hierarchy is collapsed into a single
/// escaped identifier (`\soc.sram.wb_bus__ack`) with dots as
/// content rather than separators, while other tools preserve the
/// structural hierarchy and the dots are real splits. Rather than
/// guess, try both: candidate-0 is the flat-leaf interpretation
/// (entire name as leaf, empty hier — matches Yosys-style flattened
/// nets); candidate-1 is the dot-split interpretation (last segment
/// as leaf, everything before as hier).
///
/// Trailing `[N]` bit index is stripped first and applied to both
/// candidates. Leading `\` is stripped (Verilog escaped-identifier
/// syntax); we treat the name without the backslash since netlistdb
/// stores names canonically.
///
/// Examples:
/// - `"clk"` → one candidate: flat leaf `"clk"`, no hier
/// - `"top.dm.haltreq_q[0]"` → two candidates:
///   - flat: leaf `"top.dm.haltreq_q"`, bit `Some(0)` (Yosys-flattened)
///   - dotted: hier `["top","dm"]`, leaf `"haltreq_q"`, bit `Some(0)`
/// - `"\soc.bus__addr[3]"` → flat: leaf `"soc.bus__addr"`, bit `Some(3)`
pub fn parse_signal_name(raw: &str) -> Result<Vec<ParsedSignalName>, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err("empty name".to_string());
    }

    // Verilog escaped-identifier syntax: leading `\` + trailing
    // space delimits a name with arbitrary characters. Strip both;
    // netlistdb stores canonical forms.
    let raw = raw.strip_prefix('\\').unwrap_or(raw);
    let raw = raw.trim_end();

    // Split off trailing [N] bit index if present. Brackets only
    // valid at the very end of the name; intermediate brackets
    // would be part of a (rare) escaped-identifier and aren't
    // supported.
    let (head, bit) = if let Some(open) = raw.rfind('[') {
        if !raw.ends_with(']') {
            return Err(format!("unmatched `[` in `{raw}`"));
        }
        let idx_str = &raw[open + 1..raw.len() - 1];
        let bit_val: isize = idx_str
            .parse()
            .map_err(|e| format!("bit index `{idx_str}` not an integer: {e}"))?;
        (&raw[..open], Some(bit_val))
    } else {
        (raw, None)
    };

    if head.is_empty() {
        return Err("empty leaf segment".to_string());
    }

    let mut candidates = Vec::new();

    // Candidate 0: flat-leaf with explicit bit index (Yosys-
    // flattened design with dotted escaped-identifier net names,
    // where the bus index is a structured field in netlistdb).
    // Always emit; common for synthesized netlists with real bus
    // declarations.
    candidates.push(ParsedSignalName {
        hier: HierName::empty(),
        leaf: head.into(),
        bit,
    });

    // Candidate 1: flat-leaf where the bracket-index is part of
    // the leaf name itself (Yosys-flattened design where bus
    // expansion produced 32 scalar wires named
    // `\foo.bar[0]`...`\foo.bar[31]`, each registered in
    // netlistdb as `(empty_hier, "foo.bar[N]", None)`). Emit only
    // when `bit` was actually stripped — otherwise candidate 0
    // already covers this shape.
    if bit.is_some() {
        candidates.push(ParsedSignalName {
            hier: HierName::empty(),
            leaf: raw.into(),
            bit: None,
        });
    }

    // Candidate 2: structural-hierarchy split on `.`. Emit only
    // when the name contains a dot (otherwise candidate 0 covers
    // it). Skips empty segments to avoid degenerate matches on
    // names with leading/trailing/consecutive dots.
    if head.contains('.') {
        let mut parts: Vec<&str> = head.split('.').collect();
        let leaf = parts.pop().unwrap();
        if !leaf.is_empty() && !parts.iter().any(|p| p.is_empty()) {
            candidates.push(ParsedSignalName {
                hier: HierName::from_topdown_hier_iter(parts.iter().copied()),
                leaf: leaf.into(),
                bit,
            });
        }
    }

    Ok(candidates)
}

/// Look up each parsed signal name in netlistdb, register the
/// resolved aigpin (iv) into `aig.extra_observable_names` and
/// `aig.primary_outputs`. Returns `(registered, dropped)`.
/// Unresolved names emit a `clilog::warn!` and increment `dropped`
/// but don't abort the registration loop — the rest of the list
/// still lands. Names whose resolved aigpin is the constant-0/1
/// (`iv <= 1`) are dropped with a warning since they don't need
/// runtime observation.
pub fn register_trace_signals(
    aig: &mut AIG,
    netlistdb: &NetlistDB,
    names: &[String],
) -> (usize, usize) {
    let mut registered = 0usize;
    let mut dropped = 0usize;

    for raw in names {
        let candidates = match parse_signal_name(raw) {
            Ok(c) => c,
            Err(e) => {
                clilog::warn!("--trace-signals: `{raw}` — parse error: {e}");
                dropped += 1;
                continue;
            }
        };

        // Try each candidate against netname2id; the first hit
        // wins. Both flat-leaf and dotted-hierarchy shapes are
        // attempted so the same syntax works for Yosys-flattened
        // and structurally-hierarchical netlists.
        let netid = candidates.iter().find_map(|c| {
            let key = (c.hier.clone(), c.leaf.clone(), c.bit);
            netlistdb.netname2id.get(&key).copied()
        });
        let Some(netid) = netid else {
            clilog::warn!(
                "--trace-signals: `{raw}` — not found in netlistdb \
                 (tried {} candidate(s))",
                candidates.len()
            );
            dropped += 1;
            continue;
        };

        // Any pin on the net carries the same aigpin iv (all pins
        // share the net's driver). Take the first.
        let Some(pinid) = netlistdb.net2pin.iter_set(netid).next() else {
            clilog::warn!("--trace-signals: `{raw}` — net has no pins");
            dropped += 1;
            continue;
        };

        let iv = aig.pin2aigpin_iv[pinid];
        if iv == usize::MAX {
            clilog::warn!(
                "--trace-signals: `{raw}` — pin {} has no AIG mapping \
                 (likely a non-logic pin or stripped cone)",
                pinid
            );
            dropped += 1;
            continue;
        }
        if iv <= 1 {
            clilog::warn!(
                "--trace-signals: `{raw}` — resolves to constant {} \
                 (iv={}); skipping runtime observation",
                if iv == 0 { 0 } else { 1 },
                iv
            );
            dropped += 1;
            continue;
        }

        aig.primary_outputs.insert(iv);
        aig.extra_observable_names
            .entry(iv)
            .or_default()
            .push(raw.clone());
        // Surface RTL source provenance (WS-B B3) when the netlist carried it:
        // the traced net's aigpin resolves back to `(* src *)` file:line(s).
        let src = aig.aigpin_src_locations(iv >> 1, netlistdb);
        if !src.is_empty() {
            clilog::info!("--trace-signals: `{raw}` → RTL {}", src.join(", "));
        }
        registered += 1;
    }

    (registered, dropped)
}

/// Resolve a hierarchical signal name to its bit position in the GPU
/// output state buffer, using the same multi-candidate matching as
/// [`register_trace_signals`] (so Yosys-flattened / scalar-expanded /
/// structural naming all work) but reading `script.output_map` instead
/// of registering an observable.
///
/// The signal must already be present in `output_map` — i.e. it is a
/// primary output or was registered as observable before partitioning
/// (see `DesignArgs::extra_observable_signals`). Returns `None` if the
/// name can't be parsed, isn't in netlistdb, has no AIG mapping, or
/// didn't land in the output state (e.g. tied to a constant).
///
/// Used by the cosim bus-trace param builder to bind each protocol pin
/// to a state position the GPU kernel can read.
/// Parse `name` and resolve it to a netlistdb net id, scanning the
/// multi-candidate interpretations (`parse_signal_name`) until one hits
/// `netname2id`. Shared by the state-position resolvers below. Returns `None`
/// if the name can't be parsed or matches no net.
pub fn resolve_net_id(netlistdb: &NetlistDB, name: &str) -> Option<usize> {
    let candidates = parse_signal_name(name).ok()?;
    candidates.iter().find_map(|c| {
        let key = (c.hier.clone(), c.leaf.clone(), c.bit);
        netlistdb.netname2id.get(&key).copied()
    })
}

pub fn resolve_to_state_pos(
    aig: &AIG,
    netlistdb: &NetlistDB,
    script: &FlattenedScriptV1,
    name: &str,
) -> Option<u32> {
    let netid = resolve_net_id(netlistdb, name)?;
    let pinid = netlistdb.net2pin.iter_set(netid).next()?;
    let iv = aig.pin2aigpin_iv[pinid];
    if iv == usize::MAX {
        return None;
    }
    // The net's driver may be stored under either polarity in the
    // output map; try the pin's polarity first, then its inversion.
    if let Some(&pos) = script.output_map.get(&iv) {
        return Some(pos);
    }
    if let Some(&pos) = script.output_map.get(&(iv ^ 1)) {
        return Some(pos);
    }
    None
}

/// Resolve a hierarchical register (DFF) name to its bit position in the
/// **input** state slot — the DFF Q slot a power-up value must be deposited
/// into for `reg_init` value-injection (issue #108).
///
/// Distinct from [`resolve_to_state_pos`], which reads `output_map` (keyed by
/// DFF *D* inputs) for bus-trace pin binding. A register *name* resolves to
/// the net driven by the DFF's *Q*, whose aigpin lives in `input_map`.
/// Returns `None` if the name doesn't resolve to a DFF Q present in the
/// flattened script (a pure-comb net, a constant, or an unknown name).
pub fn resolve_to_input_state_pos(
    aig: &AIG,
    netlistdb: &NetlistDB,
    script: &FlattenedScriptV1,
    name: &str,
) -> Option<u32> {
    let netid = resolve_net_id(netlistdb, name)?;
    // Scan every pin on the net, not just the first: the DFF Q *driver* pin
    // is the one keyed in `input_map`, but it need not be the net's first pin
    // — load pins (e.g. adder inputs consuming the register) share the net.
    // `input_map` is keyed by the **bare** aigpin (`dff.q`, from
    // `add_aigpin`), whereas `pin2aigpin_iv` is iv-encoded
    // (`aigpin << 1 | invert`); recover the bare aigpin with `>> 1`, which
    // also drops the per-pin invert so both polarities collapse to one key.
    for pinid in netlistdb.net2pin.iter_set(netid) {
        let iv = aig.pin2aigpin_iv[pinid];
        if iv == usize::MAX {
            continue;
        }
        if let Some(&pos) = script.input_map.get(&(iv >> 1)) {
            return Some(pos);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_flat_name_without_hier() {
        let cs = parse_signal_name("clk").unwrap();
        // No dots — only one candidate (flat).
        assert_eq!(cs.len(), 1);
        assert!(cs[0].hier.is_empty());
        assert_eq!(cs[0].leaf.as_str(), "clk");
        assert_eq!(cs[0].bit, None);
    }

    #[test]
    fn parses_hierarchical_name_with_bit_emits_all_three_candidates() {
        let cs = parse_signal_name("top.dm.haltreq_q[0]").unwrap();
        // Dots + bit index → three candidates.
        assert_eq!(cs.len(), 3);

        // Candidate 0: flat-leaf, bit as structured field.
        assert!(cs[0].hier.is_empty());
        assert_eq!(cs[0].leaf.as_str(), "top.dm.haltreq_q");
        assert_eq!(cs[0].bit, Some(0));

        // Candidate 1: flat-leaf, bracket in name (Yosys scalar
        // expansion per bit).
        assert!(cs[1].hier.is_empty());
        assert_eq!(cs[1].leaf.as_str(), "top.dm.haltreq_q[0]");
        assert_eq!(cs[1].bit, None);

        // Candidate 2: dot-split (structurally-hierarchical style).
        let hier_parts: Vec<&CompactString> = cs[2].hier.iter().collect();
        assert_eq!(hier_parts.len(), 2);
        assert_eq!(hier_parts[0].as_str(), "dm"); // bottom-up
        assert_eq!(hier_parts[1].as_str(), "top");
        assert_eq!(cs[2].leaf.as_str(), "haltreq_q");
        assert_eq!(cs[2].bit, Some(0));
    }

    #[test]
    fn parses_yosys_flattened_escaped_identifier() {
        // Leading backslash should be stripped — matches Verilog
        // escaped-identifier syntax. The full dotted name becomes
        // the flat-leaf candidate.
        let cs = parse_signal_name("\\soc.sram.read_port__data[3]").unwrap();
        assert!(cs[0].hier.is_empty());
        assert_eq!(cs[0].leaf.as_str(), "soc.sram.read_port__data");
        assert_eq!(cs[0].bit, Some(3));
    }

    #[test]
    fn parses_negative_bit_index() {
        let cs = parse_signal_name("top.bus[-3]").unwrap();
        // Three candidates: flat-with-bit, flat-with-bracket-in-name, dot-split.
        assert_eq!(cs.len(), 3);
        assert_eq!(cs[0].bit, Some(-3));
        assert_eq!(cs[1].bit, None);
        assert_eq!(cs[1].leaf.as_str(), "top.bus[-3]");
        assert_eq!(cs[2].bit, Some(-3));
    }

    #[test]
    fn rejects_empty_string() {
        assert!(parse_signal_name("").is_err());
        assert!(parse_signal_name("   ").is_err());
    }

    #[test]
    fn rejects_unmatched_bracket() {
        let r = parse_signal_name("a.b.c[0");
        assert!(r.is_err());
        assert!(r.unwrap_err().contains("unmatched"));
    }

    #[test]
    fn rejects_non_integer_bit_index() {
        let r = parse_signal_name("a.b[xyz]");
        assert!(r.is_err());
    }

    #[test]
    fn trailing_dot_emits_only_flat_candidate() {
        // `a.b.` has an empty dot-split leaf, so we skip the
        // dotted-hierarchy candidate but the flat-leaf "a.b."
        // candidate still goes through. netname2id lookup will
        // then fail at runtime (warning logged) — this is the
        // correct outcome since "a.b." could be a legitimate
        // escaped-identifier net name even if unusual.
        let cs = parse_signal_name("a.b.").unwrap();
        assert_eq!(cs.len(), 1);
        assert_eq!(cs[0].leaf.as_str(), "a.b.");
    }

    /// End-to-end: build a small AIG, register a trace signal by
    /// hierarchical name, verify the resolved aigpin lands in
    /// `extra_observable_names` AND `primary_outputs` so the
    /// existing VCD emission path picks it up.
    #[test]
    fn register_resolves_and_pushes_into_extra_observables() {
        const TOP_VERILOG: &str = "\
module top(clk, d, q);
  input clk, d;
  output q;
  wire q_internal;
  DFF u_dff_a(.D(d), .CLK(clk), .Q(q_internal));
  DFF u_dff_b(.D(q_internal), .CLK(clk), .Q(q));
endmodule
";
        let nl = netlistdb::NetlistDB::from_sverilog_source(
            TOP_VERILOG,
            Some("top"),
            &crate::aigpdk::AIGPDKLeafPins(),
        )
        .expect("netlist parse");
        let mut aig = crate::aig::AIG::from_netlistdb(&nl);

        // The intermediate wire `q_internal` should resolve to the
        // first DFF's Q output aigpin.
        let names = vec!["q_internal".to_string()];
        let (registered, dropped) = register_trace_signals(&mut aig, &nl, &names);
        assert_eq!(registered, 1);
        assert_eq!(dropped, 0);

        // The aigpin should now be in extra_observable_names AND
        // primary_outputs — both are needed for the partitioner to
        // give it a state-buffer slot.
        assert_eq!(aig.extra_observable_names.len(), 1);
        let (iv, names) = aig.extra_observable_names.iter().next().unwrap();
        assert_eq!(names, &vec!["q_internal".to_string()]);
        assert!(
            aig.primary_outputs.contains(iv),
            "registered trace signal must be in primary_outputs"
        );
    }

    /// WS-B B3: a traced signal on a `(* src *)`-annotated netlist resolves back
    /// to its RTL source location (what `register_trace_signals` surfaces).
    #[test]
    fn register_surfaces_src_provenance() {
        const TOP_VERILOG: &str = "\
module top(clk, d, q);
  input clk, d;
  output q;
  wire q_internal;
  (* src = \"top.v:5.3-5.34\" *)
  DFF u_dff_a(.D(d), .CLK(clk), .Q(q_internal));
  DFF u_dff_b(.D(q_internal), .CLK(clk), .Q(q));
endmodule
";
        let nl = netlistdb::NetlistDB::from_sverilog_source(
            TOP_VERILOG,
            Some("top"),
            &crate::aigpdk::AIGPDKLeafPins(),
        )
        .expect("netlist parse");
        let mut aig = crate::aig::AIG::from_netlistdb(&nl);

        let names = vec!["q_internal".to_string()];
        let (registered, dropped) = register_trace_signals(&mut aig, &nl, &names);
        assert_eq!((registered, dropped), (1, 0));

        // `q_internal` is driven by the annotated DFF, so its aigpin resolves
        // to that cell's src (this is exactly what registration logs).
        let (iv, _) = aig.extra_observable_names.iter().next().unwrap();
        let src = aig.aigpin_src_locations(*iv >> 1, &nl);
        assert_eq!(
            src,
            vec![compact_str::CompactString::from("top.v:5.3-5.34")]
        );
    }

    /// Unresolved names increment `dropped`, log a warning, and do
    /// NOT pollute `extra_observable_names`. Valid names alongside
    /// them still register cleanly.
    #[test]
    fn register_skips_unresolved_names_but_keeps_others() {
        const TOP_VERILOG: &str = "\
module top(clk, d, q);
  input clk, d;
  output q;
  DFF u_dff(.D(d), .CLK(clk), .Q(q));
endmodule
";
        let nl = netlistdb::NetlistDB::from_sverilog_source(
            TOP_VERILOG,
            Some("top"),
            &crate::aigpdk::AIGPDKLeafPins(),
        )
        .expect("netlist parse");
        let mut aig = crate::aig::AIG::from_netlistdb(&nl);

        let names = vec![
            "q".to_string(),                   // resolves
            "this.does.not.exist".to_string(), // doesn't
            "d".to_string(),                   // resolves (input port net)
        ];
        let (registered, dropped) = register_trace_signals(&mut aig, &nl, &names);
        assert_eq!(registered, 2);
        assert_eq!(dropped, 1);
        // Two observable entries — one per resolved name. Both
        // entries are distinct aigpins, so two top-level keys.
        assert_eq!(aig.extra_observable_names.len(), 2);
    }

    /// Real trace-file parsing: comments, blank lines, mixed shapes
    /// all coexist; raw strings come back ready for
    /// `parse_signal_name`.
    #[test]
    fn read_trace_file_strips_comments_and_blanks() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("trace.txt");
        std::fs::write(
            &path,
            "\
# JTAG-DM internals
top.dm.haltreq_q[0]
top.dm.haltreq_q[1]

   # indented comment
chip_core.sram_u._00147_

# trailing blank line below
",
        )
        .unwrap();
        let names = read_trace_file(&path).unwrap();
        assert_eq!(
            names,
            vec![
                "top.dm.haltreq_q[0]".to_string(),
                "top.dm.haltreq_q[1]".to_string(),
                "chip_core.sram_u._00147_".to_string(),
            ]
        );
    }
}
