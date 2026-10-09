
local i = 0
repeat i = i + 1 until i > 10
repeat
  local x = i * 2
  i = i - 1
until x < 3
repeat
  local y = i
  local f = function() return y end
  i = i + 1
until y > 20
repeat
  local z = i
  if z == 3 then break end
  local g = function() return z end
until g() > 5
repeat until true
repeat break until false
repeat
  local w = i
  function cb() return w end
until w
