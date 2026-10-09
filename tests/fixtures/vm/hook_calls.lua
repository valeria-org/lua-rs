local function tconcat(t, sep) local s = "" for i=1,#t do s = s .. (i>1 and sep or "") .. t[i] end return s end
local function f(a)
  return a + 1
end
local ev = {}
local function h(e, l) ev[#ev+1] = e .. ":" .. tostring(l) end
debug.sethook(h, "lc")
local x = f(1) x = f(x) x = type(1.5) x = 3
debug.sethook()
print(tconcat(ev, " "))
ev = {}
debug.sethook(h, "l")
local x = f(1) x = type(1.5) x = 3
debug.sethook()
print(tconcat(ev, " "))
ev = {}
debug.sethook(h, "l")
local x = type(1.5) x = type(1.5) x = 3
local t = setmetatable({}, {__index = function() return 1 end}) x = t.x x = 4
debug.sethook()
print(tconcat(ev, " "))
