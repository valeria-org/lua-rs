-- table.sort with and without comparator.
local seed = 42
local function rnd() seed = (seed * 1103515245 + 12345) % 2147483648; return seed end
local s = 0
for r = 1, 5 do
  local t = {}
  for i = 1, 2e5 do t[i] = rnd() end
  table.sort(t)
  s = s + t[1]
  local u = {}
  for i = 1, 1e5 do u[i] = rnd() end
  table.sort(u, function(a, b) return a > b end)
  s = s + u[1]
end
print(s)
