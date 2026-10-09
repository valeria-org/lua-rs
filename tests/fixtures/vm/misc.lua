local function cnt(...) return select('#', ...) end
print(cnt(), cnt(nil), cnt(nil, nil), select(2, "a", "b", "c"), select(-2, "a", "b", "c"))
local function tail(n) if n == 0 then return "done" end return tail(n - 1) end
print(tail(100000))
local function vtail(n, ...) if n == 0 then return select('#', ...) end return vtail(n - 1, ...) end
print(vtail(10000, 1, 2, 3))
local fs = {}
for i = 1, 3 do fs[i] = function() return i end end
print(fs[1](), fs[2](), fs[3]())
local fs2 = {}
local j = 0
while j < 3 do j = j + 1 local k = j fs2[j] = function() k = k + 10 return k end end
print(fs2[1](), fs2[1](), fs2[2]())
local function counter() local c = 0 return function() c = c + 1 return c end, function() return c end end
local inc, get = counter()
inc() inc()
print(get())
for k, v in pairs({10, 20, 30}) do io = nil print(k, v) end
local t = {}
for i = 1, 10 do t[i] = i end
local s = 0 for i, v in ipairs(t) do s = s + v end print(s)
for i = 10, 1, -3 do print(i) end
for i = 1, 0 do print("never") end
for i = 1.5, 3 do print(i) end
for i = 0.1, 0.35, 0.1 do print(i) end
print(getfenv(1) == _G, getfenv(print) == _G)
local env = {print = print}
local function fenvtest() return x end
setfenv(fenvtest, {x = "envx"})
print(fenvtest())
print(pcall(setfenv, 1, {}))
print(pcall(setfenv, 100, {}))
print(getfenv(0) == _G)
print(tostring(nil), tostring(true), tostring(1e300*1e300), tostring(-1e300*1e300), tostring(0/0) ~= nil, tostring(1/3), 2^53, 100, -0.0, 1e15, 1e16, 123456789012)
print(tonumber("  0x1F  "), tonumber("1e2"), tonumber("abc"), tonumber("10", 16), tonumber("ff", 16), tonumber("zz", 36), tonumber("-ff", 16), tonumber("8", 8), tonumber(" 11 ", 2))
print(pcall(tonumber), tonumber(nil))
print(#"abc", #{1, 2, 3, nil, 5})
print(type(print), type(nil), type({}), type("s"), type(2), type(true), type(coroutine.create(print)), type(newproxy()))
local a, b, c = (function() return 1, 2, 3 end)()
print(a, b, c)
local t2 = {(function() return 1, 2, 3 end)(), (function() return 4, 5 end)()}
print(#t2, t2[1], t2[2], t2[3])
print(unpack({1, 2, 3}))
print(unpack({1, 2, 3}, -1, 1))
local big = {} for i = 1, 300 do big[i] = i end
print(select('#', unpack(big)))
print(rawequal(0, -0), 0 == -0, 1/0 > 0, -(0/0) ~= -(0/0))
print(5 % -3, -5 % 3, 5.5 % 2, -5.5 % 2, 5 % 0 ~= 5 % 0, 2^0.5, (-8)^(1/3) ~= (-8)^(1/3))
