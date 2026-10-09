
local a = 1
local function l1()
  local b = 2
  return function()
    local c = 3
    return function()
      local d = 4
      return function()
        local e = 5
        return function()
          a = a + b + c + d + e
          return a
        end
      end
    end
  end
end
local function l2()
  return function() return function() return function() return a end end end
end
return l1, l2
