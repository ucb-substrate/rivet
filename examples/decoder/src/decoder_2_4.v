module decoder(
  input [1:0] A,
  output [3:0] Z,
  input clk
);

	
// Only one output should ever be high.  For example,
// Z[2] = !A[1] & !A[2] & A[1] & !A[0], etc

// Hint: a 2 to 1 mux looks like:
// assign y = (sel) ? b : a;
// And you can replace "a" with a further condition
// assign y = (sel1) ? c
//          : (sel2) ? b
//          : a
// and sel1 can be a condition like:
// A == 2'd0

assign Z
  = (A[1:0] == 2'b0000) ? 4'0001
  : (A[1:0] == 2'b0001) ? 4'0010
  : (A[1:0] == 2'b0010) ? 4'0100
  : (A[1:0] == 2'b0011) ? 4'1000
  : 4'0000;
endmodule
