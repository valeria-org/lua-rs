local weak = setmetatable({}, {__mode = "k"})
local weakv = setmetatable({}, {__mode = "v"})
do local k = {} weak[k] = 1 weakv[1] = {} weakv[2] = "str" end
local keep = {} weak[keep] = 2
collectgarbage()
local n = 0 for k, v in pairs(weak) do n = n + 1 end
print(n, weakv[1], weakv[2])
local order = {}
for i = 1, 3 do local u = newproxy(true) getmetatable(u).__gc = function(o) order[#order+1] = i end end
collectgarbage()
print(#order)
local resurrect
do local u = newproxy(true) getmetatable(u).__gc = function(o) resurrect = o end end
collectgarbage()
print(type(resurrect))
resurrect = nil
collectgarbage()
print(collectgarbage("count") > 0, collectgarbage("isrunning"))
collectgarbage("stop") print(collectgarbage("isrunning")) collectgarbage("restart") print(collectgarbage("isrunning"))
local fin = newproxy(true)
getmetatable(fin).__gc = function() print("closing finalizer") end
local p2 = newproxy(fin)
print(getmetatable(p2) == getmetatable(fin))
local errf = newproxy(true) getmetatable(errf).__gc = function() error("in gc") end
errf = nil collectgarbage()
print("after")
local t = {}
for i = 1, 200000 do t[#t + 1] = {i} if #t > 1000 then t = {} end end
print("alloc done")
