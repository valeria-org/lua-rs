-- FFI callbacks, cdata finalizers, metatypes and string.buffer (references, serialization) under GC stress.
local ffi = require("ffi")
local buffer = require("string.buffer")
local function junk(n) local t = {} for i = 1, n or 5 do t[i] = {i, tostring(i) .. "j"} end return t end
ffi.cdef[[
typedef int (*cmp_t)(const void *, const void *);
void qsort(void *base, size_t n, size_t sz, cmp_t cmp);
size_t strlen(const char *);
int snprintf(char *buf, size_t n, const char *fmt, ...);
typedef struct { int x; double y; const char *name; } pt_t;
void *malloc(size_t); void free(void *);
]]
-- callbacks called from C that allocate on every call
local arr = ffi.new("int[64]")
for i = 0, 63 do arr[i] = (i * 29) % 64 end
local calls = 0
local cb = ffi.cast("cmp_t", function(a, b)
  calls = calls + 1
  junk(2)
  local x, y = ffi.cast("const int *", a)[0], ffi.cast("const int *", b)[0]
  local s = tostring(x) .. "<" .. tostring(y)
  return x < y and -1 or (x > y and 1 or 0)
end)
ffi.C.qsort(arr, 64, 4, cb)
local o = {} for i = 0, 9 do o[#o+1] = arr[i] end print(table.concat(o, ","), calls > 0)
cb:set(function(a, b) junk(1) return ffi.cast("const int *", b)[0] - ffi.cast("const int *", a)[0] end)
ffi.C.qsort(arr, 64, 4, cb)
print(arr[0], arr[63])
cb:free()
-- callback erroring inside C
local cb2 = ffi.cast("cmp_t", function() junk(1) error("cb" .. "error") end)
print(pcall(ffi.C.qsort, arr, 2, 4, cb2))
cb2:free()
-- cdata finalizers: ffi.gc, resurrect-free ordering, replaced/removed finalizers
local flog = {}
do
  local hold = {}
  for i = 1, 8 do
    local p = ffi.gc(ffi.C.malloc(16), function(q) junk(2) flog[#flog+1] = "free" .. i ffi.C.free(q) end)
    hold[i] = p
  end
  ffi.gc(hold[2], nil) ffi.C.free(hold[2])
  ffi.gc(hold[3], function(q) flog[#flog+1] = "replaced" ffi.C.free(q) end)
end
collectgarbage() collectgarbage()
table.sort(flog) print(table.concat(flog, " "))
-- ffi.new with __gc metatype; structs holding pointers to Lua strings (anchored separately)
local mtlog = {}
local P
P = ffi.metatype("pt_t", {
  __index = function(p, k) junk(1) return k .. "@" .. p.x end,
  __tostring = function(p) return "pt(" .. p.x .. "," .. p.y .. ")" end,
  __gc = function(p) mtlog[#mtlog+1] = p.x end,
  __add = function(a, b) return P(a.x + b.x, a.y + b.y) end,
  __eq = function(a, b) return a.x == b.x end,
  __len = function(p) return p.x end,
  __concat = function(a, b) return tostring(a) .. tostring(b) end,
})
do
  local pts = {}
  for i = 1, 10 do pts[i] = P(i, i / 2) end
  local s = pts[1] + pts[2] + pts[3]
  print(tostring(s), s.foo, #s, s == P(6, 0), pts[4] .. pts[5])
end
collectgarbage() collectgarbage()
table.sort(mtlog) print(#mtlog, mtlog[1], mtlog[#mtlog])
-- ffi.string / ffi.copy / ffi.fill on fresh buffers
local strs = {}
for i = 1, 50 do
  local b = ffi.new("char[?]", 32)
  ffi.C.snprintf(b, 32, "n=%d s=%s", ffi.new("int", i), "x" .. i)
  strs[i] = ffi.string(b)
end
print(strs[1], strs[50], ffi.string("abcdef", 3))
-- 64-bit cdata arithmetic creating boxed results
local acc = 0LL for i = 1, 200 do acc = acc + i * 3LL - 1ULL end print(acc, tostring(acc / 7))
-- ctypes as keys of weak tables
local wct = setmetatable({}, {__mode = "k"})
for i = 1, 10 do wct[ffi.new("int[1]", i)] = i end collectgarbage()
local n = 0 for _ in pairs(wct) do n = n + 1 end print("weak cdata", n)
-- string.buffer: put objects with __tostring, set() references, decode/encode with dict/metatable
local tsmt = {__tostring = function(t) junk(2) return "<" .. t.v .. ">" end}
local buf = buffer.new()
for i = 1, 30 do buf:put(setmetatable({v = i}, tsmt), " ") end
print(#buf, buf:get(20))
local src = "reference" .. "-" .. "target" .. ("x"):rep(10)
buf:set(src) src = nil junk(30) collectgarbage()
print(buf:tostring())
local r = buffer.new(64)
for i = 1, 20 do r:putf("%d:%s;", i, "v" .. i) end
print(r:get(30), #r)
local ptr, len = r:ref() print(len, ffi.string(ptr, 5))
local enc = buffer.new({dict = {"alpha", "beta", "gamma"}, metatable = {tsmt}})
local val = {alpha = 1, beta = {2, 3, "four"}, gamma = setmetatable({v = "mt"}, tsmt), [10] = "ten"}
enc:encode(val)
junk(30) collectgarbage()
local dec = enc:decode()
print(dec.alpha, dec.beta[3], tostring(dec.gamma), dec[10])
local ser = buffer.encode({1, 2, {3, {4, {5, "deep" .. "s"}}}, x = 1/0, y = -0.0, z = 2^53, w = 12345678901234LL})
for i = 1, 5 do junk(10) end collectgarbage()
local d2 = buffer.decode(ser)
print(d2[3][2][2][2], d2.x, d2.z, d2.w, #ser)
print(pcall(buffer.decode, ser:sub(1, 10)))
-- buffer methods that call into Lua: put with __tostring erroring
print(pcall(buf.put, buf, setmetatable({}, {__tostring = function() error("ts" .. "err") end})))
local rb = buffer.new() rb:put("abc") print(rb:skip(1):get())
print(pcall(function() local b = buffer.new() b:free() return b:put("x"):tostring() end))
