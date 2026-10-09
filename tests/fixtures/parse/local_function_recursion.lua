
local function fib(n)
  if n < 2 then return n end
  return fib(n - 1) + fib(n - 2)
end
local fact
fact = function(n) return n <= 1 and 1 or n * fact(n - 1) end
local function even(n) if n == 0 then return true end return odd(n - 1) end
function odd(n) if n == 0 then return false end return even(n - 1) end
return fib(10), fact(5)
