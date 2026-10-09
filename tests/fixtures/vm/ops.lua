-- Every bytecode the parser emits, with upstream semantics.
local function p(...) print(...) end
-- comparisons incl. NaN
local nan = 0/0
p(1 < 2, 2 < 1, 1 <= 1, nan < 1, nan >= 1, not (nan < 1), nan == nan, nan ~= nan, 1 > nan, 1 >= nan)
p("a" < "b", "a" <= "a", "b" > "a", "abc" < "abd", "" < "a", "a\0b" < "a\0c")
local x, y = 3, "3"
p(x == 3, x ~= 4, y == "3", y ~= "x", x == nil, nil == false, true == true, false ~= nil)
-- unary / test-and-copy
local z = nil
p(z or "dflt", z and 1, 0 and "zero-true", false or nil, not nil, not 0, -x, #"four")
-- arithmetic VN/NV/VV + coercion
p(x + 1, 1 + x, x - 1, 10 - x, x * 2, 2 * x, x / 2, 9 / x, x % 2, 7 % x, x ^ 2, 2 ^ x)
p("10" + 1, 1 + "0x10", "2" * "3", "8" / 2, " 5 " - 1, "3" ^ 2, -"2", "7" % 4)
p(1e308 * 10, -1e308 * 10, 0 / 0 ~= 0 / 0, 5 % -2, -5 % 2, 5.25 % 1, -0.0 == 0, 1 / -0.0)
-- constants
local kn, ks, kb, kt = 1.5, "str", true, {1, 2, "three", k = "v", [10] = 10}
p(kn, ks, kb, kt[1], kt[3], kt.k, kt[10], 32767, -32768, 65536, 1e100)
local n1, n2, n3, n4, n5 = nil
p(n1, n2, n3, n4, n5)
-- concat
p("a" .. "b" .. 1 .. 2.5 .. "c", 1 .. "", -0.0 .. "", 1e100 .. "")
-- upvalues: get/set of each kind
local uv = 1
local function setters() uv = "s" ; local a = uv; uv = 2.5; local b = uv; uv = true; local c = uv; uv = nil; return a, b, c, uv end
p(setters())
-- closures capturing loop variables, UCLO
local fns = {}
for i = 1, 3 do local j = i * 10 fns[i] = function() j = j + 1 return i, j end end
p(fns[1](), fns[1](), fns[3]())
local acc = {}
local k = 1
while k <= 3 do local kk = k acc[k] = function() return kk end k = k + 1 end
p(acc[1](), acc[2](), acc[3]())
repeat local r = 5 acc[4] = function() return r end until true
p(acc[4]())
-- tables
local tt = {}
tt.a = 1 tt["b"] = 2 tt[1] = "one" tt[2.5] = "f" tt[true] = "t" tt[tt] = "self"
p(tt.a, tt.b, tt[1], tt[2.5], tt[true], tt[tt], tt.missing, #tt)
local tm = {1, 2, (function() return 3, 4, 5 end)()}
p(#tm, tm[5])
local function three() return 7, 8, 9 end
local tm2 = {three(), three()}
p(#tm2, tm2[1], tm2[2], tm2[4])
local big = {}
for i = 1, 100 do big[i] = i end
p(#big, big[100])
-- globals
gx = 5 p(gx, rawget(_G, "gx")) gx = nil p(gx)
-- calls, varargs, multiple results
local function va(...) local a, b = ... return select('#', ...), a, b, ... end
p(va()) p(va(1)) p(va(1, 2, 3)) p((va(1, 2, 3)))
local function pass(...) return ... end
p(pass(1, nil, 3, nil))
p(select('#', pass(nil, nil)))
local function tailv(...) return pass(...) end
p(tailv("t", 1))
local function fixed(a, b, c) return a, b, c end
p(fixed(1), fixed(1, 2, 3, 4))
local vt = {pass(1, 2, 3)}
p(#vt)
-- method calls
local obj = {v = 42}
function obj:get(d) return self.v + (d or 0) end
p(obj:get(), obj:get(8), obj.get(obj, 1))
-- numeric for: int/float/negative/zero-step-free
local s = "" for i = 1, 3 do s = s .. i end p(s)
s = "" for i = 3, 1, -1 do s = s .. i end p(s)
s = "" for i = 0, 1, 0.25 do s = s .. i .. "," end p(s)
s = "" for i = "1", "3" do s = s .. i end p(s)
s = "" for i = 1, 3 do local i = i * 2 s = s .. i end p(s)
-- generic for with next/pairs/ipairs/custom iterators
local cnt = 0 for k, v in next, {a = 1, b = 2} do cnt = cnt + v end p(cnt)
cnt = 0 for k, v in pairs({1, 2, 3, x = 4}) do cnt = cnt + v end p(cnt)
cnt = 0 for i, v in ipairs({5, 6, nil, 8}) do cnt = cnt + i end p(cnt)
local function range(n) local i = 0 return function() i = i + 1 if i <= n then return i end end end
cnt = 0 for i in range(4) do cnt = cnt + i end p(cnt)
local mod = {}
for k, v in pairs({10, 20, 30}) do mod[#mod + 1] = k * v end p(#mod, mod[3])
-- break / goto / continue
for i = 1, 10 do if i == 3 then break end p("brk", i) end
do local i = 1 ::top:: if i <= 2 then p("goto", i) i = i + 1 goto top end end
for i = 1, 4 do if i % 2 == 1 then continue end p("cont", i) end
-- bit operators
p(5 & 3, 5 | 3, 5 ~ 3, ~5, 1 << 4, 256 >> 4, -1 >> 28, -16 >> 2, 1 << 32, 1 << 31, 2.5 & 7, 0xffffffff & 1)
-- new operators
local o = {a = {b = 3}}
p(o?.a?.b, o?.z?.b, nil ?? "d", false ?? "f", 1 != 1, !nil, nil && 1, 1 || 2)
local ca = 10 ca += 5 ca -= 3 ca *= 2 p(ca, (ca > 20) ? "gt" : "le")
