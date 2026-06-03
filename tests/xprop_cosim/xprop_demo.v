// SPDX-License-Identifier: Apache-2.0
//
// Minimal design demonstrating selective X-propagation in cosim (#95).
//
// Covers all the X-source classes the cosim X-prop path must model:
//
//   q_unreset       — bit 0 of an UNRESET counter (`count <= count + 1`).
//                     At power-up its bits are unknown and `X + 1 = X`, so
//                     it stays X forever. (A self-holding `D = Q` register
//                     would be constant-folded; a counter survives as real
//                     adder+flop logic.) This is the power-up DFF X-source.
//
//   q_reset         — a flop reset by `rst_n`, then toggling. Resolves to a
//                     known 0/1 once reset is released — never X afterwards.
//
//   q_undriven_reg  — a RESET flop that captures `unconnected` (an undriven
//                     primary input). Known (0) during/right after reset;
//                     becomes X once it samples the undriven input under
//                     `--xprop` (Phase 3). Without undriven-input X it would
//                     stay 0. This exercises the registered/X-capable cone.
//
//   q_undriven_comb — a PURE-COMBINATIONAL function of `unconnected`
//                     (`unconnected ^ reset_ff`, a real XOR gate so it is a
//                     distinct output net, not an output=input alias that the
//                     output-VCD setup filters out). X^anything = X, so it is
//                     X for the whole run under `--xprop`; toggles known 0/1
//                     in two-state. Exercises undriven-input X through comb
//                     logic (needs the input treated as an X-source, Phase 3).
//
// `unconnected` is a primary input that the cosim config drives with NO
// model/clock/reset/constant — i.e. genuinely undriven, so it reads X.

module xprop_demo (
    input  wire clk,
    input  wire rst_n,
    input  wire unconnected,      // undriven primary input → X
    output wire q_unreset,        // X (unreset counter bit)
    output wire q_reset,          // known after reset
    output wire q_undriven_reg,   // X after reset (reset flop sampling undriven input)
    output wire q_undriven_comb   // X (pure-comb passthrough of undriven input)
);
    (* keep *) reg [3:0] unreset_count;
    (* keep *) reg       reset_ff;
    (* keep *) reg       undriven_ff;

    // No reset → uninitialised; X + 1 = X keeps every bit X.
    always @(posedge clk) begin
        unreset_count <= unreset_count + 4'd1;
    end

    // Reset to 0, then toggles → known.
    always @(posedge clk or negedge rst_n) begin
        if (!rst_n)
            reset_ff <= 1'b0;
        else
            reset_ff <= ~reset_ff;
    end

    // Reset flop that samples the undriven input: known after reset only
    // if the input is known; X once it captures the undriven (X) input.
    always @(posedge clk or negedge rst_n) begin
        if (!rst_n)
            undriven_ff <= 1'b0;
        else
            undriven_ff <= unconnected;
    end

    assign q_unreset       = unreset_count[0];
    assign q_reset         = reset_ff;
    assign q_undriven_reg  = undriven_ff;
    assign q_undriven_comb = unconnected ^ reset_ff;  // real gate, X^any = X
endmodule
