// SPDX-License-Identifier: Apache-2.0
//
// Multi-memory cosim DUT: exercises Jacquard's PLURAL QSPI-memory peripheral
// (ADR 0013 Stage B — three independent SPI flash instances) alongside two
// independent on-chip SRAMs ($__RAMGEM_SYNC_). Proves each memory has its own
// backing store: the three flashes return three distinct bytes and the two
// SRAMs read back two distinct values.
//
// Each flash reader issues a single-lane SPI READ (0x03) of one byte at
// address 0 and captures it. SPI mode 0, MSB first; one SPI clock period = two
// system-clock cycles (the GPU flash model steps twice per system tick, so it
// sees a clean rising/falling SCK edge per SPI cycle). CS# is held low across
// the whole transaction; the reader starts after reset deasserts.

// ── Single-byte SPI flash reader (0x03 READ, addr 0, 1 byte) ─────────────────
module spi_read_byte (
    input  wire       clk,
    input  wire       rst_n,
    output reg        cs_n,
    output reg        sck,
    output reg  [3:0] sio_o,   // MOSI on lane 0
    input  wire [3:0] sio_i,   // MISO on lane 1
    output reg  [7:0] data,    // captured byte
    output reg        done
);
    // 32 SPI cycles: 8 command (0x03) + 24 address (0x000000), then 8 data.
    // Total SPI cycles = 40. Each SPI cycle = 2 system cycles.
    localparam CMD  = 8'h03;
    // bit index within the 40-cycle transaction; -1 = idle before start.
    reg [6:0] bitidx;   // 0..39
    reg       phase;    // 0 = SCK low half, 1 = SCK high half
    reg [7:0] shreg;    // MISO capture

    // 40-bit MOSI vector: {cmd[7:0], addr[23:0], 8x don't-care}
    wire [39:0] mosi = {CMD, 24'h000000, 8'h00};

    always @(posedge clk or negedge rst_n) begin
        if (!rst_n) begin
            cs_n   <= 1'b1;
            sck    <= 1'b0;
            sio_o  <= 4'b0;
            data   <= 8'h00;
            done   <= 1'b0;
            bitidx <= 7'd0;
            phase  <= 1'b0;
            shreg  <= 8'h00;
        end else if (!done) begin
            cs_n <= 1'b0;               // select for the whole transaction
            if (phase == 1'b0) begin
                // SCK low half: drive MOSI bit, then raise SCK next cycle.
                sio_o[0] <= mosi[39 - bitidx];
                sck      <= 1'b0;
                phase    <= 1'b1;
            end else begin
                // SCK high half: model samples MOSI + drives MISO; capture it.
                sck   <= 1'b1;
                if (bitidx >= 7'd32)
                    shreg <= {shreg[6:0], sio_i[1]};  // data phase: capture MISO
                phase <= 1'b0;
                if (bitidx == 7'd39) begin
                    data <= {shreg[6:0], sio_i[1]};
                    done <= 1'b1;
                    cs_n <= 1'b1;
                end else begin
                    bitidx <= bitidx + 7'd1;
                end
            end
        end
    end
endmodule

// ── Top: 3 flashes + 2 SRAMs ─────────────────────────────────────────────────
module multi_mem_dut (
    input  wire       clk,
    input  wire       rst_n,

    // Flash 0
    output wire       cs_n_0, output wire sck_0,
    output wire [3:0] sio_o_0, input wire [3:0] sio_i_0,
    // Flash 1
    output wire       cs_n_1, output wire sck_1,
    output wire [3:0] sio_o_1, input wire [3:0] sio_i_1,
    // Flash 2
    output wire       cs_n_2, output wire sck_2,
    output wire [3:0] sio_o_2, input wire [3:0] sio_i_2,

    // Observation
    output wire [7:0] f0, output wire [7:0] f1, output wire [7:0] f2,
    output wire [7:0] s0, output wire [7:0] s1,
    output wire       done,
    output wire       all_match
);
    wire d0, d1, d2;
    spi_read_byte rd0 (.clk(clk), .rst_n(rst_n), .cs_n(cs_n_0), .sck(sck_0),
                       .sio_o(sio_o_0), .sio_i(sio_i_0), .data(f0), .done(d0));
    spi_read_byte rd1 (.clk(clk), .rst_n(rst_n), .cs_n(cs_n_1), .sck(sck_1),
                       .sio_o(sio_o_1), .sio_i(sio_i_1), .data(f1), .done(d1));
    spi_read_byte rd2 (.clk(clk), .rst_n(rst_n), .cs_n(cs_n_2), .sck(sck_2),
                       .sio_o(sio_o_2), .sio_i(sio_i_2), .data(f2), .done(d2));

    // ── Two independent on-chip SRAMs ────────────────────────────────────────
    // Write a distinct value into each at cycle 1, read it back at cycle 3.
    // Distinct addresses + distinct data prove independent backing stores.
    reg [31:0] mem0 [0:8191];
    reg [31:0] mem1 [0:8191];
    reg [12:0] waddr, raddr;
    reg        we;
    reg [31:0] wdat0, wdat1;
    reg [31:0] rd0_q, rd1_q;
    reg [4:0]  seq;

    always @(posedge clk or negedge rst_n) begin
        if (!rst_n) begin
            seq <= 5'd0; we <= 1'b0; waddr <= 13'd0; raddr <= 13'd0;
            wdat0 <= 32'd0; wdat1 <= 32'd0;
        end else begin
            case (seq)
                5'd0: begin waddr <= 13'd7; wdat0 <= 32'h000000A1;
                            wdat1 <= 32'h000000B2; we <= 1'b1; seq <= 5'd1; end
                5'd1: begin we <= 1'b0; raddr <= 13'd7; seq <= 5'd2; end
                default: seq <= seq;  // hold
            endcase
        end
    end
    always @(posedge clk) begin
        if (we) begin mem0[waddr] <= wdat0; mem1[waddr] <= wdat1; end
        rd0_q <= mem0[raddr];
        rd1_q <= mem1[raddr];
    end
    assign s0 = rd0_q[7:0];
    assign s1 = rd1_q[7:0];

    assign done = d0 & d1 & d2;
    // Expected: flash byte 0 preloaded distinct per instance (0xA1/0xB2/0xC3);
    // SRAM readback 0xA1 / 0xB2. all_match iff every store is independent.
    assign all_match = done
        & (f0 == 8'hA1) & (f1 == 8'hB2) & (f2 == 8'hC3)
        & (s0 == 8'hA1) & (s1 == 8'hB2);
endmodule
