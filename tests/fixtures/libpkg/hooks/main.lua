local function f(n) local x = n * 2 return x end
local log = {}
local function hook(ev, line)
  local info = debug.getinfo(2, "nS")
  print(ev, line, info.what, info.name, info.short_src)
end
debug.sethook(hook, "crl")
f(3)
debug.sethook()
print(debug.gethook())
debug.sethook(hook, "", 3)
local s = 0
for i = 1, 3 do s = s + i end
debug.sethook()
print(debug.gethook())
debug.sethook(hook, "l", 0)
print(debug.gethook())
debug.sethook(nil)
debug.sethook(coroutine.create(print), hook, "r")
print(debug.gethook())
debug.sethook()
print(pcall(debug.sethook, 1, "c"))
print(pcall(debug.sethook, hook))
print(pcall(debug.sethook, hook, "c", "x"))
print(debug.gethook())
local cnt = 0
debug.sethook(function(ev) cnt = cnt + 1 end, "c")
print(1) print(2)
debug.sethook()
print("calls", cnt)
-- hook error propagates
print(pcall(function()
  debug.sethook(function() debug.sethook() error("in hook") end, "l")
  local a = 1
end))
debug.sethook()
-- locals from hook
debug.sethook(function(ev, line)
  if line == 49 then print("local", debug.getlocal(2, 1)) end
end, "l")
local function g(zz)
  local yy = zz + 1
  return yy
end
g(41)
debug.sethook()
