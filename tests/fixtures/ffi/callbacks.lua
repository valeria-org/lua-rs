local ffi = require("ffi")
-- yield across callback
local cb = ffi.cast("int (*)(int)", function(x) coroutine.yield(x) return 1 end)
local co = coroutine.create(function() return cb(5) end)
print(coroutine.resume(co))
cb:free()
-- callback error in coroutine
local cb2 = ffi.cast("int (*)(int)", function(x) error({code = x}) end)
local ok, e = pcall(cb2, 9) print(ok, type(e), e.code)
-- nested callbacks
ffi.cdef[[ void qsort(void *, size_t, size_t, int (*)(const void *, const void *)); ]]
local outer = ffi.new("int[3]", 3, 1, 2)
local inner = ffi.new("int[3]", 9, 7, 8)
ffi.C.qsort(outer, 3, 4, function(a, b)
  ffi.C.qsort(inner, 3, 4, function(c, d) return ffi.cast("int*", c)[0] - ffi.cast("int*", d)[0] end)
  return ffi.cast("int*", a)[0] - ffi.cast("int*", b)[0]
end)
print(outer[0], outer[1], outer[2], inner[0], inner[1], inner[2])
-- many callbacks / slot exhaustion
local cbs = {}
local ok, err = pcall(function() for i = 1, 1000 do cbs[i] = ffi.cast("int (*)(void)", function() return i end) end end)
print(ok, err, #cbs)
for i = 1, #cbs do cbs[i]:free() end
local again = ffi.cast("int (*)(void)", function() return 77 end) print(again())
-- GC stress with cdata
local t = {}
for i = 1, 20000 do t[i % 100 + 1] = ffi.new("struct { int a[10]; }", {{i}}) end
collectgarbage()
print(t[1].a[0], t[100].a[0])
