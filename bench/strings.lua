-- String building, concatenation, format, buffers.
local s = 0
for r = 1, 50 do
  local t = {}
  for i = 1, 10000 do t[#t + 1] = "item" .. i .. ";" end
  s = s + #table.concat(t)
end
for i = 1, 1e6 do s = s + #string.format("%d:%s:%5.2f", i, "x", i / 3) end
local str = ""
for i = 1, 20000 do str = str .. "x" end
for i = 1, 2e6 do s = s + #tostring(i) + #(i .. "") end
print(s, #str, ("abc"):rep(3):upper(), string.sub("hello world", 2, 5))
