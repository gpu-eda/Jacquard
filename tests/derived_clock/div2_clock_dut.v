// SPDX-License-Identifier: Apache-2.0
//
// Minimal on-die ÷2 clock-divider DUT — the reduced form of gpu-eda/Jacquard#185.
// A toggle flip-flop divides the master clock by 2; downstream logic is clocked
// by that DERIVED clock. Without derived-clock support the AIG clock tracer
// panics (aig.rs:2676) because a multi-input sequential cell (the divider FF)
// sits on the clock path of the downstream flops.
//
// With a `clocks[]` derived-clock declaration (clkdiv = clk ÷ 2), cosim cuts the
// divider, drives clkdiv as a commensurable ÷2 scheduler domain, and the counter
// increments once per two master-clock cycles.

module div2_clock_dut (
    input  wire       clk,     // master clock (the "clk_pad")
    input  wire       rst_n,   // active-low async reset
    output wire       clkdiv,  // the ÷2 derived clock (exposed for observation)
    output wire [7:0] count     // counter clocked by the DERIVED clock
);
    // ── ÷2 toggle divider: D = ~Q, clocked by the master clock ───────────────
    reg clkdiv_r;
    always @(posedge clk or negedge rst_n) begin
        if (!rst_n) clkdiv_r <= 1'b0;
        else        clkdiv_r <= ~clkdiv_r;
    end

    // ── Downstream counter clocked by the DIVIDED clock ──────────────────────
    // This is the flop whose CLK trips the tracer (its clock traces back through
    // the multi-input divider FF).
    reg [7:0] cnt;
    always @(posedge clkdiv_r or negedge rst_n) begin
        if (!rst_n) cnt <= 8'd0;
        else        cnt <= cnt + 8'd1;
    end

    assign clkdiv = clkdiv_r;
    assign count  = cnt;
endmodule
