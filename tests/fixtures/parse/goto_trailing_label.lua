
do
  goto e
  local x = 1
  ::e::
end
for i = 1, 3 do
  if i == 2 then goto continue end
  local v = i
  ::continue::
end
while true do
  goto c
  local y = 2
  ::c:: ::d::
end
repeat
  goto r
  ::r::
  local z
until z
local function f()
  goto out
  local w
  ::out::
end
