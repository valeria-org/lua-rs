
local a, b, c = ...
local x
x = a > b and a or b
x = (a > b) == (b > c)
x = not (a and b) or c
x = a and b or not c
x = (a or b) and (c or a)
x = (a and nil) or b
x = a and (b == c)
x = (a == b) or (b == c) or (c == a)
x = a < b and b < c and c < 10
if a then x = 1 elseif b then x = 2 elseif c then x = 3 end
if (a > 0) == true then end
if (a and b) == nil then end
if a == (b and c) then end
local y = a == nil and 1 or a
local z = (a or b)
z = a
z = z or 5
z = z and 5
