
local function r0() return end
local function r1() return 1 end
local function r2() return 1, 2 end
local function r3(a) return a end
local function r4(a, b) return b, a end
local function rv(...) return ... end
local function rf(...) return f(...) end
local function rt(x) return x(1, 2) end
local function rm(o) return o:m(1) end
local function rtv(...) return 1, ... end
local function rfc() return 1, f() end
local function rpar() return (f()) end
local function rn() return nil end
local function rtf() return true, false end
local function rs() return "s" end
local function rk() return 1.5 end
local function rt2() return {1, 2} end
local function rcl() return function() end end
local function rcond(a) if a then return 1 else return 2 end end
local function rexpr(a, b) return a + b end
local function rsemi() return; end
local function rnested() do return 1 end end
local function rloop() for i = 1, 2 do return i end end
local function rdeep(a) return a.b.c.d end
local function rstr() return f"x" end
local function rtab() return f{1} end
return r0, r1
