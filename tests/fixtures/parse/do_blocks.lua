
do end
do local a = 1 end
do
  local b = 2
  do
    local c = b
    do print(c) end
  end
end
do local f = function() return d end local d = 1 end
do
  local x = 1
  local g = function() return x end
end
do
  local y = 1
  do
    local z = 2
    h = function() return y + z end
  end
end
