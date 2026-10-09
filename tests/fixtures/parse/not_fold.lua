
local a = ...
local r
r = not nil
r = not false
r = not true
r = not 1
r = not 0
r = not "s"
r = not not nil
r = not not 1
r = not a
r = not not a
r = not not not a
r = not (a == 1)
r = not (a ~= 1)
r = not {}
r = not 1LL
r = !a
r = !!a
r = !(a < 2)
if not nil then end
if not true then end
if not a then end
if not not a then end
if !a then end
