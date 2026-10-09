
do
  goto fwd
  print("skipped")
  ::fwd::
end
do
  local i = 1
  ::top::
  i = i + 1
  if i < 3 then goto top end
end
do
  for i = 1, 3 do
    for j = 1, 3 do
      if j == 2 then goto next_i end
    end
    ::next_i::
  end
end
do
  local x = 1
  ::a:: ::b::
  if x then goto a end
  goto b
end
do
  goto done
  local y = 1
  print(y)
  ::done::
end
do
  local k = 0
  ::again::
  local z = k
  local f = function() return z end
  k = k + 1
  if k < 3 then goto again end
end
do
  while true do
    local q = 1
    local f = function() return q end
    goto out
  end
  ::out::
end
do
  if a then goto l1 else goto l2 end
  ::l1:: print(1)
  ::l2:: print(2)
end
local goto = 1
goto = goto + 1
print(goto)
do goto e ::e:: end
