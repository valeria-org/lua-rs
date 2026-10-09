local s
s = [[simple]]
s = [[
skip first newline]]
s = [[multi
line
string]]
s = [=[with ]] inside]=]
s = [==[level two ]=] ]] ]==]
s = [===[
]===]
s = [[]]
s = [====[ [[ nested ]] ]====]
s = [[with "quotes" and 'apos' and \n escapes]]
--[[ long comment ]]
--[==[ long
comment ]] level 2 ]==]
--[[ multi
line
comment ]] s = "after comment"
-- short comment
--[ not long
--[=x not long either
s = [[trailing]] .. [=[concat]=]
f[[call]]
f[=[call2]=]
--[[]]
---[[ disabled long comment
s = 1
--]]
s = [[
]]
s = [[

two blank]]
