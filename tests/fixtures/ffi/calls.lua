local ffi = require("ffi")
local function p(...) print(...) end
local function e(f, ...) local ok, m = pcall(f, ...) p(ok, m) end
ffi.cdef[[
size_t strlen(const char *s);
int snprintf(char *buf, size_t n, const char *fmt, ...);
typedef struct { int quot, rem; } div_t;
div_t div(int a, int b);
typedef struct { long quot, rem; } ldiv_t;
ldiv_t ldiv(long a, long b);
double ldexp(double x, int e);
float fabsf(float x);
double strtod(const char *s, char **end);
long strtol(const char *s, char **end, int base);
int abs(int);
char *strchr(const char *s, int c);
void qsort(void *base, size_t n, size_t sz, int (*cmp)(const void *, const void *));
int atoi(const char *);
double cabs(complex double z);
complex double csqrt(complex double z);
void *malloc(size_t);
void free(void *);
int *__errno_location(void);
int sprintf(char *buf, const char *fmt, ...);
]]
local C = ffi.C
p(C.strlen("hello world"), type(C.strlen("x")))
local buf = ffi.new("char[128]")
local n = C.snprintf(buf, 128, "%d %s %.3f %c %ld %u %x %g", ffi.new("int", 42), "str", 3.14159, ffi.new("char", 65), 123456789012LL, ffi.new("unsigned", 4000000000), ffi.new("int", 255), ffi.new("float", 1.5))
p(n, ffi.string(buf))
C.snprintf(buf, 128, "%d|%d|%d|%d|%d|%d|%d|%d|%f|%f|%f|%f|%f|%f|%f|%f|%f|%f", ffi.new("int",1),2LL,ffi.new("int",3),4LL,ffi.new("int",5),6LL,ffi.new("int",7),8LL, 1.5,2.5,3.5,4.5,5.5,6.5,7.5,8.5,9.5,10.5)
p(ffi.string(buf))
local d = C.div(17, 5) p(d.quot, d.rem, ffi.typeof(d))
local ld = C.ldiv(-17, 5) p(ld.quot, ld.rem)
p(C.ldexp(1.5, 4), C.fabsf(-2.25), C.abs(-7), C.atoi("1234"))
local endp = ffi.new("char*[1]")
p(C.strtod("3.25xyz", endp), ffi.string(endp[0]))
p(C.strchr("hello", 108) ~= nil, ffi.string(C.strchr("hello", 108)), C.strchr("hello", 122) == nil)
p(C.cabs(ffi.new("complex", 3, 4)), C.csqrt(-4))
e(C.strlen)
e(C.strlen, "a", "b")
e(C.strlen, 5)
e(C.abs, "x")
e(function() return C.nonexistent_function_xyz end)
e(function() return ffi.C.this_is_undeclared end)
e(function() C.strlen = 1 end)
p(ffi.errno(0))
C.strtol("99999999999999999999999", nil, 10)
p(ffi.errno(), ffi.errno(5), ffi.errno())
-- callbacks
local arr = ffi.new("int[8]", 5, 3, 8, 1, 9, 2, 7, 4)
local calls = 0
C.qsort(arr, 8, 4, function(a, b)
  calls = calls + 1
  local x, y = ffi.cast("const int*", a)[0], ffi.cast("const int*", b)[0]
  return x < y and -1 or x > y and 1 or 0
end)
local t = {} for i = 0, 7 do t[i+1] = arr[i] end
p(t[1], t[2], t[3], t[4], t[5], t[6], t[7], t[8], calls > 0)
local cb = ffi.cast("int (*)(int, int)", function(x, y) return x * 10 + y end)
p(cb(4, 2), type(cb))
cb:set(function(x, y) return x - y end)
p(cb(4, 2))
cb:free()
e(cb, 1, 2)
e(function() cb:free() end)
local cb2 = ffi.cast("double (*)(double, float, int64_t, bool, char)", function(a, b, c, d, e2) p(a, b, c, d, e2) return a + b end)
p(cb2(1.5, 2.25, 3LL, true, 65))
cb2:free()
local cb3 = ffi.cast("int (*)(int)", function(x) error("boom " .. x) end)
e(cb3, 7)
cb3:free()
e(function()
  C.qsort(arr, 8, 4, function(a, b) error("in qsort") end)
end)
local cbv = ffi.cast("void (*)(void)", function() p("void cb") end) cbv() cbv:free()
local cbp = ffi.cast("const char *(*)(int)", function(i) return "abc" end) p(ffi.string(cbp(1))) cbp:free()
e(ffi.cast, "int (*)(int, ...)", function() end)
e(ffi.cast, "struct {int x;} (*)(int)", function() end)
local cbn = ffi.cast("int (*)(int)", function(x) end)
e(cbn, 1)
cbn:free()
-- function pointers from C, tostring of functions
local fp = ffi.cast("size_t (*)(const char *)", C.strlen)
p(fp("abcd"), ffi.typeof(fp), ffi.typeof(C.strlen))
local m = C.malloc(16) p(m ~= nil) C.free(m)
local gcd = ffi.gc(C.malloc(8), C.free) p(gcd ~= nil) ffi.gc(gcd, nil)
C.free(gcd)
