// Annotated sky130 netlist for WS-B B2: the AIG must resolve a sim-visible
// signal back to the RTL `(* src = "file:line" *)` provenance captured on the
// netlist cell. `g0` carries src; the DFF does not.
module prov(CLK, D, A, Y);
  input CLK;
  input D;
  input A;
  output Y;
  wire q;
  sky130_fd_sc_hd__dfxtp_1 dff_in (.CLK(CLK), .D(D), .Q(q));
  (* src = "prov.v:12.3-12.44" *)
  sky130_fd_sc_hd__nand2_1 g0 (.A(A), .B(q), .Y(Y));
endmodule
