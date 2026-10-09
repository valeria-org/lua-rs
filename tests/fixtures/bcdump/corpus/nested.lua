-- Nested prototypes, upvalues (local and parent refs, immutable), varargs, methods.
local M = {}
local counter = 0
local function inc(n) counter = counter + (n or 1); return counter end

function M.new(name, ...)
  local self = { name = name, args = { ... }, n = select("#", ...) }
  local secret = name .. "!"
  function self:get() return secret, self.name end
  function self.set(v) secret = v end
  self.deep = function(a)
    return function(b)
      return function(c, ...)
        local d = a + b + c + inc()
        return d, secret, ..., function() return d * counter end
      end
    end
  end
  return setmetatable(self, { __index = M, __tostring = function(s) return "M(" .. s.name .. ")" end })
end

function M:method(x, y, z)
  local r = {}
  for i = 1, x do r[#r + 1] = function() return i + y + (z or 0) end end
  return r
end

local function rec(n) if n <= 0 then return 0 end return n + rec(n - 1) end
local co = coroutine.wrap(function(...) local a, b = ...; coroutine.yield(a + b) return rec(10) end)

return M, inc, rec, co, function(...) return select("#", ...), ... end
