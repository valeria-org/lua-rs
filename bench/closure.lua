-- Closures and upvalues.
local function counter()
  local n = 0
  return function(d) n = n + d; return n end
end
local s = 0
for i = 1, 1e6 do local c = counter(); s = s + c(i) + c(1) end
local acc = counter()
for i = 1, 2e7 do acc(1) end
print(s, acc(0))
