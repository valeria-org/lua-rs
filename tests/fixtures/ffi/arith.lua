local ffi = require("ffi")
local function p(...) print(...) end
local function e(f, ...) local ok, m = pcall(f, ...) p(ok, m) end
local a, b = 7LL, 3ULL
p(a+2, a-10, a*a, a/2, a%3, -a%3, a^3, -a, a+b, a-b, b-a, b*2, b/2, b%2, b^2)
p(-7LL/2, -7LL%2, 7LL/0, 7ULL/0, 7LL%0, (-9223372036854775807LL-1)/-1, (-9223372036854775807LL-1)%-1)
p(2LL^-1, 0LL^-1, 1LL^-5, (-1LL)^-3, (-1LL)^-4, 3ULL^40)
p(a == 7, a == 7LL, 7 == a, a ~= b, a < b, b < a, a <= 7, -1LL < 1ULL, -1LL < 1LL, 1 < a, a > 2.5)
p(ffi.new("int", 5) + 1, ffi.new("uint32_t", 5) * 2, ffi.new("double", 1.5) + 1, ffi.new("float", 2) + ffi.new("int8_t", -3))
p(1LL + 0.5, 2^53+1 == 2^53+1LL, 5LL == 5.0)
e(function() return a + "x" end)
e(function() return a .. "x" end)
e(function() return "x" .. a end)
e(function() return #a end)
e(function() return a < "x" end)
e(function() return a + {} end)
e(function() return a + nil end)
e(function() return ffi.new("struct {int x;}") + 1 end)
e(function() return ffi.new("struct {int x;}") < 1 end)
-- pointers
local arr = ffi.new("int[10]", 0,1,2,3,4,5,6,7,8,9)
local p0 = ffi.cast("int*", arr)
local p3 = p0 + 3
p(p3[0], (p3 - 1)[0], (2 + p0)[0], p3 - p0, p0 - p3, p3 > p0, p0 < p3, p0 <= p0, p3 == p0 + 3, p0 == arr, arr + 1 == p0 + 1)
p(p0 == nil, ffi.new("void*") == nil, ffi.cast("void*", 0) == nil, nil == ffi.cast("int*", 0))
e(function() return p0 + p3 end)
e(function() return p0 * 2 end)
e(function() return ffi.cast("char*", p0) - p0 end)
e(function() return ffi.cast("void*", p0) + 1 end)
p(arr[ffi.new("int", 4)], arr[2LL], p0[-0], p0[1.9])
local ptr = ffi.new("int[1][2]")
p(ffi.sizeof(ptr[0]), ffi.typeof(ptr[0]))
-- enum arith
ffi.cdef[[ typedef enum { E_A = 1, E_B = 10 } e_t; ]]
local ev = ffi.new("e_t", "E_B")
p(ev + 1, ev == "E_B", ev == "E_A", ev < 11)
e(function() return ev + "E_C" end)
e(function() return ev == "zz" end)
-- comparisons on different types
p(ffi.new("int", 1) == ffi.new("int", 1), ffi.new("char*") == ffi.new("int*"))
e(function() return ffi.new("int*") < ffi.new("char*") end)
p(1LL == {}, {} == 1LL, 1LL == "1", ffi.new("int[1]") == "x")
-- tostring forms
p(tostring(0LL), tostring(-9223372036854775807LL-1), tostring(18446744073709551615ULL), tostring(1i), tostring(-1.5i), tostring(ffi.new("complex float", 1, -0.0)))
p(ffi.typeof("int"), ffi.typeof("struct { int a; }"), ffi.typeof("int(*)(void)"), ffi.typeof("int &"), ffi.typeof("const char * const"))
p(tostring(ffi.cast("uint64_t", 3)), ffi.cast("int64_t*", 0), ffi.cast("enum { Z1 = -5 }", -5))
p(tonumber(5LL), tonumber(-3ULL), tonumber(ffi.new("int8_t", -3)), tonumber(ffi.new("int*")), tonumber(2i), tonumber(ffi.typeof("int")))
p(type(1LL), type(ffi.typeof("int")), getmetatable(1LL))
-- bitops on cdata via arith metamethods not bit lib; concat
local ok = pcall(function() return 1LL .. 2LL end) p(ok)
