local t = setmetatable({}, {__gc = function() print("finalized") end})
print("end")
