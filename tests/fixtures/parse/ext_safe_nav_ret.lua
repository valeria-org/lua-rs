
local function f1(a) return a?.b end
local function f2(a) return a?.b() end
local function f3(a) return a?.b:c() end
local function f4(a) return a:b?.() end
local function f5(a) return a?.b(), 1 end
local function f6(a) return 1, a?.b() end
local function f7(a, ...) return a?.b(...) end
local function f8(a) local x = a?.b return x end
local function f9(a) return (a?.b()) end
local function f10(a) local u = 1; local g = function() return u end; return a?.b() end
return f1
