-- string.find/match/gmatch/gsub.
local text = string.rep("the quick brown fox jumps over the lazy dog 12345 ", 2000)
local n = 0
for r = 1, 60 do
  for w in text:gmatch("%a+") do n = n + #w end
  n = n + select(2, text:gsub("o", "0"))
  local p = 1
  while true do local a, b = text:find("%d+", p); if not a then break end; n = n + b - a; p = b + 1 end
end
for i = 1, 1e6 do local k, v = ("key" .. i .. "=val"):match("^(%w+)=(%w+)$"); n = n + #k + #v end
print(n)
