
local x = 0
local function step() x = x + 1 return x end
while step() < 5 do
  if x == 2 then
    for i = 1, 3 do
      repeat
        local y = i
        if y > 1 then break end
      until true
    end
  elseif x == 3 then
    do
      local z = x
      goto skip
      ::skip::
    end
  else
    print(x)
  end
end
