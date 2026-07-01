/*
 * Port-only stubs for the IHP SG13G2 combinational cells used by
 * ihp_comb_top.v. Passed via --cell-library so the netlist reader learns each
 * leaf cell's pin directions (ADR 0010, RuntimeCellLibrary) -- an orthogonal
 * data layer, NOT Rust. Port directions mirror the real IHP sg13g2_stdcell.v
 * module headers exactly (output first, then inputs).
 *
 * The behaviour of each cell is NOT taken from here; it is spliced from the
 * build-time-embedded cell-model-IR descriptor. These stubs supply only pin
 * directions. A future descriptor-backed leaf-pin provider (C3.3d) would let
 * the descriptor's L1 directions replace even this stub, dropping the file.
 */
module sg13g2_inv_1   (Y, A);              output Y; input A;              endmodule
module sg13g2_buf_1   (X, A);              output X; input A;              endmodule
module sg13g2_nand2_1 (Y, A, B);           output Y; input A, B;           endmodule
module sg13g2_nor2_1  (Y, A, B);           output Y; input A, B;           endmodule
module sg13g2_and2_1  (X, A, B);           output X; input A, B;           endmodule
module sg13g2_or2_1   (X, A, B);           output X; input A, B;           endmodule
module sg13g2_xor2_1  (X, A, B);           output X; input A, B;           endmodule
module sg13g2_xnor2_1 (Y, A, B);           output Y; input A, B;           endmodule
module sg13g2_a21oi_1 (Y, A1, A2, B1);     output Y; input A1, A2, B1;     endmodule
module sg13g2_a22oi_1 (Y, A1, A2, B1, B2); output Y; input A1, A2, B1, B2; endmodule
module sg13g2_o21ai_1 (Y, A1, A2, B1);     output Y; input A1, A2, B1;     endmodule
module sg13g2_mux2_1  (X, A0, A1, S);      output X; input A0, A1, S;      endmodule
