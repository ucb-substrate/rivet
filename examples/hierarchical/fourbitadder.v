module fourbitadder (
    input wire [3:0] a,
    input wire [3:0] b,
    input wire cin,
    output wire [3:0] sum,
    output wire cout
);

  wire [3:1] carry;


  fulladder fa_1 (
      .a(a[0]),
      .b(b[0]),
      .cin(cin),
      .sum(sum[0]),
      .cout(carry[1])
  );

  fulladder fa_2 (
      .a(a[1]),
      .b(b[1]),
      .cin(carry[1]),
      .sum(sum[1]),
      .cout(carry[2])
  );

  fulladder fa_3 (
      .a(a[2]),
      .b(b[2]),
      .cin(carry[2]),
      .sum(sum[2]),
      .cout(carry[3])
  );

  fulladder fa_4 (
      .a(a[3]),
      .b(b[3]),
      .cin(carry[3]),
      .sum(sum[3]),
      .cout(cout)
  );

endmodule
