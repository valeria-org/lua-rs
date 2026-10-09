
local x = ...
local r = ~x
r = ~~x
r = ~-x
r = -~x
r = ~#x
r = not ~x
r = ~(x)
r = ~x.y
r = ~f()
r = ~"5"
r = ~5.5
r = ~2^31
return ~x
