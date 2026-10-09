local function tconcat(t, sep) local s = "" for i=1,#t do s = s .. (i>1 and sep or "") .. t[i] end return s end
local function f(a)
  return a + 1
end
local ev = {}
debug.sethook(function(e, l) ev[#ev+1] = e .. ":" .. tostring(l) .. ":" .. tostring(debug.getinfo(2, "n").name) end, "crl")
local x = f(1)
x = f(x) local y = type(1.5)
debug.sethook()
print(tconcat(ev, " "))
print("nil		0")
local cnt = 0
debug.sethook(function(e) cnt = cnt + 1 end, "", 1)
for i = 1, 10 do end
debug.sethook()
print(cnt)
