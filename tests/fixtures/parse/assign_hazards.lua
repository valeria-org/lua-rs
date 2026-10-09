
local t, i = {}, 1
t[i], i = 1, 2
t[i], t = 1, 2
i, t[i] = i + 1, 20
t, t[1] = {}, 3
local a, b, c = {}, 1, 2
a[b], b, a = b, a, 3
a[b][c], b = 1, 2
a.x, a = 1, 2
a[b], a[c], b, c = 1, 2, 3, 4
local o = {}
o[i], o, i = 1, 2, 3
