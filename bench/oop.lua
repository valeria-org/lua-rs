-- Method calls through metatables (__index chains).
local Point = {}
Point.__index = Point
function Point.new(x, y) return setmetatable({x = x, y = y}, Point) end
function Point:add(o) self.x = self.x + o.x; self.y = self.y + o.y; return self end
function Point:len2() return self.x * self.x + self.y * self.y end
local Point3 = setmetatable({}, {__index = Point})
Point3.__index = Point3
function Point3.new(x, y, z) local p = setmetatable(Point.new(x, y), Point3); p.z = z; return p end
local a, b = Point.new(0, 0), Point.new(1, 2)
local c = Point3.new(1, 1, 1)
local s = 0
for i = 1, 5e6 do a:add(b); s = s + c:len2() end
print(a.x, a.y, s)
