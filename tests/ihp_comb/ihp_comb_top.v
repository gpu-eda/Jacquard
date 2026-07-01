/*
 * Combinational IHP SG13G2 gate-level cone -- the ADR 0019 D7a / C3a
 * zero-per-PDK-Rust proof design.
 *
 * The cells UNDER TEST are a cone of REAL sg13g2_* COMBINATIONAL standard
 * cells (inv/buf/nand2/nor2/and2/or2/xor2/xnor2/a21oi/a22oi/o21ai/mux2). Every
 * one is spliced from the build-time-embedded IHP cell-model-IR descriptor,
 * auto-selected by the sg13g2_ prefix, with NO IHP-specific Rust in Jacquard.
 *
 * IHP flops are deliberately NOT used: sequential descriptor-driving for
 * non-GF180 PDKs is the deferred C3.3d work. GEM's simulator is clock-edge
 * driven, so to sweep many input vectors past the combinational cone we clock
 * the primary inputs through a bank of AIGPDK DFFs (the internal library's
 * fully-supported legacy flop) -- a pure timing harness. Each clock cycle the
 * IHP combinational logic is re-evaluated on the registered inputs and checked
 * GPU-vs-CPU (jacquard sim --check-with-cpu).
 *
 * Leaf pin directions for the sg13g2_* cells come from the sibling port-only
 * stub sg13g2_pins.v via --cell-library (ADR 0010) -- an orthogonal data
 * layer, not Rust. The AIGPDK DFF pins are built in.
 */
module ihp_comb_top(
    clk, a, b, c, d, e, f, sel,
    y_nand, y_nor, y_and, y_or, y_xor, y_xnor,
    y_aoi, y_a22, y_oai, y_mux, y_deep
);
  input clk, a, b, c, d, e, f, sel;
  output y_nand, y_nor, y_and, y_or, y_xor, y_xnor;
  output y_aoi, y_a22, y_oai, y_mux, y_deep;

  /* Timing harness: register the primary inputs (AIGPDK DFFs). */
  wire ra, rb, rc, rd, re, rf, rsel;
  DFF ff_a   (.CLK(clk), .D(a),   .Q(ra));
  DFF ff_b   (.CLK(clk), .D(b),   .Q(rb));
  DFF ff_c   (.CLK(clk), .D(c),   .Q(rc));
  DFF ff_d   (.CLK(clk), .D(d),   .Q(rd));
  DFF ff_e   (.CLK(clk), .D(e),   .Q(re));
  DFF ff_f   (.CLK(clk), .D(f),   .Q(rf));
  DFF ff_sel (.CLK(clk), .D(sel), .Q(rsel));

  wire n_inv, n_buf, w_nand, w_nor, t_xor, t_mux;

  /* IHP SG13G2 combinational cone under test (output pin: Y for
   * inv/nand/nor/xnor/aoi/oai, X for buf/and/or/xor/mux). */
  sg13g2_inv_1   u_inv  (.A(ra),  .Y(n_inv));
  sg13g2_buf_1   u_buf  (.A(rb),  .X(n_buf));
  sg13g2_nand2_1 u_nand (.A(ra),  .B(rb),  .Y(y_nand));
  sg13g2_nor2_1  u_nor  (.A(rc),  .B(rd),  .Y(y_nor));
  sg13g2_and2_1  u_and  (.A(ra),  .B(rc),  .X(y_and));
  sg13g2_or2_1   u_or   (.A(rb),  .B(rd),  .X(y_or));
  sg13g2_xor2_1  u_xor  (.A(ra),  .B(re),  .X(y_xor));
  sg13g2_xnor2_1 u_xnor (.A(rc),  .B(rf),  .Y(y_xnor));

  /* Multi-input cells (A1/A2/B1[/B2], A0/A1/S) -- what the PDK-neutral
   * dependency-walk generalization exists for. */
  sg13g2_a21oi_1 u_aoi  (.A1(ra), .A2(rb), .B1(rc),          .Y(y_aoi));
  sg13g2_a22oi_1 u_a22  (.A1(ra), .A2(rb), .B1(rc), .B2(rd), .Y(y_a22));
  sg13g2_o21ai_1 u_oai  (.A1(n_inv), .A2(n_buf), .B1(re),    .Y(y_oai));
  sg13g2_mux2_1  u_mux  (.A0(ra), .A1(rb), .S(rsel),         .X(y_mux));

  /* Deep multi-level cone: nand -> nor -> xor -> mux -> inv. */
  sg13g2_nand2_1 u_d1   (.A(ra),     .B(rb),     .Y(w_nand));
  sg13g2_nor2_1  u_d2   (.A(rc),     .B(rd),     .Y(w_nor));
  sg13g2_xor2_1  u_d3   (.A(w_nand), .B(w_nor),  .X(t_xor));
  sg13g2_mux2_1  u_d4   (.A0(t_xor), .A1(n_inv), .S(rsel), .X(t_mux));
  sg13g2_inv_1   u_d5   (.A(t_mux),  .Y(y_deep));
endmodule
