
local a, b, c, d = ...
local r
r = a and b
r = a or b
r = a and b or c
r = a or b and c
r = (a or b) and c
r = a and (b or c)
r = a and b and c and d
r = a or b or c or d
r = a and b or c and d
r = not a and b
r = not (a and b)
r = not (a or b)
r = a and 1 or 2
r = a and nil or 2
r = a or nil
r = nil or a
r = nil and a
r = false or a
r = true and a
r = true or a
r = 1 and 2
r = 1 or 2
r = nil or false
r = a == 1 and b == 2
r = a == 1 or b == 2
r = a < b and b < c or c < d
r = a and f() or g()
r = (a or b).x
r = a and {} or {1}
r = a or "default"
r = a and a.b and a.b.c
r = x and y
r = a or b or 1
local t = {a and b, a or b}
f(a and b, a or b)
if a and b or c then end
if a or b and c then end
if (a and b) or (c and d) then end
if not (a and b) then end
if a and not b then end
while a and b do end
repeat until a or b
if nil and a then end
if true or a then end
if a and true then end
if a or false then end
r = (a and b) == (c or d)
