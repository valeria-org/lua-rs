
local function t1(f) return f() end
local function t2(f, ...) return f(...) end
local function t3(o) return o:method() end
local function t4(o) return o.a.b(1, 2, 3) end
local function t5(f) return f(f()) end
local function t6(f) return (f()) end
local function t7(f) return f "str" end
local function t8(f) return f { 1, 2 } end
local function t9(n) if n == 0 then return 0 end return t9(n - 1) end
local function t10(f, ...) return f(1, ...) end
local function t11(f) return f(g(), h()) end
return t1(t2)
