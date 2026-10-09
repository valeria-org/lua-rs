-- Library table layout (pairs order is checked sorted) and builtin ids.
local function keys(t) local r = {} for k in pairs(t) do r[#r + 1] = k end table.sort(r) return table.concat(r, " ") end
print(keys(string)) print(keys(table)) print(keys(math)) print(keys(bit))
print(string.byte, string.format, string.gmatch("", ""), table.insert, math.abs, math.randomseed, bit.tohex)
print(type(string.len), type(table.remove), type(math.deg))
local b = require("string.buffer")
local o = b.new()
print(keys(b), getmetatable(o), o.put, o.decode, o.__len, o.tostring == o.__tostring, o.__gc)
print(b.new, b.encode, getmetatable("").__index == string)
