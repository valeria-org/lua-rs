
local function a(x)
  if x == 1 then return end
  if x == 2 then return x end
  if x == 3 then return x, x end
  if x == 4 then return f(x) end
  if x == 5 then return x:m() end
  local up = x
  local cl = function() return up end
  if x == 6 then return cl end
  return f(up)
end
local function b(...)
  if ... then return ... end
  if select('#', ...) > 1 then return f(...) end
  local v = ...
  return function() return v end
end
local function c(x)
  while x do
    if x then return 1 end
    x = function() return x end
  end
end
return a, b, c
