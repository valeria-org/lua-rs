
foo = 1
bar = "x"
baz = foo
foo = bar
qux = nil
a, b, c = 1, 2, 3
a, b = b, a
print(foo, bar, baz)
_G.x = 1
_G["y z"] = 2
local t = _G
t.a.b.c = 4
