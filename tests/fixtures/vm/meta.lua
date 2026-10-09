local mt = {}
mt.__add = function(a, b) return "add" end
mt.__sub = function(a, b) return "sub" end
mt.__mul = function(a, b) return "mul" end
mt.__div = function(a, b) return "div" end
mt.__mod = function(a, b) return "mod" end
mt.__pow = function(a, b) return "pow" end
mt.__unm = function(a, b) return "unm" .. tostring(rawequal(a, b)) end
mt.__concat = function(a, b) return "cat(" .. tostring(type(a) == "table" and "T" or a) .. "," .. tostring(type(b) == "table" and "T" or b) .. ")" end
mt.__len = function(a, b) return "len" .. tostring(b) end
mt.__eq = function(a, b) return true end
mt.__lt = function(a, b) return true end
mt.__le = function(a, b) return false end
mt.__call = function(self, ...) return "call", select('#', ...), ... end
mt.__index = function(t, k) return "idx:" .. tostring(k) end
local a, b = setmetatable({}, mt), setmetatable({}, mt)
print(a + 1, 1 - a, a * a, a / 2, a % 3, a ^ 2, -a)
print(a .. "x", "x" .. a, 1 .. a .. 2, "a" .. "b" .. a .. "c" .. "d")
print(#a, a == b, a ~= b, a < b, a <= b, a > b, a >= b)
print(a(1, 2, 3))
print(a.foo, a[1], a[true])
local c = setmetatable({}, {__eq = function() return true end})
print(a == c, c == a)
local d = setmetatable({}, {__lt = function(x, y) print("lt", x == d, y == d) return true end})
print(d <= d)
print(pcall(function() return d <= {} end))
local chain = setmetatable({}, {__index = setmetatable({x = 1}, {__index = {y = 2}})})
print(chain.x, chain.y, chain.z)
local log = {}
local ni = setmetatable({}, {__newindex = function(t, k, v) log[#log+1] = k .. "=" .. tostring(v) rawset(t, k, v) end})
ni.a = 1 ni.a = 2 ni.b = 3
print(table and "" or "", log[1], log[2], ni.a, ni.b)
local loopt = setmetatable({}, {}) getmetatable(loopt).__index = loopt
print(pcall(function() return loopt.x end))
getmetatable(loopt).__newindex = loopt
print(pcall(function() loopt.x = 1 end))
local cnt = 0
local gx = setmetatable({}, {__index = function(t, k) cnt = cnt + 1 return k * 2 end})
local s = 0 for i = 1, 100 do s = s + gx[i] end print(s, cnt)
local cc = setmetatable({}, {__call = function(self, a) return a * 2 end})
print(cc(21), pcall(cc, 4))
print(select('#', a(nil, nil)))
local n1 = setmetatable({}, {__concat = function(a, b) return 7 end})
print(n1 .. n1, "x" .. 1 .. n1)
print(tostring(setmetatable({}, {__tostring = function() return "TS" end})))
print(rawequal(a, b), rawequal(a, a))
