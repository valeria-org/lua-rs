/* Embedding test for the Lua C API: run against libluajit.so of the reference build and of
** luajit-rs; the outputs must be identical. */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "lua.h"
#include "lauxlib.h"
#include "lualib.h"
#include "luajit.h"

static void stack_dump(lua_State *L, const char *tag)
{
  int i, n = lua_gettop(L);
  printf("[%s] top=%d:", tag, n);
  for (i = 1; i <= n; i++) {
    int t = lua_type(L, i);
    switch (t) {
    case LUA_TNUMBER: printf(" %.14g", lua_tonumber(L, i)); break;
    case LUA_TSTRING: printf(" '%s'", lua_tostring(L, i)); break;
    case LUA_TBOOLEAN: printf(" %s", lua_toboolean(L, i) ? "true" : "false"); break;
    default: printf(" <%s>", lua_typename(L, t)); break;
    }
  }
  printf("\n");
}

static void report(lua_State *L, const char *what, int status)
{
  if (status) {
    const char *msg = lua_tostring(L, -1);
    printf("%s: status %d: %s\n", what, status, msg ? msg : "(non-string error)");
    lua_pop(L, 1);
  } else {
    printf("%s: ok\n", what);
  }
}

static void dostring(lua_State *L, const char *code)
{
  report(L, code, luaL_dostring(L, code));
}

/* -- Userdata with metatable -------------------------------------------- */

typedef struct Point { double x, y; } Point;

static int point_new(lua_State *L)
{
  double x = luaL_checknumber(L, 1);
  double y = luaL_optnumber(L, 2, 0);
  Point *p = (Point *)lua_newuserdata(L, sizeof(Point));
  if (((size_t)p & 7) != 0) printf("misaligned userdata\n");
  p->x = x; p->y = y;
  luaL_getmetatable(L, "Point");
  lua_setmetatable(L, -2);
  return 1;
}

static int point_getx(lua_State *L)
{
  Point *p = (Point *)luaL_checkudata(L, 1, "Point");
  lua_pushnumber(L, p->x);
  return 1;
}

static int point_add(lua_State *L)
{
  Point *a = (Point *)luaL_checkudata(L, 1, "Point");
  Point *b = (Point *)luaL_checkudata(L, 2, "Point");
  Point *r = (Point *)lua_newuserdata(L, sizeof(Point));
  r->x = a->x + b->x; r->y = a->y + b->y;
  luaL_getmetatable(L, "Point");
  lua_setmetatable(L, -2);
  return 1;
}

static int point_tostring(lua_State *L)
{
  Point *p = (Point *)luaL_checkudata(L, 1, "Point");
  lua_pushfstring(L, "Point(%f, %f)", p->x, p->y);
  return 1;
}

static int point_gc(lua_State *L)
{
  Point *p = (Point *)luaL_checkudata(L, 1, "Point");
  if (p->x == 1234) printf("gc Point(%g)\n", p->x);
  return 0;
}

static const luaL_Reg point_m[] = {
  { "getx", point_getx },
  { "__add", point_add },
  { "__tostring", point_tostring },
  { "__gc", point_gc },
  { NULL, NULL }
};

static const luaL_Reg point_f[] = {
  { "new", point_new },
  { NULL, NULL }
};

/* -- C closures with upvalues --------------------------------------------- */

static int counter(lua_State *L)
{
  lua_Number n = lua_tonumber(L, lua_upvalueindex(1)) + luaL_optnumber(L, 1, 1);
  lua_pushnumber(L, n);
  lua_pushvalue(L, -1);
  lua_replace(L, lua_upvalueindex(1));
  lua_pushstring(L, lua_tostring(L, lua_upvalueindex(2)));
  return 2;
}

static int newcounter(lua_State *L)
{
  lua_pushnumber(L, luaL_optnumber(L, 1, 0));
  lua_pushstring(L, luaL_optstring(L, 2, "ctr"));
  lua_pushcclosure(L, counter, 2);
  return 1;
}

/* -- Errors --------------------------------------------------------------- */

static int cerror(lua_State *L)
{
  const char *kind = luaL_checkstring(L, 1);
  if (strcmp(kind, "luaL_error") == 0)
    return luaL_error(L, "luaL_error: %s %d %f %c%% [%s]", "str", 42, 2.5, 'x', NULL);
  if (strcmp(kind, "table") == 0) {
    lua_newtable(L);
    lua_pushstring(L, "payload");
    lua_setfield(L, -2, "msg");
    return lua_error(L);
  }
  if (strcmp(kind, "argerror") == 0)
    return luaL_argerror(L, 2, "custom message");
  if (strcmp(kind, "typerror") == 0)
    return luaL_typerror(L, 1, "widget");
  if (strcmp(kind, "option") == 0) {
    static const char *const opts[] = { "alpha", "beta", NULL };
    return luaL_checkoption(L, 2, NULL, opts);
  }
  if (strcmp(kind, "checkint") == 0) {
    lua_pushinteger(L, luaL_checkinteger(L, 2));
    return 1;
  }
  if (strcmp(kind, "where") == 0) {
    luaL_where(L, 1);
    return 1;
  }
  lua_pushvalue(L, 1);
  return lua_error(L);
}

static int callback(lua_State *L)
{
  /* Calls its argument, unprotected: errors propagate through this C frame. */
  luaL_checktype(L, 1, LUA_TFUNCTION);
  lua_pushvalue(L, 1);
  lua_pushstring(L, "from C");
  lua_call(L, 1, 1);
  lua_pushstring(L, " and back");
  lua_concat(L, 2);
  return 1;
}

/* -- Yield from C --------------------------------------------------------- */

static int cyield(lua_State *L)
{
  int n = lua_gettop(L);
  lua_pushstring(L, "yielded");
  lua_insert(L, 1);
  return lua_yield(L, n + 1);
}

/* -- Reader / writer ------------------------------------------------------ */

typedef struct ReadState { const char *s; size_t pos, len; int calls; } ReadState;

static const char *reader(lua_State *L, void *ud, size_t *size)
{
  ReadState *rs = (ReadState *)ud;
  (void)L;
  rs->calls++;
  if (rs->pos >= rs->len) return NULL;
  *size = rs->len - rs->pos > 3 ? 3 : rs->len - rs->pos;
  rs->pos += *size;
  return rs->s + rs->pos - *size;
}

typedef struct WriteState { char buf[65536]; size_t len; } WriteState;

static int writer(lua_State *L, const void *p, size_t sz, void *ud)
{
  WriteState *ws = (WriteState *)ud;
  (void)L;
  memcpy(ws->buf + ws->len, p, sz);
  ws->len += sz;
  return 0;
}

/* -- Debug API ------------------------------------------------------------ */

static int inspect(lua_State *L)
{
  lua_Debug ar;
  int level;
  for (level = 0; lua_getstack(L, level, &ar); level++) {
    const char *name;
    int i;
    lua_getinfo(L, "nSlu", &ar);
    printf("  level %d: %s %s '%s' %s line %d [%d-%d] nups=%d src=%s\n", level,
	   ar.what, ar.namewhat, ar.name ? ar.name : "?", ar.short_src,
	   ar.currentline, ar.linedefined, ar.lastlinedefined, ar.nups,
	   ar.source);
    for (i = 1; (name = lua_getlocal(L, &ar, i)) != NULL; i++) {
      printf("    local %d %s = %s\n", i, name, luaL_typename(L, -1));
      lua_pop(L, 1);
    }
  }
  lua_pushvalue(L, 1);
  lua_getinfo(L, ">Su", &ar);
  printf("  arg: %s linedefined=%d nups=%d\n", ar.what, ar.linedefined, ar.nups);
  return 0;
}

static int hook_lines;
static void line_hook(lua_State *L, lua_Debug *ar)
{
  lua_getinfo(L, "Sl", ar);
  if (ar->event == LUA_HOOKLINE) {
    hook_lines++;
    printf("  hook line %d (%s)\n", ar->currentline, ar->short_src);
  } else if (ar->event == LUA_HOOKCALL) {
    printf("  hook call %s\n", ar->what);
  }
}

static int traceback(lua_State *L)
{
  luaL_traceback(L, L, lua_tostring(L, 1), 1);
  return 1;
}

static int cpcall_fn(lua_State *L)
{
  int *p = (int *)lua_touserdata(L, 1);
  printf("cpcall got %d, top=%d\n", *p, lua_gettop(L));
  if (*p == 2) luaL_error(L, "cpcall failure %d", *p);
  return 0;
}

static int panicf(lua_State *L)
{
  printf("custom panic: %s\n", lua_tostring(L, -1));
  fflush(stdout);
  return 0;
}

int main(int argc, char **argv)
{
  lua_State *L = luaL_newstate();
  int status, i, r1, r2, r3;
  luaL_openlibs(L);

  if (argc > 1 && strcmp(argv[1], "panic") == 0) {
    lua_pushstring(L, "unprotected!");
    lua_error(L);
    return 0;
  }
  if (argc > 1 && strcmp(argv[1], "atpanic") == 0) {
    lua_atpanic(L, panicf);
    lua_getglobal(L, "error");
    lua_pushstring(L, "boom");
    lua_call(L, 1, 0);
    return 0;
  }

  printf("== basics\n");
  printf("version %.0f\n", *lua_version(L));
  dostring(L, "print('hello from Lua', 1/3, 2^53, -0.0, 1e300*10)");
  dostring(L, "x = ");
  dostring(L, "error('runtime')");
  dostring(L, "error({})");
  dostring(L, "local t = nil; return t.x");
  status = luaL_loadbuffer(L, "return 1, 'two', nil, {}", 24, "=chunk");
  printf("load status %d\n", status);
  lua_call(L, 0, LUA_MULTRET);
  stack_dump(L, "results");
  lua_settop(L, 0);

  printf("== stack ops\n");
  lua_pushnumber(L, 1); lua_pushstring(L, "a"); lua_pushboolean(L, 0);
  lua_pushnil(L); lua_pushinteger(L, 42);
  stack_dump(L, "push");
  lua_insert(L, 1); stack_dump(L, "insert");
  lua_remove(L, 2); stack_dump(L, "remove");
  lua_replace(L, 1); stack_dump(L, "replace");
  lua_pushvalue(L, 2); stack_dump(L, "pushvalue");
  lua_settop(L, 6); stack_dump(L, "settop6");
  lua_settop(L, -3); stack_dump(L, "settop-3");
  lua_pushnumber(L, 3.25);
  printf("tostring in place: %s type now %s\n", lua_tostring(L, -1), luaL_typename(L, -1));
  lua_pushstring(L, "0x10");
  printf("isnumber %d tonumber %g tointeger %ld\n", lua_isnumber(L, -1), lua_tonumber(L, -1), (long)lua_tointeger(L, -1));
  lua_pushstring(L, "abc");
  printf("isnumber %d isstring %d objlen %d\n", lua_isnumber(L, -1), lua_isstring(L, -1), (int)lua_objlen(L, -1));
  printf("type of 100: %d (%s), type of upvalue: %d\n", lua_type(L, 100), lua_typename(L, lua_type(L, 100)), lua_type(L, lua_upvalueindex(1)));
  lua_settop(L, 0);
  lua_pushliteral(L, "con"); lua_pushnumber(L, 7); lua_pushliteral(L, "cat");
  lua_concat(L, 3);
  lua_concat(L, 0);
  stack_dump(L, "concat");
  lua_settop(L, 0);
  {
    lua_State *L1 = lua_newthread(L);
    lua_pushnumber(L, 1); lua_pushnumber(L, 2); lua_pushnumber(L, 3);
    lua_xmove(L, L1, 2);
    printf("xmove: L top %d, L1 top %d, L1[1]=%g\n", lua_gettop(L), lua_gettop(L1), lua_tonumber(L1, 1));
    r1 = lua_tothread(L, 1) == L1;
    r2 = lua_pushthread(L);
    r3 = lua_pushthread(L1);
    printf("tothread roundtrip %d, pushthread main %d, L1 %d, L1 top %d\n", r1, r2, r3, lua_gettop(L1));
    printf("pushthread value equal %d\n", lua_tothread(L, -1) == L);
    lua_settop(L, 0);
  }

  printf("== tables\n");
  lua_createtable(L, 4, 4);
  for (i = 1; i <= 4; i++) { lua_pushinteger(L, i * 10); lua_rawseti(L, -2, i); }
  lua_pushstring(L, "v"); lua_setfield(L, -2, "k");
  lua_getfield(L, -1, "k");
  lua_rawgeti(L, -2, 3);
  stack_dump(L, "get");
  lua_pop(L, 2);
  printf("objlen %d\n", (int)lua_objlen(L, -1));
  lua_pushnil(L);
  while (lua_next(L, 1)) {
    if (lua_type(L, -2) == LUA_TNUMBER)
      printf("  [%g] = %s\n", lua_tonumber(L, -2), lua_tostring(L, -1));
    lua_pop(L, 1);
  }
  lua_setglobal(L, "T");
  dostring(L, "setmetatable(T, {__index=function(t,k) return 'idx:'..k end, __newindex=function(t,k,v) rawset(t,k,v..'!') end})");
  lua_getglobal(L, "T");
  lua_getfield(L, -1, "missing");
  lua_pushstring(L, "new"); lua_pushstring(L, "val"); lua_settable(L, 1);
  lua_getfield(L, 1, "new");
  lua_pushstring(L, "missing"); lua_rawget(L, 1);
  stack_dump(L, "meta");
  printf("getmetatable %d\n", lua_getmetatable(L, 1));
  lua_settop(L, 0);
  luaL_findtable(L, LUA_GLOBALSINDEX, "a.b.c", 2);
  lua_pushnumber(L, 5); lua_setfield(L, -2, "d");
  lua_settop(L, 0);
  dostring(L, "print('a.b.c.d =', a.b.c.d)");
  printf("findtable conflict: %s\n", luaL_findtable(L, LUA_GLOBALSINDEX, "a.b.c.d.e", 0));
  lua_settop(L, 0);

  printf("== userdata\n");
  luaL_newmetatable(L, "Point");
  lua_pushvalue(L, -1);
  lua_setfield(L, -2, "__index");
  luaL_register(L, NULL, point_m);
  printf("newmetatable again: %d\n", luaL_newmetatable(L, "Point"));
  lua_settop(L, 0);
  luaL_register(L, "point", point_f);
  lua_settop(L, 0);
  dostring(L, "local a, b = point.new(1, 2), point.new(10) print(a:getx(), tostring(a + b), type(a))");
  dostring(L, "print(pcall(point.new().getx))");
  dostring(L, "print(pcall(point.new(1).getx, {}))");
  dostring(L, "print(pcall(function() return point.new(1):getx() + point.new('x') end))");
  dostring(L, "do local p = point.new(1234) end collectgarbage() collectgarbage()");
  lua_pushnumber(L, 1);
  printf("testudata on number: %p\n", luaL_testudata(L, -1, "Point"));
  lua_settop(L, 0);

  printf("== closures and registry\n");
  lua_register(L, "newcounter", newcounter);
  dostring(L, "local c = newcounter(10, 'C') print(c()) print(c(5)) local d = newcounter() print(d(), c())");
  lua_pushstring(L, "first"); r1 = luaL_ref(L, LUA_REGISTRYINDEX);
  lua_pushstring(L, "second"); r2 = luaL_ref(L, LUA_REGISTRYINDEX);
  lua_pushnil(L); printf("ref nil: %d\n", luaL_ref(L, LUA_REGISTRYINDEX));
  luaL_unref(L, LUA_REGISTRYINDEX, r1);
  lua_pushstring(L, "third"); r3 = luaL_ref(L, LUA_REGISTRYINDEX);
  printf("ref reuse: %d\n", r3 == r1);
  lua_rawgeti(L, LUA_REGISTRYINDEX, r2);
  lua_rawgeti(L, LUA_REGISTRYINDEX, r3);
  stack_dump(L, "refs");
  lua_settop(L, 0);
  lua_newtable(L);
  for (i = 0; i < 5; i++) { lua_pushinteger(L, i); printf("%d ", luaL_ref(L, 1)); }
  luaL_unref(L, 1, 2); luaL_unref(L, 1, 4);
  lua_pushinteger(L, 9); printf("-> %d", luaL_ref(L, 1));
  lua_pushinteger(L, 9); printf(" %d", luaL_ref(L, 1));
  lua_pushinteger(L, 9); printf(" %d\n", luaL_ref(L, 1));
  lua_settop(L, 0);

  printf("== errors\n");
  lua_register(L, "cerror", cerror);
  lua_register(L, "callback", callback);
  dostring(L, "print(pcall(cerror, 'luaL_error'))");
  dostring(L, "local ok, e = pcall(cerror, 'table') print(ok, type(e), e.msg)");
  dostring(L, "print(pcall(cerror, 'argerror'))");
  dostring(L, "print(pcall(cerror, 'typerror'))");
  dostring(L, "print(pcall(cerror, 'option', 'gamma'))");
  dostring(L, "print(pcall(cerror, 'option', 'beta'))");
  dostring(L, "print(pcall(cerror, 'option'))");
  dostring(L, "print(pcall(cerror, 'checkint', '12.7'))");
  dostring(L, "print(pcall(cerror, 'checkint', 'z'))");
  dostring(L, "print(pcall(cerror))");
  dostring(L, "print(cerror('where'))");
  dostring(L, "print(pcall(cerror, 'plain'))");
  dostring(L, "print(pcall(cerror, 'luaL_error'))\nlocal function f() cerror('luaL_error') end\nprint(pcall(f))");
  dostring(L, "print(callback(function(s) return '<' .. s .. '>' end))");
  dostring(L, "print(pcall(callback, function(s) error('inner: '..s) end))");
  dostring(L, "print(pcall(callback, function(s) cerror('luaL_error') end))");
  dostring(L, "print(xpcall(function() cerror('luaL_error') end, function(m) return 'handled: '..m end))");
  dostring(L, "print(pcall(callback, 42))");
  /* lua_pcall with an error handler on the stack. */
  lua_getglobal(L, "debug");
  lua_getfield(L, -1, "traceback");
  lua_remove(L, -2);
  luaL_loadstring(L, "local function lvl2() error('deep error') end\nlocal function lvl1() lvl2() end\nlvl1()");
  status = lua_pcall(L, 0, 0, 1);
  printf("pcall with traceback handler: %d\n%s\n", status, lua_tostring(L, -1));
  lua_settop(L, 0);
  lua_pushcfunction(L, cerror);
  lua_pushstring(L, "luaL_error");
  status = lua_pcall(L, 1, 0, 0);
  printf("pcall C function: %d %s\n", status, lua_tostring(L, -1));
  lua_settop(L, 0);
  i = 1; status = lua_cpcall(L, cpcall_fn, &i); printf("cpcall status %d top %d\n", status, lua_gettop(L));
  i = 2; status = lua_cpcall(L, cpcall_fn, &i); printf("cpcall status %d: %s\n", status, lua_tostring(L, -1));
  lua_settop(L, 0);
  /* Error in error handler. */
  lua_pushcfunction(L, cerror);
  luaL_loadstring(L, "error('x')");
  printf("errerr status %d: %s\n", lua_pcall(L, 0, 0, 1), lua_tostring(L, -1));
  lua_settop(L, 0);

  printf("== coroutines\n");
  lua_register(L, "cyield", cyield);
  dostring(L, "local co = coroutine.wrap(function(a) local x, y, z = cyield(a, a * 2) print('resumed with', x, y, z) return 'done' end)\n"
	      "print(co(5)) print(co('p', 'q'))");
  dostring(L, "print(pcall(cyield, 1))");
  dostring(L, "print(coroutine.resume(coroutine.create(function() return pcall(cyield, 1) end)))");
  {
    lua_State *co = lua_newthread(L);
    luaL_loadstring(co, "local a, b = ... print('co start', a, b) local c = coroutine.yield(a + b) print('co got', c) local d = cyield(c) print('co got', d) return 'fin', 99");
    lua_pushnumber(co, 1); lua_pushnumber(co, 2);
    status = lua_resume(co, 2);
    printf("resume1 status %d (lua_status %d)\n", status, lua_status(co));
    stack_dump(co, "co");
    lua_settop(co, 0);
    lua_pushstring(co, "three");
    status = lua_resume(co, 1);
    printf("resume2 status %d\n", status);
    stack_dump(co, "co");
    lua_settop(co, 0);
    lua_pushstring(co, "four");
    status = lua_resume(co, 1);
    printf("resume3 status %d (lua_status %d)\n", status, lua_status(co));
    stack_dump(co, "co");
    lua_settop(L, 0);
    co = lua_newthread(L);
    luaL_loadstring(co, "error('in coroutine')");
    status = lua_resume(co, 0);
    printf("resume error: %d %s (lua_status %d)\n", status, lua_tostring(co, -1), lua_status(co));
    status = lua_resume(co, 0);
    printf("resume dead: %d %s\n", status, lua_tostring(co, -1));
    lua_settop(L, 0);
    co = lua_newthread(L);
    lua_getglobal(co, "print");
    lua_pushstring(co, "plain C function via lua_resume");
    printf("resume C: %d top %d\n", lua_resume(co, 1), lua_gettop(co));
    lua_settop(L, 0);
  }

  printf("== load and dump\n");
  {
    ReadState rs;
    WriteState ws;
    const char *src = "local a = ... return 'read ' .. a, #a";
    rs.s = src; rs.pos = 0; rs.len = strlen(src); rs.calls = 0;
    status = lua_load(L, reader, &rs, "=reader");
    printf("lua_load status %d after %d reader calls\n", status, rs.calls);
    lua_pushvalue(L, -1);
    lua_pushstring(L, "abc");
    lua_call(L, 1, 2);
    stack_dump(L, "loaded");
    lua_pop(L, 2);
    ws.len = 0;
    status = lua_dump(L, writer, &ws);
    printf("dump status %d, starts with ESC L J: %d\n", status, ws.len > 3 && memcmp(ws.buf, "\033LJ", 3) == 0);
    lua_settop(L, 0);
    status = luaL_loadbuffer(L, ws.buf, ws.len, "=dumped");
    printf("reload status %d\n", status);
    lua_pushstring(L, "xy");
    lua_call(L, 1, 2);
    stack_dump(L, "reloaded");
    lua_settop(L, 0);
    rs.s = "return +"; rs.pos = 0; rs.len = 8; rs.calls = 0;
    status = lua_load(L, reader, &rs, "@broken.lua");
    printf("syntax error status %d: %s\n", status, lua_tostring(L, -1));
    lua_settop(L, 0);
    status = luaL_loadbufferx(L, "return 1", 8, "=mode", "b");
    printf("mode error status %d: %s\n", status, lua_tostring(L, -1));
    lua_settop(L, 0);
    status = luaL_loadfile(L, "/nonexistent/file.lua");
    printf("loadfile status %d: %s\n", status, lua_tostring(L, -1));
    lua_settop(L, 0);
    lua_pushcfunction(L, cerror);
    printf("dump of C function: %d\n", lua_dump(L, writer, &ws));
    lua_settop(L, 0);
  }

  printf("== debug\n");
  lua_register(L, "inspect", inspect);
  dostring(L, "local function outer(p, q)\n  local r = p + q\n  inspect(outer)\n  return r\nend\nouter(1, 2)");
  lua_sethook(L, line_hook, LUA_MASKLINE | LUA_MASKCALL, 0);
  printf("hook mask %d count %d set %d\n", lua_gethookmask(L), lua_gethookcount(L), lua_gethook(L) == line_hook);
  luaL_loadbuffer(L, "local x = 1\nx = x + 1\nreturn x", 30, "=hooked");
  lua_call(L, 0, 0);
  lua_sethook(L, NULL, 0, 0);
  printf("hook lines %d, hook now %d\n", hook_lines, lua_gethook(L) == NULL);
  lua_register(L, "ctraceback", traceback);
  dostring(L, "local function f() print(ctraceback('msg here')) end f()");
  luaL_traceback(L, L, NULL, 0);
  printf("toplevel traceback: %s\n", lua_tostring(L, -1));
  lua_settop(L, 0);
  luaL_loadstring(L, "local up1, up2 = 1, 'two' return function() return up1, up2 end");
  lua_call(L, 0, 1);
  for (i = 1; i <= 2; i++) {
    const char *uvn = lua_getupvalue(L, -1, i);
    printf("upvalue %d %s = %s\n", i, uvn, lua_tostring(L, -1));
    lua_pop(L, 1);
  }
  printf("upvalue 3 %p\n", (void *)lua_getupvalue(L, -1, 3));
  lua_pushstring(L, "changed");
  printf("setupvalue %s\n", lua_setupvalue(L, -2, 2));
  lua_call(L, 0, 2);
  stack_dump(L, "upvalues");
  lua_settop(L, 0);
  lua_pushnumber(L, 0);
  lua_pushcclosure(L, counter, 1);
  printf("C upvalue name '%s'\n", lua_getupvalue(L, -1, 1));
  lua_settop(L, 0);

  printf("== auxiliary\n");
  printf("gsub: %s\n", luaL_gsub(L, "hello world, hello moon", "hello", "bye"));
  lua_settop(L, 0);
  {
    luaL_Buffer b;
    int k;
    luaL_buffinit(L, &b);
    for (k = 0; k < 3000; k++) {
      char tmp[32];
      sprintf(tmp, "%d,", k);
      luaL_addstring(&b, tmp);
    }
    luaL_addchar(&b, '|');
    lua_pushnumber(L, 12.5);
    luaL_addvalue(&b);
    luaL_addlstring(&b, "xyz", 2);
    luaL_pushresult(&b);
    printf("buffer len %d tail %s top %d\n", (int)lua_objlen(L, -1), lua_tostring(L, -1) + lua_objlen(L, -1) - 12, lua_gettop(L));
    lua_settop(L, 0);
  }
  printf("pushfstring: %s\n", lua_pushfstring(L, "%s|%d|%f|%c|%%|%s|%5d|%x", "s", -7, 0.1, 'Z', "end", 42, 255));
  lua_settop(L, 0);
  lua_pushnumber(L, 1); lua_pushnumber(L, 2);
  printf("lessthan %d %d equal %d rawequal %d\n", lua_lessthan(L, 1, 2), lua_lessthan(L, 2, 1), lua_equal(L, 1, 1), lua_rawequal(L, 1, 2));
  lua_settop(L, 0);
  printf("gc collect %d\n", lua_gc(L, LUA_GCCOLLECT, 0));
  printf("gc running %d\n", lua_gc(L, LUA_GCISRUNNING, 0));
  lua_gc(L, LUA_GCSTOP, 0);
  printf("gc running after stop %d\n", lua_gc(L, LUA_GCISRUNNING, 0));
  lua_gc(L, LUA_GCRESTART, 0);
  printf("gc setpause old %d\n", lua_gc(L, LUA_GCSETPAUSE, 150));
  printf("gc setpause old %d\n", lua_gc(L, LUA_GCSETPAUSE, 200));
  printf("gc bad option %d\n", lua_gc(L, 42, 0));
  printf("setmode engine on %d off %d\n", luaJIT_setmode(L, 0, LUAJIT_MODE_ENGINE | LUAJIT_MODE_ON), luaJIT_setmode(L, 0, LUAJIT_MODE_ENGINE | LUAJIT_MODE_OFF));
  {
    void *ud;
    lua_Alloc f = lua_getallocf(L, &ud);
    void *p = f(ud, NULL, 0, 100);
    memset(p, 1, 100);
    p = f(ud, p, 100, 200);
    printf("allocf works %d\n", f(ud, p, 200, 0) == NULL);
  }
  printf("checkstack %d %d\n", lua_checkstack(L, 100), lua_checkstack(L, 100000));
  printf("isyieldable main %d\n", lua_isyieldable(L));
  LUAJIT_VERSION_SYM();
  printf("top at end %d\n", lua_gettop(L));
  lua_close(L);
  printf("closed\n");
  return 0;
}
