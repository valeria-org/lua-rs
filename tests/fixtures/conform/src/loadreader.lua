-- lua_load reader semantics: the reader is called only when the lexer or the bytecode reader
-- needs more input; W mode returns non-native prototypes.
local function cnt(s, step)
  local i, n = 1, 0
  local f, e = load(function() n = n + 1; local r = s:sub(i, i + step - 1); i = i + step; return r end)
  return n, f and "ok" or e
end
local d = string.dump(loadstring("local a = {1,2,x=3}; return function() return a, 1.5, \"s\" end"))
print(#d, cnt(d, 1), cnt(d, 3), cnt(d, 7), cnt(d, 1000))
local d2 = string.dump(loadstring("return 1"), true)
print(#d2, cnt(d2, 1), cnt(d2, 2), cnt(d2, 5))
print(cnt(d:sub(1, 20), 1), cnt(d .. "x", 1), cnt(d .. "\0\0", 1))
print(cnt("return 1", 1), cnt("return 1", 100), cnt("--x\nreturn", 2))
print(cnt("#!x\nreturn 1", 1))
print(cnt("\239\187\191return 1", 1))
print(cnt("\239\187\191return 1", 3))
print(cnt("*a = 123", 1))
print(cnt("x = = 1", 1))
local parts = {"return ", "1", {}}; local k = 0
print(pcall(load, function() k = k + 1; return parts[k] end))
parts = {"x = = ", "1", {}}; k = 0
print(load(function() k = k + 1; return parts[k] end))
k = 0
print(pcall(load, function() k = k + 1; if k == 2 then error("boom") end; return "return 1" end))
local p = load("return 1", "x", "tW")
print(type(p), #string.dump(p) > 0, load(string.dump(function() return 1 end), "y", "bW"))
print(load("return 1", "x", "b"))
local src = [[
local t = {1, 2, "three", x = "ex", [10] = {nested = "yes"}, 5LL, 2i}
local function f(a, ...)
  local s, n = "str" .. a, select('#', ...)
  return function(b) return s .. b .. n .. t.x .. t[10].nested end
end
local names = {}
for i = 1, 20 do names[#names + 1] = "n" .. i end
return f("A", 1, 2)("B"), #t, t[3], table.concat(names, ","), tostring(t[4]), tostring(t[5])
]]
local function reader(s, step, gc)
  local i = 1
  return function()
    if gc then collectgarbage() end
    local r = s:sub(i, i + step - 1)
    i = i + step
    -- create garbage strings with names used in the chunk
    local junk = {}
    for k = 1, 5 do junk[k] = "str" .. k .. "n" .. k end
    return r
  end
end
for _, step in ipairs{1, 2, 7, 64} do
  local f = assert(load(reader(src, step, true), "=src"))
  print(step, f())
  local d = string.dump(f)
  local g = assert(load(reader(d, step, true), "=dump"))
  print(step, g())
  local ds = string.dump(f, true)
  local h = assert(load(reader(ds, step, true), "=sdump"))
  print(step, h())
end
-- reader that loads recursively
local depth = 0
local function rec(s)
  local done = false
  return function()
    if done then return nil end
    done = true
    depth = depth + 1
    if depth < 5 then
      local inner = load(rec("return " .. depth))
      collectgarbage()
      return "return " .. tostring(inner()) .. " + 100"
    end
    return s
  end
end
print(load(rec("return 7"))())
