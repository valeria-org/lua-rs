-- Weak tables, finalizers and string interning under GC stress. Objects only become
-- unreachable together (when a holding table is dropped), so the output does not depend on
-- when collections happen.
local function junk(n) local t = {} for i = 1, n or 5 do t[i] = {i, tostring(i) .. "j"} end return t end
local function count(t) local n = 0 for _ in pairs(t) do n = n + 1 end return n end
local function full() collectgarbage() collectgarbage() end
-- weak keys/values/kv with every collectable type
local wk = setmetatable({}, {__mode = "k"})
local wv = setmetatable({}, {__mode = "v"})
local wkv = setmetatable({}, {__mode = "kv"})
local keep = {}
local function fill()
  for i = 1, 20 do
    local t, f, u, c = {i}, function() return i end, newproxy(false), coroutine.create(function() end)
    wk[t] = i wk[f] = i wk[u] = i wk[c] = i
    wv[i] = t wv[i + 100] = f wv[i + 200] = u wv[i + 300] = c
    wkv[t] = f
    wv["s" .. i] = "str" .. i   -- strings are never cleared
    if i % 5 == 0 then keep[#keep + 1] = t keep[#keep + 1] = f keep[#keep + 1] = u keep[#keep + 1] = c end
  end
end
fill() junk(30) full()
print(count(wk), count(wv), count(wkv))
keep = nil full()
print(count(wk), count(wv), count(wkv))
-- weak table referencing itself, weak table as its own key
local self = setmetatable({}, {__mode = "k"}) self[self] = self full() print(count(self))
-- finalizers: order (newest first), resurrection, finalizer creating objects with finalizers
local log = {}
local resurrected = {}
local pending = {}
for i = 1, 6 do
  local u = newproxy(true)
  getmetatable(u).__gc = function(o)
    junk(5)
    log[#log + 1] = "f" .. i
    if i % 2 == 0 then resurrected[#resurrected + 1] = o end
    if i == 3 then local v = newproxy(true) getmetatable(v).__gc = function() log[#log + 1] = "inner" end end
  end
  pending[i] = u
end
pending = nil
full() full()
print(table.concat(log, " "), #resurrected)
resurrected = {} full() full()
print(#log)
-- resurrected userdata in weak values: cleared at finalization, keys kept
local wvu = setmetatable({}, {__mode = "v"})
local wku = setmetatable({}, {__mode = "k"})
local held
do
  local u = newproxy(true)
  getmetatable(u).__gc = function(o) held = o end
  wvu[1] = u wku[u] = "val"
end
full()
print(wvu[1] == nil, wku[held], held ~= nil)
held = nil full() print(count(wku))
-- finalizer errors are reported, not raised
do local u = newproxy(true) getmetatable(u).__gc = function() error("fin" .. "err") end end
full() print("after errfin")
-- collectgarbage inside finalizer
do local u = newproxy(true) getmetatable(u).__gc = function() collectgarbage() log[#log + 1] = "cg" end end
full() print(log[#log])
-- string interning: drop and recreate the same contents
local ids = {}
for round = 1, 3 do
  for i = 1, 200 do local s = "intern" .. i ids[s] = (ids[s] or 0) + 1 end
  junk(50) full()
end
print(ids.intern1, ids.intern200, count(ids))
local s1 = "x" .. "yz" full() junk(20) full() local s2 = "xy" .. "z" print(s1 == s2, rawequal(s1, s2))
-- table keys that are strings created and dropped while the table lives
local tk = {} for i = 1, 300 do tk["key" .. i] = i end full() for i = 1, 300 do assert(tk["key" .. i] == i) end print("keys ok")
-- big stacks: grow and shrink across collections
local function deep(n, acc) if n == 0 then junk(5) return #acc end return deep(n - 1, acc .. "a") end
print(deep(3000, ""))
local function vararg(...) junk(2) return select("#", ...) end
local big = {} for i = 1, 5000 do big[i] = i end
print(vararg(unpack(big)))
print(collectgarbage("count") > 0, collectgarbage("step", 0) ~= nil)
-- __gc on tables is ignored in 5.1
setmetatable({}, {__gc = function() print("table gc!") end}) full()
print("end")
