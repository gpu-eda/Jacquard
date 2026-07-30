// SPDX-FileCopyrightText: Copyright (c) 2024 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0
//! Generic Liberty (`.lib`) parser.
//!
//! This is the single Liberty front-end in Jacquard. It produces a
//! dialect-agnostic group/attribute tree ([`LibertyGroup`]) that downstream
//! consumers walk to extract whatever they need — jacquard core's
//! `TimingLibrary` (L4 timing characterization) and the future
//! Liberty -> cell-model-IR converter (L1-L3) both build on this crate.
//! See `docs/architecture/decisions/0019-cell-model-ir.md` decision D6.
//!
//! Liberty's concrete syntax has exactly two statement forms:
//!
//! - **attributes**: `name : value ;` (also `name : v1, v2, ... ;` lists)
//! - **groups**: `name ( arg, arg, ... ) { ... }`
//!
//! The parser models both faithfully and is *tolerant*: any construct it
//! does not recognise structurally is skipped gracefully rather than
//! aborting the parse, matching the behaviour of the hand-rolled parser
//! this crate replaces. Tricky real-world cases preserved verbatim:
//!
//! - `//` line comments and `/* */` block comments,
//! - quoted group/value names (SKY130) vs bare identifiers (AIGPDK),
//! - synthetic cell names like `$__RAMGEM_SYNC_` whose tokens are joined
//!   across underscore runs,
//! - list-valued attributes such as `index_1 ("0.1, 0.2, 0.3")` and
//!   comma/space-separated value lists.

pub mod json;

/// A single Liberty attribute: `name : value ... ;`.
#[derive(Debug, Clone, PartialEq)]
pub struct Attribute {
    /// Attribute name (left of the colon), e.g. `direction`, `time_unit`.
    pub name: String,
    /// One or more values (right of the colon). Liberty allows
    /// comma-separated lists; a scalar attribute has a single value.
    pub values: Vec<Value>,
}

impl Attribute {
    /// The first value rendered as a string, regardless of whether it was
    /// originally quoted, a bareword, or a number. Convenience for the
    /// common "read one scalar" case.
    pub fn first_string(&self) -> Option<&str> {
        self.values.first().map(Value::as_str)
    }

    /// The first value as a number, if it parses as one.
    pub fn first_number(&self) -> Option<f64> {
        self.values.first().and_then(Value::as_number)
    }
}

/// A Liberty value: quoted string, number, or bareword identifier.
///
/// The distinction is preserved from the source so consumers can tell a
/// quoted `"input"` from a bare `input` if they care; most don't and use
/// [`Value::as_str`].
///
/// A bare numeric token (e.g. `area : 1.0;`) parses to [`Value::Number`],
/// which keeps both the `f64` and the original source text so
/// [`Value::as_str`] can return a borrow without re-formatting (preserving
/// the exact spelling, e.g. trailing zeros) and [`Value::as_number`] is
/// free.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// A double-quoted string (quotes stripped). Note: Liberty often packs
    /// a whole list inside one quoted string, e.g. `"0.1, 0.2"` — that is
    /// kept as a single `String` value here; callers split if needed.
    String(String),
    /// A bare numeric literal: the parsed value plus its source text.
    Number(NumberValue),
    /// A bare identifier / unquoted token (e.g. `input`, `setup_rising`).
    Ident(String),
}

/// A numeric Liberty value: the parsed `f64` plus its original source text.
#[derive(Debug, Clone)]
pub struct NumberValue {
    /// The parsed numeric value.
    pub value: f64,
    /// The original token text, preserved for `as_str` / round-tripping.
    pub text: String,
}

impl PartialEq for NumberValue {
    fn eq(&self, other: &Self) -> bool {
        // Compare by parsed value (text is incidental); NaN never equal.
        self.value == other.value
    }
}

impl Value {
    /// Construct a [`Value::Number`] from raw token text. Assumes `text`
    /// parses as `f64` (callers gate on `parse::<f64>()`).
    fn number(text: String, value: f64) -> Value {
        Value::Number(NumberValue { value, text })
    }

    /// Render the value as a string slice, regardless of variant. For a
    /// [`Value::Number`] this borrows the original source text.
    pub fn as_str(&self) -> &str {
        match self {
            Value::String(s) => s,
            Value::Ident(s) => s,
            Value::Number(n) => &n.text,
        }
    }

    /// The numeric interpretation, if the value parses as a number.
    pub fn as_number(&self) -> Option<f64> {
        match self {
            Value::Number(n) => Some(n.value),
            Value::String(s) | Value::Ident(s) => s.trim().parse::<f64>().ok(),
        }
    }
}

/// A Liberty group: `group_type ( names... ) { attributes; subgroups }`.
///
/// Groups nest arbitrarily (`library` > `cell` > `pin` > `timing` > ...).
/// Order is preserved from the source for both `attributes` and `groups`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct LibertyGroup {
    /// The group keyword, e.g. `library`, `cell`, `pin`, `timing`, `ff`.
    pub group_type: String,
    /// Header arguments inside the parentheses, in source order. A `cell`
    /// has one name; a `timing` group usually has none; `cell_rise` has a
    /// template name like `scalar`.
    pub names: Vec<String>,
    /// Attributes (`name : value;`) directly in this group, in order.
    pub attributes: Vec<Attribute>,
    /// Nested groups, in source order.
    pub groups: Vec<LibertyGroup>,
}

impl LibertyGroup {
    /// Iterate over the direct child groups whose `group_type` matches.
    pub fn groups_of_type<'a>(
        &'a self,
        ty: &'a str,
    ) -> impl Iterator<Item = &'a LibertyGroup> + 'a {
        self.groups.iter().filter(move |g| g.group_type == ty)
    }

    /// The first child group of the given type, if any.
    pub fn group_of_type(&self, ty: &str) -> Option<&LibertyGroup> {
        self.groups.iter().find(|g| g.group_type == ty)
    }

    /// The first attribute with the given name, if any.
    pub fn attr(&self, name: &str) -> Option<&Attribute> {
        self.attributes.iter().find(|a| a.name == name)
    }

    /// The first header name, if any (most groups that have a name have
    /// exactly one).
    pub fn first_name(&self) -> Option<&str> {
        self.names.first().map(String::as_str)
    }
}

/// Parse Liberty `content` into its root `library` group.
///
/// Returns `Err` only on *structural* failure (no `library` keyword, an
/// unbalanced brace at top level). Constructs the parser does not model
/// are skipped, not rejected.
///
/// Assumes a single top-level `library(..) { .. }` group (the case for every
/// PDK Jacquard consumes). A file containing multiple `library` blocks — rare,
/// e.g. some split-corner vendor bundles — would need this relaxed to return a
/// list; not supported today.
pub fn parse(content: &str) -> Result<LibertyGroup, String> {
    let mut parser = Parser::new(content);
    parser.parse_root()
}

/// Internal tokenizer + recursive-descent group parser.
///
/// The low-level methods (`skip_whitespace`, `peek_byte`, `peek_char`,
/// `read_identifier`, `read_string`, `read_value`, `expect_char`) are
/// ported verbatim from jacquard's former hand-rolled parser so tokenizer
/// behaviour is byte-for-byte identical.
struct Parser<'a> {
    content: &'a str,
    pos: usize,
}

impl<'a> Parser<'a> {
    fn new(content: &'a str) -> Self {
        Self { content, pos: 0 }
    }

    fn skip_whitespace(&mut self) {
        let bytes = self.content.as_bytes();
        let len = bytes.len();
        while self.pos < len {
            let ch = bytes[self.pos];
            if ch == b' ' || ch == b'\t' || ch == b'\n' || ch == b'\r' {
                self.pos += 1;
            } else if ch == b'/' && self.pos + 1 < len {
                let next = bytes[self.pos + 1];
                if next == b'*' {
                    // Skip block comment - search for */
                    self.pos += 2;
                    while self.pos + 1 < len {
                        if bytes[self.pos] == b'*' && bytes[self.pos + 1] == b'/' {
                            self.pos += 2;
                            break;
                        }
                        self.pos += 1;
                    }
                } else if next == b'/' {
                    // Skip line comment - search for newline
                    self.pos += 2;
                    while self.pos < len && bytes[self.pos] != b'\n' {
                        self.pos += 1;
                    }
                    if self.pos < len {
                        self.pos += 1;
                    }
                } else {
                    break;
                }
            } else {
                break;
            }
        }
    }

    #[inline]
    fn peek_byte(&self) -> Option<u8> {
        if self.pos < self.content.len() {
            Some(self.content.as_bytes()[self.pos])
        } else {
            None
        }
    }

    fn peek_char(&mut self) -> Option<char> {
        self.skip_whitespace();
        self.peek_byte().map(|b| b as char)
    }

    fn expect_char(&mut self, ch: char) -> Result<(), String> {
        self.skip_whitespace();
        if self.peek_byte() == Some(ch as u8) {
            self.pos += 1;
            Ok(())
        } else {
            Err(format!(
                "Expected '{}' at position {}, found '{}'",
                ch,
                self.pos,
                self.peek_byte().map(|b| b as char).unwrap_or('?')
            ))
        }
    }

    fn read_identifier(&mut self) -> String {
        self.skip_whitespace();
        let start = self.pos;
        while self.pos < self.content.len() {
            let ch = self.content.as_bytes()[self.pos];
            if ch.is_ascii_alphanumeric() || ch == b'_' || ch == b'$' {
                self.pos += 1;
            } else {
                break;
            }
        }
        self.content[start..self.pos].to_string()
    }

    fn read_string(&mut self) -> Result<String, String> {
        self.skip_whitespace();
        if self.peek_byte() != Some(b'"') {
            return Err(format!("Expected string at position {}", self.pos));
        }
        self.pos += 1;
        let start = self.pos;
        let bytes = self.content.as_bytes();
        while self.pos < bytes.len() && bytes[self.pos] != b'"' {
            self.pos += 1;
        }
        let s = self.content[start..self.pos].to_string();
        if self.pos < self.content.len() {
            self.pos += 1; // Skip closing quote
        }
        Ok(s)
    }

    /// Read a single raw value token (quoted string or bareword run) up to
    /// the next `;`, `,`, or top-level `)`. Ported from the former
    /// `read_value`; nested parens are balanced so `f(a, b)` style values
    /// survive intact.
    fn read_value(&mut self) -> Result<String, String> {
        self.skip_whitespace();
        if self.peek_byte() == Some(b'"') {
            self.read_string()
        } else {
            // Read until semicolon, comma, or closing paren
            let start = self.pos;
            let mut depth = 0;
            while self.pos < self.content.len() {
                let ch = self.content.as_bytes()[self.pos];
                if ch == b'(' {
                    depth += 1;
                    self.pos += 1;
                } else if ch == b')' {
                    if depth == 0 {
                        break;
                    }
                    depth -= 1;
                    self.pos += 1;
                } else if (ch == b';' || ch == b',') && depth == 0 {
                    break;
                } else {
                    self.pos += 1;
                }
            }
            Ok(self.content[start..self.pos].trim().to_string())
        }
    }

    /// Classify a raw token (already de-quoted-or-not) into a [`Value`].
    /// Whether the token was originally quoted is tracked by `quoted` so a
    /// quoted numeric like `"50.0"` is preserved as a `String` value (its
    /// `as_number` still works), while a bare `50.0` becomes `Number`.
    fn classify_value(raw: String, quoted: bool) -> Value {
        if quoted {
            Value::String(raw)
        } else if let Ok(n) = raw.parse::<f64>() {
            Value::number(raw, n)
        } else {
            Value::Ident(raw)
        }
    }

    /// Parse the top-level `library (...) { ... }` group.
    fn parse_root(&mut self) -> Result<LibertyGroup, String> {
        self.skip_whitespace();
        let keyword = self.read_identifier();
        if keyword != "library" {
            return Err(format!("Expected 'library', found '{}'", keyword));
        }
        self.parse_group_body(keyword)
    }

    /// Read the header `( names... )` then the `{ ... }` body for a group
    /// whose keyword was already consumed.
    fn parse_group_body(&mut self, group_type: String) -> Result<LibertyGroup, String> {
        let mut group = LibertyGroup {
            group_type,
            ..Default::default()
        };

        self.expect_char('(')?;
        self.read_paren_values(&mut group.names)?;
        self.expect_char(')')?;
        self.expect_char('{')?;
        self.parse_block_into(&mut group)?;
        Ok(group)
    }

    /// Read the contents of a `( ... )` header up to (not including) the
    /// closing `)`. Returns the args as [`Value`]s (preserving quoted-ness)
    /// and pushes their string forms into `names`. Handles: empty headers
    /// (`timing ()`), quoted names (`cell ("foo")`), bareword names
    /// (`cell (foo)`), comma-separated lists (`ff (IQ, IQN)`), and the
    /// synthetic `$__RAMGEM_SYNC_` case where underscore-joined runs form
    /// one name. The caller has already consumed the opening `(`.
    fn read_paren_values(&mut self, names: &mut Vec<String>) -> Result<Vec<Value>, String> {
        let mut values = Vec::new();
        loop {
            match self.peek_char() {
                Some(')') | None => break,
                Some('"') => {
                    let s = self.read_string()?;
                    names.push(s.clone());
                    values.push(Value::String(s));
                }
                Some(',') => {
                    self.expect_char(',')?;
                }
                _ => {
                    let mut name = self.read_identifier();
                    // Synthetic names like `$__RAMGEM_SYNC_`: the tokenizer
                    // stops identifier runs at characters it cannot
                    // include, so a trailing-underscore run the tokenizer
                    // split needs re-joining. Mirror the former parser's
                    // behaviour exactly.
                    while self.peek_char() == Some('_') {
                        self.expect_char('_')?;
                        name.push('_');
                        let extra = self.read_identifier();
                        name.push_str(&extra);
                    }
                    if name.is_empty() {
                        // Non-identifier, non-delimiter byte in the header
                        // (e.g. an operator inside a complex arg). Consume
                        // it to make progress.
                        self.pos += 1;
                    } else {
                        names.push(name.clone());
                        values.push(Self::classify_value(name, false));
                    }
                }
            }
        }
        Ok(values)
    }

    /// Dispatch on what follows an already-read `keyword`: `:` => attribute,
    /// `(` => nested group (or paren'd attribute), `{` => bare block. Any
    /// other shape is skipped, matching the tolerant former parser.
    fn parse_statement(&mut self, keyword: String, group: &mut LibertyGroup) -> Result<(), String> {
        match self.peek_char() {
            Some(':') => {
                // Attribute: name : value [, value ...] ;
                self.expect_char(':')?;
                let values = self.read_value_list()?;
                if self.peek_char() == Some(';') {
                    self.expect_char(';')?;
                }
                group.attributes.push(Attribute {
                    name: keyword,
                    values,
                });
            }
            Some('(') => {
                // `name ( args... )` followed by either:
                //   - `{ ... }`  => a nested group, args are header names
                //     (e.g. `cell_rise (scalar) { ... }`), or
                //   - `;` / nothing => a paren-style attribute whose values
                //     ARE the parenthesised args (e.g. `values ("50.0");`,
                //     `index_1 ("0.1, 0.2");`). The former hand-rolled
                //     parser treated the latter by skipping; we capture the
                //     args as the attribute's values so consumers (the
                //     timing tree-walk) can read them.
                self.expect_char('(')?;
                let mut names = Vec::new();
                let paren_values = self.read_paren_values(&mut names)?;
                self.expect_char(')')?;
                if self.peek_char() == Some('{') {
                    self.expect_char('{')?;
                    let mut sub = LibertyGroup {
                        group_type: keyword,
                        names,
                        ..Default::default()
                    };
                    self.parse_block_into(&mut sub)?;
                    group.groups.push(sub);
                } else {
                    if self.peek_char() == Some(';') {
                        self.expect_char(';')?;
                    }
                    group.attributes.push(Attribute {
                        name: keyword,
                        values: paren_values,
                    });
                }
            }
            Some('{') => {
                // Bare block with no header — treat as a group with no
                // names so its contents are still captured.
                self.expect_char('{')?;
                let mut sub = LibertyGroup {
                    group_type: keyword,
                    ..Default::default()
                };
                self.parse_block_into(&mut sub)?;
                group.groups.push(sub);
            }
            _ => {
                // Dangling identifier with nothing after it (or EOF). The
                // identifier is already consumed; loop again.
            }
        }
        Ok(())
    }

    /// Parse statements until the matching `}` for a block whose `{` was
    /// already consumed.
    fn parse_block_into(&mut self, group: &mut LibertyGroup) -> Result<(), String> {
        while self.peek_char() != Some('}') {
            if self.peek_char().is_none() {
                break;
            }
            self.skip_whitespace();
            let keyword = self.read_identifier();
            if keyword.is_empty() {
                self.pos += 1;
                continue;
            }
            self.parse_statement(keyword, group)?;
        }
        if self.peek_char() == Some('}') {
            self.expect_char('}')?;
        }
        Ok(())
    }

    /// Read a `:`-introduced value list: one or more comma-separated
    /// values terminated by `;`. A scalar attribute yields a single value.
    fn read_value_list(&mut self) -> Result<Vec<Value>, String> {
        let mut values = Vec::new();
        loop {
            self.skip_whitespace();
            let quoted = self.peek_byte() == Some(b'"');
            let raw = self.read_value()?;
            if !raw.is_empty() || quoted {
                values.push(Self::classify_value(raw, quoted));
            }
            match self.peek_char() {
                Some(',') => {
                    self.expect_char(',')?;
                }
                _ => break,
            }
        }
        Ok(values)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root(content: &str) -> LibertyGroup {
        parse(content).expect("parse failed")
    }

    #[test]
    fn parses_library_name_unquoted_and_attributes() {
        let lib = root(
            r#"library(aigpdk) {
                time_unit : "1ps";
                cell(AND2) {
                    pin(Y) { direction : output; }
                }
            }"#,
        );
        assert_eq!(lib.group_type, "library");
        assert_eq!(lib.first_name(), Some("aigpdk"));
        assert_eq!(lib.attr("time_unit").unwrap().first_string(), Some("1ps"));
        let cell = lib.group_of_type("cell").unwrap();
        assert_eq!(cell.first_name(), Some("AND2"));
    }

    #[test]
    fn parses_quoted_sky130_names() {
        let lib = root(
            r#"library ("sky130_fd_sc_hd__tt_025C_1v80") {
                cell ("sky130_fd_sc_hd__inv_1") {
                    pin ("Y") { direction : output; }
                }
            }"#,
        );
        assert_eq!(lib.first_name(), Some("sky130_fd_sc_hd__tt_025C_1v80"));
        let cell = lib.group_of_type("cell").unwrap();
        assert_eq!(cell.first_name(), Some("sky130_fd_sc_hd__inv_1"));
    }

    #[test]
    fn full_tree_cell_pin_timing() {
        let lib = root(
            r#"library (testlib) {
                time_unit : "1ns";
                cell ("foo_inv") {
                    pin ("A") { direction : input; }
                    pin ("Y") {
                        direction : output;
                        function : "(!A)";
                        timing () {
                            related_pin : "A";
                            timing_type : combinational;
                            cell_rise (scalar) {
                                values ("50.0");
                            }
                        }
                    }
                }
            }"#,
        );

        let cell = lib.group_of_type("cell").unwrap();
        assert_eq!(cell.first_name(), Some("foo_inv"));

        // Two pins.
        let pins: Vec<_> = cell.groups_of_type("pin").collect();
        assert_eq!(pins.len(), 2);

        let y = pins.iter().find(|p| p.first_name() == Some("Y")).unwrap();
        assert_eq!(y.attr("direction").unwrap().first_string(), Some("output"));
        assert_eq!(y.attr("function").unwrap().first_string(), Some("(!A)"));

        // Nested timing group with a related_pin and a cell_rise subgroup.
        let timing = y.group_of_type("timing").unwrap();
        assert_eq!(
            timing.attr("related_pin").unwrap().first_string(),
            Some("A")
        );
        assert_eq!(
            timing.attr("timing_type").unwrap().first_string(),
            Some("combinational")
        );
        let cell_rise = timing.group_of_type("cell_rise").unwrap();
        assert_eq!(cell_rise.first_name(), Some("scalar"));
        assert_eq!(
            cell_rise.attr("values").unwrap().first_string(),
            Some("50.0")
        );
    }

    #[test]
    fn synthetic_ramgem_cell_name() {
        let lib = root(
            r#"library(memlib) {
                cell ($__RAMGEM_SYNC_) {
                    pin (PORT_R_RD_DATA) { direction : output; }
                }
            }"#,
        );
        let cell = lib.group_of_type("cell").unwrap();
        assert_eq!(cell.first_name(), Some("$__RAMGEM_SYNC_"));
        let pin = cell.group_of_type("pin").unwrap();
        assert_eq!(pin.first_name(), Some("PORT_R_RD_DATA"));
    }

    #[test]
    fn quoted_numeric_value_keeps_text_and_number() {
        let lib = root(
            r#"library(t) {
                cell (c) {
                    pin (Y) {
                        timing () {
                            cell_rise (scalar) { values ("1.0"); }
                        }
                    }
                }
            }"#,
        );
        let values = lib
            .group_of_type("cell")
            .unwrap()
            .group_of_type("pin")
            .unwrap()
            .group_of_type("timing")
            .unwrap()
            .group_of_type("cell_rise")
            .unwrap()
            .attr("values")
            .unwrap();
        assert_eq!(values.first_string(), Some("1.0"));
        assert_eq!(values.first_number(), Some(1.0));
    }

    #[test]
    fn comments_are_skipped() {
        let lib = root(
            r#"// leading line comment
            library(t) { /* block */ cell(c) { // trailing
                pin(Y) { direction : output; }
            } }"#,
        );
        assert_eq!(lib.first_name(), Some("t"));
        assert!(lib.group_of_type("cell").is_some());
    }

    #[test]
    fn unknown_constructs_are_tolerated() {
        // ff group with parenthesised args, statetable, and an unknown
        // scalar attribute should all be parsed without aborting.
        let lib = root(
            r#"library(t) {
                cell (c) {
                    area : 1.0;
                    ff (IQ, IQN) {
                        next_state : "D";
                        clocked_on : "CLK";
                    }
                    pin (Q) { direction : output; }
                }
            }"#,
        );
        let cell = lib.group_of_type("cell").unwrap();
        // ff captured as a group with two header names.
        let ff = cell.group_of_type("ff").unwrap();
        assert_eq!(ff.names, vec!["IQ".to_string(), "IQN".to_string()]);
        assert_eq!(ff.attr("clocked_on").unwrap().first_string(), Some("CLK"));
        // pin still found after the ff group.
        assert!(cell.group_of_type("pin").is_some());
    }

    #[test]
    fn rejects_non_library_root() {
        let err = parse("cell(foo) {}").expect_err("should reject");
        assert!(err.contains("Expected 'library'"), "got: {err}");
    }
}
