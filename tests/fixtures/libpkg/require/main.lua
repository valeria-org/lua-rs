
package.path = "./?.lua;./?/init.lua"
package.cpath = "./?.so"
print(require("m1").name, require("m1") == package.loaded.m1)
print(require("m2"), package.loaded.m2)
print(pcall(require, "m3"))
print(pcall(require, "m4"))
print(pcall(require, "m4"))
print(pcall(require, "m5"))
print(pcall(require, "nonexistent"))
print(pcall(require, "a.nope"))
print(require("m6").hello(), m6.print == print)
print(require("sub"))
print(require("a.b"))
print(pcall(require, "m7"))
print(require("m8"))
package.preload.pre = function(...) print("preload", ...) return 42 end
print(require("pre"))
print(pcall(require))
print(pcall(require, {}))
print(require(5 == 5 and "m1" or ""))
package.path = 7
print(pcall(require, "zz"))
package.path = "./?.lua"
package.preload = 1
print(pcall(require, "zz"))
package.preload = {}
local ol = package.loaders
package.loaders = nil
print(pcall(require, "zz"))
package.loaders = {}
print(pcall(require, "zz"))
package.loaders = { function(n) return "\n\tmine " .. n end, function(n) return 12 end, function() return nil end }
print(pcall(require, "zz"))
package.loaders = ol
print(package.searchpath("m1", "./?.lua;./?.x"))
print(package.searchpath("a.b", "./?.lua"))
print(package.searchpath("a.b", "./?.lua", "", ""))
print(package.searchpath("a_b", "./?.lua", "_", "/"))
print(package.searchpath("nope", ";;./?.lua;;x?y"))
print(pcall(package.searchpath))
print(pcall(module))
print(pcall(module, "zzmod"))
local t = setmetatable({}, {}) package.seeall(t) print(getmetatable(t).__index == _G)
print(pcall(package.seeall, 1))
x = 5
print(pcall(module, "x.y"))
local function f() module("q.r.s") end
f()
print(q.r.s._NAME, q.r.s._PACKAGE, q.r.s._M == q.r.s, getfenv(f) == q.r.s)
print(package.loaded["q.r.s"] == q.r.s)
module("q.r.s", function(m) print("opt1", m._NAME) end, function(m) print("opt2") end)
print(pcall(require, "jit.opt"))
package.path = "./?.lua"
print(require("sub") == package.loaded.sub, package.loaded["a.b"] ~= nil)
-- require a module given as number and with embedded NUL
print(pcall(require, 12))
print(pcall(require, "m1\0zzz"))
-- module() called through pcall (not from Lua), _LOADED entry not a table
package.loaded.notab = 5
print(pcall(module, "notab"))
