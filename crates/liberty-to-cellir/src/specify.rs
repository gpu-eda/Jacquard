// SPDX-License-Identifier: Apache-2.0

//! Verilog `specify` block parsing for the D6 L4 **arc-set agreement** check
//! (ADR 0019 D6).
//!
//! A standard-cell `.v` `specify` block carries timing *arc topology* — which
//! delay paths exist — but only zero/placeholder values (an SDF
//! back-annotation scaffold). So the cross-check on stdcell timing is **arc-set
//! agreement, not value comparison**: every Liberty `timing()` delay arc
//! (`related_pin` → output) should correspond to a `.v` specify path
//! `(src => dst)` and vice versa. This module extracts the specify delay-path
//! set; [`crate::crosscheck`] compares it against the Liberty-derived
//! [`cell_model_ir::DelayArc`] set and surfaces missing / extra arcs.
//!
//! Specify path forms handled (GF180 / SKY130 shapes):
//!
//! - combinational: `(A1 => ZN) = (1.0,1.0);`
//! - sequential clock→Q: `(posedge CLK => (Q : D)) = (1.0,1.0);`
//! - async control→Q: `(RN => Q) = (1.0,1.0);`
//! - parallel `*>` connections and comma lists on either side.
//!
//! Timing *checks* (`$setup` / `$hold` / `$width`) are **not** delay paths and
//! are skipped here — they are constraint arcs, whose multi-line `&&&`
//! conditional forms are deferred (the delay-path set is the high-signal half
//! of arc-set agreement and is reliably single-line).

use std::collections::{BTreeSet, HashMap};

/// A normalized delay arc: source pin → destination pin (edge specifiers and
/// the `(dst : data)` data annotation stripped).
pub type Arc = (String, String);

/// An index of `.v` specify delay-arc sets, keyed by module name.
#[derive(Debug, Default)]
pub struct SpecifyIndex {
    pub arcs: HashMap<String, BTreeSet<Arc>>,
}

impl SpecifyIndex {
    /// Merge the specify arcs parsed from one `.v` source into the index. A
    /// file may declare several modules; each `specify` block is attributed to
    /// the enclosing `module`.
    pub fn add_source(&mut self, src: &str) {
        for (module, arcs) in parse_specify_modules(src) {
            self.arcs.entry(module).or_default().extend(arcs);
        }
    }

    /// The delay-arc set for a module, if any was parsed.
    pub fn get(&self, module: &str) -> Option<&BTreeSet<Arc>> {
        self.arcs.get(module)
    }
}

/// Parse every `module ... specify ... endspecify` in a `.v` source into
/// `(module_name, delay_arc_set)` pairs.
fn parse_specify_modules(src: &str) -> Vec<(String, BTreeSet<Arc>)> {
    let mut out = Vec::new();
    let mut current_module: Option<String> = None;
    let mut in_specify = false;
    let mut arcs: BTreeSet<Arc> = BTreeSet::new();

    for raw in src.lines() {
        let line = strip_line_comment(raw).trim();
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix("module ") {
            // Flush a previous module (defensive; specify normally closes
            // before the next module).
            if let Some(m) = current_module.take() {
                out.push((m, std::mem::take(&mut arcs)));
            }
            let name = rest
                .split(|c: char| c == '(' || c.is_whitespace())
                .find(|s| !s.is_empty())
                .unwrap_or("")
                .to_string();
            current_module = Some(name);
            in_specify = false;
            continue;
        }
        if line.starts_with("endmodule") {
            if let Some(m) = current_module.take() {
                out.push((m, std::mem::take(&mut arcs)));
            }
            in_specify = false;
            continue;
        }
        if line.starts_with("specify") {
            in_specify = true;
            continue;
        }
        if line.starts_with("endspecify") {
            in_specify = false;
            continue;
        }
        if in_specify {
            // A path declaration contains `=>` or `*>` and an `=` assignment.
            // Timing checks start with `$` and are skipped.
            if line.starts_with('$') {
                continue;
            }
            if line.contains("=>") || line.contains("*>") {
                parse_path_line(line, &mut arcs);
            }
        }
    }
    if let Some(m) = current_module.take() {
        out.push((m, arcs));
    }
    out
}

/// Strip a `//` line comment, preserving everything before it.
fn strip_line_comment(line: &str) -> &str {
    match line.find("//") {
        Some(i) => &line[..i],
        None => line,
    }
}

/// Parse a single specify path line such as `(posedge CLK => (Q : D)) = ...;`,
/// `if(cond) (RN => Q) = ...;`, or `(A1, A2 *> Y) = ...;` into one or more
/// delay arcs. Anchored on the `=>` / `*>` connector so a leading `if(...)`
/// guard is not mistaken for the source terminal.
fn parse_path_line(line: &str, arcs: &mut BTreeSet<Arc>) {
    let Some(conn) = line.find("=>").or_else(|| line.find("*>")) else {
        return;
    };
    let before = &line[..conn];
    let after = &line[conn + 2..];

    // Source = the terminals inside the innermost `(` group immediately before
    // the connector (skip any leading `if(...)` guard).
    let src_part = match before.rfind('(') {
        Some(i) => &before[i + 1..],
        None => before,
    };
    // Destination = everything up to the `=` assignment (or `;`).
    let dst_end = after
        .find('=')
        .or_else(|| after.find(';'))
        .unwrap_or(after.len());
    let dst_part = &after[..dst_end];

    let sources = parse_terminals(src_part);
    let destinations = parse_terminals(dst_part);
    for s in &sources {
        for d in &destinations {
            arcs.insert((s.clone(), d.clone()));
        }
    }
}

/// Extract the pin terminals from one side of a path connection, dropping edge
/// keywords (`posedge`/`negedge`), the `(dst : data)` data annotation, and any
/// surrounding parens/brackets. Returns the destination/source pin name(s).
fn parse_terminals(part: &str) -> Vec<String> {
    // A `(Q : D)` data-dependent destination: keep only the pin before `:`.
    let part = if let Some(colon) = part.find(':') {
        // Only treat as a data annotation if a `(` introduced it.
        &part[..colon]
    } else {
        part
    };
    let mut out = Vec::new();
    for tok in part.split(',') {
        let cleaned: String = tok
            .chars()
            .map(|c| {
                if c == '(' || c == ')' || c == '[' || c == ']' {
                    ' '
                } else {
                    c
                }
            })
            .collect();
        for word in cleaned.split_whitespace() {
            if word == "posedge" || word == "negedge" || word.is_empty() {
                continue;
            }
            // Stop at an `=` (the assignment) or `:` leftover.
            let pin = word.trim_matches(|c: char| !c.is_alphanumeric() && c != '_');
            if !pin.is_empty()
                && pin
                    .chars()
                    .next()
                    .map(|c| c.is_alphabetic() || c == '_')
                    .unwrap_or(false)
            {
                out.push(pin.to_string());
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn combinational_arcs_parsed() {
        let src = r#"
        module demo__nand2( A1, A2, ZN );
        input A1, A2; output ZN;
        nand g(ZN, A1, A2);
        specify
            // comb arc A1 --> ZN
            (A1 => ZN) = (1.0,1.0);
            (A2 => ZN) = (1.0,1.0);
        endspecify
        endmodule
        "#;
        let mut idx = SpecifyIndex::default();
        idx.add_source(src);
        let arcs = idx.get("demo__nand2").unwrap();
        assert!(arcs.contains(&("A1".to_string(), "ZN".to_string())));
        assert!(arcs.contains(&("A2".to_string(), "ZN".to_string())));
        assert_eq!(arcs.len(), 2);
    }

    #[test]
    fn sequential_clk_and_async_arcs_parsed() {
        let src = r#"
        module demo__dffrsnq( CLK, D, SETN, RN, Q, notifier );
        specify
            (posedge CLK => (Q : D))  = (1.0,1.0);
            if(CLK===1'b0) (RN => Q)  = (1.0,1.0);
            (SETN => Q)  = (1.0,1.0);
            $setup(D, posedge CLK, 1.0);
            $hold(posedge CLK, D, 1.0);
            $width(negedge CLK, 1.0);
        endspecify
        endmodule
        "#;
        let mut idx = SpecifyIndex::default();
        idx.add_source(src);
        let arcs = idx.get("demo__dffrsnq").unwrap();
        // Delay paths: CLK->Q, RN->Q, SETN->Q. Timing checks are skipped.
        assert!(arcs.contains(&("CLK".to_string(), "Q".to_string())));
        assert!(arcs.contains(&("RN".to_string(), "Q".to_string())));
        assert!(arcs.contains(&("SETN".to_string(), "Q".to_string())));
        assert_eq!(arcs.len(), 3, "got {arcs:?}");
    }

    #[test]
    fn parallel_star_connection_and_lists() {
        let src = r#"
        module m( A, B, Y );
        specify
            (A, B *> Y) = (1.0, 1.0);
        endspecify
        endmodule
        "#;
        let mut idx = SpecifyIndex::default();
        idx.add_source(src);
        let arcs = idx.get("m").unwrap();
        assert!(arcs.contains(&("A".to_string(), "Y".to_string())));
        assert!(arcs.contains(&("B".to_string(), "Y".to_string())));
    }
}
