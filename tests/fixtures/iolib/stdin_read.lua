-- stdin is fed from stdin_read.stdin
print(io.read())
print(io.read("*n", "*n"))
print(io.read("*L"))
print(io.read(3))
print(io.stdin:read("*l"))
local n = 0
for l in io.lines() do n = n + 1; io.write(n, ":", l, "\n") end
print(io.read(), io.read("*a"), io.read(0))
print(io.type(io.stdin), io.input() == io.stdin)
for l in io.lines(nil, 1) do print(l) end
print(pcall(io.lines, nil, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20,
  21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31, 32, 33, 34, 35, 36, 37, 38, 39, 40,
  41, 42, 43, 44, 45, 46, 47, 48, 49, 50, 51, 52, 53, 54, 55, 56, 57, 58, 59, 60))
print(type(select(2, pcall(io.lines, nil, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20,
  21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31, 32, 33, 34, 35, 36, 37, 38, 39, 40,
  41, 42, 43, 44, 45, 46, 47, 48, 49, 50, 51, 52, 53, 54, 55, 56, 57, 58, 59))))
print(pcall(io.read, -1))
