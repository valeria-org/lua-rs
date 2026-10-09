-- Library functions that call back into Lua (sort, gsub, tostring, format, metamethods, xpcall handlers, iterators) under GC stress.
local function junk(n) local t = {} for i = 1, n or 5 do t[i] = {i, tostring(i) .. "j"} end return t end
-- table.sort with allocating comparator and __lt metamethods
local mt = {__lt = function(a, b) junk(3) return a.v < b.v end}
local arr = {}
for i = 1, 60 do arr[i] = setmetatable({v = (i * 37) % 61, s = "s" .. i}, mt) end
table.sort(arr)
local o = {} for i = 1, #arr do o[i] = arr[i].v end print(table.concat(o, ","))
local strs = {} for i = 1, 80 do strs[i] = "k" .. ((i * 13) % 80) end
table.sort(strs, function(a, b) junk(2) return (a .. "x") < (b .. "x") end)
print(table.concat(strs, " ", 1, 10))
print(pcall(table.sort, {3, 2, 1, 5, 4}, function(a, b) junk(1) error({tag = "cmp"}) end))
-- gsub with function/table replacements producing fresh strings
print((string.gsub("hello world from lua", "(%w+)", function(w) junk(2) return w:upper() .. #w end)))
print((string.gsub("a b c d e f", "%w", setmetatable({}, {__index = function(t, k) junk(2) return k .. k end}))))
print((string.gsub("abc", "", function() junk(1) return "-" end)))
for k, v in string.gmatch("a=1, b=2, c=3", "(%w+)=(%w+)") do junk(1) io.write(k, v, ";") end print()
-- tostring/print/string.format with __tostring
local tsmt = {__tostring = function(t) junk(4) return "T<" .. t.n .. ">" end}
local objs = {} for i = 1, 10 do objs[i] = setmetatable({n = i}, tsmt) end
print(tostring(objs[1]), objs[2], objs[3])
print(string.format("%s|%s|%5s", objs[4], objs[5], objs[6]))
print(pcall(string.format, "%s", setmetatable({}, {__tostring = function() return {} end})))
-- __index / __newindex / __call / __concat / __eq / __le / __unm / __len through natives
local prox = setmetatable({}, {__index = function(t, k) junk(2) return k .. "!" end,
  __newindex = function(t, k, v) junk(2) rawset(t, k, v .. "?") end,
  __call = function(self, a, b) junk(2) return a .. b end,
  __concat = function(a, b) junk(2) return "cat" end,
  __unm = function(a) junk(1) return "neg" end,
  __len = function(a) return 42 end})
print(prox.foo, prox("x", "y"), prox .. "z", "z" .. prox, -prox, #prox)
prox.bar = "b" print(rawget(prox, "bar"))
print(select("#", unpack({1, 2, 3, nil, 5}, 1, 5)))
-- xpcall handlers that allocate; error objects that are fresh tables
for i = 1, 5 do
  local ok, e = xpcall(function() junk(3) error({code = i, msg = "m" .. i}) end, function(e) junk(5) return {wrapped = e, s = "w" .. e.msg} end)
  io.write(tostring(ok), " ", e.s, " ", e.wrapped.code, "; ")
end
print()
print(xpcall(function() local x = nil; return x.y end, function(m) junk(2) return "H:" .. m end))
-- error with non-string values and levels
print(pcall(error, setmetatable({}, {__tostring = function() return "custom" end})))
print(select(2, pcall(error, "lvl", 2)))
-- table.foreach / foreachi / ipairs / pairs / next with allocation inside loops
local t = {} for i = 1, 30 do t[i] = "v" .. i end
local acc = {} table.foreachi(t, function(i, v) junk(1) acc[#acc+1] = v:sub(2) end) print(table.concat(acc, ","))
local cnt = 0 table.foreach({a = 1, b = 2, c = 3}, function(k, v) junk(1) cnt = cnt + v end) print(cnt)
for i, v in ipairs(t) do if i % 10 == 0 then junk(3) t[i] = v .. "+" end end print(t[10], t[20], t[30])
-- string methods on fresh strings
local parts = {} for i = 1, 50 do parts[#parts+1] = string.rep("ab", i % 5) .. string.char(65 + i % 26) end
print(table.concat(parts):len(), (table.concat(parts)):sub(1, 30))
print(("x"):rep(3, ","), string.byte("hello", 1, -1))
print(string.format("%d %5.2f %q %s", 42, 3.14159, "a\nb\0c", nil))
-- tonumber / tostring on many values
local s = 0 for i = 1, 200 do s = s + tonumber(tostring(i * 1.5)) end print(s)
-- setmetatable/getmetatable with __metatable
print(getmetatable(setmetatable({}, {__metatable = "locked"})))
-- rawequal/rawlen-like/select
print(rawequal("a" .. "b", "ab"), select(-1, 1, 2, 3))
-- coroutine.wrap as iterators with allocation
local function gen(n) return coroutine.wrap(function() for i = 1, n do junk(2) coroutine.yield(i, "s" .. i) end end) end
local r = {} for i, s in gen(15) do r[#r+1] = s end print(table.concat(r, ","))
-- os/io helpers producing strings
print(type(os.time()), type(os.clock()), os.date("!%Y", 0))
