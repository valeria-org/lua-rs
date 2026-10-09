
local f = function() end
local g = function(a) return a end
local h = function(a, b, c) return a, b, c end
local i = function(...) return ... end
local j = function(a, ...) return a, ... end
local k = function(self) return self end
local m = (function() return 1 end)()
local n = function() return function() return function() end end end
function globalf(a, b) end
local function localf(a, b) return localf end
local t = {f = function() end}
t.g = function() end
local unused = function(a, b, c, d, e, f, g, h, i, j) return j end
local recursive
recursive = function(n) return n > 0 and recursive(n - 1) or 0 end
