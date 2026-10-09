
const a = 1
const b, c = 2, 3
const s = "str"
const t = {1, 2}
const n = nil
const function f() return a + b end
local function g() return a, s, t end
const x = a + b
t[1] = 5
t.x = 6
do
  const y = 10
  local h = function() return y end
end
const k = ...
const u, v = f()
local const = 1
const = 2
const.x = 3
print(const)
const z = const
local function rec()
  const r = 1
  local function inner() return r end
  return inner
end
for i = 1, 2 do
  const ci = i
  print(ci)
end
const ff = function() return ff end
