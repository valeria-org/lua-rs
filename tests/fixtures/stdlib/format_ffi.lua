-- string.format with int64/uint64 cdata produced by FFI arithmetic.
local F = string.format
print(F("%d %x %u", -1LL, -1LL, 2ULL^63), F("%5d|%-5d|%X", -3LL, 12LL, -1LL))
print(F("%s %s", 1LL, -2ULL), pcall(F, "%.3f", require("ffi").new("double", 1.5)))
print(pcall(F, "%d", require("ffi").new("int", 5)))
