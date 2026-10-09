
local function a()
  local x = 1
  local f = function() return x end
  return f
end
local function b(n)
  local x = n
  if n > 0 then
    local g = function() return x end
    return g()
  end
  return 0
end
local function c()
  for i = 1, 3 do
    local h = function() return i end
    if i == 2 then return h end
  end
end
local function d(a)
  if a then return 1 end
  local y = a
  local f = function() return y end
  return f
end
local function e(a)
  if a then return a end
  if not a then return end
  local z = function() return a end
  return z
end
local function e2(a, ...)
  if a then return a, 1 end
  if a == 1 then return ... end
  local z = function() return a end
  return z
end
local function fx(a)
  while a do
    if a then return end
  end
  local b = function() a = 1 end
end
return a, b, c, d, e
