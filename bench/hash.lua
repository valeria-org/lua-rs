-- Table hash part: string keys, numeric keys, field access.
local keys = {}
for i = 1, 1000 do keys[i] = "k" .. i end
local t = {}
local s = 0
for r = 1, 2000 do
  for i = 1, 1000 do local k = keys[i]; t[k] = (t[k] or 0) + i end
end
for k, v in pairs(t) do s = s + v end
local p = {x = 1, y = 2, z = 3}
for i = 1, 1e7 do p.x = p.y + p.z; p.y = p.x - i end
local h = {}
for i = 1, 2e5 do h[i * 7.5] = i end
for r = 1, 10 do for i = 1, 2e5 do s = s + h[i * 7.5] end end
print(s, p.x)
