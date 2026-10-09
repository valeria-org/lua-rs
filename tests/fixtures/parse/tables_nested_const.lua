
local t = {
  a = {1, 2, 3},
  b = {x = "y", z = {true, false}},
  c = {{1}, {2}, {3, {4, {5}}}},
  [1] = "one",
  [2] = {nested = {deeper = {deepest = 1}}},
  "pos1", "pos2",
}
local u = {1, {2, {3, {4, {5, {6}}}}}}
local v = {{a = 1}, {a = 2}, {a = 3, f = function() end}}
local w = {x = {y = {z = ...}}}
return t, u, v, w
