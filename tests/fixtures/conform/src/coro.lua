-- Coroutines interacting with pcall, error handlers and metamethods.
local function show(...) local t = {n = select('#', ...), ...}; for i = 1, t.n do t[i] = tostring(t[i]) end print(table.concat(t, " ", 1, t.n)) end

-- yield across pcall / xpcall
local co = coroutine.create(function(a)
  local ok, v = pcall(function() local x = coroutine.yield(a + 1); error("err" .. x) end)
  show("pcall result", ok, v)
  local ok2, v2 = xpcall(function() return coroutine.yield("in xpcall") end, function(m) return "H" .. m end)
  show("xpcall result", ok2, v2)
  return "done"
end)
show(coroutine.resume(co, 1))
show(coroutine.resume(co, "X"))
show(coroutine.resume(co, "Y"))
show(coroutine.resume(co))
show(coroutine.status(co))

-- yield inside metamethods
local mt = {}
mt.__index = function(t, k) return coroutine.yield("index " .. k) end
mt.__newindex = function(t, k, v) coroutine.yield("newindex " .. k) rawset(t, k, v) end
mt.__add = function(a, b) return coroutine.yield("add") end
mt.__eq = function(a, b) return coroutine.yield("eq") end
mt.__lt = function(a, b) return coroutine.yield("lt") end
mt.__le = function(a, b) return coroutine.yield("le") end
mt.__concat = function(a, b) return coroutine.yield("concat") end
mt.__call = function(self, x) return coroutine.yield("call " .. tostring(x)) end
mt.__unm = function(a) return coroutine.yield("unm") end
mt.__len = function(a) return coroutine.yield("len") end
local obj, obj2 = setmetatable({}, mt), setmetatable({}, mt)
local co2 = coroutine.wrap(function()
  show("got index", obj.foo)
  obj.bar = 5
  show("raw bar", rawget(obj, "bar"))
  show("add", obj + 1)
  show("eq", obj == obj2)
  show("lt", obj < obj2)
  show("le", obj <= obj2)
  show("concat", obj .. "x")
  show("call", obj(42))
  show("unm", -obj)
  show("len", #obj)
  for i = 1, 2 do show("loop", i, obj[i]) end
  return "co2 end"
end)
local r = co2()
while r ~= "co2 end" do
  show("yielded:", r)
  if r == "eq" or r == "lt" then r = co2(false)
  elseif r == "le" then r = co2(1)
  else r = co2("R:" .. r) end
end
show(r)

-- yield across non-yieldable boundaries
local co3 = coroutine.create(function()
  table.sort({3, 2, 1}, function(a, b) coroutine.yield() return a < b end)
end)
show(coroutine.resume(co3))
local co4 = coroutine.create(function()
  return tostring(setmetatable({}, {__tostring = function() coroutine.yield() end}))
end)
show(coroutine.resume(co4))
local co5 = coroutine.create(function() return string.gsub("a", "a", function() coroutine.yield() end) end)
show(coroutine.resume(co5))
local co6 = coroutine.create(function() for k in pairs(setmetatable({}, {__index = function() coroutine.yield() end})) do end return "ok" end)
show(coroutine.resume(co6))
local co7 = coroutine.create(function() return select(2, coroutine.yield(1, 2, 3)) end)
show(coroutine.resume(co7))
show(coroutine.resume(co7, "a", "b", "c"))
-- yield from iterator
local function iter()
  return coroutine.wrap(function() for i = 1, 3 do coroutine.yield(i, i * i) end end)
end
for i, sq in iter() do io.write(i, ":", sq, " ") end print()
-- generic for with yielding iterator function
local co8 = coroutine.wrap(function()
  local function it(s, c) if c < 3 then coroutine.yield("it" .. c) return c + 1 end end
  for i in it, nil, 0 do io.write("body", i, " ") end
  return "end8"
end)
show(co8(), co8(), co8(), co8())
print()
-- errors in coroutines
local co9 = coroutine.create(function() local x = nil; return x.y end)
show(coroutine.resume(co9))
show(coroutine.status(co9), coroutine.resume(co9))
local co10 = coroutine.wrap(function() error({code = 1}) end)
local ok, e = pcall(co10)
show(ok, type(e), e and e.code)
local co11 = coroutine.wrap(function() error("wrapped", 2) end)
show(pcall(co11))
show(pcall(coroutine.wrap(function() coroutine.yield() end)))
local dead = coroutine.wrap(function() end)
dead()
show(pcall(dead))
-- running / isyieldable / status
show(coroutine.running())
show(coroutine.isyieldable())
local co12
co12 = coroutine.create(function()
  show(coroutine.running() == co12, coroutine.isyieldable(), coroutine.status(co12))
  local inner = coroutine.create(function() show("outer status", coroutine.status(co12)) end)
  coroutine.resume(inner)
end)
coroutine.resume(co12)
show(pcall(coroutine.resume, coroutine.running()))
show(pcall(coroutine.yield))
-- resume passing values through many levels
local function gen(n)
  return coroutine.wrap(function()
    if n == 0 then coroutine.yield("leaf") return end
    local g = gen(n - 1)
    coroutine.yield(g() .. n)
  end)
end
show(gen(10)())
-- deep recursion inside a coroutine with error handler and traceback
local co13 = coroutine.create(function()
  local function rec(n) if n == 0 then error("bottom") end return rec(n - 1) + 1 end
  return xpcall(rec, function(m) return m .. " | " .. select(2, debug.traceback():gsub("\n", "\n")) end, 50)
end)
show(coroutine.resume(co13))
-- coroutine.wrap error message has position info
show(pcall(coroutine.wrap(function() local t; t.x = 1 end)))
-- yield across pcall inside metamethod inside coroutine
local co14 = coroutine.wrap(function()
  local t = setmetatable({}, {__index = function(_, k) return select(2, pcall(coroutine.yield, k)) end})
  return t.abc
end)
show(co14())
show(co14("resumed"))
-- debug.traceback of a coroutine
local co15 = coroutine.create(function() local function f() coroutine.yield() end f() end)
coroutine.resume(co15)
print(debug.traceback(co15))
print(debug.traceback(co15, "msg"))
print(debug.traceback(co15, "msg", 1))
print(debug.traceback("top", 1):gsub("\n[^\n]*%[C%][^\n]*$", ""))
-- getinfo on coroutine
local info = debug.getinfo(co15, 1, "Sl")
show(info and info.short_src, info and info.currentline)
show(debug.getlocal(co15, 1, 1))
