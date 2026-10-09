-- FFI conversions, arithmetic and error texts.
local ffi = require("ffi")
local function show(...) local t = {n = select('#', ...), ...}; for i = 1, t.n do t[i] = tostring(t[i]) end print(table.concat(t, " ", 1, t.n)) end
local function e(f, ...) show(pcall(f, ...)) end

ffi.cdef[[
typedef struct { int x, y; } point_t;
typedef struct { uint8_t a; double d; char s[4]; int bits:3; unsigned ubits:5; } mixed_t;
typedef union { uint32_t u; float f; uint8_t b[4]; } pun_t;
typedef enum { RED, GREEN = 5, BLUE } color_t;
typedef struct { int n; int data[?]; } vla_t;
typedef int (*cb_t)(int, int);
struct incomplete;
int strcmp(const char *, const char *);
size_t strlen(const char *);
int abs(int);
double floor(double);
char *strerror(int);
int snprintf(char *, size_t, const char *, ...);
]]

-- integer conversions
local function N(t, v) local ok, r = pcall(ffi.new, t, v) if not ok then return r end local x = tonumber(r) return x == nil and tostring(r) or x end
for _, t in ipairs{"int8_t", "uint8_t", "int16_t", "uint16_t", "int32_t", "uint32_t", "int64_t", "uint64_t", "bool", "char", "float", "double", "long", "size_t"} do
  local r = {}
  for i, v in ipairs{0, 1, -1, 127, 128, 255, 256, 70000, 2^31, -2^31, 2^32, 2^53, 2^63, -2^63, 2^64, 3.9, -3.9, 0/0, 1/0, -1/0, 1/3, 1e40, true, false, "x"} do
    r[#r + 1] = tostring(N(t, v))
  end
  print(t, table.concat(r, " "))
end
print(N("bool", nil), N("int", nil), N("double", nil))
show(tonumber(ffi.new("int64_t", 2^53 + 1)), tonumber(ffi.new("uint64_t", 2^64 - 1)), tonumber(ffi.new("float", 0.1)))
show(1LL, -1LL, 1ULL, 0x7fffffffffffffffLL, 0xffffffffffffffffULL, 18446744073709551615ULL, 12i, 1.5i)
show(type(1LL), ffi.typeof(1LL), ffi.typeof(1ULL), ffi.typeof(1i))
-- 64 bit arithmetic
show(1LL + 2, 2^62 * 4LL, 5LL / 2LL, -5LL / 2LL, 5LL % -3LL, -5LL % 3LL, 2LL ^ 10, -(1LL), 7ULL - 8)
show(1LL == 1, 1LL < 2, 1ULL < -1LL, -1LL < 0LL, 0ULL - 1 > 0)
e(function() return 1LL / 0LL end)
e(function() return 1LL % 0LL end)
e(function() return 1ULL / 0ULL end)
show(tostring(1LL):len(), pcall(function() return 1LL .. "" end))
show(bit.tohex(-1LL), bit.band(0xffffffffffLL, 0xff00ff00ff), bit.lshift(1LL, 40), bit.tobit(2^40 + 5LL))
show(pcall(math.floor, 5LL))
-- complex
local c = ffi.new("complex", 1, 2)
show(c, c.re, c.im, ffi.new("complex float", 3), ffi.new("complex double", 0, -1))
show(pcall(function() return c + 1 end))
-- strings and pointers
local buf = ffi.new("char[16]", "hello")
show(ffi.string(buf), ffi.string(buf, 3), #ffi.string(buf, 8), ffi.sizeof(buf))
show(ffi.C.strlen("abc"), ffi.C.strcmp("a", "b") < 0, ffi.C.abs(-5), ffi.C.floor(2.7))
show(ffi.string(ffi.C.strerror(2)))
local out = ffi.new("char[64]")
show(ffi.C.snprintf(out, 64, "%d-%s-%.2f", ffi.new("int", 42), "str", 1.5), ffi.string(out))
local a3 = ffi.new("int[3]", 1, 2, 3) local p = ffi.cast("int *", a3) -- a3 keeps p's memory alive
show(p[0], p[1], p[2])
show(ffi.cast("uintptr_t", 0), ffi.cast("void *", 0) == nil, ffi.new("void *") == nil, ffi.cast("int", 3.7))
show(ffi.cast("int8_t", 300), ffi.cast("uint16_t", -1), ffi.cast("double", 5LL), ffi.cast("int64_t", "x" == "x" and 1 or 0))
-- structs, unions, arrays, enums
local pt = ffi.new("point_t", 1, 2)
show(pt.x, pt.y, ffi.sizeof(pt), ffi.alignof("mixed_t"), ffi.sizeof("mixed_t"), ffi.offsetof("mixed_t", "d"), ffi.offsetof("mixed_t", "s"))
show(ffi.offsetof("mixed_t", "bits"), ffi.offsetof("mixed_t", "ubits"))
local m = ffi.new("mixed_t", {a = 300, d = 1.5, s = "abc", bits = 7, ubits = 33})
show(m.a, m.d, ffi.string(m.s), m.bits, m.ubits)
local u = ffi.new("pun_t")
u.f = 1.0
show(u.u, u.b[3], bit.tohex(u.u))
show(ffi.new("color_t", "GREEN"), ffi.new("color_t", 6), ffi.cast("color_t", 1), pcall(ffi.new, "color_t", "PURPLE"))
local v = ffi.new("vla_t", 4)
v.n = 4
show(ffi.sizeof(v), ffi.sizeof("vla_t", 10), v.data[3])
local arr = ffi.new("int[2][3]", {{1, 2, 3}, {4, 5, 6}})
show(arr[1][2], ffi.sizeof(arr), #tostring(arr) > 0)
show(ffi.new("int[?]", 5, 9)[4], ffi.new("int[4]", 1)[3], ffi.new("int[4]", 1, 2)[3])
-- typeof, istype, metatypes
local P = ffi.typeof("point_t")
show(ffi.istype(P, pt), ffi.istype("int", 1), ffi.istype("point_t *", ffi.new("point_t *")), tostring(P))
local V = ffi.metatype(ffi.typeof("struct { double x, y; }"), {
  __add = function(a, b) return a.x + b.x end,
  __tostring = function(s) return "V(" .. s.x .. "," .. s.y .. ")" end,
  __index = {len = function(s) return math.sqrt(s.x * s.x + s.y * s.y) end},
  __len = function() return 2 end,
  __eq = function(a, b) return a.x == b.x end,
  __call = function(s, k) return k * s.x end,
  __concat = function(a, b) return "cat" end,
})
local v1, v2 = V(3, 4), V(3, 5)
show(v1 + v2, tostring(v1), v1:len(), #v1, v1 == v2, v1(10), v1 .. "x")
show(pcall(ffi.metatype, V, {}))
-- callbacks
local cb = ffi.cast("cb_t", function(a, b) return a * b end)
show(cb(6, 7), ffi.typeof(cb))
cb:set(function(a, b) return a - b end)
show(cb(6, 7))
cb:free()
e(function() return cb(1, 2) end)
local arr2 = ffi.new("int[5]", 5, 3, 1, 4, 2)
ffi.cdef("void qsort(void *, size_t, size_t, int (*)(const void *, const void *));")
local cmp = ffi.cast("int (*)(const void *, const void *)", function(a, b)
  return ffi.cast("const int *", a)[0] - ffi.cast("const int *", b)[0]
end)
ffi.C.qsort(arr2, 5, 4, cmp)
cmp:free()
show(arr2[0], arr2[1], arr2[2], arr2[3], arr2[4])
-- gc / finalizers
local freed = 0
do local x = ffi.gc(ffi.new("int[1]"), function() freed = freed + 1 end) end
collectgarbage() collectgarbage()
show(freed)
-- errors
e(ffi.new, "struct incomplete")
e(ffi.new, "nosuchtype")
e(ffi.cdef, "int x(")
e(ffi.cdef, "typedef int x; typedef float x;")
e(ffi.typeof, "int[")
e(ffi.new, "int[2]", 1, 2, 3)
e(ffi.new, "point_t", 1, 2, 3)
e(function() return pt.z end)
e(function() pt.z = 1 end)
e(function() return pt + 1 end)
e(function() return pt < pt end)
e(function() return #pt end)
e(function() return pt() end)
e(function() return ffi.new("int", "x") end)
e(function() return ffi.new("int *", 1) end)
e(function() local q = ffi.new("int[2]"); q[0] = "str" end)
e(function() return ffi.cast("point_t", 1) end)
e(function() ffi.C.nonexistent_function() end)
e(function() return ffi.C.strlen(1) end)
e(function() return ffi.C.strlen() end)
e(function() return ffi.C.abs("x") end)
e(function() return ffi.sizeof("int[?]") end)
e(function() return ffi.new("int[?]") end)
e(function() return ffi.new("int[?]", -1) end)
e(function() return ffi.load("nonexistent_library_xyz") end)
e(function() local s = ffi.new("const int[1]"); s[0] = 1 end)
e(function() return 1LL + {} end)
e(function() return ffi.new("uint64_t", 1) .. {} end)
e(function() return tostring(ffi.new("int[1]")):match("^cdata<int %[1%]>: 0x") ~= nil end)
show(ffi.abi("64bit"), ffi.abi("le"), ffi.abi("win"), ffi.os, ffi.arch)
show(ffi.errno(5), ffi.errno())
show(ffi.typeof("int(*)(int)"), ffi.typeof("struct { int a[2]; } *"), ffi.typeof("unsigned long long"), ffi.typeof("const char * volatile"))
show(ffi.new("int[3]") == ffi.new("int[3]"), ffi.cast("void*", 1) == ffi.cast("char*", 1))
show(ffi.typeof(ffi.new("point_t")) == P, tostring(ffi.typeof("int")), ffi.sizeof("long double"), ffi.sizeof("wchar_t"))
