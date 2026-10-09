-- goto/continue and the syntax extensions backported from LuaJIT 3.0.
local function show(...) local t = {n = select('#', ...), ...}; for i = 1, t.n do t[i] = tostring(t[i]) end print(table.concat(t, " ", 1, t.n)) end

-- goto
do
  local i = 1
  ::top::
  if i <= 3 then io.write(i, " ") i = i + 1 goto top end
  print()
end
for i = 1, 3 do
  for j = 1, 3 do
    if j == 2 then goto next end
    io.write(i, j, " ")
  end
  ::next::
end
print()
do goto skip; print("skipped") ::skip:: end
-- closures in goto loops capture fresh locals
local fs = {}
do
  local i = 1
  ::again::
  local x = i
  fs[#fs + 1] = function() return x end
  i = i + 1
  if i <= 3 then goto again end
end
show(fs[1](), fs[2](), fs[3]())

-- continue
for i = 1, 6 do
  if i % 2 == 0 then continue end
  io.write(i, " ")
end
print()
local n = 0
while n < 5 do
  n = n + 1
  if n == 3 then continue end
  io.write("w", n, " ")
end
print()
local m = 0
repeat
  m = m + 1
  local stop = m >= 4
  if m == 2 then continue end
  io.write("r", m, " ")
until stop
print()
for k, v in ipairs{"a", "b", "c"} do
  if v == "b" then continue end
  io.write(k, v, " ")
end
print()

-- bit operators
show(5 & 3, 5 | 3, 5 ~ 3, ~5, 1 << 4, 256 >> 4, -256 >> 4, -256 ~>> 4, 1 << 31, 1 << 32)
show(0xff & -1, 2^32 + 5 & 0xff, 1.5 | 0, pcall(function() return "3" & 1 end))
show(pcall(load("return {} & 1")))
show(pcall(load("local t = {} return t | 1")))
-- customary operators
show(not nil, !nil, !true, true && false, nil || "x", 1 != 2, 1 != 1, "a" != "a")
-- ternary
local function tern(a) return a ? "yes" : "no" end
show(tern(true), tern(false), tern(nil), tern(0))
show(1 ? (2 ? 3 : 4) : 5, false ? 1 : (nil ? 2 : 3))
local x = 5
show(x > 3 ? "big" : "small", x < 3 ? "big" : "small")
-- safe navigation
local t = {a = {b = {c = 42}}, f = function(self, v) return v end, s = "str"}
show(t?.a?.b?.c, t?.x?.y?.z, t.a?.b.c)
local none
show(none?.x, pcall(function() return none?.x.y.z end), none?.["k"], none?.f())
show(t?.f(t, 7), t.s:upper(), pcall(load("return none?:m()")))
show(pcall(function() return t?.x.y end))
-- nil coalescing
show(nil ?? "default", false ?? "default", 0 ?? 1, none?.x ?? "fallback", nil ?? nil ?? 3)
local cnt = 0
local function side() cnt = cnt + 1 return cnt end
show(1 ?? side(), cnt)
-- compound assignment
local a = 10
a += 5; a -= 3; a *= 2; a /= 4; a %= 4
show(a)
local s = "x"
s ..= "y"; s ..= 1
show(s)
local b = 0xf0
b &= 0x3c; b |= 1; b ~= 0xff; b <<= 2; b >>= 1; b ~>>= 1
show(b)
local tt = {v = 1, arr = {1, 2}}
tt.v += 10; tt.arr[2] *= 50; tt["v"] -= 1
show(tt.v, tt.arr[2])
local calls = 0
local function key() calls = calls + 1 return "k" end
local kt = {k = 1}
kt[key()] += 1
show(kt.k, calls)
-- const
do
  const c1 = 42
  const c2, c3 = "x", {1}
  show(c1, c2, c3[1])
  show(load("const k = 1; k = 2"))
  show(load("const k = 1; local function f() k = 3 end"))
end
-- short function expressions
local sq = |x| -> x * x
local add = |a, b| -> a + b
local nop = || -> nil
local id = v -> v
show(sq(5), add(2, 3), nop(), id("i"))
local blk = |x| -> do local y = x * 2 return y + 1 end
show(blk(10))
show(table.concat((function(...) local r = {} for i, v in ipairs{...} do r[i] = (|z| -> z .. "!")(v) end return r end)("a", "b"), ","))
local sorted = {3, 1, 2}
table.sort(sorted, |p, q| -> p > q)
show(sorted[1], sorted[2], sorted[3])
-- underscores in numbers
show(1_000_000, 0x_ff_ff, 1_0.5_0, 1e1_0, 0b1010_1010)
-- unicode escapes
show("\u{48}\u{49}", #"\u{7FF}", #"\u{FFFF}", #"\u{10FFFF}", load("return '\\u{7FFFFFFF}'"))
-- syntax errors from the extensions
for _, src in ipairs{
  "x = 1 ? 2", "x = a?.", "x = 1 ?? ", "a, b += 1", "local x += 1", "continue", "goto nowhere",
  "do ::l:: ::l:: end", "for i=1,2 do local x goto l; local y ::l:: print(y) end", "x = |a -> a",
  "x = 0x_", "x = 1__0", "x = '\\u{110000000}'", "x = '\\u{}'", "const x", "x = 1 ~= 2 ~= 3",
  "break", "return return", "x = y?:z", "a?.b = 1", "f() = 1", "x = 1 << ", "local function f() continue end",
  "repeat local x continue until x", "while true do local function g() continue end end",
} do
  local f, err = load(src, "=src")
  show(src, f and "ok" or err)
end
