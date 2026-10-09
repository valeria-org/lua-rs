
local i = 0
while i < 10 do i = i + 1 end
while true do
  i = i - 1
  if i < 0 then break end
end
while false do print("never") end
while i do
  local x = i
  while x > 0 do
    x = x - 1
    if x == 5 then break end
  end
  break
end
while not i do end
while i ~= 3 and i < 100 do i = i + 1 end
