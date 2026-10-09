module("m6", package.seeall)
function hello() return "hi from " .. _NAME end
print(_M == m6, _NAME, _PACKAGE)
