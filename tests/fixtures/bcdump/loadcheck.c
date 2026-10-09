/*
** Load one chunk with the reference lua_load() and print the outcome.
** Usage: loadcheck <input hex> <chunkname hex | ->   ("-": the input itself, like loadstring)
** Output: "OK <status> <source hex> <string.dump(f, "d") hex>" or "ERR <status> <message>",
** or "UNSTABLE".
**
** The reader returns the input in one piece, like luaL_loadbuffer. The input is followed by
** 1 MiB of filler; upstream reads malformed dumps past the end of their buffer, so the chunk is
** loaded once per filler byte and a result that depends on the filler is reported as UNSTABLE.
** Built by gen.sh against the reference libluajit.a.
*/
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "lua.h"
#include "lauxlib.h"
#include "lualib.h"

#define FILLER_SIZE (1 << 20)

typedef struct Chunk { const char *p; size_t n; int done; } Chunk;

static const char *reader(lua_State *L, void *ud, size_t *sz)
{
  Chunk *c = (Chunk *)ud;
  (void)L;
  if (c->done || c->n == 0) return NULL;
  c->done = 1;
  *sz = c->n;
  return c->p;
}

static size_t unhex(const char *h, char *out)
{
  size_t n = 0;
  for (; h[0] && h[1]; h += 2) {
    unsigned int b;
    sscanf(h, "%2x", &b);
    out[n++] = (char)b;
  }
  return n;
}

static void hex(char *out, const char *s, size_t n)
{
  size_t i;
  for (i = 0; i < n; i++) sprintf(out + 2 * i, "%02x", (unsigned char)s[i]);
  out[2 * n] = 0;
}

/* Load with the given filler byte; returns a malloc'ed result line. */
static char *run(const char *input, size_t n, const char *name, int fill)
{
  char *buf = malloc(n + FILLER_SIZE + 1);
  char *res;
  Chunk c;
  int status;
  lua_State *L = luaL_newstate();
  luaL_openlibs(L);
  memcpy(buf, input, n);
  memset(buf + n, fill, FILLER_SIZE);
  buf[n + FILLER_SIZE] = 0;
  c.p = buf; c.n = n; c.done = 0;
  status = lua_load(L, reader, &c, name ? name : buf);
  if (status == 0) {
    lua_Debug ar;
    size_t len, dlen;
    const char *d;
    lua_getglobal(L, "string");
    lua_getfield(L, -1, "dump");
    lua_pushvalue(L, -3);
    lua_pushliteral(L, "d");
    lua_call(L, 2, 1);  /* fn string dump */
    d = lua_tolstring(L, -1, &dlen);
    lua_pushvalue(L, -3);
    lua_getinfo(L, ">S", &ar);
    len = strlen(ar.source);
    res = malloc(40 + 2 * len + 2 * dlen);
    sprintf(res, "OK %d ", status);
    hex(res + strlen(res), ar.source, len);
    strcat(res, " ");
    hex(res + strlen(res), d, dlen);
  } else {
    size_t len;
    const char *msg = lua_tolstring(L, -1, &len);
    res = malloc(32 + len);
    sprintf(res, "ERR %d %s", status, msg);
  }
  lua_close(L);
  free(buf);
  return res;
}

int main(int argc, char **argv)
{
  static const int fills[] = { 0x00, 0xaa, 0xff, 0x01 };
  char *input, *name = NULL, *first;
  size_t n, i;
  if (argc != 3) return 1;
  input = malloc(strlen(argv[1]) / 2 + 1);
  n = unhex(argv[1], input);
  if (strcmp(argv[2], "-") != 0) {
    size_t nn;
    name = malloc(strlen(argv[2]) / 2 + 1);
    nn = unhex(argv[2], name);
    name[nn] = 0;
  }
  first = run(input, n, name, fills[0]);
  for (i = 1; i < sizeof(fills) / sizeof(fills[0]); i++) {
    char *r = run(input, n, name, fills[i]);
    if (strcmp(r, first) != 0) { puts("UNSTABLE"); return 0; }
    free(r);
  }
  puts(first);
  return 0;
}
