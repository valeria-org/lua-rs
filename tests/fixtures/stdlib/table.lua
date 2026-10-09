-- table library: results and error messages.
local function e(...) print(pcall(...)) end
local function dump(t, n) local r = {}; for i = 1, n or #t do r[#r + 1] = tostring(t[i]) end; return table.concat(r, ",") end

-- concat
print(table.concat({}), table.concat({1, 2, 3}), table.concat({1, 2, 3}, ", "), table.concat({"a", 2.5, "c"}, "-", 2), table.concat({1, 2, 3}, "", 2, 3))
print(table.concat({1, 2, 3}, ",", 3, 2), table.concat({1, 2}, 0), table.concat({[-1] = "x", [0] = "y"}, ":", -1, 0))
e(table.concat, {1, {}, 3})
e(table.concat, {1, 2}, ",", 1, 3)
e(table.concat, {true})
e(table.concat, {})
e(table.concat)
e(table.concat, {}, {})
e(table.concat, {1}, ",", "x")

-- insert / remove
local t = {}
table.insert(t, "a"); table.insert(t, "b"); table.insert(t, 1, "c"); table.insert(t, 4, "d"); table.insert(t, 2, "e")
print(dump(t))
table.insert(t, 10, "far"); print(dump(t, 10), #t)
e(table.insert, t)
e(table.insert, t, 1, 2, 3)
e(table.insert, nil, 1)
e(table.insert, t, "x", 2)
local r = {1, 2, 3, 4, 5}
print(table.remove(r), dump(r), table.remove(r, 1), dump(r), table.remove(r, 2), dump(r))
print(table.remove({}), table.remove({}, 1), table.remove({1}, 5), table.remove({1}, 0), table.remove({1}, -1))
print(select("#", table.remove({})), select("#", table.remove({1}, 3)))
e(table.remove)
e(table.remove, {1}, "x")
e(table.remove, {1}, {})

-- maxn / getn / foreach / foreachi / move
print(table.maxn({}), table.maxn({1, 2, 3}), table.maxn({[1.5] = 1, [-3] = 2}), table.maxn({nil, nil, 3}), table.maxn({[100] = 1, x = 2, [2^40] = 1}))
print(table.getn({1, 2, 3}), table.getn({}))
e(table.maxn)
e(table.getn, "x")
table.foreachi({"a", "b", "c"}, function(i, v) print(i, v) end)
print(table.foreachi({"a", "b"}, function(i, v) if v == "b" then return "stop" .. i end end))
local cnt = 0; table.foreach({x = 1, y = 2, 3}, function(k, v) cnt = cnt + v end); print(cnt)
print(table.foreach({5}, function(k, v) return k + v end))
e(table.foreach, {}, nil)
e(table.foreachi, nil, print)
local m = {1, 2, 3, 4, 5}
print(dump(table.move(m, 2, 4, 1)), dump(table.move({1, 2, 3, 4, 5}, 1, 3, 3)), dump(table.move({1, 2, 3}, 1, 3, 1, {}), 3))
print(dump(table.move({1, 2, 3}, 3, 1, 1)))
e(table.move, {}, 1)
e(table.move, {}, 1, 2, 3, "x")

-- sort
local s = {5, 3, 8, 1, 9, 2, 7, 4, 6, 0}
table.sort(s); print(dump(s))
table.sort(s, function(a, b) return a > b end); print(dump(s))
local w = {"banana", "apple", "Cherry", "date"}; table.sort(w); print(dump(w))
local big = {}; for i = 1, 200 do big[i] = (i * 7919) % 211 end; table.sort(big)
local okk = true; for i = 2, 200 do if big[i - 1] > big[i] then okk = false end end; print(okk, big[1], big[200])
local omt = {__lt = function(a, b) return a.v < b.v end}
local objs = {}; for i = 1, 20 do objs[i] = setmetatable({v = (i * 13) % 20}, omt) end
table.sort(objs); print(objs[1].v, objs[20].v)
table.sort({}); table.sort({1})
e(table.sort, {3, 1, "x"})
e(table.sort, {1, 2, 3}, 1)
e(table.sort)
e(table.sort, {1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15}, function(a, b) return true end)
e(table.sort, {3, 2, 1}, function(a, b) error("cmp") end)
local calls = 0; table.sort({4, 3, 2, 1, 5, 6}, function(a, b) calls = calls + 1; return a < b end); print(calls)

-- table.new / table.clear
local tnew = require("table.new")
local tclear = require("table.clear")
print(type(tnew), tnew == table.new, tclear == table.clear, require("table.new") == tnew)
local nt = tnew(10, 5); print(type(nt), #nt, next(nt))
nt[1] = 1; nt.x = 2; tclear(nt); print(#nt, next(nt))
e(tnew, 1)
e(tnew, "x", 1)
e(tclear, 1)
print(tostring(table.new):match("builtin#%d+"), tostring(table.clear):match("builtin#%d+"), tostring(table.sort):match("builtin#%d+"))

-- error messages naming the called function
print(pcall(function() table.insert({}, 1, 2, 3) end))
print(pcall(function() table.concat({{}}) end))
print(pcall(function() table.sort({1, "x", 2}) end))
print(pcall(function() table.sort({1, 2, 3, 4, 5, 6, 7, 8}, function() return true end) end))
print(pcall(function() table.remove("x") end))
print(pcall(function() local t = table; t.getn(1) end))
print(pcall(function() table.foreach({}, 1) end))
