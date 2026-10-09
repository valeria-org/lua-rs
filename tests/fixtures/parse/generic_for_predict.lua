
local t = {}
do
  local pairs = pairs
  for k, v in pairs(t) do end
end
do
  local next = next
  for k, v in next, t do end
end
do
  local mypairs = pairs
  for k, v in mypairs(t) do end
end
local function f()
  for k, v in pairs(t) do end
end
local pairs, next = pairs, next
local function g()
  for k, v in pairs(t) do end
  for k, v in next, t do end
end
local function h()
  local pairs = 1
  for k in pairs do end
end
for k, v in (pairs)(t) do end
for k, v in t.pairs(t) do end
for k, v in pairs, t do end
