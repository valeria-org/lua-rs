
local fs = {}
for i = 1, 10 do
  fs[i] = function() return i end
end
for k, v in pairs({a=1}) do
  fs[k] = function() return k, v end
end
local j = 0
while j < 10 do
  local x = j
  fs[#fs+1] = function() return x end
  j = j + 1
end
repeat
  local y = j
  fs[#fs+1] = function() return y end
  j = j - 1
until y < 0
do
  local z = 1
  fs.z = function() z = z + 1 return z end
end
for i = 1, 3 do
  local a = i
  if a == 2 then break end
  fs[a] = function() return a end
end
