
local a = f()
local b, c = f()
local d, e, g = f(), 1
local h, k, l = 1, f()
local m, n = f(), f()
f()
f(g())
f(g(), h())
f(1, g())
f(g(), 1)
f((g()))
local p = {f()}
local q = {f(), f()}
local r = {(f())}
local s = #f()
local tt = f() + g()
f()()
f()()()
f().x = 1
f()[1] = 2
f():m()
o:m():n():p()
f"str":m{1}
f{1}"s"
f[[long]]
f[==[long2]==]
print(f(1,2,3))
local u1, u2, u3, u4, u5 = g(), g(), g()
