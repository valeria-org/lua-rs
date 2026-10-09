
local a, b = ...
local r
r = a & b; r = a | b; r = a ~ b; r = a << b; r = a >> b; r = a ~>> b; r = ~a
r = a & 1; r = 1 & a; r = a | 0xff; r = a ~ 1; r = a << 2; r = 2 << a; r = a >> 3; r = a ~>> 4
r = 0xff & 0x0f
r = 0xf0 | 0x0f
r = 0xff ~ 0x0f
r = 1 << 4
r = 256 >> 4
r = -256 ~>> 4
r = -256 >> 4
r = ~0
r = ~0xff
r = ~~5
r = ~-1
r = 1 << 31
r = 1 << 32
r = 1 << 33
r = 0xffffffff & 1
r = 2^40 | 0
r = 1.5 | 0
r = 0.5 & 1
r = 1e100 & 1
r = (0/0) | 0
r = 1 & 2 | 4 ~ 8
r = 1 | 2 & 4
r = 1 << 2 + 3
r = a & b | a ~ b
r = a << 1 .. "x"
r = a == b & 1
r = -a & b
r = ~a + 1
r = ~(a + 1)
r = 1LL << 40
r = 0xffULL & 0x0f
r = ~0LL
r = 1LL | 2
r = 3 ~ 3
r = -1 >> 28
r = -1 ~>> 28
r = 1 << -1
r = 5 & -1
