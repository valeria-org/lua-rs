
for i = 1, 10 do
  if i == 2 then continue end
  if i == 8 then break end
  print(i)
end
local i = 0
while i < 10 do
  i = i + 1
  if i % 2 == 0 then continue end
  print(i)
end
repeat
  i = i - 1
  if i == 5 then continue end
until i <= 0
for k, v in pairs(t) do
  if not v then continue end
  local f = function() return v end
  if k then break end
end
for j = 1, 3 do
  local x = j
  local g = function() return x end
  if x then continue end
end
repeat
  local y = 1
  local h = function() return y end
  if i then continue end
until true
-- continue as soft keyword used as identifier
continue = 1
continue.x = 2
local continue = 3
print(continue)
continue = continue + 1
continue()
for z = 1, 2 do
  do continue end
end
for z = 1, 2 do
  for w = 1, 2 do
    if w then continue end
    break
  end
  continue
end
