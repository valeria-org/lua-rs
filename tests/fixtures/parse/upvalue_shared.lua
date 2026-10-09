
local function outer()
  local a, b, c, d = 1, 2, 3, 4
  local function f1() return a, b end
  local function f2() return c, d, a end
  local function f3() a = 10; return f1, f2 end
  return function()
    return function()
      return function() return a, b, c, d, f1, f2, f3 end
    end
  end
end
local x = 1
local function rec(n) if n > 0 then return rec(n - 1) end return x end
return outer, rec
