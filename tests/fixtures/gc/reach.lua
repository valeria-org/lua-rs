-- Reachability details observable through finalizers and weak tables.

local function full() collectgarbage() collectgarbage() end

-- The frame-link slot of a call (upstream: the return PC) never keeps a stale value alive.
-- Here `u` is left in the argument slot of getmetatable(u), which becomes the frame-link slot
-- of full()'s call to collectgarbage.
local held
do
  local u = newproxy(true)
  getmetatable(u).__gc = function(o) held = "finalized" end
end
full()
print("stale link slot", held)

-- Same for the frame-link slot of a call to a Lua function: `u` is left in the argument slot
-- of getmetatable(u), two slots above the block; full() is then called from that height.
local held2
do
  local u = newproxy(true)
  getmetatable(u).__gc = function(o) held2 = "finalized" end
end
local a, b = 1, 2
full()
print("stale link slot (Lua callee)", held2, a + b)

local wk = setmetatable({}, {__mode = "k"})
local fin
do
  local u = newproxy(true)
  getmetatable(u).__gc = function(o) fin = true end
  wk[u] = "val"
end
full()
print("weak key", fin, next(wk))

-- A suspended coroutine reachable only through the open upvalues of closures it created is
-- collected; the closures keep working on the closed upvalues.
local wco = setmetatable({}, {__mode = "k"})
local f
do
  local co = coroutine.create(function()
    local x = 10
    f = function(d) x = x + d return x end
    coroutine.yield()
  end)
  coroutine.resume(co)
  wco[co] = true
end
full()
print("coroutine collected", next(wco) == nil, f(1), f(2))
full()
print("closed upvalue", f(3))

-- Several closures sharing the open upvalues of a dropped coroutine still share them.
local get, set
do
  local co = coroutine.wrap(function()
    local v = "a"
    get = function() return v end
    set = function(n) v = n end
    coroutine.yield()
  end)
  co()
  wco[co] = true
end
full()
set("b" .. 1)
print("shared upvalue", get(), next(wco) == nil)
