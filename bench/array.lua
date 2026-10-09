-- Table array part read/write.
local t = {}
for i = 1, 1e6 do t[i] = i end
local s = 0
for r = 1, 30 do
  for i = 1, #t do s = s + t[i] end
  for i = 1, #t do t[i] = t[i] + 1 end
end
print(s)
