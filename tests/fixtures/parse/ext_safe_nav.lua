
local a, k = ...
local r
r = a?.b
r = a?.b?.c
r = a?.b.c
r = a.b?.c
r = a?.[k]
r = a?.[1]?.["x"]
r = a?.b:c()
r = a?.b:c(1, 2)
r = a:b?.()
r = a:b?.(1)
r = a?.b?.()
r = a?.("x")
r = a?.{1}
r = a?.b"s"
r = a?.b()()
r = a?.b().c
r = a?.b():c()
r = (a?.b)
r = a?.b + 1
r = a?.b or 2
r = a?.b ?? 3
local x, y = a?.b()
local p, q, s = a?.b()
local u, v, w = a?.b(), 1
x = a?.b()
f(a?.b())
f(a?.b(), 1)
local t = {a?.b()}
local t2 = {a?.b}
a?.b()
a?.b:c()
a:b?.()
a?.b.c = 1
a?.b[k] = 2
a?.b.c.d = 3
a?.x += 1
a?.b?.c = 4
a.b?.c = 5
if a?.b then end
if a?.b == 1 then end
while a?.b?.c do break end
return a?.b
