local a, b = 1, "two\n\t\"q\"\0end"
local t = {1, 2, 3, x = {y = "z"}, [10] = false, 1.5, -0.0, 1e300, 0/0}
local function vararg(...)
  local n = select('#', ...)
  for i = 1, n do
    local v = select(i, ...)
    if v == nil then goto continue end
    t[#t + 1] = v
    ::continue::
  end
  return ...
end
local function closure(x)
  local y = x
  return function(z) y = y + z; return y, x end
end
for k, v in pairs(t) do if k == "x" then break end end
for i = 10, 1, -2 do a = a + i * 2 ^ 3 % 7 - (a or 0) end
while a < 1000 do a = a * 2 end
repeat a = a - 1 until a < 0
local s = b .. a .. "x"
local u = not a and #t or -a
t.x.y = t[1] and t[2] or t[3]
print(vararg(1, nil, 3), closure(5)(6), s, u, a ~= b, a == 1, a <= 2, "a" < "b")
local long = "this is a rather long string constant with more than forty characters"
return long, {f = function() return 65535, 65536, -32768, -32769 end}
