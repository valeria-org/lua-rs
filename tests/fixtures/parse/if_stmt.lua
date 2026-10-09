
local a, b, c = ...
if a then print(1) end
if a then print(1) else print(2) end
if a then print(1) elseif b then print(2) end
if a then print(1) elseif b then print(2) elseif c then print(3) else print(4) end
if not a then print(5) end
if a and b then print(6) end
if a or b then print(7) end
if a and b or c then print(8) end
if (a or b) and c then print(9) end
if true then print(10) end
if false then print(11) end
if nil then print(12) end
if 1 then print(13) end
if "x" then print(14) end
if a then else print(15) end
if a then elseif b then else end
if a == nil then x = 1 end
if not (a == b) then x = 2 end
if a then local q = 1; print(q) end
if a then return end
if b then return 1 end
print("end")
