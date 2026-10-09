-- Control flow: numeric/generic for (internal VARNAME_* locals), loops, goto, bit ops.
local t, n = ...
local sum = 0
for i = 1, 10 do sum = sum + i end
for i = n, 1, -2 do sum = sum - i end
for k, v in pairs(t) do sum = sum + #tostring(k) + (tonumber(v) or 0) end
for i, v in ipairs(t) do if i > 3 then break end sum = sum + v end
for k in next, t do sum = sum + 1 end
local i = 0
while i < 100 do i = i + 7; if i % 3 == 0 then goto continue end; sum = sum + i ::continue:: end
repeat local j = i; i = i - 13 until j < 0
do
  local a, b, c = 1, 2, 3
  ::top::
  a = a + 1
  if a < 5 then goto top end
  sum = sum + a + b + c
end
local x, y = 0xF0, 0x3C
local bits = { x & y, x | y, x ~ y, ~x, x << 4, x >> 2 }
sum += bits[1]
sum -= 1
local ne = sum != 0 and not (sum == 1) and sum >= -1 and sum <= 1e9 and sum > -5 and sum < 1e10
local s = "a" .. "b" .. sum .. (ne and "y" or "n")
local cond = (t and n) or (not t and 1) or nil
return sum, bits, ne, s, cond, #t, -n
