// SPDX-License-Identifier: Apache-2.0
//
// Minimal self-contained APB3 system for exercising Jacquard's
// config-driven bus-transaction tracer (ADR 0013).
//
// An APB master FSM issues a fixed program of four transfers to an
// always-ready APB slave (a 2-word register file), then idles:
//
//   0: WRITE 0x00 <= 0xCAFEBABE
//   1: WRITE 0x04 <= 0x12345678
//   2: READ  0x00  (-> 0xCAFEBABE)
//   3: READ  0x04  (-> 0x12345678)
//
// All APB master outputs and the slave's PRDATA are registered so the
// net names (psel, penable, pwrite, paddr, pwdata, prdata) survive
// synthesis as DFF Q outputs — that is what `resolve_to_state_pos`
// binds the GPU capture to. PREADY is tied high and PSLVERR low; both
// fold to constants in synthesis and the tracer treats unresolved
// PREADY as 1 (always-ready) and unresolved PSLVERR as 0.
//
// The whole program completes within ~30 clock cycles, so a short
// `--max-clock-edges` budget suffices.

module apb_trace (
    input  wire clk,
    input  wire rst_n,
    // Single observable tap: a reduction over every traced APB signal.
    // Its only purpose is to anchor the datapath so synthesis can't
    // prune the (otherwise output-less) design. Cosim never reads it.
    output wire trace_tap
);
    localparam [1:0] IDLE   = 2'd0,
                     SETUP  = 2'd1,
                     ACCESS = 2'd2,
                     DONE   = 2'd3;

    localparam NSTEP = 3'd4;

    reg [1:0]  state;
    reg [2:0]  step;

    // ── APB master outputs (registered) ──────────────────────────────
    // `(* keep *)` preserves these net names through synthesis so the
    // bus tracer can resolve them by name (psel, paddr[i], …).
    (* keep *) reg        psel;
    (* keep *) reg        penable;
    (* keep *) reg        pwrite;
    (* keep *) reg [7:0]  paddr;
    (* keep *) reg [31:0] pwdata;

    // ── APB slave outputs ─────────────────────────────────────────────
    (* keep *) reg [31:0] prdata;  // registered read data
    wire        pready  = 1'b1;    // always ready (no wait states)
    wire        pslverr = 1'b0;    // never errors

    // Transaction program selected by `step`.
    reg        nxt_pwrite;
    reg [7:0]  nxt_paddr;
    reg [31:0] nxt_pwdata;
    always @(*) begin
        case (step)
            3'd0: begin nxt_pwrite = 1'b1; nxt_paddr = 8'h00; nxt_pwdata = 32'hCAFEBABE; end
            3'd1: begin nxt_pwrite = 1'b1; nxt_paddr = 8'h04; nxt_pwdata = 32'h12345678; end
            3'd2: begin nxt_pwrite = 1'b0; nxt_paddr = 8'h00; nxt_pwdata = 32'h0;        end
            3'd3: begin nxt_pwrite = 1'b0; nxt_paddr = 8'h04; nxt_pwdata = 32'h0;        end
            default: begin nxt_pwrite = 1'b0; nxt_paddr = 8'h0; nxt_pwdata = 32'h0;       end
        endcase
    end

    // ── APB master FSM ────────────────────────────────────────────────
    always @(posedge clk or negedge rst_n) begin
        if (!rst_n) begin
            state   <= IDLE;
            step    <= 3'd0;
            psel    <= 1'b0;
            penable <= 1'b0;
            pwrite  <= 1'b0;
            paddr   <= 8'h0;
            pwdata  <= 32'h0;
        end else begin
            case (state)
                IDLE: begin
                    if (step < NSTEP) begin
                        // Drive SETUP phase next cycle.
                        state   <= SETUP;
                        psel    <= 1'b1;
                        penable <= 1'b0;
                        pwrite  <= nxt_pwrite;
                        paddr   <= nxt_paddr;
                        pwdata  <= nxt_pwdata;
                    end else begin
                        state   <= DONE;
                        psel    <= 1'b0;
                        penable <= 1'b0;
                    end
                end
                SETUP: begin
                    // Enter ACCESS phase.
                    state   <= ACCESS;
                    psel    <= 1'b1;
                    penable <= 1'b1;
                end
                ACCESS: begin
                    // PREADY is always high → transfer completes now.
                    state   <= IDLE;
                    psel    <= 1'b0;
                    penable <= 1'b0;
                    step    <= step + 3'd1;
                end
                default: begin // DONE
                    psel    <= 1'b0;
                    penable <= 1'b0;
                end
            endcase
        end
    end

    // ── APB slave: 2-word register file, registered read data ─────────
    reg [31:0] mem0;
    reg [31:0] mem1;

    // Anchor every traced signal so synthesis retains the datapath.
    assign trace_tap = psel ^ penable ^ pwrite ^ (^paddr) ^ (^pwdata) ^ (^prdata);

    wire access_complete = psel & penable & pready;

    always @(posedge clk or negedge rst_n) begin
        if (!rst_n) begin
            mem0   <= 32'h0;
            mem1   <= 32'h0;
            prdata <= 32'h0;
        end else begin
            if (access_complete & pwrite) begin
                case (paddr[3:2])
                    2'd0: mem0 <= pwdata;
                    2'd1: mem1 <= pwdata;
                    default: ;
                endcase
            end
            // Registered read: PRDATA reflects the addressed word and is
            // valid during the ACCESS phase (PADDR is stable from SETUP).
            prdata <= (paddr[3:2] == 2'd0) ? mem0 : mem1;
        end
    end
endmodule
