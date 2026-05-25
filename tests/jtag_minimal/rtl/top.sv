// SPDX-License-Identifier: Apache-2.0
//
// tests/jtag_minimal/rtl/top.sv
//
// Minimal JTAG cosim regression: vendored Hazard3 DTM + DM (N_HARTS=1)
// + a "fake hart" stub. Drives the chip from a captured remote_bitbang
// stream via JtagReplayModel; observable outputs let `--trace-signals`
// confirm DMI APB writes actually land on DM-internal registers.
//
// What this exercises:
//   - JtagReplayModel feeds TCK/TMS/TDI/TRST_N from a captured
//     OpenOCD remote_bitbang stream
//   - TAP state machine (hazard3_jtag_dtm.tap_state) traverses
//     RTI → SelectDR → ... → ShiftIR → UpdateIR cleanly
//   - DMI APB transactions cross from TCK domain to chip-clock
//     domain via the DTM's built-in 2FF CDC on dmi_psel/dmi_penable
//   - hazard3_dm decodes DMI writes and updates internal registers
//     (dmcontrol.dmactive, data0, etc.)
//
// What this does NOT exercise:
//   - RISC-V hart instruction execution (no CPU; hart_halted is tied
//     to ~hart_req_halt so DM thinks a hart exists and is responsive)
//   - System-bus access (HAVE_SBA=0)
//   - Memory abstract commands (no memory backing the hart)
//
// Expected bitbang stream sequence (see scripts/capture_bitbang.py
// for how this is generated):
//   1. TAP reset via 5×TMS=1
//   2. IR=DMI (0x10)
//   3. Write DMCONTROL.dmactive=1, DMCONTROL.haltreq=1
//   4. Poll DMSTATUS.allhalted (expect 1 immediately — fake hart)
//   5. Write DATA0=0xCAFEBABE
//   6. Shutdown
//
// Pass criterion (asserted from cosim's --timing-vcd output):
//   data0_obs ends up at 32'hCAFEBABE
//   dmactive_obs ends up at 1
//   haltreq_obs sees at least one 0→1 transition

`include "hazard3_compat.vh"

`timescale 1ns/1ps

`default_nettype none

module top (
    // Chip clock + reset (system / DMI domain)
    input  wire        clk,
    input  wire        rst_n,
    // JTAG TAP (TCK is async to chip clock; CDC happens inside DTM)
    input  wire        tck,
    input  wire        tms,
    input  wire        tdi,
    output wire        tdo,
    input  wire        trst_n,
    // Observable outputs — what cosim's --trace-signals reads
    output wire        dmactive_obs,
    output wire        haltreq_obs,
    output wire [31:0] data0_obs
);

    // ---- DTM ↔ DM DMI bus wires ----
    wire        dmi_psel;
    wire        dmi_penable;
    wire        dmi_pwrite;
    wire [8:0]  dmi_paddr;
    wire [31:0] dmi_pwdata;
    wire [31:0] dmi_prdata;
    wire        dmi_pready;
    wire        dmi_pslverr;
    wire        dmihardreset_req;

    // DMI reset: chip reset OR the DTM's hard-reset request.
    wire rst_n_dmi = rst_n & ~dmihardreset_req;

    // ---- DTM instance ----
    // IDCODE chosen to be visible + distinctive in OpenOCD logs.
    // W_PADDR=9 → ABITS=7 (RISC-V Debug Spec 7-bit DMI address).
    hazard3_jtag_dtm #(
        .IDCODE          (32'hdeadbeef),
        .DTMCS_IDLE_HINT (3'd4),
        .W_PADDR         (9)
    ) u_dtm (
        .tck              (tck),
        .trst_n           (trst_n),
        .tms              (tms),
        .tdi              (tdi),
        .tdo              (tdo),
        .dmihardreset_req (dmihardreset_req),
        .clk_dmi          (clk),
        .rst_n_dmi        (rst_n_dmi),
        .dmi_psel         (dmi_psel),
        .dmi_penable      (dmi_penable),
        .dmi_pwrite       (dmi_pwrite),
        .dmi_paddr        (dmi_paddr),
        .dmi_pwdata       (dmi_pwdata),
        .dmi_prdata       (dmi_prdata),
        .dmi_pready       (dmi_pready),
        .dmi_pslverr      (dmi_pslverr)
    );

    // ---- "fake hart" wires ----
    wire        hart_req_halt;
    wire        hart_req_halt_on_reset;
    wire        hart_req_resume;
    wire        hart_reset_req;
    wire [31:0] hart_data0_rdata;
    wire [31:0] hart_instr_data;
    wire        hart_instr_data_vld;

    // ---- DM instance ----
    hazard3_dm #(
        .N_HARTS      (1),
        .NEXT_DM_ADDR (32'h0000_0000),
        .HAVE_SBA     (0)
    ) u_dm (
        .clk         (clk),
        .rst_n       (rst_n),

        .dmi_psel    (dmi_psel),
        .dmi_penable (dmi_penable),
        .dmi_pwrite  (dmi_pwrite),
        .dmi_paddr   (dmi_paddr),
        .dmi_pwdata  (dmi_pwdata),
        .dmi_prdata  (dmi_prdata),
        .dmi_pready  (dmi_pready),
        .dmi_pslverr (dmi_pslverr),

        // System reset request — wired open, ack immediately.
        .sys_reset_req       (),
        .sys_reset_done      (1'b1),

        // Per-hart reset — wired open, ack immediately.
        .hart_reset_req      (hart_reset_req),
        .hart_reset_done     (1'b1),

        // Halt / run handshake — fake hart halts the instant DM
        // asks for it (hart_halted = hart_req_halt), runs otherwise.
        .hart_req_halt          (hart_req_halt),
        .hart_req_halt_on_reset (hart_req_halt_on_reset),
        .hart_req_resume        (hart_req_resume),
        .hart_halted            (hart_req_halt),
        .hart_running           (~hart_req_halt),

        // DATA0 register — DM stores via DMI writes, surfaces via
        // hart_data0_rdata. No hart-side writes (hart_data0_wen=0).
        .hart_data0_rdata     (hart_data0_rdata),
        .hart_data0_wdata     (32'h0),
        .hart_data0_wen       (1'b0),

        // Program-buffer instruction handshake — wired open + always
        // ready, since no hart actually executes.
        .hart_instr_data      (hart_instr_data),
        .hart_instr_data_vld  (hart_instr_data_vld),
        .hart_instr_data_rdy  (1'b1),
        .hart_instr_caught_exception (1'b0),
        .hart_instr_caught_ebreak    (1'b0),

        // System Bus Access disabled (HAVE_SBA=0) — ports unused.
        .sbus_addr            (),
        .sbus_write           (),
        .sbus_size            (),
        .sbus_vld             (),
        .sbus_rdy             (1'b1),
        .sbus_err             (1'b0),
        .sbus_wdata           (),
        .sbus_rdata           (32'h0)
    );

    // ---- Observable outputs ----
    // dmactive: reach inside the DM hierarchy. After PR #79's inout
    // observability fix this is no longer strictly necessary, but
    // keeping the explicit pin makes the test self-documenting and
    // independent of the trace-signals path.
    assign dmactive_obs = u_dm.dmactive;
    assign haltreq_obs  = hart_req_halt;
    assign data0_obs    = hart_data0_rdata;

endmodule

`default_nettype wire
