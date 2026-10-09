
local function v1(...) return ... end
local function v2(...) local a, b = ... return a, b end
local function v3(...) local t = {...} return t end
local function v4(...) local t = {1, 2, ...} return t end
local function v5(...) local t = {..., 1} return t end
local function v6(...) return select('#', ...) end
local function v7(...) return select(2, ...) end
local function v8(a, b, ...) print(a, b, ...) end
local function v9(...) local a = ... return a end
local function v10(...) local a, b, c, d, e = ... end
local function v11(...) a, b = ... end
local function v12(...) a = ... end
local function v13(...) t[1], t[2] = ... end
local function v14(...) return (...) end
local function v15(...) local x = (...) end
local function v16(...) f(..., 1) end
local function v17(...) f(1, ...) end
local function v18(...) return ..., ... end
local function v19(...) local x, y = 1, ... end
local function v20(...) local x, y, z = ..., 2 end
local function v21(...) return {n = select('#', ...), ...} end
local function v22(...) return ... and 1 end
local function v23(...) return #{...} end
local function v24(...) local f = function(...) return ... end return f(...) end
local function v25(...) for i = 1, select('#', ...) do local v = select(i, ...) end end
local function v26(...) x.y, x.z = ... end
local a, b = ...
print(...)
return ...
