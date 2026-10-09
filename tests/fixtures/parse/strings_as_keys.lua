
local t = {}
t["a"] = 1
t.a = 2
t["not an identifier"] = 3
t["123"] = 4
t[""] = 5
t["\0"] = 6
print(t["a"], t.a, t["123"])
local s = "key"
t[s] = 7
