-- Coroutines, open upvalues, hooks and debug introspection of suspended coroutines under GC stress.
local function junk(n) local t = {} for i = 1, n or 5 do t[i] = {i, tostring(i) .. "j"} end return t end
-- many short-lived coroutines, yielding fresh values across pcall/metamethods/iterators
local total = 0
for k = 1, 40 do
  local co = coroutine.create(function(a, b)
    local acc = {}
    for i = 1, 5 do
      local ok, v = pcall(function() junk(2) return coroutine.yield({i = i, s = "y" .. i .. a}) end)
      acc[#acc+1] = v
    end
    local m = setmetatable({}, {__index = function(t, key) junk(1) return coroutine.yield(key .. "?") end})
    local z = m.zed
    return table.concat(acc, ","), z, b
  end)
  local ok, v = coroutine.resume(co, "A" .. k, {k})
  while coroutine.status(co) == "suspended" do
    junk(2)
    local arg = type(v) == "table" and (v.s .. "!") or ("R" .. tostring(v))
    ok, v, z, b = coroutine.resume(co, arg)
  end
  if k == 1 or k == 40 then print(ok, v, z, type(b)) end
  total = total + #v
end
print(total)
-- coroutine errors carrying tables; dead coroutine inspection
local co = coroutine.create(function() junk(3) local x = {err = "boom"} error(x) end)
local ok, e = coroutine.resume(co) print(ok, type(e), e.err, coroutine.status(co))
print(coroutine.resume(co))
-- coroutine.wrap error propagation
print(pcall(coroutine.wrap(function() junk(2) error("wrapped " .. "err") end)))
-- open upvalues of suspended coroutines; coroutine dropped while closure alive
local fs = {}
for i = 1, 10 do
  local c = coroutine.create(function()
    local x = i * 10
    local s = "up" .. i
    fs[i] = function(d) x = x + d; s = s .. "+"; return x, s end
    coroutine.yield()
    x = x + 1000
    coroutine.yield()
  end)
  coroutine.resume(c)
  if i % 2 == 0 then coroutine.resume(c) end
end
collectgarbage()
junk(10)
for i = 1, 10, 3 do print(fs[i](1)) end
-- closures over loop variables, closed on loop exit / break / goto-less returns
local cl = {}
for i = 1, 20 do local v = {i} cl[i] = function() return v[1] + i end if i == 15 then break end end
junk(20) collectgarbage()
local s = 0 for i = 1, 15 do s = s + cl[i]() end print(s)
local function mk() local a, b, c = {1}, "two" .. 2, 3 return function() return a[1], b, c end, function(x) a = {x} end end
local g, set = mk() junk(5) set(99) junk(5) print(g())
-- hooks that allocate heavily (count + line + call/return)
local n = 0
debug.sethook(function(ev, line) n = n + 1 local t = junk(2) t.ev = ev .. (line or "") end, "crl", 7)
local function fib(x) if x < 2 then return x end return fib(x - 1) + fib(x - 2) end
local r = fib(12)
local strs = {} for i = 1, 30 do strs[i] = "h" .. i end
debug.sethook()
print(r, n > 0, #strs)
-- hook inside coroutine
local co2 = coroutine.create(function()
  debug.sethook(function(ev) junk(3) end, "l")
  local x = 0 for i = 1, 20 do x = x + i coroutine.yield(x) end
  debug.sethook()
  return "done"
end)
local last for i = 1, 25 do local ok, v = coroutine.resume(co2) if not ok then print(v) break end last = v end
print(last, coroutine.status(co2))
-- debug.getinfo / getlocal / traceback in coroutines
local co3 = coroutine.create(function(a) local loc = "local" .. a coroutine.yield() end)
coroutine.resume(co3, "X")
junk(10)
print(debug.getlocal(co3, 1, 1), debug.getlocal(co3, 1, 2))
print(debug.traceback(co3, "tb" .. "msg"))
print(debug.getinfo(co3, 1, "Sl").short_src)
-- coroutine.running / isyieldable
print(coroutine.running())
print(coroutine.wrap(function() return coroutine.running() ~= nil end)())
-- deep recursion with coroutines inside coroutines
local function nest(d) if d == 0 then return "bottom" end local c = coroutine.wrap(function() junk(1) coroutine.yield(nest(d - 1) .. d) end) return c() end
print(nest(30))
