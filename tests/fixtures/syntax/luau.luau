-- Luau syntax: types, generics, type functions, expression syntax, and bindings.
export type Pair<T = number> = {first: T, second: T}
type Result<T, E> = {ok: true, value: T} | {ok: false, error: E}
type Callback<T, U...> = (T, U...) -> (T, U...)
type Named = {name: string} & {id: number}
type Optional = string?
type Values = {[string]: number}
type Queried = typeof(workspace)
type function identity(t)
    return t
end
const scale: number = 2
const function double(value: number): number
    return value * scale
end
@native
local function choose<T>(value: T, fallback: T): T
    return if value ~= nil then value else fallback
end
local function variadic<T...>(...: T...): T...
    return ...
end
local count: number = 0b10_10
count += 1
count //= 2
local greeting = `hello {choose("世界", "world")}, {double(count)}`
local asserted = (count :: number)
for key: string, value: number in {one = 1} do
    if value == 0 then continue end
    count += value
end
return greeting, asserted, variadic(count)
