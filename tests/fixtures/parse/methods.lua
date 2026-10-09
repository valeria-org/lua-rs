
local obj = {}
function obj.f(a) return a end
function obj:m(a) return self, a end
function obj.a.b.c(x) end
function obj.a.b.c:m2() return self end
function gf() end
function gt.x() end
function gt.x.y:z(...) return ... end
local function lf() end
obj:m(1)
obj.a.b:c(1, 2)
obj["m"](obj, 1)
obj:m"s"
obj:m{1}
obj:m()
local r = obj:m(1):m(2)
local s = obj.a:b().c:d()
local function lm()
  local self = 1
  function obj:x() return self end
end
local k = "key"
obj[k]:m()
obj[1]:m()
obj.long_method_name_that_exceeds_things:another_long_method_name()
