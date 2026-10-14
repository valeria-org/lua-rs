-- Lua 5.4 syntax: attributes, operators, labels, and the common statement forms.
local immutable <const>, resource <close> = 0x1.fp+3, acquire()
local bits = (1 << 3) & ~2 | (5 ~ 1) >> 1
local quotient = 9 // 2
local text = "\u{1F980}" .. [==[long string]==]
local function sequence(...)
    return {...}
end
function module:method(a, b)
    if a > b then
        return a ^ 2
    elseif a == b then
        return #text
    else
        return -b
    end
end
do
    local t = {["key"] = true, nil, 3; named = false}
    for i = 1, 10, 2 do t[i] = i % 3 end
    for key, value in pairs(t) do print(key, value) end
    while quotient > 0 do quotient = quotient - 1 end
    repeat quotient = quotient + 1 until quotient == 2
end
::again::
if quotient > 2 then goto again end
goto finished
::finished::
return immutable, bits, sequence(1, 2)
