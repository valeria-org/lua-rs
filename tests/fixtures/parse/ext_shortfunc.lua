
local f
f = x -> x + 1
f = x -> x
f = |a, b| -> a + b
f = |a| -> a * 2
f = || -> 42
f = |...| -> ...
f = |a, ...| -> select('#', ...)
f = || -> do print("hi") end
f = |x| -> do local y = x * 2; return y end
f = x -> do return x end
f = || -> do end
f = x -> y -> x + y
f = |a| -> |b| -> a .. b
f = x -> x ? 1 : 2
f = x -> x ?? 0
f = x -> (x)
f = x -> {x}
f = x -> f(x)
f = x -> x:m()
f = || -> nil
local up = 10
f = x -> x + up
f = || -> do up = up + 1 end
local t = {map = x -> x * 2, filter = |x| -> x > 0}
g(x -> x * x)
g(|a, b| -> a < b)
table.sort(t, |a, b| -> a > b)
local id = x -> x
local cmp = |a, b| -> a == b
f = x -> x, 2
f = (x -> x)
f = || -> do return 1, 2 end
f = |a, b, c, d| -> a + b + c + d
f = || -> f
f = || -> || -> || -> 1
