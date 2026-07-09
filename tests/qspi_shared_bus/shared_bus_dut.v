// SPDX-License-Identifier: Apache-2.0
//
// Shared-bus QSPI cosim DUT: one SPI master, TWO flash instances on a SHARED
// SCK + SIO bus, selected by distinct CS lines. Proves the CS-gated MISO
// arbitration: a deselected flash must present high-Z and not drive the shared
// MISO, otherwise it clobbers the selected flash's read data.
//
// The master performs two back-to-back single-lane READ (0x03) transactions of
// address 0: first with cs_n_a asserted (flash A), then cs_n_b (flash B). Both
// flashes share sck / sio_o (MOSI, lane 0) / sio_i (MISO, lane 1). With correct
// arbitration fa == flash A's byte (0xA1) and fb == flash B's byte (0xB2). With
// the pre-fix bug, the deselected flash drives the shared MISO and fb is wrong.
//
// SPI mode 0, MSB first; one SPI clock period = two system-clock cycles (the
// flash model steps twice per system tick). Each read is 40 SPI cycles:
// 8 command + 24 address + 8 data.

module shared_bus_dut (
    input  wire       clk,
    input  wire       rst_n,

    // Shared SPI bus (both flashes)
    output wire       sck,
    output wire [3:0] sio_o,     // MOSI on lane 0
    input  wire [3:0] sio_i,     // MISO on lane 1
    // Distinct chip selects
    output wire       cs_n_a,
    output wire       cs_n_b,

    // Observation
    output wire [7:0] fa,        // byte read from flash A
    output wire [7:0] fb,        // byte read from flash B
    output wire       done,
    output wire       all_match
);
    localparam CMD = 8'h03;
    // 40-bit frame: {cmd, 24-bit addr 0, 8 don't-care (data phase)}.
    wire [39:0] frame = {CMD, 24'h000000, 8'h00};

    reg [7:0] a_data, b_data;
    reg       cs_a, cs_b;
    reg       sck_r;
    reg [3:0] mosi_r;
    reg [6:0] bitidx;   // 0..39 within a transaction
    reg       phase;    // 0 = SCK low half, 1 = SCK high half
    reg [1:0] txn;      // 0 = read A, 1 = read B, 2 = done
    reg [7:0] shreg;

    wire sel_b = (txn == 2'd1);

    always @(posedge clk or negedge rst_n) begin
        if (!rst_n) begin
            cs_a <= 1'b1; cs_b <= 1'b1;
            sck_r <= 1'b0; mosi_r <= 4'b0;
            bitidx <= 7'd0; phase <= 1'b0; txn <= 2'd0; shreg <= 8'h00;
            a_data <= 8'h00; b_data <= 8'h00;
        end else if (txn != 2'd2) begin
            // Select exactly the current target for the whole transaction.
            cs_a <= sel_b ? 1'b1 : 1'b0;
            cs_b <= sel_b ? 1'b0 : 1'b1;
            if (phase == 1'b0) begin
                // SCK low half: drive MOSI bit, hold SCK low.
                mosi_r[0] <= frame[39 - bitidx];
                sck_r     <= 1'b0;
                phase     <= 1'b1;
            end else begin
                // SCK high half: flash samples MOSI and drives MISO; capture it.
                sck_r <= 1'b1;
                if (bitidx >= 7'd32)
                    shreg <= {shreg[6:0], sio_i[1]};  // data phase: capture MISO
                phase <= 1'b0;
                if (bitidx == 7'd39) begin
                    if (sel_b) b_data <= {shreg[6:0], sio_i[1]};
                    else       a_data <= {shreg[6:0], sio_i[1]};
                    // Deassert both, advance to the next transaction. The
                    // CS# high→low edge on the next target resets its byte FSM.
                    cs_a   <= 1'b1;
                    cs_b   <= 1'b1;
                    bitidx <= 7'd0;
                    txn    <= txn + 2'd1;
                end else begin
                    bitidx <= bitidx + 7'd1;
                end
            end
        end else begin
            cs_a <= 1'b1; cs_b <= 1'b1; sck_r <= 1'b0;
        end
    end

    assign sck    = sck_r;
    assign sio_o  = mosi_r;
    assign cs_n_a = cs_a;
    assign cs_n_b = cs_b;
    assign fa     = a_data;
    assign fb     = b_data;
    assign done   = (txn == 2'd2);
    // Distinct bytes prove the deselected flash did NOT clobber the shared MISO.
    assign all_match = done & (fa == 8'hA1) & (fb == 8'hB2);
endmodule
