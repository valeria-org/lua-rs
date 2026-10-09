local a = nil
local t = {x = {y = 5}}
print(t?.x?.y, a?.b, a ?? "dflt", false ?? 1, 1 != 2, !true, true && false, nil || 3)
local b = 5
b += 2 b -= 1 b *= 3 b /= 2 b %= 5 b ..= "s"
print(b)
local c = 10
print(c, (c > 5) ? "big" : "small")
local sq = function(x) return x * x end
print(sq(4))
for i = 1, 5 do if i % 2 == 0 then continue end print(i) end
print(~0, 1 << 33, 0xff >> 4, 5 ~ 1, -1 >> 1, 3 & 5 | 8)
