// SPDX-License-Identifier: Apache-2.0
//
// Minimal QSPI PSRAM host for exercising Jacquard's RAM-mode flash cosim
// peripheral (the APS6404L-class extension of the SPI-flash GPU peripheral).
//
// It plays a fixed micro-program against an external QSPI PSRAM model
// (Jacquard's `flash` peripheral with `writable = true`):
//
//   1. Enter QPI   (0x35, single-lane)  — latches the model into 4-lane mode.
//   2. Quad write  (0x38) 0xA5 -> addr 0x000001.
//   3. Quad read   (0xEB)          <- addr 0x000001, capture the byte.
//   4. Assert the captured byte == 0xA5 via `match`.
//
// Wire protocol (matches cocotb qspi_psram_model.py / the qspi_psram_ctrl
// controller): SPI mode 0, MSB/high-nibble first, host drives MOSI stable
// across each SCK period, samples MISO on the rising SCK edge; the PSRAM model
// samples MOSI on rising SCK and drives MISO on falling SCK. The 0xEB read has
// 6 dummy SCK cycles after the 24-bit address (QRD_DUMMY = 6).
//
// Timing: one SPI clock period = two system-clock cycles (SCK low for one
// cycle, high for the next), so the GPU model — which steps twice per system
// tick — sees a clean single rising/falling SCK edge per SPI cycle. SCK is
// free-running; the model only samples while CS# is low, so the idle gaps
// between sub-transactions (CS# high) also produce the posedge-CS# resets the
// model needs between commands.

module qspi_psram_dut (
    input  wire       clk,
    input  wire       rst_n,

    // QSPI PSRAM pads (as seen by the model)
    output wire       cs_n,      // chip select, active low
    output wire       sck,       // serial clock (data waveform)
    output wire [3:0] sio_o,     // data driven onto the lanes (host -> PSRAM)
    output wire [3:0] sio_oe,    // per-lane output enable (documentation/debug)
    input  wire [3:0] sio_i,     // data sampled from the lanes (PSRAM -> host)

    // Observation
    output wire [7:0] rdata,     // captured read byte
    output wire       done,      // program finished
    output wire       match      // rdata == 0xA5 (the write value)
);
    // ── System-cycle counter → SPI cycle index + phase ───────────────────────
    // cyc counts system-clock cycles; each SPI cycle is 2 system cycles.
    reg  [8:0] cyc;
    wire [7:0] spi   = cyc[8:1];  // SPI cycle index
    wire       phase = cyc[0];    // 0 = SCK low, 1 = SCK high

    always @(posedge clk or negedge rst_n) begin
        if (!rst_n) cyc <= 9'd0;
        else        cyc <= cyc + 9'd1;
    end

    // SCK is the phase bit: low for the first system cycle of an SPI period,
    // high for the second. Free-running; gated logically by CS#.
    assign sck = phase;

    // ── Micro-program decode (per SPI cycle) ─────────────────────────────────
    // For each SPI cycle: whether CS# is asserted, whether the host drives the
    // lanes, single vs quad lanes, the nibble/bit to drive, and whether to
    // sample MISO (during the high phase).
    reg        cs_low;    // 1 = CS# asserted (low)
    reg        drive;     // 1 = host drives sio_o this cycle
    reg        quad;      // 1 = 4-lane nibble, 0 = 1-lane bit (on sio_o[0])
    reg  [3:0] nib;       // nibble (quad) or bit in [0] (single)
    reg        samp;      // 1 = sample MISO this SPI cycle

    // Program constants.
    // Enter-QPI 0x35 = 0011_0101, MSB first.
    // Quad-write 0x38, addr 0x000001, data 0xA5.
    // Quad-read  0xEB, addr 0x000001, 6 dummy, data.
    always @(*) begin
        cs_low = 1'b0;
        drive  = 1'b0;
        quad   = 1'b0;
        nib    = 4'h0;
        samp   = 1'b0;
        case (spi)
            // ── Enter QPI (0x35), single-lane, bits 7..0 ──
            8'd2:  begin cs_low=1; drive=1; quad=0; nib=4'h0; end // b7=0
            8'd3:  begin cs_low=1; drive=1; quad=0; nib=4'h0; end // b6=0
            8'd4:  begin cs_low=1; drive=1; quad=0; nib=4'h1; end // b5=1
            8'd5:  begin cs_low=1; drive=1; quad=0; nib=4'h1; end // b4=1
            8'd6:  begin cs_low=1; drive=1; quad=0; nib=4'h0; end // b3=0
            8'd7:  begin cs_low=1; drive=1; quad=0; nib=4'h1; end // b2=1
            8'd8:  begin cs_low=1; drive=1; quad=0; nib=4'h0; end // b1=0
            8'd9:  begin cs_low=1; drive=1; quad=0; nib=4'h1; end // b0=1
            // spi 10: idle (CS# high) — model latches QPI + resets.

            // ── Quad write (0x38), addr 0x000001, data 0xA5 ──
            8'd11: begin cs_low=1; drive=1; quad=1; nib=4'h3; end // cmd hi
            8'd12: begin cs_low=1; drive=1; quad=1; nib=4'h8; end // cmd lo
            8'd13: begin cs_low=1; drive=1; quad=1; nib=4'h0; end // addr[23:20]
            8'd14: begin cs_low=1; drive=1; quad=1; nib=4'h0; end // addr[19:16]
            8'd15: begin cs_low=1; drive=1; quad=1; nib=4'h0; end // addr[15:12]
            8'd16: begin cs_low=1; drive=1; quad=1; nib=4'h0; end // addr[11:8]
            8'd17: begin cs_low=1; drive=1; quad=1; nib=4'h0; end // addr[7:4]
            8'd18: begin cs_low=1; drive=1; quad=1; nib=4'h1; end // addr[3:0]=1
            8'd19: begin cs_low=1; drive=1; quad=1; nib=4'hA; end // data hi
            8'd20: begin cs_low=1; drive=1; quad=1; nib=4'h5; end // data lo
            // spi 21: idle (CS# high) — write committed.

            // ── Quad read (0xEB), addr 0x000001, 6 dummy, data ──
            8'd22: begin cs_low=1; drive=1; quad=1; nib=4'hE; end // cmd hi
            8'd23: begin cs_low=1; drive=1; quad=1; nib=4'hB; end // cmd lo
            8'd24: begin cs_low=1; drive=1; quad=1; nib=4'h0; end // addr[23:20]
            8'd25: begin cs_low=1; drive=1; quad=1; nib=4'h0; end // addr[19:16]
            8'd26: begin cs_low=1; drive=1; quad=1; nib=4'h0; end // addr[15:12]
            8'd27: begin cs_low=1; drive=1; quad=1; nib=4'h0; end // addr[11:8]
            8'd28: begin cs_low=1; drive=1; quad=1; nib=4'h0; end // addr[7:4]
            8'd29: begin cs_low=1; drive=1; quad=1; nib=4'h1; end // addr[3:0]=1
            8'd30: begin cs_low=1; drive=0; end // dummy 0
            8'd31: begin cs_low=1; drive=0; end // dummy 1
            8'd32: begin cs_low=1; drive=0; end // dummy 2
            8'd33: begin cs_low=1; drive=0; end // dummy 3
            8'd34: begin cs_low=1; drive=0; end // dummy 4
            8'd35: begin cs_low=1; drive=0; end // dummy 5
            8'd36: begin cs_low=1; drive=0; samp=1; end // data hi nibble
            8'd37: begin cs_low=1; drive=0; samp=1; end // data lo nibble
            // spi >= 38: idle (CS# high), done.
            default: begin cs_low=1'b0; drive=1'b0; end
        endcase
    end

    assign cs_n   = ~cs_low;
    // Host drives lane data; when not driving, present 0.
    assign sio_o  = drive ? (quad ? nib : {3'b000, nib[0]}) : 4'h0;
    // Output-enable is documentation/debug only (the model reads it not).
    assign sio_oe = drive ? (quad ? 4'b1111 : 4'b0001) : 4'b0000;

    // ── Capture read data (sampled on the SCK rising / high phase) ───────────
    reg [3:0] rd_hi;
    reg [3:0] rd_lo;
    reg       done_r;
    always @(posedge clk or negedge rst_n) begin
        if (!rst_n) begin
            rd_hi  <= 4'h0;
            rd_lo  <= 4'h0;
            done_r <= 1'b0;
        end else begin
            if (samp && phase) begin
                if (spi == 8'd36) rd_hi <= sio_i;
                if (spi == 8'd37) rd_lo <= sio_i;
            end
            if (spi >= 8'd38) done_r <= 1'b1;
        end
    end

    assign rdata = {rd_hi, rd_lo};
    assign done  = done_r;
    assign match = done_r && (rdata == 8'hA5);
endmodule
