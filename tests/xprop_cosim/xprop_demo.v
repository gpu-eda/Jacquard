// SPDX-License-Identifier: Apache-2.0
//
// Minimal design demonstrating selective X-propagation in cosim (#95).
//
// `unreset_count` is a counter with NO reset. At power-up its bits are
// unknown; `X + 1 = X` propagates through the adder, so it stays X
// forever. (A self-holding `D = Q` register would be constant-folded by
// synthesis; a counter survives as real adder+flop logic.) Under
// two-state sim it silently reads a definite value; under `--xprop` its
// bits are X — so `q_unreset` shows `x` in the output VCD for the whole
// run.
//
// `reset_ff` is reset by rst_n and then toggles, so it resolves to a
// known 0/1 once reset is released — `q_reset` is never X after reset.

module xprop_demo (
    input  wire clk,
    input  wire rst_n,
    output wire q_unreset,   // X (unreset counter bit)
    output wire q_reset      // known after reset
);
    (* keep *) reg [3:0] unreset_count;
    (* keep *) reg       reset_ff;

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

    assign q_unreset = unreset_count[0];
    assign q_reset   = reset_ff;
endmodule
