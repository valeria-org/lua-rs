local function show(t) local ks = {} for k, v in pairs(t) do if k ~= "func" and k ~= "activelines" then ks[#ks+1] = k end end
  local s = "" for i = 1, #ks do local m = i for j = i+1, #ks do if ks[j] < ks[m] then m = j end end ks[i], ks[m] = ks[m], ks[i] s = s .. ks[i] .. "=" .. tostring(t[ks[i]]) .. " " end return s end
local function f(a, b)
  local c = a + b
  print(show(debug.getinfo(1, "Slnu")))
  print(show(debug.getinfo(2, "Sln")))
  print(debug.getlocal(1, 1), debug.getlocal(1, 2), debug.getlocal(1, 3), debug.getlocal(1, 4))
  return c
end
f(1, 2)
local t = {m = f}
t.m(3, 4)

print(show(debug.getinfo(print)))
print(show(debug.getinfo(f, "S")))
print(show(debug.getinfo(1, "S")))
print(debug.traceback("msg"))
print(debug.traceback())
local function rec(n) if n == 0 then return debug.traceback("deep") end return (rec(n - 1)) end
print(rec(30))
local co = coroutine.create(function() local x = 1 coroutine.yield() end)
coroutine.resume(co)
print(debug.traceback(co))
local co2 = coroutine.create(function() local y = nil; y() end)
print(coroutine.resume(co2))
print(debug.traceback(co2))
local function va(...) print(debug.getlocal(1, -1), debug.getlocal(1, -2), debug.getlocal(1, -3)) end
va(10, 20)
print(xpcall(function() local q = nil; q.x = 1 end, debug.traceback))
local mt = setmetatable({}, {__index = function(t, k) return debug.traceback("in index") end})
print(mt.foo)
print(pcall(error, "x", 1))
for k in pairs({1}) do print(debug.traceback("iter")) end
for k in (function() return function(s, c) if c then return nil end return debug.traceback("gen") end end)() do print(k) end
print(show(debug.getinfo(1, "Sl")))
