
for i = 1, 10 do print(i) end
for i = 1, 10, 2 do print(i) end
for i = 10, 1, -1 do print(i) end
for i = 0, 1, 0.1 do print(i) end
for i = 1.5, 10.5 do print(i) end
for i = 10, 1, -0.5 do end
local a, b, c = 1, 100, 3
for i = a, b, c do print(i) end
for i = a, b do end
for i = -1, -10, -1 do end
for i = 1, 0 do end
for i = 1, 3 do
  for j = i, 3 do
    for k = j, 3 do print(i, j, k) end
  end
end
for i = 1, 2 do local i = i * 2; print(i) end
for i = f(), g(), h() do end
for i = 1, #t do t[i] = i end
for i = 1, 100000, 32768 do end
