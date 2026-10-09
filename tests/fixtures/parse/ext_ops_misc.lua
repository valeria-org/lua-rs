
local a, b = ...
local r
r = a && b
r = a || b
r = a && b || r
r = !a
r = a != b
r = a != nil
r = a != 1
r = a != "s"
if a != b && !b || a then end
while a && !b do break end
r = !(a && b)
r = a != b == true
