-- Exercises lua-cjson (built from source against include/) through require.
local cjson = require("cjson")
local safe = require("cjson.safe")
print(type(cjson.encode), cjson._NAME, cjson._VERSION ~= nil)
local function show(v, indent)
  indent = indent or ""
  if type(v) ~= "table" then return print(indent .. tostring(v) .. " (" .. type(v) .. ")") end
  local n = #v
  if n > 0 then
    print(indent .. "array[" .. n .. "]")
    for i = 1, n do show(v[i], indent .. "  ") end
  else
    print(indent .. "object")
    for _, k in ipairs({"a", "b", "c", "name", "list", "nested", "x"}) do
      if v[k] ~= nil then print(indent .. "  ." .. k .. ":") show(v[k], indent .. "    ") end
    end
  end
end
print(cjson.encode({1, 2, 3, "four", true, false}))
print(cjson.encode({a = 1}))
print(cjson.encode({nested = {list = {1.5, -2, 1e10, 0.1}}}))
print(cjson.encode("quote \" backslash \\ newline \n tab \t ctrl \1 slash /"))
print(cjson.encode(cjson.null), cjson.encode({}), cjson.encode(12345678901234))
show(cjson.decode('[1, 2.5, "s", true, false, null, {"a": [1, {"b": "deep"}]}]'))
show(cjson.decode('{"name": "J\\u00e9r\\u00f4me \\ud83d\\ude00", "x": -0.5e-3}'))
print(cjson.decode('null') == cjson.null, cjson.decode('"str"'), cjson.decode('123.25'))
print(pcall(cjson.decode, '{"a": }'))
print(pcall(cjson.decode, '[1, 2'))
print(pcall(cjson.encode, {f = print}))
print(pcall(cjson.encode, {[1] = 1, [3] = 3, [1000] = 1000}))
print(safe.decode('{bad json'))
print(safe.encode(function() end))
local t = {} t.self = t
print(pcall(cjson.encode, t))
print(cjson.encode_sparse_array(true, 2, 3))
print(cjson.encode({[1] = "a", [5] = "e"}))
print(cjson.encode_keep_buffer(false), cjson.encode_number_precision(3))
print(cjson.encode({math_pi = 3.14159265358979}))
print(cjson.encode_number_precision(14))
print(cjson.decode_invalid_numbers(false))
print(pcall(cjson.decode, "[NaN]"))
print(pcall(cjson.encode_max_depth, "x"))
local deep = {}
local cur = deep
for i = 1, 1100 do cur[1] = {} cur = cur[1] end
print(pcall(cjson.encode, deep))
local big = {}
for i = 1, 2000 do big[i] = i * 3 end
local s = cjson.encode(big)
local back = cjson.decode(s)
print(#s, #back, back[1], back[2000])
local new = cjson.new()
print(new ~= cjson, new.encode({1}))
