local function f() f() end
print(pcall(f))
local function f2() return f2() + 1 end
print(pcall(f2))
local function g(...) g(1, ...) end
print(pcall(g))
local t = setmetatable({}, {__index = function(t, k) return t[k] end})
print(pcall(function() return t.x end))
print(xpcall(f, function(m) return "H:" .. tostring(m) end))
local function h() local ok, e = pcall(h) return e end
print(select('#', pcall(h)))
local co = coroutine.wrap(function() local function r() r() end r() end)
print(pcall(co))
local function deepcat(n) if n == 0 then return "" end return "a" .. deepcat(n - 1) end
print(#deepcat(5000))
local tt = setmetatable({}, {__tostring = function(s) return tostring(s) end})
