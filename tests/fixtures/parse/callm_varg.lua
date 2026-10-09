
local function f(...)
  g(...)
  g(1, ...)
  g(1, 2, ...)
  g(h(...))
  g(1, h(...))
  local t = {...}
  local u = {1, ...}
  local v = {h(...)}
  o:m(...)
  o:m(1, ...)
  return g(...)
end
local function k(...)
  return o:m(...)
end
local function l(...)
  return 1, 2, g(...)
end
return f, k, l
