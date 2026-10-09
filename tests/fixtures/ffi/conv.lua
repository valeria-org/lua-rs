local ffi = require("ffi")
local function p(...) print(...) end
local function e(f, ...) local ok, m = pcall(f, ...) p(ok, m) end
-- numeric conversions
for _, t in ipairs{"int8_t","uint8_t","int16_t","uint16_t","int32_t","uint32_t","int64_t","uint64_t","float","double","bool","char","long","unsigned long"} do
  for _, v in ipairs{0, 1, -1, 127.9, -128.5, 255, 300, 65535.5, 2^31, -2^31, 2^32+5, 2^53, -2^63, 2^64, 1e300, 0/0, 1/0} do
    local ok, x = pcall(ffi.new, t, v)
    print(t .. " " .. tostring(v) .. " -> " .. (ok and ((ffi.istype("int64_t", x) or ffi.istype("uint64_t", x)) and tostring(x) or tostring(tonumber(x))) or x))
  end
end
p(ffi.new("int", true), ffi.new("bool", 0), ffi.new("bool", 5), ffi.new("bool", ffi.new("int64_t", 0)))
p(tonumber(ffi.new("bool", 1)), tonumber(ffi.new("int", true)))
e(ffi.new, "int", "abc")
e(ffi.new, "int", {})
e(ffi.new, "int*", 1)
e(ffi.new, "double", nil)
e(ffi.new, "int", function() end)
p(ffi.cast("int", 3.9), ffi.cast("uint8_t", 257), ffi.cast("int64_t", -1), ffi.cast("uint64_t", -1))
p(ffi.cast("int*", 0x1234), ffi.cast("void*", 12345678), ffi.cast("intptr_t", ffi.cast("void*", 77)))
p(ffi.cast("double", 5LL), ffi.cast("float", 1/3), ffi.cast("int32_t", 0xffffffffULL))
e(ffi.cast, "struct {int x;}", 1)
e(ffi.cast, "int")
p(ffi.cast("const char*", "abc") ~= nil)
-- complex
local c = ffi.new("complex double", 1.5, -2)
p(c, c.re, c.im, tonumber(c), ffi.new("complex float", 3), ffi.new("complex", {5, 6}))
p(ffi.new("complex", 0/0, 1/0), ffi.new("complex", 1e100, -1e-100))
-- enums
ffi.cdef[[ typedef enum { RED, GREEN = 5, BLUE } color; ]]
local col = ffi.new("color", "BLUE")
p(col, tonumber(col), ffi.new("color", 5), col == 6, col == "BLUE")
e(ffi.new, "color", "PURPLE")
p(ffi.C.GREEN, ffi.cast("color", 1))
-- arrays and strings
local a = ffi.new("char[4]", "abcdefg")
p(ffi.string(a, 4), ffi.sizeof(a))
local b = ffi.new("uint8_t[?]", 3, {1,2,3})
p(b[0], b[1], b[2], ffi.sizeof(b))
local b2 = ffi.new("int[5]", 7)
p(b2[0], b2[4])
local b3 = ffi.new("int[5]", {[0]=1, 2, 3})
p(b3[0], b3[1], b3[2], b3[3])
e(ffi.new, "int[2]", {1,2,3})
local v = ffi.new("int[?]", 4, 1, 2)
p(v[0], v[1], v[2], v[3], ffi.sizeof(v))
-- structs
ffi.cdef[[
struct S { int a; double b; char c[4]; struct { int x, y; } inn; };
union U { int i; float f; uint8_t b[4]; };
struct BF { unsigned a:3; int b:5; bool c:1; unsigned d:23; };
struct VLS { int n; double d[?]; };
]]
local s = ffi.new("struct S", {a = 1, b = 2.5, c = "hi", inn = {3, 4}})
p(s.a, s.b, ffi.string(s.c), s.inn.x, s.inn.y)
local s2 = ffi.new("struct S", 9, 8, "x", {1, 2})
p(s2.a, s2.b, ffi.string(s2.c), s2.inn.x, s2.inn.y)
e(ffi.new, "struct S", 1, 2, "x", {1,2}, 5)
local u = ffi.new("union U", {f = 1})
p(u.i, u.f, u.b[3])
local bf = ffi.new("struct BF", {a = 9, b = -3, c = true, d = 12345})
p(bf.a, bf.b, bf.c, bf.d, ffi.sizeof(bf))
bf.b = 31 p(bf.b) bf.a = -1 p(bf.a) bf.c = false p(bf.c)
p(ffi.offsetof("struct BF", "b"))
p(ffi.offsetof("struct S", "c"), ffi.offsetof("struct S", "inn"), ffi.offsetof("struct S", "zz"))
local vls = ffi.new("struct VLS", 3)
p(ffi.sizeof(vls), ffi.sizeof("struct VLS", 5), vls.n, vls.d[2])
p(ffi.alignof("struct S"), ffi.alignof("char"), ffi.alignof("double"))
e(function() s.zz = 1 end)
e(function() return s[0] end)
e(function() return s.inn.zz end)
local cs = ffi.new("const struct S")
e(function() cs.a = 1 end)
p(ffi.sizeof("int[?]", 7), ffi.sizeof("struct S"), ffi.sizeof("void"), ffi.sizeof("int(*)(int)"))
