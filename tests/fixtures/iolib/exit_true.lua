local u = newproxy(true)
getmetatable(u).__gc = function() print("finalized") end
_G.keep = u
io.write("buffered")
os.exit(true, true)
