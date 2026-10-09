-- Library functions (debug, jit.util, io, package, string, math, bit, table, load) under GC stress.
local function junk(n) local t = {} for i = 1, n or 5 do t[i] = {i, tostring(i) .. "j"} end return t end
-- debug library
local function outer() local a, b = {1}, "bee" .. 1 return function() return a[1], b end end
local f = outer()
print(debug.getupvalue(f, 1) ~= nil, select(2, debug.getupvalue(f, 2)))
debug.setupvalue(f, 2, "new" .. "val") junk(20) collectgarbage() print(f())
local g = outer()
debug.upvaluejoin(g, 1, f, 2) junk(20) collectgarbage() print(g())
print(debug.upvalueid(f, 1) == debug.upvalueid(f, 1), debug.upvalueid(f, 1) == debug.upvalueid(g, 1))
local info = debug.getinfo(f, "fSlnu") print(info.func == f, info.short_src, info.nups, info.what)
print(type(debug.getregistry()))
debug.setmetatable(10, {__index = function(n, k) junk(1) return k .. n end}) print((5).hello) debug.setmetatable(10, nil)
local env = setmetatable({}, {__index = _G}) local h = setfenv(function() return marker end, env) env.marker = "m" .. 1 junk(9) collectgarbage() print(h(), getfenv(h) == env)
local u = newproxy(true) debug.setfenv(u, {tag = "ud" .. "env"}) collectgarbage() print(debug.getfenv(u).tag)
-- debug.getinfo on all levels during allocation-heavy recursion
local function rec(n) if n == 0 then local names = {} for l = 1, 6 do local i = debug.getinfo(l, "nSl") names[#names+1] = (i.name or "?") .. ":" .. i.currentline junk(2) end return table.concat(names, " ") end return (rec(n - 1)) end
print(rec(4))
print((debug.traceback("msg" .. 1, 1):gsub("0x%x+", "")))
-- string.dump / load round trips
local function src(x) local t = {x, "k" .. x, 1.5, -2, true} return t[1] + #t[2], t end
for _, strip in ipairs({false, true}) do
  local d = string.dump(src, strip)
  junk(20) collectgarbage()
  local l = assert(loadstring(d))
  print(l(7))
end
-- jit.util introspection
local ju = require("jit.util")
local fi = ju.funcinfo(src) print(fi.linedefined, fi.params, fi.stackslots, fi.upvalues, fi.bytecodes > 0, fi.children)
local k = {} for i = -6, -1 do local v = ju.funck(src, i) if v ~= nil then k[#k+1] = tostring(v) end end print(#k > 0)
print(ju.funcbc(src, 0) ~= nil, ju.funcuvname(f, 0))
-- io: lines iterators and files created/closed/garbage-collected
local name = os.tmpname()
local fh = assert(io.open(name, "w")) for i = 1, 50 do fh:write("line", i, "\n") end fh:close()
local cnt = 0 for l in io.lines(name) do junk(1) cnt = cnt + #l end print(cnt)
local fh2 = io.open(name) local lines = {} for l in fh2:lines() do lines[#lines+1] = l if #lines % 10 == 0 then collectgarbage() end end fh2:close() print(#lines, lines[50])
do local fx = io.open(name) end collectgarbage() collectgarbage()   -- file closed by __gc
print(io.type(fh2), pcall(fh2.read, fh2))
local fh3 = io.open(name) print(fh3:read("*l", "*n", 3, "*a"):sub(1, 5)) fh3:close()
os.remove(name)
-- package / require / module
package.preload["gcmod"] = function(name) junk(10) local M = {name = name .. "!"} return M end
print(require("gcmod").name, package.loaded.gcmod.name)
package.preload["gcmod2"] = function(name) module(name, package.seeall) value = "v" .. 2 end
require("gcmod2") collectgarbage() print(gcmod2.value, gcmod2._NAME)
print(select(2, pcall(require, "no.such.mod")):match("module 'no.such.mod' not found") ~= nil)
-- string library under pressure
local t = {} for i = 1, 100 do t[i] = string.format("%5.1f|%x|%s", i / 3, i, ("z"):rep(i % 7)) end print(t[1], t[100])
print(("abc"):reverse():upper():lower(), ("%d"):format(3), string.find("hello world", "o w"), string.match("key=val", "(%w+)=(%w+)"))
-- math
local s = 0 for i = 1, 100 do s = s + math.floor(math.sqrt(i) * 100) / 100 end print(s)
math.randomseed(42) local r1 = math.random(1, 1000) math.randomseed(42) print(r1 == math.random(1, 1000))
-- bit
print(bit.tohex(bit.bor(0xf0, 0x0f)), bit.band(-1, 0xff), bit.lshift(1, 31))
-- table.insert/remove/concat with growth
local q = {} for i = 1, 300 do table.insert(q, 1, "e" .. i) if i % 3 == 0 then table.remove(q) end end print(#q, q[1], q[#q])
print(table.concat(q, ",", 1, 3), table.maxn(q))
local tn = require("table.new") local nt = tn(100, 10) nt[1] = 1 print(#nt)
local tc = require("table.clear") tc(q) print(#q, next(q))
-- load with reader functions that allocate between pieces (the parser holds its state across
-- the reader calls)
local src = {}
for i = 1, 40 do src[#src+1] = "local v"..i.." = 'str"..i.."' .. "..i.."\n" end
src[#src+1] = "return function(x) return x .. v1 .. v40 end"
local s = table.concat(src)
local pos = 0
local lf = assert(load(function()
  junk(3)
  if pos >= #s then return nil end
  local n = math.min(16, #s - pos)
  local piece = s:sub(pos + 1, pos + n)
  pos = pos + n
  return piece
end, "=reader"))
print(lf()("x"))
local d = string.dump(lf)
local i = 0
local lg = assert(load(function()
  junk(2)
  if i >= #d then return nil end
  i = i + 8
  return d:sub(i - 7, i)
end, "=bin"))
print(lg()("y"))
print(loadstring("return ...", "=ls")(1, 2, 3))
print(pcall(load, function() error("reader err") end))
print(load(function() return {} end))
