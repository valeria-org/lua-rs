-- bit library with int64/uint64 cdata operands (needs the FFI library hooks).
local function e(...) print(pcall(...)) end
print(bit.band(0xffffffffffLL, 0xff00ff00ffLL), bit.bor(1LL, 2), bit.bxor(-1LL, 1ULL), bit.bnot(0LL), bit.bnot(0ULL))
print(bit.lshift(1LL, 40), bit.lshift(1LL, 64), bit.rshift(-1LL, 60), bit.arshift(-1LL, 60), bit.rshift(-1ULL, 60), bit.rol(1LL, 63), bit.ror(1ULL, 1))
print(bit.bswap(0x0102030405060708LL), bit.tobit(0x100000001LL), bit.tohex(-1LL), bit.tohex(255LL, 4), bit.tohex(1LL, -20))
print(bit.band(3, 6LL), bit.lshift(1, 2LL), bit.tohex(255, 4LL), bit.band(-1, 0xffffffffffULL), bit.bor(2^40, 1LL))
print(type(bit.band(1LL, 1)), tostring(bit.band(1LL, 1)), bit.bxor(1LL, 2ULL, 4LL))
local ffi = require("ffi")
print(bit.band(ffi.new("int8_t", -1), 0xffLL), bit.bor(ffi.new("uint16_t", 5), 0LL), bit.tobit(ffi.new("double", 3.9)), bit.band(ffi.new("uint64_t", 7), 3))
e(bit.band, ffi.new("struct { int x; }"), 1LL)
e(bit.lshift, 1LL, ffi.new("struct { int x; }"))
