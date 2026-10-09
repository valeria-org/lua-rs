/* A C module for the luajit binary: loaded with require/package.loadlib. */
#include <string.h>

#include "lua.h"
#include "lauxlib.h"

typedef struct Acc { double sum; int n; } Acc;

static int acc_new(lua_State *L)
{
  Acc *a = (Acc *)lua_newuserdata(L, sizeof(Acc));
  a->sum = 0; a->n = 0;
  luaL_getmetatable(L, "cmod.Acc");
  lua_setmetatable(L, -2);
  return 1;
}

static int acc_add(lua_State *L)
{
  Acc *a = (Acc *)luaL_checkudata(L, 1, "cmod.Acc");
  int i, n = lua_gettop(L);
  for (i = 2; i <= n; i++) { a->sum += luaL_checknumber(L, i); a->n++; }
  lua_settop(L, 1);
  return 1;
}

static int acc_mean(lua_State *L)
{
  Acc *a = (Acc *)luaL_checkudata(L, 1, "cmod.Acc");
  if (a->n == 0) return luaL_error(L, "empty accumulator");
  lua_pushnumber(L, a->sum / a->n);
  return 1;
}

static int acc_tostring(lua_State *L)
{
  Acc *a = (Acc *)luaL_checkudata(L, 1, "cmod.Acc");
  lua_pushfstring(L, "Acc(n=%d, sum=%f)", a->n, a->sum);
  return 1;
}

static int map(lua_State *L)
{
  int i, n;
  luaL_checktype(L, 1, LUA_TTABLE);
  luaL_checktype(L, 2, LUA_TFUNCTION);
  n = (int)lua_objlen(L, 1);
  lua_createtable(L, n, 0);
  for (i = 1; i <= n; i++) {
    lua_pushvalue(L, 2);
    lua_rawgeti(L, 1, i);
    lua_call(L, 1, 1);
    lua_rawseti(L, -2, i);
  }
  return 1;
}

static int counter_next(lua_State *L)
{
  int i = (int)lua_tointeger(L, lua_upvalueindex(1)) + 1;
  int lim = (int)lua_tointeger(L, lua_upvalueindex(2));
  if (i > lim) return 0;
  lua_pushinteger(L, i);
  lua_replace(L, lua_upvalueindex(1));
  lua_pushinteger(L, i);
  return 1;
}

static int range(lua_State *L)
{
  lua_pushinteger(L, 0);
  lua_pushinteger(L, luaL_checkinteger(L, 1));
  lua_pushcclosure(L, counter_next, 2);
  return 1;
}

static int gen(lua_State *L)
{
  /* Yields each argument in turn, then returns "end". */
  int n = lua_gettop(L);
  if (n == 0) { lua_pushliteral(L, "end"); return 1; }
  return lua_yield(L, 1);
}

static int join(lua_State *L)
{
  luaL_Buffer b;
  int i, n = lua_gettop(L);
  const char *sep = luaL_checkstring(L, 1);
  luaL_buffinit(L, &b);
  for (i = 2; i <= n; i++) {
    lua_pushvalue(L, i);
    luaL_addvalue(&b);
    if (i < n) luaL_addstring(&b, sep);
  }
  luaL_pushresult(&b);
  return 1;
}

static int env(lua_State *L)
{
  lua_getfield(L, LUA_ENVIRONINDEX, "tag");
  return 1;
}

static const luaL_Reg acc_m[] = {
  { "add", acc_add }, { "mean", acc_mean }, { "__tostring", acc_tostring }, { NULL, NULL }
};

static const luaL_Reg funcs[] = {
  { "new", acc_new }, { "map", map }, { "range", range }, { "gen", gen },
  { "join", join }, { "env", env }, { NULL, NULL }
};

int luaopen_cmod(lua_State *L)
{
  luaL_newmetatable(L, "cmod.Acc");
  lua_pushvalue(L, -1);
  lua_setfield(L, -2, "__index");
  luaL_register(L, NULL, acc_m);
  lua_pop(L, 1);
  lua_newtable(L);
  lua_pushliteral(L, "module-env");
  lua_setfield(L, -2, "tag");
  lua_replace(L, LUA_ENVIRONINDEX);
  luaL_register(L, "cmod", funcs);
  lua_pushliteral(L, "1.0");
  lua_setfield(L, -2, "version");
  return 1;
}

int luaopen_cmod_sub(lua_State *L)
{
  lua_newtable(L);
  lua_pushstring(L, lua_tostring(L, 1));
  lua_setfield(L, -2, "name");
  return 1;
}

int plain_function(lua_State *L)
{
  lua_pushinteger(L, lua_gettop(L));
  lua_pushliteral(L, "plain");
  return 2;
}
