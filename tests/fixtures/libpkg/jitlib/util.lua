function sortedkeys(t)
  local ks = {}
  for k in pairs(t) do
    local i = #ks + 1
    ks[i] = k
    while i > 1 and tostring(ks[i-1]) > tostring(ks[i]) do ks[i], ks[i-1] = ks[i-1], ks[i]; i = i - 1 end
  end
  return ks
end
function dumpt(t, pre)
  for _, k in ipairs(sortedkeys(t)) do
    local v = t[k]
    local tv = type(v)
    if tv == "function" or tv == "table" or tv == "userdata" or tv == "thread" or tv == "proto" then v = tv end
    print((pre or "") .. tostring(k), v)
  end
end
