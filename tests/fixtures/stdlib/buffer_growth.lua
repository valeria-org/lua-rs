-- string.buffer growth: remaining capacity reported by reserve() after random operations
-- (upstream's exact lj_buf_more reservations and compaction policy).
local buffer = require("string.buffer")
local out = {}
for seed = 1, 6 do
  math.randomseed(seed)
  for round = 1, 12 do
    local b = buffer.new(math.random(0, 3) == 0 and math.random(1, 300) or nil)
    for step = 1, 50 do
      local op = math.random(1, 14)
      if op == 1 then b:put(("x"):rep(math.random(0, 200)))
      elseif op == 2 then b:get(math.random(0, 100))
      elseif op == 3 then b:skip(math.random(0, 50))
      elseif op == 4 then local _, n = b:reserve(math.random(0, 500)); out[#out+1] = n
      elseif op == 5 then local _, n = b:reserve(math.random(0, 20)); b:commit(math.min(n, 0))
      elseif op == 6 then b:put(math.random(1, 1e6), math.random() * 10^math.random(-300, 300), -0.0, 0/0, 1/0)
      elseif op == 7 then b:encode({1, 2, "abc", {x = 1}, ("z"):rep(math.random(0, 300))})
      elseif op == 8 then if #b > 0 and math.random(1, 4) == 1 then b:reset() end
      elseif op == 9 then b:putf("%5d%s", step, ("y"):rep(math.random(0, 40)))
      elseif op == 10 then b:putf("%q|%g|%e|%.3f|%a", ("\0\1\"\n9a"):rep(math.random(0, 30)), math.random(), 1e300 * math.random(), -math.random(), math.random())
      elseif op == 11 then b:putf("%-20s|%x|%#o|%c|%10.4s", "abc", math.random(0, 2^31), 8, 65, "abcdefgh")
      elseif op == 12 then if #b > 0 then pcall(b.decode, b) end
      elseif op == 13 then b:set(("s"):rep(math.random(0, 100)))
      else b:put(buffer.new():put(("w"):rep(math.random(0, 70)))) end
      out[#out+1] = #b
    end
    local _, n = b:reserve(0); out[#out+1] = n
  end
end
for i = 1, #out, 25 do print(table.concat(out, " ", i, math.min(#out, i + 24))) end
