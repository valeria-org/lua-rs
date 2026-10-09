-- string library: results and error messages (compared with the reference binary).
local function e(...) print(pcall(...)) end
local function show(...) local t = {...}; for i = 1, select("#", ...) do t[i] = tostring(t[i]) end; print(select("#", ...), table.concat(t, " ")) end

-- byte / char
show(string.byte("hello"))
show(string.byte("hello", 2, 4))
show(string.byte("hello", -3, -1))
show(string.byte("hello", 0, 100))
show(string.byte("hello", 4, 2))
show(string.byte(""))
show(("A"):byte(), string.byte(123))
show(string.byte("hello", 1.9, "3"))
e(string.byte)
e(string.byte, {})
e(string.byte, "x", "y")
show(string.char(), string.char(104, 105), string.char("65", 66.7))
e(string.char, 256)
e(string.char, -1)
e(string.char, 65, "x")
e(string.char, 65, nil)

-- sub
for _, a in ipairs{{2}, {2, 3}, {-3}, {-3, -2}, {0}, {0, 0}, {10}, {3, 2}, {-100, 2}, {2, 100}, {-100, -100}, {2^31}, {-2^31}, {1, 2^31}, {1, -2^31}} do
  show(("hello"):sub(a[1], a[2]))
end
e(string.sub)
e(string.sub, "x")
e(string.sub, "x", "y")
e(("x").sub, "x", 1, {})

-- rep / reverse / lower / upper / len
show(string.rep("ab", 3), string.rep("ab", 3, ","), string.rep("x", 1, ","), string.rep("x", 0, ","), string.rep("x", -1), string.rep("", 100))
show(string.rep(12, 2), string.rep("a", 2.9))
e(string.rep, "x")
e(string.rep, nil, 1)
e(string.rep, "x", 1, {})
e(string.rep, "abc", 2^30)
show(string.reverse("hello"), string.reverse(""), string.reverse(123))
show(string.lower("HeLLo \200 W"), string.upper("HeLLo \200 w"), string.upper(1.5))
e(string.reverse)
e(string.lower, {})
e(string.upper, nil)
show(string.len("abc"), string.len(""), string.len(12345), string.len("\0\0"))
e(string.len)
e(string.len, {})

-- find / match
show(string.find("hello world", "wor"))
show(string.find("hello world", "o", 6))
show(string.find("hello world", "o", -3))
show(string.find("hello", "l", -100))
show(string.find("hello", "", 10))
show(string.find("hello", "", 6))
show(string.find("a+b", "+", 1, true))
show(string.find("a.b", ".", 2, 1))
show(string.find("hello", "(l)(l)"))
show(string.find("hello", "()ll()"))
show(string.find("hello", "^h"), string.find("hello", "^e"), string.find("hello", "o$"))
show(string.find("  x", "^%s*"))
show(string.match("hello 123 world", "%d+"))
show(string.match("key = value", "(%w+)%s*=%s*(%w+)"))
show(string.match("hello", ".-(l+)(.*)"))
show(string.match("[[x]]", "%[(%b[])%]"))
show(string.match("THE (quick) fox", "%f[%a]%a+", 5))
show(string.match("hello", "xyz"))
show(string.match("hello", "()", 3))
show(string.match("abc", "%a", 0), string.match("abc", "%a", -1))
for _, p in ipairs{"(", "%", "[a", "(()", "%1", "(%1)", "%b", "%f", "[%", ")", "%g"} do e(string.find, "abc", p) end
e(string.find, "abc", ("(."):rep(33))
e(string.match)
e(string.match, "x")
e(string.find, "x", "x", "y")

-- gmatch
for w in string.gmatch("one two  three", "%a+") do io_write = nil; print(w) end
for k, v in string.gmatch("a=1, b=2, c=3", "(%w+)=(%w+)") do print(k, v) end
for p in string.gmatch("abc", "()") do print(p) end
for x in string.gmatch("abc", "") do print("[" .. x .. "]") end
for x in string.gmatch("^a^b", "^.") do print(x) end
e(string.gmatch, "x")
e(string.gmatch("x", "("))

-- gsub
show(string.gsub("hello world", "o", "0"))
show(string.gsub("hello world", "o", "0", 1))
show(string.gsub("hello world", "o", "0", 0))
show(string.gsub("hello world", "o", "0", -1))
show(string.gsub("hello world", "o", "0", 2.7))
show(string.gsub("hello", "", "-"))
show(string.gsub("hello", "^", ">"))
show(string.gsub("hello", "$", "<"))
show(string.gsub("hello", "^h", "H"))
show(string.gsub("hello world", "(%w+)", "<%1>"))
show(string.gsub("hello world", "%w+", "%0 %0"))
show(string.gsub("abc", "(b)", "%%%1%%"))
show(string.gsub("abc", "b", "%x"))
show(string.gsub("abc", "b", "%"))
show(string.gsub("abc", "()b()", "%1-%2"))
show(string.gsub("abc", "b", 42))
show(string.gsub(123, 2, 9))
show(string.gsub("hello world", "%w+", {hello = "HI", world = false}))
show(string.gsub("hello world", "%w+", {hello = 1.5}))
show(string.gsub("hello world", "(%w+) (%w+)", function(a, b) return b .. " " .. a end))
show(string.gsub("abc", "%w", function(c) if c == "b" then return nil end return c:upper() end))
show(string.gsub("abc", "%w", function() return 7 end))
local mt = setmetatable({}, {__index = function(t, k) return k:upper() end})
show(string.gsub("abc", "%w", mt))
e(string.gsub, "abc", "%w", function() return {} end)
e(string.gsub, "abc", "%w", {a = true})
e(string.gsub, "abc", "%w")
e(string.gsub, "abc", "%w", true)
e(string.gsub, "abc", "(", "")
e(string.gsub, "abc", "b", "%2")
e(string.gsub, "abc", "b", "%1")
e(string.gsub, "abc", "b", "x", "y")
e(string.gsub)

-- string metatable
print(("x"):rep(3), ("%d"):format(5), #getmetatable("").__index == nil, getmetatable("").__index == string)
local ok, msg = pcall(function() return ("x"):nosuch() end) print(ok, msg)
print(rawequal(getmetatable("a"), getmetatable("b")))

-- dump
local function f(a, b) local c = a + b; return c * 2 end
local d = string.dump(f)
print(type(d), d:sub(1, 4) == "\27LJ\2", loadstring(d)(3, 4))
print(#string.dump(f, true) < #d, string.dump(f, "s") == string.dump(f, true), #string.dump(f, "d") == #d)
print(string.dump(f, "sd") == string.dump(f, true))
e(string.dump, print)
e(string.dump, 1)
e(string.dump)
print(loadstring(string.dump(function(...) return "ok", ... end))(1, 2))

-- error messages naming the called function / method
print(pcall(function() string.rep() end))
print(pcall(function() local s = string; s.char(300) end))
print(pcall(function() ("x"):rep({}) end))
print(pcall(function() ("x"):format("%d", {}) end))
print(pcall(function() string.format("%d %d", 1) end))
print(pcall(function() local fmt = string.format; fmt("%q") end))
print(pcall(function() string.gsub("x", "x", "%2") end))
print(pcall(function() ("x"):gsub(".", {x = {}}) end))
print(pcall(function() string.find("x", "[") end))
print(pcall(function() string.dump(print) end))
