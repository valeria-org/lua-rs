-- tostring / string.format / tonumber of numbers.
local nums = {0, -0, 1, -1, 0.1, 1/3, -1/3, 1e15, 1e16, 1e17, 123456789012345, 2^53, 2^53 + 1, 2^63, -2^63, 2^64,
  1e100, 1e300, 1e308, 1.7976931348623157e308, 5e-324, 2.2250738585072014e-308, 1e-5, 1e-4, 0.0001234,
  123.456, 100, 1e14, 3.14159265358979, 1/0, -1/0, 0/0, -(0/0), 255, 65535, 4294967295, 4294967296, -4294967296,
  0.5, 1.5, 2.5, -0.5, 1e-310, 12345678.9, 99999999999999.9, 0x7fffffff, 0x80000000, 2^31, -2^31, 2^32 - 0.5}
for i, n in ipairs(nums) do
  io.write(i, " ", tostring(n), " ", string.format("%.14g", n), " ", string.format("%g", n), " ", string.format("%e", n),
    " ", string.format("%.3f", n), " ", string.format("%a", n), " ", string.format("%5.1f|%-8.2g|%+d|% d", n, n, n == n and n < 2^63 and n > -2^63 and n or 0, 1), "\n")
end
-- %d / %x / %o / %u / %c / %i with various numbers
for _, n in ipairs{0, 1, -1, 255, -255, 2^31, -2^31, 2^32, 2^52, -2^52, 2^53, 1.5, -1.5, 3.99, -3.99} do
  print(n, pcall(string.format, "%d %i %x %X %o %u", n, n, n, n, n, n))
end
print(pcall(string.format, "%d", 2^63))
print(pcall(string.format, "%d", -2^63))
print(pcall(string.format, "%x", -1))
print(pcall(string.format, "%5.2s|%-5s|%5s|%.0s|", "abcdef", "ab", "ab", "xyz"))
print(string.format("%q", "a\0b\n\r\t\"\\\1\127\255 end"))
print(string.format("%q", 1/3), string.format("%q", 1e100), string.format("%q", -0.0), string.format("%q", 1/0))
print(pcall(string.format, "%q", {}))
print(string.format("%s %s %s", 1, 1.5, -0.0))
print(string.format("%s", setmetatable({}, {__tostring = function() return "custom" end})))
print(pcall(string.format, "%s", setmetatable({}, {__tostring = function() return 1 end})))
print(string.format("%c%c%c", 72, 105, 0):byte(1, -1))
print(string.format("%%"))
print(pcall(string.format, "%5%"))
print(pcall(string.format, "%-+ #0123d", 1))
print(string.format("%#x %#o %#.3g %#e", 255, 8, 1, 1))
print(string.format("%.0f %.0f %.0f %.0f", 0.5, 1.5, 2.5, -0.5))
print(string.format("%.20f", 0.1))
print(string.format("%.99f", 1/3):len())
print(string.format("%099d", 7):len())
print(string.format("%a %A", 1, 255.5))
print(string.format("%.3a", 1/3))
print(string.format("%g %g %g %g", 1e-5, 1e-4, 123456, 1234567))
print(string.format("%10.4s|", "abcdefg"))
print(#string.format("%s", string.rep("x", 1000)))
-- tonumber
local strs = {"10", "  10  ", "0x10", "0X1p4", "1e2", "1E+2", ".5", "5.", "0x.8", "inf", "nan", "-inf", "1e", "0x",
  "", " ", "1 2", "1_000", "0b101", "  -0x10  ", "+5", "--5", "1e400", "-1e400", "0x1P-1074", "4294967296",
  "9007199254740993", "0x7fffffffffffffff", "1LL", "1ULL", "1i", "\t\n5\v\f\r", "5\0", "0x1.8p1"}
for _, s in ipairs(strs) do
  print(string.format("%q", s), tonumber(s))
end
for _, b in ipairs{2, 8, 10, 16, 36} do
  print(b, tonumber("10", b), tonumber("zz", b), tonumber("-ff", b), tonumber("7fffffff", b), tonumber("1.5", b), tonumber("", b))
end
print(tonumber(nil), tonumber(true), tonumber({}), tonumber(10), tonumber(10, 16), tonumber("10", 16.5))
-- number to string coercions in concat and string functions
print(1 .. "", 1.5 .. "", -0.0 .. "", (2^53) .. "", 1e15 .. "", (0/0) .. "", (1/0) .. "")
print(("x"):rep(3.0), ("abc"):sub(2.0), string.len(123.0), pcall(function() return (12):len() end))
print(10 == "10", "10" + 0 == 10, "0x10" + 0, "1e1" * 1, " 5 " * 2)
print(math.floor(-0.0), math.ceil(-0.5), math.abs(-0.0), math.fmod(-6, 4), math.fmod(6, -4), -6 % 4, 6 % -4, 5.5 % 2)
print(math.huge, -math.huge, math.pi, math.max(1, 2.5, -1), math.min(3, 1/0))
print(0/0 == 0/0, 1 < 0/0, (0/0) ~= (0/0))
print(math.tointeger, math.type, string.format("%5.1f", 1e300))
