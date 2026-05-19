//! Verilog input filtering for OpenSTA's structural-only reader.
//!
//! OpenSTA's `read_verilog` accepts cell instantiations and bare-net
//! assigns only. RTL operators (`~`, `&`, `|`, `^`), bit-selects in
//! assigns, and ranged concatenations cause a parse-time `syntax
//! error`. The LibreLane + wafer.space integration flow wraps the
//! post-P&R structural body in an integration module (e.g.
//! `openframe_project_wrapper`) that patches the chip-frame pad ring
//! using exactly those operators. The wrapped file contains both a
//! readable-by-OpenSTA structural module *and* the unreadable
//! integration wrapper.
//!
//! `extract_named_module` lifts just the named module's
//! `module … endmodule` block out of the file, leaving any other
//! modules — including offending wrappers — behind. Multi-file
//! invocations get this treatment per-file; files that do not
//! contain the named module are passed to OpenSTA unchanged so
//! hierarchical designs that split sub-modules across files still
//! link cleanly.
//!
//! See ADR 0009 for the broader rule and rationale.

/// Extract the `module <name> … endmodule` block from `content`.
///
/// Returns `None` if no module declaration matching `name` is found,
/// in which case callers should pass the file through unchanged
/// (it likely contains different sub-modules that OpenSTA should
/// still link).
///
/// Recognition is anchored at line start so embedded comments and
/// string literals are safe; the `module <name>` identifier must be
/// followed by a non-identifier byte to avoid prefix-matches (`top`
/// must not match `top_synth`). Line-comment `//` and block-comment
/// `/* … */` between `module` and the name are not currently
/// supported — machine-generated post-P&R netlists put them on
/// dedicated lines, so this hasn't surfaced.
pub fn extract_named_module(content: &str, name: &str) -> Option<String> {
    let lines: Vec<&str> = content.lines().collect();
    let start = find_module_start(&lines, name)?;
    let end = find_endmodule(&lines, start)?;
    let mut out = lines[start..=end].join("\n");
    out.push('\n');
    Some(out)
}

fn find_module_start(lines: &[&str], name: &str) -> Option<usize> {
    let needle = format!("module {name}");
    for (idx, line) in lines.iter().enumerate() {
        let trimmed = line.trim_start();
        let Some(after) = trimmed.strip_prefix(&needle) else {
            continue;
        };
        let next_byte = after.as_bytes().first().copied();
        // Identifier must be terminated. Valid follows: whitespace, '(',
        // ';', or end of line. Reject alphanumerics and '_' (which
        // would mean `name` is just a prefix of a longer identifier).
        let terminated = match next_byte {
            None => true,
            Some(b) => !(b.is_ascii_alphanumeric() || b == b'_'),
        };
        if terminated {
            return Some(idx);
        }
    }
    None
}

fn find_endmodule(lines: &[&str], start: usize) -> Option<usize> {
    // Verilog forbids nested modules, so the first `endmodule` after
    // `start` terminates the block. Anchor at line start to ignore
    // the token inside comments or string literals.
    for (idx, line) in lines.iter().enumerate().skip(start + 1) {
        let trimmed = line.trim_start();
        if trimmed == "endmodule"
            || trimmed.starts_with("endmodule ")
            || trimmed.starts_with("endmodule\t")
            || trimmed.starts_with("endmodule;")
            || trimmed.starts_with("endmodule//")
            || trimmed.starts_with("endmodule/*")
        {
            return Some(idx);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_named_module_from_two_module_file() {
        let v = "\
module wrapper(in, out);
  assign out = ~in;
endmodule

module top(a, b, q);
  AND2 g0 (.A(a), .B(b), .Y(q));
endmodule
";
        let extracted = extract_named_module(v, "top").unwrap();
        assert!(extracted.contains("AND2 g0"));
        assert!(!extracted.contains("assign out = ~in"));
        assert!(extracted.contains("endmodule"));
    }

    #[test]
    fn extracts_when_top_is_first_module() {
        let v = "\
module top(a, q);
  BUF g0 (.A(a), .Y(q));
endmodule

module other;
endmodule
";
        let extracted = extract_named_module(v, "top").unwrap();
        assert!(extracted.contains("BUF g0"));
        assert!(!extracted.contains("module other"));
    }

    #[test]
    fn returns_none_when_module_absent() {
        let v = "\
module other(a, q);
  BUF g0 (.A(a), .Y(q));
endmodule
";
        assert!(extract_named_module(v, "top").is_none());
    }

    #[test]
    fn does_not_match_prefix_identifier() {
        // `top` must not match `top_synth`.
        let v = "\
module top_synth(a, q);
  BUF g0 (.A(a), .Y(q));
endmodule
";
        assert!(extract_named_module(v, "top").is_none());
    }

    #[test]
    fn handles_indented_module_keyword() {
        // Some netlists indent module declarations (rare but valid).
        let v = "\
  module top(a, q);
    BUF g0 (.A(a), .Y(q));
  endmodule
";
        let extracted = extract_named_module(v, "top").unwrap();
        assert!(extracted.contains("BUF g0"));
    }

    #[test]
    fn extracts_module_with_no_port_list() {
        let v = "\
module top;
  initial $finish;
endmodule
";
        let extracted = extract_named_module(v, "top").unwrap();
        assert!(extracted.contains("initial $finish"));
    }

    #[test]
    fn ignores_endmodule_in_comment() {
        // Block-comment endmodule inside the wanted module shouldn't
        // truncate the extraction. (Current implementation anchors on
        // line-start of `endmodule` token, so a block comment whose
        // content starts a line with `endmodule` would still trigger;
        // this test documents the common case where `endmodule` is
        // embedded inline in a comment.)
        let v = "\
module top(a, q);
  // marker only: endmodule (this is not the real one)
  BUF g0 (.A(a), .Y(q));
endmodule
";
        let extracted = extract_named_module(v, "top").unwrap();
        assert!(extracted.contains("BUF g0"));
    }
}
