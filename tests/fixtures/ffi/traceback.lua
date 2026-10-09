local ffi = require("ffi")
print(xpcall(function()
    local x = (1ll).foo
  end, debug.traceback))
print(xpcall(function()
    local x = 1ll + {}
  end, debug.traceback))
print(xpcall(function()
    local x = ffi.new("int[3]")
    x.y = 1
  end, debug.traceback))
local t = setmetatable({}, {__index = function(t, k) return (1ll)[k] end})
print(xpcall(function() return t.z end, debug.traceback))
print(xpcall(function() return ffi.new("int", {}) end, debug.traceback))
