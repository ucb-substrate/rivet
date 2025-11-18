module fulladder (
    input  wire a,
    input  wire b,
    input  wire cin,
    output wire sum,
    output wire cout
);

  wire sum1, carry1, carry2;

  halfadder ha1 (
      .a(a),
      .b(b),
      .sum(sum1),
      .carry(carry1)
  );

  halfadder ha2 (
      .a(sum1),
      .b(cin),
      .sum(sum),
      .carry(carry2)
  );

  assign cout = carry1 | carry2;

endmodule
