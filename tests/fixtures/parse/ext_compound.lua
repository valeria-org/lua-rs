
local a, b = 1, 2
a += 1
a -= 2
a *= 3
a /= 4
a %= 5
a ..= "x"
a &= 0xff
a |= 0x100
a ~= 7
a <<= 1
a >>= 2
a ~>>= 3
a += b
a += b * 2
a ..= b .. "y"
g += 1
g ..= "s"
g ~= 1
local t = {x = 1, 1}
t.x += 1
t[1] += 2
t[a] += 3
t["k"] -= 4
t.x.y *= 5
t[f()] += 1
t.x ..= "z"
t[a] ..= "z"
local u = 0
local function inc() u += 1 return u end
local function cat(s) u ..= s end
for i = 1, 10 do a += i end
a += a
t.x += t.x
t[1.5] += 1
t[true] += 1
f().x += 1
