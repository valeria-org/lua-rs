
local function f(n)
  ::retry::
  do
    local a = n
    do
      local b = a
      if b > 0 then n = n - 1 goto retry end
      local c = function() return a + b end
      if c() then goto exit end
    end
  end
  ::exit::
  return n
end
local function g()
  do
    local x = 1
    goto skip
    local cl = function() return x end
    ::skip::
  end
  for i = 1, 3 do
    local v = i
    local fn = function() return v end
    if i == 2 then goto cont end
    print(fn())
    ::cont::
  end
end
return f, g
