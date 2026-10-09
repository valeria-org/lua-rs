
local t = {1, 2, 3, a = 4}
for k, v in pairs(t) do print(k, v) end
for k in pairs(t) do print(k) end
for k, v in next, t do print(k, v) end
for k, v in next, t, nil do print(k, v) end
for i, v in ipairs(t) do print(i, v) end
for a, b, c, d, e in pairs(t) do end
for a, b, c, d, e, f in pairs(t) do end
for a, b, c, d, e, f, g in next, t do end
for line in io.lines("x") do print(line) end
for w in string.gmatch("a b", "%a+") do end
local function iter(s, c) if c < 3 then return c + 1 end end
for i in iter, nil, 0 do print(i) end
for k, v in pairs(t), 1 do end
for k, v in t do end
for x, y in coroutine.wrap(function() end) do end
