
local u = 1
local s = "x"
local function f(v)
  u = v
  u = 1
  u = "str"
  u = nil
  u = true
  u = false
  u = 1.5
  u = {}
  u = u
  s = s .. "y"
  u, s = s, u
  u = function() return u end
end
local function g()
  local w = u
  w = s
  return w
end
return f, g
