// SPDX-License-Identifier: Apache-2.0

//! Liberty `function` expression -> [`cell_model_ir::CombLogic`] (single output).
//!
//! This is the crux of the converter (Decision 0019 D3/D6): it turns a Liberty
//! boolean function string into a pre-built and-inverter graph that the
//! runtime splices into the design AIG with no further decomposition.
//!
//! ## Grammar / operators
//!
//! Operands are pin identifiers (`[A-Za-z0-9_$]+`) and the constants `0`/`1`.
//!
//! | Operator | Meaning | Fixity | Precedence |
//! |----------|---------|--------|------------|
//! | `'`      | invert  | postfix (binds to preceding operand / group) | highest |
//! | `!`      | invert  | prefix | highest |
//! | `*`, `&`, juxtaposition | AND | infix | high |
//! | `^`      | XOR     | infix | medium |
//! | `+`, `\|` | OR      | infix | low |
//!
//! Parentheses override precedence. Juxtaposition (whitespace between two
//! operands, e.g. `A B`) is AND.
//!
//! ## Two paths, cross-checked in tests
//!
//! 1. [`Expr`] — the parsed AST, evaluated directly by [`Expr::eval`] (the
//!    independent reference evaluator).
//! 2. [`compile`] — lowers the AST into a [`CombLogic`] AIG.
//!
//! The unit tests assert, for *every* input assignment, that
//! `CombLogic::eval == Expr::eval`. This catches precedence and lowering
//! bugs structurally rather than by spot-checking a few vectors.

use std::collections::BTreeSet;

use cell_model_ir::{AndNode, CombLogic, OutputPin, Ref};

// ============================================================================
// Expression AST
// ============================================================================

/// A parsed Liberty boolean function expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expr {
    /// Constant 0 or 1.
    Const(bool),
    /// A pin reference by name.
    Pin(String),
    /// Logical NOT of the inner expression.
    Not(Box<Expr>),
    /// Logical AND.
    And(Box<Expr>, Box<Expr>),
    /// Logical XOR.
    Xor(Box<Expr>, Box<Expr>),
    /// Logical OR.
    Or(Box<Expr>, Box<Expr>),
}

impl Expr {
    /// Independent reference evaluator: evaluate the AST directly against a
    /// pin-value map, without going through the AIG. The test gate compares
    /// this against [`CombLogic::eval`] for every input assignment.
    pub fn eval(&self, vals: &std::collections::HashMap<String, bool>) -> bool {
        match self {
            Expr::Const(b) => *b,
            Expr::Pin(name) => *vals
                .get(name)
                .unwrap_or_else(|| panic!("reference eval: missing pin '{name}'")),
            Expr::Not(e) => !e.eval(vals),
            Expr::And(a, b) => a.eval(vals) && b.eval(vals),
            Expr::Xor(a, b) => a.eval(vals) ^ b.eval(vals),
            Expr::Or(a, b) => a.eval(vals) || b.eval(vals),
        }
    }

    /// Collect the pin names referenced by the expression, sorted & deduped.
    pub fn pins(&self) -> Vec<String> {
        let mut set = BTreeSet::new();
        self.collect_pins(&mut set);
        set.into_iter().collect()
    }

    fn collect_pins(&self, set: &mut BTreeSet<String>) {
        match self {
            Expr::Const(_) => {}
            Expr::Pin(name) => {
                set.insert(name.clone());
            }
            Expr::Not(e) => e.collect_pins(set),
            Expr::And(a, b) | Expr::Xor(a, b) | Expr::Or(a, b) => {
                a.collect_pins(set);
                b.collect_pins(set);
            }
        }
    }

    /// Return a copy with every reference to pin `from` replaced by `to`.
    /// Used to fold a flip-flop's inverted state variable (`IQN`) into
    /// `!IQ` so the next-state AIG needs only a single self-feedback input
    /// representing the current state.
    pub fn substitute_pin(&self, from: &str, to: &Expr) -> Expr {
        match self {
            Expr::Pin(name) if name == from => to.clone(),
            Expr::Const(_) | Expr::Pin(_) => self.clone(),
            Expr::Not(e) => Expr::Not(Box::new(e.substitute_pin(from, to))),
            Expr::And(a, b) => Expr::And(
                Box::new(a.substitute_pin(from, to)),
                Box::new(b.substitute_pin(from, to)),
            ),
            Expr::Xor(a, b) => Expr::Xor(
                Box::new(a.substitute_pin(from, to)),
                Box::new(b.substitute_pin(from, to)),
            ),
            Expr::Or(a, b) => Expr::Or(
                Box::new(a.substitute_pin(from, to)),
                Box::new(b.substitute_pin(from, to)),
            ),
        }
    }
}

// ============================================================================
// Parser
// ============================================================================

/// Parse a Liberty `function` expression string into an [`Expr`] AST.
///
/// Returns `Err` with a human-readable message on a syntax error.
pub fn parse(src: &str) -> Result<Expr, String> {
    let tokens = tokenize(src)?;
    let mut p = ExprParser { tokens, pos: 0 };
    let e = p.parse_or()?;
    if p.pos != p.tokens.len() {
        return Err(format!(
            "trailing tokens after expression in '{src}' at token {}",
            p.pos
        ));
    }
    Ok(e)
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Tok {
    Ident(String),
    Const(bool),
    Not,    // !
    Invert, // ' (postfix)
    And,    // * or &
    Xor,    // ^
    Or,     // + or |
    LParen,
    RParen,
}

fn tokenize(src: &str) -> Result<Vec<Tok>, String> {
    let mut toks = Vec::new();
    let bytes = src.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i] as char;
        match c {
            c if c.is_whitespace() => {
                // Whitespace between two operands means juxtaposition-AND.
                // We emit a synthetic AND only if it separates operand-ish
                // tokens; the parser's juxtaposition handling (parse_and)
                // already treats adjacency as AND, so we simply drop
                // whitespace here and let the parser detect adjacency.
                i += 1;
            }
            '!' => {
                toks.push(Tok::Not);
                i += 1;
            }
            '\'' => {
                toks.push(Tok::Invert);
                i += 1;
            }
            '*' | '&' => {
                toks.push(Tok::And);
                i += 1;
            }
            '^' => {
                toks.push(Tok::Xor);
                i += 1;
            }
            '+' | '|' => {
                toks.push(Tok::Or);
                i += 1;
            }
            '(' => {
                toks.push(Tok::LParen);
                i += 1;
            }
            ')' => {
                toks.push(Tok::RParen);
                i += 1;
            }
            c if is_ident_char(c) => {
                let start = i;
                while i < bytes.len() && is_ident_char(bytes[i] as char) {
                    i += 1;
                }
                let word = &src[start..i];
                if word == "0" {
                    toks.push(Tok::Const(false));
                } else if word == "1" {
                    toks.push(Tok::Const(true));
                } else {
                    toks.push(Tok::Ident(word.to_string()));
                }
            }
            other => {
                return Err(format!(
                    "unexpected character '{other}' in function '{src}'"
                ))
            }
        }
    }
    Ok(toks)
}

fn is_ident_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '$'
}

struct ExprParser {
    tokens: Vec<Tok>,
    pos: usize,
}

impl ExprParser {
    fn peek(&self) -> Option<&Tok> {
        self.tokens.get(self.pos)
    }

    fn bump(&mut self) -> Option<Tok> {
        let t = self.tokens.get(self.pos).cloned();
        if t.is_some() {
            self.pos += 1;
        }
        t
    }

    /// OR has the lowest precedence.
    fn parse_or(&mut self) -> Result<Expr, String> {
        let mut lhs = self.parse_xor()?;
        while matches!(self.peek(), Some(Tok::Or)) {
            self.bump();
            let rhs = self.parse_xor()?;
            lhs = Expr::Or(Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    /// XOR sits between OR and AND.
    fn parse_xor(&mut self) -> Result<Expr, String> {
        let mut lhs = self.parse_and()?;
        while matches!(self.peek(), Some(Tok::Xor)) {
            self.bump();
            let rhs = self.parse_and()?;
            lhs = Expr::Xor(Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    /// AND: explicit (`*`/`&`) or implicit (juxtaposition). Juxtaposition is
    /// detected when the next token starts a new factor (operand, `!`, or
    /// `(`) without an intervening binary operator.
    fn parse_and(&mut self) -> Result<Expr, String> {
        let mut lhs = self.parse_unary()?;
        loop {
            match self.peek() {
                Some(Tok::And) => {
                    self.bump();
                    let rhs = self.parse_unary()?;
                    lhs = Expr::And(Box::new(lhs), Box::new(rhs));
                }
                // Juxtaposition-AND: a factor-starting token directly follows.
                Some(Tok::Ident(_)) | Some(Tok::Const(_)) | Some(Tok::Not) | Some(Tok::LParen) => {
                    let rhs = self.parse_unary()?;
                    lhs = Expr::And(Box::new(lhs), Box::new(rhs));
                }
                _ => break,
            }
        }
        Ok(lhs)
    }

    /// Unary: prefix `!` (highest precedence, right-associative) wrapping a
    /// postfix-inverted primary.
    fn parse_unary(&mut self) -> Result<Expr, String> {
        if matches!(self.peek(), Some(Tok::Not)) {
            self.bump();
            let inner = self.parse_unary()?;
            return Ok(Expr::Not(Box::new(inner)));
        }
        self.parse_postfix()
    }

    /// Primary followed by zero or more postfix `'` inverts.
    fn parse_postfix(&mut self) -> Result<Expr, String> {
        let mut e = self.parse_primary()?;
        while matches!(self.peek(), Some(Tok::Invert)) {
            self.bump();
            e = Expr::Not(Box::new(e));
        }
        Ok(e)
    }

    fn parse_primary(&mut self) -> Result<Expr, String> {
        match self.bump() {
            Some(Tok::Ident(name)) => Ok(Expr::Pin(name)),
            Some(Tok::Const(b)) => Ok(Expr::Const(b)),
            Some(Tok::LParen) => {
                let e = self.parse_or()?;
                match self.bump() {
                    Some(Tok::RParen) => Ok(e),
                    other => Err(format!("expected ')', found {other:?}")),
                }
            }
            other => Err(format!("expected operand, found {other:?}")),
        }
    }
}

// ============================================================================
// AIG compiler
// ============================================================================

/// A small AIG builder that emits [`cell_model_ir`] nodes directly.
///
/// Node numbering matches the IR contract: node 0 = const-0; nodes
/// `1..=inputs.len()` = input pins in `inputs` order; remaining nodes =
/// `and_nodes` in push order.
struct AigBuilder {
    inputs: Vec<String>,
    and_nodes: Vec<AndNode>,
    input_base: u32, // = 1 (const-0 is node 0)
}

impl AigBuilder {
    fn new(inputs: Vec<String>) -> Self {
        AigBuilder {
            inputs,
            and_nodes: Vec::new(),
            input_base: 1,
        }
    }

    fn input_ref(&self, idx: usize) -> Ref {
        Ref::node(self.input_base + idx as u32)
    }

    fn const0() -> Ref {
        Ref::node(0)
    }

    fn const1() -> Ref {
        Ref::inv(0)
    }

    fn invert(r: Ref) -> Ref {
        Ref {
            node: r.node,
            inverted: !r.inverted,
        }
    }

    /// Push a 2-input AND gate, returning a Ref to its (non-inverted) output.
    fn and(&mut self, a: Ref, b: Ref) -> Ref {
        let idx = self.and_nodes.len();
        self.and_nodes.push(AndNode { a, b });
        let node = self.input_base + self.inputs.len() as u32 + idx as u32;
        Ref::node(node)
    }

    /// OR via De Morgan: a | b = !(!a & !b).
    fn or(&mut self, a: Ref, b: Ref) -> Ref {
        let na = Self::invert(a);
        let nb = Self::invert(b);
        let and = self.and(na, nb);
        Self::invert(and)
    }

    /// XOR: a ^ b = !( !(a & !b) & !(!a & b) ).
    fn xor(&mut self, a: Ref, b: Ref) -> Ref {
        let a_nb = self.and(a, Self::invert(b));
        let na_b = self.and(Self::invert(a), b);
        let and = self.and(Self::invert(a_nb), Self::invert(na_b));
        Self::invert(and)
    }

    fn lower(&mut self, e: &Expr, idx_of: &dyn Fn(&str) -> usize) -> Ref {
        match e {
            Expr::Const(false) => Self::const0(),
            Expr::Const(true) => Self::const1(),
            Expr::Pin(name) => self.input_ref(idx_of(name)),
            Expr::Not(inner) => Self::invert(self.lower(inner, idx_of)),
            Expr::And(a, b) => {
                let ra = self.lower(a, idx_of);
                let rb = self.lower(b, idx_of);
                self.and(ra, rb)
            }
            Expr::Xor(a, b) => {
                let ra = self.lower(a, idx_of);
                let rb = self.lower(b, idx_of);
                self.xor(ra, rb)
            }
            Expr::Or(a, b) => {
                let ra = self.lower(a, idx_of);
                let rb = self.lower(b, idx_of);
                self.or(ra, rb)
            }
        }
    }
}

/// Compile a single-output Liberty function expression into a [`CombLogic`].
///
/// `output_pin` names the output; `inputs` is the (deterministic) input-pin
/// ordering — pins referenced by the expression but absent from `inputs`
/// are an error. Passing a superset of the referenced pins is allowed (used
/// by the per-cell shared-input merge).
pub fn compile(output_pin: &str, expr: &Expr, inputs: &[String]) -> Result<CombLogic, String> {
    let idx_of_map: std::collections::HashMap<&str, usize> = inputs
        .iter()
        .enumerate()
        .map(|(i, s)| (s.as_str(), i))
        .collect();
    for p in expr.pins() {
        if !idx_of_map.contains_key(p.as_str()) {
            return Err(format!(
                "function for '{output_pin}' references pin '{p}' not in inputs {inputs:?}"
            ));
        }
    }
    let mut b = AigBuilder::new(inputs.to_vec());
    let r = {
        let idx_of = |name: &str| idx_of_map[name];
        b.lower(expr, &idx_of)
    };
    Ok(CombLogic {
        inputs: inputs.to_vec(),
        and_nodes: b.and_nodes,
        outputs: vec![OutputPin {
            pin: output_pin.to_string(),
            r,
        }],
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// THE TDD GATE: for the given function string, enumerate every
    /// assignment of its input pins and assert the compiled AIG agrees with
    /// the independent reference evaluator on the parsed AST.
    fn assert_aig_matches_reference(src: &str) {
        let expr = parse(src).unwrap_or_else(|e| panic!("parse '{src}': {e}"));
        let pins = expr.pins();
        let logic = compile("Y", &expr, &pins).unwrap_or_else(|e| panic!("compile '{src}': {e}"));
        let n = pins.len();
        assert!(n <= 16, "test function too wide: {src}");
        for mask in 0u32..(1u32 << n) {
            let mut vals = HashMap::new();
            for (i, pin) in pins.iter().enumerate() {
                vals.insert(pin.clone(), (mask >> i) & 1 == 1);
            }
            let reference = expr.eval(&vals);
            let out = logic.eval(&vals).unwrap();
            let aig = *out.get("Y").unwrap();
            assert_eq!(
                aig, reference,
                "mismatch for '{src}' at assignment {vals:?}: aig={aig} reference={reference}"
            );
        }
    }

    #[test]
    fn nand2_bang() {
        assert_aig_matches_reference("!(A*B)");
    }

    #[test]
    fn or_paren() {
        assert_aig_matches_reference("(A+B)");
    }

    #[test]
    fn xor_basic() {
        assert_aig_matches_reference("A^B");
    }

    #[test]
    fn postfix_invert() {
        assert_aig_matches_reference("A'");
    }

    #[test]
    fn aoi_mixed() {
        assert_aig_matches_reference("!((A1*A2)+B1)");
    }

    #[test]
    fn juxtaposition_and() {
        assert_aig_matches_reference("A B");
    }

    #[test]
    fn mixed_precedence_and_over_or() {
        // A B + C  ==  (A AND B) OR C
        assert_aig_matches_reference("A B + C");
    }

    #[test]
    fn mixed_precedence_or_with_juxtaposition() {
        // A + B C  ==  A OR (B AND C)
        assert_aig_matches_reference("A + B C");
    }

    #[test]
    fn xor3() {
        assert_aig_matches_reference("(A^B^CI)");
    }

    #[test]
    fn const_one() {
        assert_aig_matches_reference("1");
    }

    #[test]
    fn const_zero() {
        assert_aig_matches_reference("0");
    }

    #[test]
    fn three_input_aoi() {
        assert_aig_matches_reference("(((!A1)&(!A2))|(!B))");
    }

    #[test]
    fn full_adder_carry() {
        assert_aig_matches_reference("((A&B)|(A&CI)|(B&CI))");
    }

    #[test]
    fn mux4() {
        assert_aig_matches_reference("((I0&(!S0)&(!S1))|(I1&S0&(!S1))|(I2&(!S0)&S1)|(I3&S0&S1))");
    }

    // --- precedence assertions on the AST itself (structural) ---

    #[test]
    fn postfix_binds_tighter_than_and() {
        // A B'  ==  A AND (B')
        let e = parse("A B'").unwrap();
        assert_eq!(
            e,
            Expr::And(
                Box::new(Expr::Pin("A".into())),
                Box::new(Expr::Not(Box::new(Expr::Pin("B".into()))))
            )
        );
    }

    #[test]
    fn postfix_on_group() {
        // (A B)'  ==  NOT(A AND B)
        let e = parse("(A B)'").unwrap();
        assert_eq!(
            e,
            Expr::Not(Box::new(Expr::And(
                Box::new(Expr::Pin("A".into())),
                Box::new(Expr::Pin("B".into()))
            )))
        );
    }

    #[test]
    fn prefix_not_binds_tighter_than_and() {
        // !A B  ==  (!A) AND B
        let e = parse("!A B").unwrap();
        assert_eq!(
            e,
            Expr::And(
                Box::new(Expr::Not(Box::new(Expr::Pin("A".into())))),
                Box::new(Expr::Pin("B".into()))
            )
        );
    }

    #[test]
    fn xor_between_and_and_or() {
        // A B ^ C + D  ==  ((A AND B) XOR C) OR D
        let e = parse("A B ^ C + D").unwrap();
        assert_eq!(
            e,
            Expr::Or(
                Box::new(Expr::Xor(
                    Box::new(Expr::And(
                        Box::new(Expr::Pin("A".into())),
                        Box::new(Expr::Pin("B".into()))
                    )),
                    Box::new(Expr::Pin("C".into()))
                )),
                Box::new(Expr::Pin("D".into()))
            )
        );
    }

    #[test]
    fn star_and_ampersand_equivalent() {
        assert_eq!(parse("A*B").unwrap(), parse("A&B").unwrap());
    }

    #[test]
    fn plus_and_pipe_equivalent() {
        assert_eq!(parse("A+B").unwrap(), parse("A|B").unwrap());
    }

    #[test]
    fn parse_errors_on_garbage() {
        assert!(parse("A @ B").is_err());
        assert!(parse("(A").is_err());
        assert!(parse("A )").is_err());
    }

    #[test]
    fn compile_rejects_unknown_pin() {
        let e = parse("A & B").unwrap();
        assert!(compile("Y", &e, &["A".to_string()]).is_err());
    }
}
