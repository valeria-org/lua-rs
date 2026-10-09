dofile"util.lua"
print(type(package.loaded), package.loaded._G == _G, package.loaded.package == package)
print(#package.loaders, type(package.preload))
dumpt(jit)
print(jit.version, jit.version_num, jit.os, jit.arch, jit.status())
print(pcall(jit.on)) print(pcall(jit.off)) print(pcall(jit.flush)) print(pcall(jit.on, print))
print(pcall(jit.off, true, true)) print(pcall(jit.on, 1)) print(pcall(jit.on, nil, 1))
for _,o in ipairs{"prng","strhash","strid","mcode"} do print(o, jit.security(o)) end
print(pcall(jit.security,"x")) print(pcall(jit.security))
local u = require"jit.util"
print(package.loaded["jit.util"] == u, jit.util)
local f=function(a,...) local x = "s" return 1.5, x end
local fi = u.funcinfo(f)
dumpt(fi)
print(u.funcinfo(f,100).loc, u.funcinfo(f,-1).currentline)
for pc=0,10 do print(pc, u.funcbc(f,pc)) end
print(u.funck(f,0), u.funck(f,1), u.funck(f,-1), u.funck(f,-2))
print(pcall(u.funcbc, print, 0))
local g = function() return f end
print(u.funcuvname(g,0), u.funcuvname(g,1))
local fi2=u.funcinfo(print) print(fi2.ffid, type(fi2.addr), fi2.upvalues)
local p = u.funck(function() return function() end end, -1)
print(type(p), u.funcinfo(p).loc)
print(pcall(u.funcinfo)) print(pcall(u.funck, 1, 1))
print(debug.getinfo(1,"S").source)
-- jit.attach / bc events
local function h(p) print("bc", type(p), u.funcinfo(p).loc) end
jit.attach(h, "bc")
local f1 = loadstring("local function a() local function b() end end local function c() end return 1", "=chunk")
print(f1())
jit.attach(h)
loadstring("return 2")
print(pcall(jit.attach)) print(pcall(jit.attach, h, 1))
jit.attach(function() error("handler err") end, "bc")
loadstring("return 3")
jit.attach(function(p) loadstring("return 4") print("nested ok") end, "bc")
loadstring("return 3")
local r = debug.getregistry()._VMEVENTS
local n = 0 for k, v in pairs(r) do n = n + 1 end print("events", n)
-- embedded modules
local vmdef = require "jit.vmdef"
print(type(vmdef.bcnames), type(vmdef.ffnames), vmdef.ffnames[29], package.loaded["jit.vmdef"] == vmdef)
-- jit.profile
local p = require "jit.profile"
print(package.loaded["jit.profile"] == p, p.start ~= nil)
print(p.dumpstack("pl|fZ;", 5))
print(p.dumpstack("F;", -3))
local function lf() return p.dumpstack("l f F;", 3) end
print(lf())
print(p.dumpstack(coroutine.create(print), "l", 3))
print(pcall(p.dumpstack)) print(pcall(p.start, "", 1))
local cnt = 0
p.start("i1", function(th, samples, vmst) cnt = cnt + samples end)
local t0 = os.clock() while os.clock() - t0 < 0.2 do end
p.stop()
print("samples", cnt > 0)
p.stop()
