local function tconcat(t, sep) local s = "" for i=1,#t do s = s .. (i>1 and sep or "") .. t[i] end return s end
local function f(a)
  return a + 1
end
local ev = {}
debug.sethook(function(e, l) ev[#ev+1] = e .. ":" .. tostring(l) end, "l")
local x = f(1) x = f(x) x = f(x)
for i = 1, 2 do x = x + 1 end
debug.sethook()
print(tconcat(ev, " "))
ev = {}
debug.sethook(function(e, l) ev[#ev+1] = e .. ":" .. tostring(l) end, "lr")
local x = f(1) x = f(x) x = f(x)
debug.sethook()
print(tconcat(ev, " "))
ev = {}
debug.sethook(function(e, l) ev[#ev+1] = e .. ":" .. tostring(l) end, "cr")
local x = f(1) x = type(1.5) local t = setmetatable({}, {__index = function() return 1 end}) x = t.x
debug.sethook()
print(tconcat(ev, " "))
