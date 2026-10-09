-- Build valid and malformed bytecode dumps and record what the reference lua_load() makes of
-- them. Output lines: <input hex> TAB <chunkname hex | "-" for the input itself> TAB <result>,
-- result = "OK <status> <source hex>" or "ERR <status> <message>" (status: LUA_ERR*).
-- Usage: LOADCHECK=path/to/loadcheck luajit mkerrors.lua > errors.txt  (see gen.sh)
--
-- Each case is loaded by the loadcheck helper, which detects outcomes that depend on the
-- memory after the input (upstream reads malformed dumps past the end of their buffer). Such
-- cases are reported on stderr and left out.

local bcnames = require("jit.vmdef").bcnames
local loadcheck = assert(os.getenv("LOADCHECK"), "LOADCHECK not set")

local function hex(s)
  return (s:gsub(".", function(c) return string.format("%02x", c:byte()) end))
end

local function emit(input, name)
  local namehex = name == nil and "-" or hex(name)
  local p = io.popen(string.format("%q %s %s 2>/dev/null", loadcheck, hex(input), namehex == "" and "''" or namehex))
  local res = p:read("*l")
  p:close()
  if res == nil or res == "UNSTABLE" then
    io.stderr:write("unstable: ", hex(input), "\n")
    return
  end
  io.write(hex(input), "\t", namehex, "\t", res, "\n")
end

local function uleb(v)
  local t = {}
  repeat
    local b = v % 128
    v = (v - b) / 128
    if v > 0 then b = b + 128 end
    t[#t + 1] = string.char(b)
  until v == 0
  return table.concat(t)
end

local function op(name)
  local i = bcnames:find(string.format("%-6s", name), 1, true)
  return (i - 1) / 6
end

local function word(v)
  local t = {}
  for _ = 1, 4 do
    local b = v % 256
    t[#t + 1] = string.char(b)
    v = (v - b) / 256
  end
  return table.concat(t)
end

local function ins_ad(o, a, d) return word(o + a * 256 + d * 65536) end

-- Stripped prototype: header, bytecode words, upvalue refs, raw constant data.
local function proto(t)
  local bc = t.bc or { ins_ad(op("RET0"), 0, 1) }
  local body = string.char(t.flags or 0, t.numparams or 0, t.framesize or 1, #(t.uv or {}))
    .. uleb(t.nkgc or 0) .. uleb(t.nkn or 0) .. uleb(#bc)
    .. table.concat(bc) .. table.concat(t.uv or {}) .. (t.k or "")
  return uleb(#body) .. body
end

local function dump(flags, protos, extra)
  return "\27LJ\2" .. uleb(flags) .. (extra or "") .. table.concat(protos) .. "\0"
end

local STRIP_FR2 = 0x0a

-- Base dumps.
local function sample(a, ...)
  local x = { 1, 2, a = 3, b = "s", [1.5] = true }
  return x, "str", 1LL, a + 1.5, function(...) return a, ... end
end
local function sample2()
  local t = { f = 1 }
  return t
end
local d1 = string.dump(sample)
local d2 = string.dump(sample, true)
local d3 = string.dump(function() end, true)
local d4 = string.dump(sample2)

emit(d1)
emit(d2)
emit(d3)
emit(d4)

-- Every truncation of the base dumps. Upstream reads a header shorter than 14 bytes from an
-- uninitialized buffer; only the cases below fail deterministically on the magic, version or
-- flags (a lone flags byte followed by garbage can even crash the reference).
for _, d in ipairs({ d1, d2, d3, d4 }) do
  for n = 14, #d - 1 do emit(d:sub(1, n)) end
end
emit("\27")
emit("\27L")
emit("\27LJ")
emit("\27LJ\2")

-- Header variants.
emit("\27LX" .. d2:sub(4))
emit("\27XJ" .. d2:sub(4))
emit("\27LJ\1" .. d2:sub(5))
emit("\27LJ\3" .. d2:sub(5))
emit("\27LJ\128" .. d2:sub(5))
emit(dump(0x02, { proto({}) }))                 -- no FR2
emit(dump(STRIP_FR2 + 0x20, { proto({}) }))     -- unknown flag
emit(dump(STRIP_FR2 + 0x100, { proto({}) }))    -- unknown flag (2 byte ULEB128)
emit(dump(STRIP_FR2 + 0x04, { proto({}) }))     -- FFI
emit(dump(STRIP_FR2 + 0x10, { proto({}) }))     -- BITOP
emit(dump(STRIP_FR2 + 0x14, { proto({}) }))     -- FFI + BITOP
emit("\27LJ\2\138\128\128\128\0" .. proto({}) .. "\0") -- overlong flags ULEB128
emit(dump(0x08, { "\1\0" }, uleb(3) .. "=ab")) -- name, proto with debug size 0 (truncated)
emit(dump(0x08, { proto({}):sub(1, 1) .. proto({}):sub(2) }, uleb(5) .. "@x.lu"))

-- Big-endian dump: swapped instruction words and upvalue refs.
do
  local w = ins_ad(op("RET0"), 0, 1)
  emit(dump(STRIP_FR2 + 0x01, { proto({ bc = { w:reverse() } }) }))
  local kw = ins_ad(op("UGET"), 0, 0)
  local u = proto({ framesize = 2, uv = { "\0\1" }, bc = { kw:reverse(), ins_ad(op("RET1"), 0, 2):reverse() } })
  emit(dump(STRIP_FR2 + 0x01, { u }))
end

-- Trailing data, missing or duplicated terminator, extra top-level prototypes.
emit(d2 .. "x")
emit(d2 .. "\0")
emit(d2:sub(1, -2))
emit(d1:sub(1, -2))
emit(d2:sub(1, -2) .. "\0\0")
emit(dump(STRIP_FR2, { proto({}), proto({}) }))
emit(dump(STRIP_FR2, {}))
emit(dump(STRIP_FR2, { proto({}) }):sub(1, -2) .. "\128\0")

-- Prototype length off by one.
do
  local p = proto({})
  local body = p:sub(2)
  emit(dump(STRIP_FR2, { uleb(#body + 1) .. body .. "\0" }))
  emit(dump(STRIP_FR2, { uleb(#body - 1) .. body }))
  emit(dump(STRIP_FR2, { uleb(#body + 1) .. body }))
  emit(dump(STRIP_FR2, { uleb(1000) .. body }))
end

-- Constant decoding errors raised while building the constants.
local KSTR = function(s) return uleb(5 + #s) .. s end
emit(dump(STRIP_FR2, { proto({ nkgc = 1, k = KSTR("hello") }) }))
emit(dump(STRIP_FR2, { proto({ nkgc = 1, k = uleb(5 + 0x7fffff00) }) }))   -- string length overflow
emit(dump(STRIP_FR2, { proto({ nkgc = 1, k = uleb(1) .. uleb(0x08000002) .. uleb(0) }) })) -- table overflow (array)
emit(dump(STRIP_FR2, { proto({ nkgc = 1, k = uleb(1) .. uleb(0) .. uleb(0x04000001) }) })) -- table overflow (hash)
emit(dump(STRIP_FR2, { proto({ nkgc = 1, k = uleb(1) .. uleb(0) .. uleb(1) .. "\0\1" }) }))  -- nil key
emit(dump(STRIP_FR2, { proto({ nkgc = 1, k = uleb(1) .. uleb(0) .. uleb(1) .. "\4\0" .. uleb(0xfff80000) .. "\1" }) })) -- NaN key
emit(dump(STRIP_FR2, { proto({ nkgc = 1, k = uleb(1) .. uleb(2) .. uleb(2) .. "\0\2" .. "\5\1" .. "\3\7" .. "\6a\0" }) }))
emit(dump(STRIP_FR2, { proto({ nkgc = 1, k = uleb(0) }) }))                -- child stack underflow
emit(dump(STRIP_FR2, { proto({}), proto({ flags = 1, nkgc = 2, k = uleb(0) .. uleb(0) }) })) -- underflow (2nd)
emit(dump(STRIP_FR2, { proto({}), proto({}), proto({ flags = 1, nkgc = 2, k = uleb(0) .. uleb(0) }) }))
emit(dump(STRIP_FR2, { proto({ nkgc = 3, k = uleb(2) .. uleb(1) .. uleb(2) .. uleb(3) .. uleb(4)
  .. uleb(1) .. uleb(2) .. uleb(3) .. uleb(4) .. uleb(4) .. uleb(1) .. uleb(2) .. uleb(3) .. uleb(4) }) }))
emit(dump(STRIP_FR2, { proto({ nkn = 3, k = "\2" .. "\3\5" .. "\255\255\255\255\31" }) }))
emit(dump(STRIP_FR2, { proto({ nkn = 1, k = "\3" }) }))                    -- truncated knum
emit(dump(STRIP_FR2, { proto({ nkn = 2, k = "\2" }) }))                    -- missing knum
emit(dump(STRIP_FR2, { proto({ nkgc = 2, k = KSTR("a") }) }))              -- missing kgc

-- Chunk names in messages and for stripped dumps.
local bad = d2:sub(1, 20)
for _, name in ipairs({ "@file.lua", "=stdin", "plain", "", "@", "=", "\27binary", "ab\0cd", "=(load)" }) do
  emit(bad, name)
  emit(d2, name)
  emit(d1, name)
end

-- Single-byte corruptions of stripped dumps, except opcode bytes (upstream does not validate
-- instructions; this implementation rejects invalid opcodes, tested separately).
local function opcode_positions(d)
  local skip = {}
  local p = 6 -- after ESC L J version flags(1 byte)
  local function rd()
    local v, sh = 0, 1
    repeat
      local b = d:byte(p)
      p = p + 1
      v = v + (b % 128) * sh
      sh = sh * 128
    until b < 128
    return v
  end
  while true do
    if d:byte(p) == 0 then break end
    local len = rd()
    local start = p
    p = p + 4
    rd(); rd()
    local nbc = rd()
    for i = 0, nbc - 1 do skip[p + 4 * i] = true end
    p = start + len
  end
  return skip
end
for _, d in ipairs({ d2, d3 }) do
  local skip = opcode_positions(d)
  for i = 6, #d do
    if not skip[i] then
      for _, v in ipairs({ 0, 1, 127, 128, 255 }) do
        local c = string.char((d:byte(i) + v) % 256)
        if v == 0 then c = "\0" end
        local m = d:sub(1, i - 1) .. c .. d:sub(i + 1)
        if m ~= d then emit(m) end
      end
    end
  end
end
