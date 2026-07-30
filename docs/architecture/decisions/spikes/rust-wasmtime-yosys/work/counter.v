// Trivial synchronous RTL to exercise the aigpdk synthesis flow.
module counter (
    input  wire       clk,
    input  wire       rst,
    input  wire       en,
    output reg  [7:0] count
);
    always @(posedge clk) begin
        if (rst)
            count <= 8'd0;
        else if (en)
            count <= count + 8'd1;
    end
endmodule
