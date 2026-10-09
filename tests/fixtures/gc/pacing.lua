-- Collector pacing in allocation loops (upstream misc/gcstep.lua with peak tracking): the work
-- of a cycle is proportional to the live heap, not to the garbage allocated meanwhile, so with
-- the default pause and step multiplier the heap stays well below 4x its live size.

local count = collectgarbage
local function testgc(what, func)
  collectgarbage()
  local oc = count("count")
  local peak = func()
  local nc = count("count")
  print(what, nc < oc * 4, peak < oc * 4)
end

local function run()
  testgc("TNEW", function()
    local p = 0
    for i = 1, 10000 do local t = {} local c = count("count") if c > p then p = c end end
    return p
  end)
  testgc("TDUP", function()
    local p = 0
    for i = 1, 10000 do local t = {1} local c = count("count") if c > p then p = c end end
    return p
  end)
  testgc("FNEW", function()
    local p = 0
    for i = 1, 10000 do local function f() end local c = count("count") if c > p then p = c end end
    return p
  end)
  testgc("CAT", function()
    local p = 0
    for i = 1, 10000 do local s = "x" .. i local c = count("count") if c > p then p = c end end
    return p
  end)
end

run()
-- More live data: tables, closures and strings.
local live = {}
for i = 1, 3000 do live[i] = {i, "s" .. i, function() return i end} end
run()
