-- Varargs, select, unpack, multiple returns.
local function sum(...)
  local s = 0
  for i = 1, select("#", ...) do s = s + (select(i, ...)) end
  return s
end
local function pass(...) return ... end
local function pack(...) return {...} end
local s = 0
for i = 1, 2e6 do s = s + sum(i, 2, 3, 4) + select("#", pass(1, 2, 3)) end
for i = 1, 2e6 do local t = pack(i, i, i); s = s + #t + unpack(t) end
print(s)
