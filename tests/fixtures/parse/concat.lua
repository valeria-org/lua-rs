
local a, b, c = ...
local r
r = a .. b
r = a .. b .. c
r = "x" .. a
r = a .. "x"
r = "a" .. "b"
r = "a" .. "b" .. "c" .. a .. "d" .. "e"
r = 1 .. 2
r = a .. 1
r = 1 .. a
r = a .. b .. c .. a .. b .. c .. a .. b .. c
r = (a .. b) .. c
r = a .. (b .. c)
r = a .. f() .. g()
r = a .. b + 1
r = a + 1 .. b
r = a .. b == c
f(a .. b, b .. c)
local t = {a .. b}
r = a.x .. a.y
r = (a or b) .. c
r = a .. (b and c)
