local ffi = require("ffi")
local function p(...) print(...) end
local function e(f, ...) local ok, m = pcall(f, ...) p(ok, m) end
ffi.cdef[[
typedef struct { double x, y; } vec2;
typedef struct { int n; } counted;
struct node { int v; struct node *next; };
]]
local vec2
local mt = {
  __add = function(a, b) return vec2(a.x + b.x, a.y + b.y) end,
  __sub = function(a, b) return vec2(a.x - b.x, a.y - b.y) end,
  __mul = function(a, b) if type(b) == "number" then return vec2(a.x*b, a.y*b) end return a.x*b.x + a.y*b.y end,
  __eq = function(a, b) return a.x == b.x and a.y == b.y end,
  __lt = function(a, b) return a.x < b.x end,
  __le = function(a, b) return a.x <= b.x end,
  __len = function(a) return a.x*a.x + a.y*a.y end,
  __unm = function(a) return vec2(-a.x, -a.y) end,
  __concat = function(a, b) return "cat" end,
  __tostring = function(a) return "vec2(" .. a.x .. ", " .. a.y .. ")" end,
  __call = function(self, k) return self.x * k end,
  __index = { norm2 = function(a) return a.x*a.x + a.y*a.y end, K = 42 },
  __newindex = function(t, k, v) rawset(_G, "lastset", k .. "=" .. tostring(v)) end,
  __pairs = function(a) return function(_, k) if not k then return "x", a.x end end, a, nil end,
  __div = function() return "div" end, __mod = function() return "mod" end, __pow = function() return "pow" end,
}
vec2 = ffi.metatype("vec2", mt)
local a, b = vec2(1, 2), vec2(3, 4)
p(tostring(a + b), tostring(a - b), a * b, tostring(a * 2), a == b, a == vec2(1, 2), a < b, b <= a, #b, tostring(-a), a .. b, a(10))
p(a:norm2(), a.K, a / b, a % b, a ^ b)
a.foo = 5 p(lastset)
for k, v in pairs(a) do p("pair", k, v) end
e(ffi.metatype, "vec2", {})
e(ffi.metatype, "int", {})
e(ffi.metatype, "vec2")
e(function() return a.nope end)
p(tostring(ffi.new("vec2*", a)) ~= nil, ffi.cast("vec2*", a).x)
local pa = ffi.new("vec2[1]", {{5, 6}}) local pp = pa + 0
p(tostring(pp[0]), pp.x, pp:norm2())
-- __new and __gc
local cnt = 0
local C2 = ffi.metatype("counted", {
  __new = function(ct, n) p("new", ct, n) return ffi.new(ct, n * 2) end,
  __gc = function(o) cnt = cnt + o.n end,
  __index = function(o, k) return "idx:" .. k end,
})
local c = C2(5) p(c.n, c.zz)
c = nil collectgarbage() collectgarbage() p("gc cnt", cnt)
do local x = ffi.new("counted", 7) end collectgarbage() p("gc cnt2", cnt)
-- ffi.gc
local fin = {}
for i = 1, 3 do ffi.gc(ffi.new("struct node", i), function(o) fin[#fin+1] = o.v end) end
collectgarbage() collectgarbage()
p("fins", #fin)
local keep = ffi.gc(ffi.new("struct node", 9), function() p("never") end)
ffi.gc(keep, nil) keep = nil collectgarbage()
e(ffi.gc, 1, print)
e(ffi.gc, ffi.new("int", 1), print)
-- linked list through pointers
local n1, n2 = ffi.new("struct node", 1), ffi.new("struct node", 2)
n1.next = n2 n2.v = 20
p(n1.next.v, n1.next.next == nil)
-- typeof params, istype
local T = ffi.typeof("struct { $ a; $ b[$]; }", ffi.typeof("int"), ffi.typeof("double"), 3)
local t = T() p(ffi.sizeof(t), ffi.offsetof(T, "b"))
p(ffi.istype("int", 1), ffi.istype("int", ffi.new("int")), ffi.istype("vec2", a), ffi.istype("vec2*", a), ffi.istype("const vec2", a), ffi.istype(ffi.typeof(a), a))
p(ffi.istype("int *", ffi.new("const int *")), ffi.istype("long", 1LL), ffi.istype("int64_t", 1ULL))
e(ffi.typeof, "int", 1)
e(ffi.typeof, ffi.typeof("int"), 1)
e(ffi.typeof, "struct { $ x; }")
e(ffi.typeof, "struct { $ x; }", {})
e(ffi.cdef, "int x(")
e(ffi.cdef, "struct foo { int a; }; struct foo { int b; };")
e(ffi.new, "int[?]")
e(ffi.new, "struct { int a[?]; }")
e(ffi.sizeof)
e(ffi.new, "void")
e(ffi.new, nil)
p(ffi.sizeof("char[?]", 10), ffi.sizeof(ffi.new("char[?]", 13)), ffi.alignof("long double"))
-- ctype constants via struct constructor
ffi.cdef[[ struct K { static const int MAX = 10; enum { A1 = 3 }; int v; }; ]]
local K = ffi.typeof("struct K")
p(K.MAX, ffi.new(K).MAX, ffi.new("struct K*", ffi.new(K)).MAX)
e(function() local k = ffi.new(K) k.MAX = 1 end)
-- copy/fill/string
local buf = ffi.new("char[16]")
ffi.copy(buf, "hello")
p(ffi.string(buf), ffi.string(buf, 3))
ffi.fill(buf, 4, 65) p(ffi.string(buf))
ffi.fill(buf, 16) p(ffi.string(buf, 2) == "\0\0")
ffi.copy(buf, "abcdef", 3) p(ffi.string(buf))
e(ffi.copy, buf)
e(ffi.string)
p(ffi.string("lua string", 3))
-- ffi.load error
e(ffi.load, "nonexistent_lib_xyz")
ffi.cdef"size_t strlen(const char *);"
local libc = ffi.load("c") p(type(libc), libc.strlen("12345"))
p(ffi.typeinfo and type(ffi.typeinfo(1)), ffi.typeinfo(100000))
