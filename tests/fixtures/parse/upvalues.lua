
local a, b = 1, 2
local c = 3
local function f()
  return a + b
end
local function g()
  c = c + 1
  return c
end
local function h()
  local x = 1
  local function h2()
    local y = 2
    local function h3()
      local z = x + y
      return function() return x + y + z + a + c end
    end
    x = x + 1
    return h3
  end
  return h2
end
local imm = "immutable"
local mut = "mutable"
mut = "changed"
local function k() return imm, mut end
local function setter(v) imm = v end
return f, g, h, k
