
local a, b = ...
local r
r = a == b; r = a ~= b; r = a != b; r = a < b; r = a <= b; r = a > b; r = a >= b
r = a == nil; r = nil == a; r = a ~= nil; r = a == true; r = a == false; r = false ~= a
r = a == 1; r = 1 == a; r = a ~= 1; r = a == 1.5; r = a == -1
r = a == "s"; r = "s" == a; r = a ~= "s"
r = a < 1; r = 1 < a; r = a <= 1; r = 1 <= a; r = a > 1; r = 1 > a; r = a >= 1; r = 1 >= a
r = 1 < 2; r = 1 == 1; r = "a" < "b"; r = nil == nil; r = 1 == "1"
r = a == b == true
r = not (a == b)
r = not (a < b)
r = a.x == b.y
r = f() == g()
r = a == 1LL
r = a < 1LL
if a == b then end
if a ~= b then end
if a < b then end
if a <= b then end
if a > b then end
if a >= b then end
if a == nil then end
if a ~= nil then end
if a == 0 then end
if 0 == a then end
if a ~= "x" then end
if a == true then end
if a ~= false then end
if not (a > b) then end
if not (a ~= 1) then end
if a > 1 then end
if 1 > a then end
if a >= 1 then end
if a < 1 or a > 10 then end
if 1 then end
while a < b do a = a + 1 end
