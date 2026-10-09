
local a, b, c, d = ...
local r
r = a ? b : c
r = a ? 1 : 2
r = a ? "x" : "y"
r = a ? nil : false
r = a == 1 ? b : c
r = a and b ? c : d
r = a ? b : c ? d : 1
r = a ? (b ? 1 : 2) : (c ? 3 : 4)
r = (a ? b : c) + 1
r = a ? b + 1 : c * 2
r = a ? f() : g()
r = a ? (b:m()) : c:m()
r = a ? {} : {1}
r = a ? function() end : nil
r = true ? 1 : 2
r = false ? 1 : 2
r = nil ? 1 : 2
r = a ? b : c .. d
f(a ? b : c, d)
local t = {a ? b : c, x = b ? 1 : 2}
local x, y = a ? 1 : 2, b ? 3 : 4
if (a ? b : c) then end
return a ? b : c
