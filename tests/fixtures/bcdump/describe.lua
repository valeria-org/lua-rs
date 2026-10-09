-- Print the prototype tree of a raw bytecode dump as loaded by the reference LuaJIT.
-- Usage: luajit describe.lua dump.raw  (needs jit.util and ffi)
-- The Rust golden test renders its own reader's result in the same format.
local jutil = require("jit.util")
local ffi = require("ffi")

local f = assert(io.open(arg[1], "rb"))
local data = f:read("*a")
f:close()
local fn = assert(loadstring(data))

local bits = ffi.new("union { double d; struct { uint32_t lo, hi; } w; }")
local function nbits(x)
  bits.d = x
  return string.format("%08x%08x", bits.w.hi, bits.w.lo)
end
local function hex(s)
  return (s:gsub(".", function(c) return string.format("%02x", c:byte()) end))
end
local i64, u64, cplx = ffi.typeof("int64_t"), ffi.typeof("uint64_t"), ffi.typeof("complex")

local function val(v, t)
  local tv = type(v)
  if tv == "string" then return "s:" .. hex(v)
  elseif tv == "number" then return "n:" .. nbits(v)
  elseif tv == "boolean" then return tostring(v)
  elseif v == t and t ~= nil then return "marker"
  end
  error("unexpected template value " .. tv)
end

local function kgc(k)
  local tk = type(k)
  if tk == "string" then return "str " .. hex(k)
  elseif tk == "table" then
    local e = {}
    for key, v in pairs(k) do e[#e + 1] = val(key) .. "=" .. val(v, k) end
    table.sort(e)
    return "tab {" .. table.concat(e, ",") .. "}"
  elseif tk == "cdata" then
    if ffi.istype(i64, k) then return "i64 " .. tostring(ffi.cast("uint64_t", k))
    elseif ffi.istype(u64, k) then return "u64 " .. tostring(k)
    elseif ffi.istype(cplx, k) then return "complex " .. nbits(k.re) .. " " .. nbits(k.im)
    end
    error("unexpected cdata " .. tostring(ffi.typeof(k)))
  elseif tk == "proto" then return "proto"
  end
  error("unexpected constant type " .. tk)
end

local function describe(p, path)
  local info = jutil.funcinfo(p)
  local out = {}
  out[#out + 1] = "proto " .. path
  out[#out + 1] = string.format("params=%d framesize=%d vararg=%s children=%s firstline=%d numline=%d",
    info.params, info.stackslots, tostring(info.isvararg), tostring(info.children),
    info.linedefined, info.lastlinedefined - info.linedefined)
  out[#out + 1] = "source " .. hex(info.source)
  local bc, lines = {}, {}
  for pc = 0, info.bytecodes - 1 do
    local ins = jutil.funcbc(p, pc)
    if ins < 0 then ins = ins + 2^32 end
    bc[#bc + 1] = string.format("%08x", ins)
    lines[#lines + 1] = tostring(jutil.funcinfo(p, pc).currentline)
  end
  out[#out + 1] = "bc " .. table.concat(bc, " ")
  out[#out + 1] = "lines " .. table.concat(lines, " ")
  for i = 0, info.upvalues - 1 do
    out[#out + 1] = string.format("uvname %d %s", i, hex(jutil.funcuvname(p, i)))
  end
  local children = {}
  for d = 0, info.gcconsts - 1 do
    local k = jutil.funck(p, -1 - d)
    out[#out + 1] = string.format("kgc %d %s", d, kgc(k))
    if type(k) == "proto" then children[#children + 1] = { k, path .. "." .. d } end
  end
  for i = 0, info.nconsts - 1 do
    out[#out + 1] = string.format("knum %d %s", i, nbits(jutil.funck(p, i)))
  end
  io.write(table.concat(out, "\n"), "\n")
  for _, c in ipairs(children) do describe(c[1], c[2]) end
end

describe(fn, "0")
