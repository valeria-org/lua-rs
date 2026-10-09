local co = coroutine.create(function(a, b)
  print("start", a, b)
  local c = coroutine.yield(a + b)
  print("got", c)
  local d, e = coroutine.yield(c * 2)
  print("got2", d, e)
  return "done", 99
end)
print(coroutine.status(co))
print(coroutine.resume(co, 1, 2))
print(coroutine.status(co))
print(coroutine.resume(co, 10))
print(coroutine.resume(co, "x", "y"))
print(coroutine.status(co))
print(coroutine.resume(co))
local w = coroutine.wrap(function(...) local n = select('#', ...) while true do n = coroutine.yield(n) end end)
print(w(1, 2, 3), w(), w(nil, nil))
-- yield across pcall
local co2 = coroutine.create(function()
  local ok, v = pcall(function() local x = coroutine.yield(1) error("err" .. x) end)
  print("pcall result", ok, v)
  local ok2, v2 = pcall(coroutine.yield, 2)
  print("pcall yield", ok2, v2)
  return 3
end)
print(coroutine.resume(co2))
print(coroutine.resume(co2, "A"))
print(coroutine.resume(co2, "B"))
print(coroutine.status(co2))
-- yield across metamethods
local mt = {__index = function(t, k) return coroutine.yield(k) end, __add = function(a, b) return coroutine.yield("add") end,
  __lt = function(a, b) return coroutine.yield("lt") end, __concat = function(a, b) return coroutine.yield("cat") end,
  __eq = function(a, b) return coroutine.yield("eq") end, __call = function(self, x) return coroutine.yield("call" .. x) end,
  __newindex = function(t, k, v) coroutine.yield("ni" .. k) end, __len = function() return coroutine.yield("len") end,
  __unm = function() return coroutine.yield("unm") end}
local co3 = coroutine.wrap(function()
  local t, u = setmetatable({}, mt), setmetatable({}, mt)
  local r = {}
  r[1] = t.foo
  r[2] = t + 1
  r[3] = t < u and "yes" or "no"
  r[4] = "a" .. t .. "b"
  r[5] = t == u
  r[6] = t(5)
  t.bar = 1
  r[7] = #t
  r[8] = -t
  for i = 1, 8 do print(i, r[i]) end
  return "end"
end)
local v = co3()
while v ~= "end" do print("yielded", v) v = co3(v == "lt" and true or v == "eq" and false or v .. "!") end
-- iterators that yield
local function gen() for i = 1, 3 do coroutine.yield(i) end end
for v in coroutine.wrap(gen) do print("gen", v) end
-- errors
local ce = coroutine.create(function() error("oops") end)
print(coroutine.resume(ce))
print(coroutine.status(ce), coroutine.resume(ce))
print(pcall(coroutine.resume))
print(pcall(coroutine.status, 1))
print(coroutine.isyieldable(), coroutine.running())
print(coroutine.wrap(function() return coroutine.isyieldable(), coroutine.running() ~= nil end)())
print(pcall(coroutine.yield, 1))
local cm
cm = coroutine.create(function() print(coroutine.status(cm)) local c2 = coroutine.create(function() print(coroutine.status(cm)) print(coroutine.resume(cm)) end) coroutine.resume(c2) end)
coroutine.resume(cm)
print(coroutine.status(cm))
print(select('#', coroutine.resume(coroutine.create(function() end))))
print(coroutine.resume(coroutine.create(function(...) return ... end), 1, nil, 3))
local cy = coroutine.wrap(coroutine.yield)
print(cy(1, 2), cy(3))
print(pcall(coroutine.wrap(function() local t = setmetatable({}, {__index = function() local co = coroutine.running() return coroutine.yield(1) end}) return t.x end)))
-- yield in xpcall handler (C boundary)
print(coroutine.resume(coroutine.create(function() return xpcall(function() error("x") end, function(m) return coroutine.yield(m) end) end)))
-- deep coroutine nesting
local depth = 0
local function nest(n) if n == 0 then return 0 end return coroutine.wrap(function() return 1 + nest(n - 1) end)() end
print(nest(100))
