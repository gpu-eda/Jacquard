// Minimal dual-UART TX design for multi-UART cosim testing.
// Two independent shift-register UART transmitters, each sending
// a different known byte sequence on separate TX pins.
//
// UART 0 (tx0): sends 0x48 ('H'), 0x69 ('i')
// UART 1 (tx1): sends 0x4F ('O'), 0x4B ('K')
//
// Baud rate: clock_hz / CYCLES_PER_BIT.
// With 25 MHz clock and CYCLES_PER_BIT=217, baud ≈ 115207.

module uart_tx (
    input  wire clk,
    input  wire rst_n,
    input  wire start,      // pulse high for one cycle to begin
    input  wire [7:0] data,
    output reg  tx,
    output reg  done
);
    localparam CYCLES_PER_BIT = 217;
    localparam IDLE  = 0, START = 1, DATA = 2, STOP = 3;

    reg [1:0]  state;
    reg [15:0] counter;
    reg [2:0]  bit_idx;
    reg [7:0]  shift;

    always @(posedge clk or negedge rst_n) begin
        if (!rst_n) begin
            state   <= IDLE;
            tx      <= 1'b1;
            done    <= 1'b0;
            counter <= 0;
            bit_idx <= 0;
            shift   <= 0;
        end else begin
            done <= 1'b0;
            case (state)
                IDLE: begin
                    tx <= 1'b1;
                    if (start) begin
                        shift   <= data;
                        state   <= START;
                        counter <= 0;
                    end
                end
                START: begin
                    tx <= 1'b0;  // start bit
                    if (counter == CYCLES_PER_BIT - 1) begin
                        counter <= 0;
                        bit_idx <= 0;
                        state   <= DATA;
                    end else begin
                        counter <= counter + 1;
                    end
                end
                DATA: begin
                    tx <= shift[0];
                    if (counter == CYCLES_PER_BIT - 1) begin
                        counter <= 0;
                        shift   <= {1'b0, shift[7:1]};
                        if (bit_idx == 7) begin
                            state <= STOP;
                        end else begin
                            bit_idx <= bit_idx + 1;
                        end
                    end else begin
                        counter <= counter + 1;
                    end
                end
                STOP: begin
                    tx <= 1'b1;  // stop bit
                    if (counter == CYCLES_PER_BIT - 1) begin
                        done  <= 1'b1;
                        state <= IDLE;
                    end else begin
                        counter <= counter + 1;
                    end
                end
            endcase
        end
    end
endmodule

module dual_uart_top (
    input  wire clk,
    input  wire rst_n,
    output wire tx0,
    output wire tx1
);
    // Sequencer: sends two bytes per UART after reset
    localparam MSG_LEN = 2;
    reg [7:0] msg0 [0:MSG_LEN-1];
    reg [7:0] msg1 [0:MSG_LEN-1];

    initial begin
        msg0[0] = 8'h48;  // 'H'
        msg0[1] = 8'h69;  // 'i'
        msg1[0] = 8'h4F;  // 'O'
        msg1[1] = 8'h4B;  // 'K'
    end

    reg [1:0] seq_idx;
    reg       seq_start;
    wire      done0, done1;
    reg [7:0] data0, data1;
    reg       sending;

    // Wait a few cycles after reset before starting
    reg [7:0] startup_cnt;

    always @(posedge clk or negedge rst_n) begin
        if (!rst_n) begin
            seq_idx     <= 0;
            seq_start   <= 0;
            sending     <= 0;
            startup_cnt <= 0;
            data0       <= 0;
            data1       <= 0;
        end else begin
            seq_start <= 1'b0;
            if (startup_cnt < 20) begin
                startup_cnt <= startup_cnt + 1;
            end else if (!sending && seq_idx < MSG_LEN) begin
                data0     <= msg0[seq_idx];
                data1     <= msg1[seq_idx];
                seq_start <= 1'b1;
                sending   <= 1'b1;
            end else if (sending && done0 && done1) begin
                seq_idx <= seq_idx + 1;
                sending <= 1'b0;
            end
        end
    end

    uart_tx u0 (
        .clk(clk), .rst_n(rst_n),
        .start(seq_start), .data(data0),
        .tx(tx0), .done(done0)
    );

    uart_tx u1 (
        .clk(clk), .rst_n(rst_n),
        .start(seq_start), .data(data1),
        .tx(tx1), .done(done1)
    );
endmodule
