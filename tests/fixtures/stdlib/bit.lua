-- bit library: 32-bit number semantics, int64/uint64 cdata semantics, errors.
local function e(...) print(pcall(...)) end
print(bit.tobit(0xffffffff), bit.tobit(2^32 + 1), bit.tobit(-1), bit.tobit(1.5), bit.tobit("10"), bit.tobit(2^52 + 7))
print(bit.bnot(0), bit.bnot(-1), bit.bnot(0x12345678))
print(bit.band(0xff00, 0x0ff0), bit.band(-1), bit.band(1, 3, 7), bit.bor(1, 2, 4, 8), bit.bxor(0xff, 0x0f, 1))
print(bit.lshift(1, 0), bit.lshift(1, 31), bit.lshift(1, 32), bit.lshift(1, 33), bit.lshift(1, -1))
print(bit.rshift(-1, 1), bit.rshift(0x80000000, 31), bit.arshift(-256, 4), bit.arshift(0x80000000, 31), bit.arshift(1, 32))
print(bit.rol(0x12345678, 8), bit.ror(0x12345678, 8), bit.rol(1, 32), bit.ror(1, -1))
print(bit.bswap(0x12345678), bit.bswap(-1), bit.bswap(0xff))
print(bit.tohex(255), bit.tohex(-1), bit.tohex(255, 2), bit.tohex(255, -2), bit.tohex(0x12345678, 4), bit.tohex(1, 0), bit.tohex(1, 100), bit.tohex(1, -100))
print(bit.band("0x10", 0x11), bit.bor(" 3 ", 4))
e(bit.band)
e(bit.band, 1, "x")
e(bit.band, 1, {})
e(bit.tobit, nil)
e(bit.lshift, 1)
e(bit.tohex, {})
e(bit.bnot, true)

-- error messages naming the called function
print(pcall(function() bit.band(1, {}) end))
print(pcall(function() bit.tohex() end))
print(pcall(function() local b = bit; b.rol(1) end))
