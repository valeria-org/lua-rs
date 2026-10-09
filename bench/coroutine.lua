-- Coroutine switching.
local co = coroutine.wrap(function() local i = 0; while true do i = i + 1; coroutine.yield(i) end end)
local s = 0
for i = 1, 1.2e7 do s = s + co() end
local function gen(n) return coroutine.wrap(function() for i = 1, n do coroutine.yield(i) end end) end
for r = 1, 4000 do for v in gen(1000) do s = s + v end end
print(s)
