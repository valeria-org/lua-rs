
local function f()
  local i = 0
  ::top::
  do
    local x = i
    local g = function() return x end
    i = i + 1
    if i < 3 then goto top end
  end
  ::bottom::
  if i < 5 then i = i + 1; goto bottom end
end
local function g()
  for i = 1, 10 do
    local c = function() return i end
    if i == 5 then goto done end
  end
  ::done::
end
local function h()
  local y = 0
  ::l::
  local z = y
  local fn = function() return z end
  y = y + 1
  if y < 3 then goto l end
  return fn
end
return f, g, h
