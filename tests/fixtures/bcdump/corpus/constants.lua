-- Number constants: narrowed integers, doubles, the 33-bit ULEB128 sign fixups.
local x = ...
local n = {}
n[1] = x + 1
n[2] = x * -7
n[3] = x - 100000
n[4] = x + 2147483647
n[5] = x + -2147483648
n[6] = x * 2147483648
n[7] = x / 4294967295
n[8] = x + 0.1
n[9] = x - 1e300
n[10] = x % 2^53
n[11] = x ^ -1.5
n[12] = x + 1e-310
n[13] = x * 123456789012
n[14] = x + 65536
n[15] = x - -1073741824
n[16] = x * 3.0
n[17] = x + 1048575.5
n[18] = 1e100
n[19] = -123456.75
n[20] = 0x7fffffff
n[21] = -0x80000000
n[22] = 2^63
-- String constants: empty, short, uleb128 length boundaries, binary bytes.
local s = {}
s[1] = ""
s[2] = "a"
s[3] = "hello world"
s[4] = "\0\1\2\255\254 embedded \0 zeros"
s[5] = string.rep("x", 1)
s[6] = "0123456789012345678901234567890123456789012345678901234567890123456789012345678901234567890123456789012345678901234567890"
s[7] = "01234567890123456789012345678901234567890123456789012345678901234567890123456789012345678901234567890123456789012345678901234567"
s[8] = [[
long bracket string
with newlines	and tabs]]
s[9] = [==[ contains ]] and ]=] ]==]
s[10] = "\u{10FFFF}\xff\z
         continued"
-- Boolean/nil constants via KPRI.
local p = { true, false, nil }
-- Cdata literals (sets PROTO_FFI / BCDUMP_F_FFI).
local c = {}
c[1] = 1LL
c[2] = 2ULL
c[3] = 3i
c[4] = -1LL
c[5] = 0xffffffffffffffffULL
c[6] = 0x7fffffffffffffffLL
c[7] = 1.5i
c[8] = 12345678901234LL
c[9] = 0x8000000000000000ULL
c[10] = 0i
c[11] = 1e300i
return n, s, p, c
