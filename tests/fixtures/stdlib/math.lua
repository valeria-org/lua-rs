-- math library: results, PRNG sequences and error messages.
local function e(...) print(pcall(...)) end
print(math.pi, math.huge, -math.huge, math.huge == 1/0)
print(math.abs(-3), math.abs(3), math.abs(-0.0), math.abs("-2"), math.abs(-1/0))
print(math.floor(3.7), math.floor(-3.7), math.floor(-0.5), math.floor("2.5"), math.floor(1/0), math.floor(2^53 + 0.5))
print(math.ceil(3.2), math.ceil(-3.2), math.ceil(-0.5), math.ceil(0))
print(math.sqrt(16), math.sqrt(2), math.sqrt(-1), math.sqrt(1/0))
print(math.sin(1), math.cos(1), math.tan(1), math.asin(0.5), math.acos(0.5), math.atan(1))
print(math.sinh(1), math.cosh(1), math.tanh(1), math.exp(1), math.exp(-1/0))
print(math.log(1), math.log(0), math.log(-1), math.log(8, 2), math.log(100, 10), math.log(27, 3), math.log(2, 0))
print(math.log10(1000), math.log10(2), math.atan2(1, 1), math.atan2(-1, -1), math.atan2(0, -0.0))
print(math.pow(2, 10), math.pow(2, 0.5), math.pow(-8, 1/3), math.pow(0, 0))
print(math.fmod(7, 3), math.fmod(-7, 3), math.fmod(7, -3), math.fmod(1, 0), math.fmod(5.5, 2))
print(math.modf(3.7), math.modf(-3.7), math.modf(5), math.modf(-0.0), math.modf(1/0), math.modf(-1/0))
print(math.frexp(8), math.frexp(0.3), math.frexp(0), math.frexp(-1), math.frexp(1e-310))
print(math.ldexp(1, 3), math.ldexp(0.5, -1), math.ldexp(1, 2.9), math.ldexp(1, -2.9), math.ldexp(3, 1024), math.ldexp(1, -1080), math.ldexp(0, 1e10), math.ldexp(1, 1/0), math.ldexp(1, -1/0))
print(math.min(1), math.min(3, 1, 2), math.min(-0.0, 0), math.max(3, 1, 2), math.max("10", 9))
print(math.min(0/0, 1), math.min(1, 0/0), math.max(0/0, 1), math.max(1, 0/0))
print(math.deg(math.pi), math.rad(180), math.deg(1), math.rad(1))
e(math.floor)
e(math.floor, "x")
e(math.sqrt, {})
e(math.max)
e(math.min, 1, "x")
e(math.max, 1, 2, nil)
e(math.log, 1, "x")
e(math.ldexp, 1)
e(math.fmod, 1)
e(math.atan2)
e(math.frexp, nil)
e(math.deg)
e(math.deg, "x")

-- PRNG: the startup state and seeded sequences are bit-identical to the reference.
local r = {}
for i = 1, 5 do r[#r + 1] = string.format("%.17g", math.random()) end
for i = 1, 5 do r[#r + 1] = math.random(100) end
for i = 1, 5 do r[#r + 1] = math.random(-10, 10) end
print(table.concat(r, " "))
math.randomseed(42)
local s1 = {}; for i = 1, 8 do s1[i] = math.random(1000) end
math.randomseed(42)
local s2 = {}; for i = 1, 8 do s2[i] = math.random(1000) end
print(table.concat(s1, " "), table.concat(s1, " ") == table.concat(s2, " "))
math.randomseed(1.5); print(string.format("%.17g", math.random()), math.random(3.5), math.random(2, 2), math.random(0), math.random(5, 1))
math.randomseed("7"); print(math.random(1e9))
e(math.random, "x")
e(math.random, 1, {})
e(math.randomseed, {})
print(pcall(math.randomseed))
print(tostring(math.random):match("builtin#%d+"), tostring(math.deg):match("function"))

-- error messages naming the called function
print(pcall(function() math.floor() end))
print(pcall(function() math.max(1, "y") end))
print(pcall(function() math.deg({}) end))
print(pcall(function() local m = math; m.random(1, {}) end))
