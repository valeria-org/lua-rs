
local a, b = ...
local r
r = a + b; r = a - b; r = a * b; r = a / b; r = a % b; r = a ^ b
r = a + 1; r = a - 1; r = a * 2; r = a / 2; r = a % 2; r = a ^ 2
r = 1 + a; r = 1 - a; r = 2 * a; r = 2 / a; r = 2 % a; r = 2 ^ a
r = a + 1.5; r = 1.5 * a; r = a - -1
r = -a; r = - -a; r = #a; r = not a; r = not not a
r = a + b * 2 - (a / b) ^ 2 % 3
r = 2 ^ 3 ^ 2
r = -a ^ 2
r = -2 ^ 2
r = (a + b) * (a - b)
r = a + (b + (a + (b + 1)))
r = a.x + b.y
r = a[1] * b[2]
r = f() + g()
r = a + "10"
r = "10" + 1
r = a .. b
r = a + b + 1 + 2
r = 1 + 2 + a
r = a * 1e300
r = a + 65536
local g1 = gx + gy
local u = 1
local function cl() return u + a end
