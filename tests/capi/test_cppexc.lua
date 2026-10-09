local cp = require("cpptest")
local function show(...) print(select('#', ...), ...) end
-- C++ catch around Lua frames without pcall
show(pcall(cp.catch, function() cp.throw("foo") end))
show(pcall(cp.catch, function() local t = setmetatable({}, {__index = function() cp.throw("idx") end}); return t.x end))
-- pcall inside
show(pcall(cp.catch, function() return pcall(cp.throw, "foo") end))
-- xpcall: handler not applied
show(xpcall(cp.throw, function(m) return "H:"..tostring(m) end, "foo"))
show(xpcall(function() cp.throw("foo") end, function(m) return "H:"..tostring(m) end))
-- coroutine
local co = coroutine.create(function() cp.throw("co") end)
show(pcall(coroutine.resume, co))
show(coroutine.status(co))
co = coroutine.create(function() cp.throw("co") end)
show(pcall(cp.catch, function() return coroutine.resume(co) end))
show(coroutine.status(co))
show(pcall(cp.catch, coroutine.wrap(function() cp.throw("wrap") end)))
-- through Rust natives with callbacks
show(pcall(cp.catch, function() table.sort({3,2,1}, function(a,b) cp.throw("sort") end) end))
show(pcall(cp.catch, function() return ("x"):gsub(".", function() cp.throw("gsub") end) end))
show(pcall(cp.catch, function() return tostring(setmetatable({}, {__tostring=function() cp.throw("ts") end})) end))
-- nested catch
show(pcall(cp.catch, function() return cp.catch(function() cp.throw("inner") end) end))
-- alloc / isalloc
show(pcall(cp.alloc, function() return "foo", error end), cp.isalloc())
show(pcall(cp.alloc, function() return "foo", cp.throw end), cp.isalloc())
-- usereg
show(cp.usereg(100, 51, function() cp.usereg(100, 40, error, "foo") end, false))
-- state still usable
local t = {}
for i = 1, 10 do t[i] = pcall(cp.catch, function() local x = {i}; cp.throw("loop"..i) end) end
print(#t)
show(select('#', pcall(cp.catch, function(...) return ... end, 1, 2, 3)))
-- error from inside catch (Lua error)
show(pcall(cp.catch, function() error("luaerr") end))
show(pcall(cp.catch, function() error({}) end))
-- C++ exception from a hook
show(pcall(cp.catch, function()
  debug.sethook(function() debug.sethook(); cp.throw("hook") end, "l")
  local a = 1
  return a
end))
debug.sethook()
print("done")
