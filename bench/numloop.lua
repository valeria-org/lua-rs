-- Numeric for loops and arithmetic.
local s = 0
for i = 1, 3e7 do s = s + i % 7 * 2 - 1 end
local x = 1.0
for i = 1, 1e7 do x = x * 1.0000001 + 0.5 / i end
print(s, string.format("%.6f", x))
