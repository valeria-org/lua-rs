local function pc(f, ...)
  print(pcall(f, ...))
end

local name = "iotest.tmp"
-- write and read back
local f = assert(io.open(name, "w"))
print(io.type(f), io.type(io.stdout), io.type(42), io.type(nil))
print(tostring(f) ~= "file (closed)", tostring(f) == tostring(f))
print(f:write("line1\n", 42, " ", 1.5, "\n", "line3\n\nlast"))
print(f:seek("cur"), f:seek("set", 2), f:seek("end"), f:seek("set", "1"))
print(f:close())
print(io.type(f), tostring(f))
pc(f.read, f)
pc(f.write, f, "x")
pc(f.close, f)
pc(f.lines, f)
pc(f.seek, f)
print(f:__tostring())

f = assert(io.open(name, "r"))
print(f:read())
print(f:read("*n", "*n"))
print(f:read("*l"))
print(f:read("*L"))
print(f:read("L"))
print(f:read(0))
print(f:read(2), f:read(100))
print(f:read(0), f:read(), f:read("*a"), f:read("*n"), f:read(1))
print(f:read("*a"), f:read("a"))
f:close()

-- read multiple formats, stop at first failure
f = assert(io.open(name, "r"))
print(f:read("*l", "*n", "*l", "*l"))
print(f:read(3, 0, "*l", "*l", "*l", "*l"))
f:close()

-- format errors
f = assert(io.open(name))
pc(f.read, f, "*x")
pc(f.read, f, "x")
pc(f.read, f, {})
pc(f.read, f, true)
pc(f.read, f, "5")
pc(f.seek, f, "bad")
pc(f.seek, f, "set", {})
pc(f.seek, f, "set", "abc")
pc(f.setvbuf, f)
pc(f.setvbuf, f, "nah")
print(f:setvbuf("full", 1024), f:setvbuf("no"), f:setvbuf("line"))
print(f:seek("set", -10))
f:close()

-- numbers
local g = assert(io.open(name, "w"))
g:write("12 0x1F -3e2 .5 inf nan 1e400 0x 7 - 8 +9 abc")
g:close()
g = assert(io.open(name))
print(g:read("*n", "*n", "*n", "*n", "*n", "*n", "*n"))
print(g:read("n"))
print(g:read("n"))
print(g:read("n"))
print(g:read("n"))
print(g:read("*a"))
g:close()

-- lines
g = assert(io.open(name, "w"))
g:write("a\nbb\n\nccc")
g:close()
for l in io.lines(name) do io.write("[", l, "]") end
print()
g = assert(io.open(name))
for a, b in g:lines(1, "*l") do io.write("<", a, ",", tostring(b), ">") end
print()
print(io.type(g))
g:close()
pc(io.lines, name .. ".nonexistent")
local it = io.lines(name)
print(it(), it(), it(), it(), it())
pc(it)
g = assert(io.open(name))
local it2 = g:lines()
g:close()
pc(it2)

-- open errors
print(io.open(name .. ".nonexistent"))
print(io.open(name .. ".nonexistent", "r"))
pc(io.open)
pc(io.open, {})
pc(io.open, name, {})
print(io.type(io.open(name, "rb")))

-- io.input / io.output
local old_in, old_out = io.input(), io.output()
print(old_in == io.stdin, old_out == io.stdout)
print(io.output(name) ~= old_out)
print(io.write("via ", "io.write", 7, "\n"))
print(io.output():close())
pc(io.write, "x")
pc(io.flush)
io.output(old_out)
print(io.input(name) ~= old_in)
print(io.read(), io.read("*a"))
print(io.read())
print(io.close(io.input()))
pc(io.read)
pc(io.input, name .. ".nonexistent")
pc(io.input, 42)
pc(io.output, {})
pc(io.input, io.input())
io.input(io.stdin)
pc(io.input, newproxy and newproxy() or nil)

-- standard files
print(io.stdout:close())
print(io.stderr:close())
print(io.close())
print(io.stdout:write("") == true)
pc(io.stdout.read, 42)
pc(io.stdout.read)
pc(io.type)
pc(io.close, 1)

-- write errors
pc(io.write, {})
pc(io.write, "a", nil)
pc(io.stdout.write, io.stdout, "ok", true)
print()

-- tmpfile
local t = io.tmpfile()
print(io.type(t), t:write("hello"), t:seek("set"), t:read("*a"))
t:close()

-- popen
local p = io.popen("echo hi; echo there")
print(io.type(p), p:read("*l"), p:read("*a"))
print(p:close())
p = io.popen("exit 3")
print(p:close())
p = io.popen("cat > /dev/null", "w")
print(p:write("data"))
print(p:close())

-- gc closes files
do
  local h = io.open(name, "w")
  h:write("gc'd")
  h = nil
  collectgarbage()
  collectgarbage()
end
g = io.open(name)
print(g:read("*a"))
g:close()

-- interleaving of print and io.write
io.write("a")
print("b")
io.write("c\n")
io.stdout:write("d")
print("e")

-- io.lines with formats
g = assert(io.open(name, "w"))
g:write("1 2 3\n4 5 6\n")
g:close()
for a, b, c in io.lines(name, "*n", "*n", "*n") do print(a, b, c) end

-- metatable
local mt = debug and debug.getmetatable and debug.getmetatable(io.stdout) or getmetatable(io.stdout)
print(type(mt), mt.__index == mt, rawequal(mt, getmetatable(io.stdout)))
local keys = {}
for k in pairs(mt) do keys[#keys + 1] = k end
print(#keys)
print(os.remove(name))
