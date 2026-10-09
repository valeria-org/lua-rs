-- Allocation-heavy: short-lived tables, closures and strings.
local s = 0
for i = 1, 3e6 do
  local t = {i, i + 1, x = i}
  s = s + t[1] + t.x
end
local keep = {}
for i = 1, 2e6 do keep[i % 1000 + 1] = {tostring(i), function() return i end} end
for i = 1, 1e6 do local str = "s" .. i; s = s + #str end
print(s, collectgarbage("count") > 0)
