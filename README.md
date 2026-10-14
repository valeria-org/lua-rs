# lua-rs

A Rust port of [LuaJIT](https://github.com/LuaJIT/LuaJIT), originally created by Mike Pall.
The interpreter, compatibility work, and bundled LuaJIT modules build on the upstream project's
design and implementation. The Rust crate and executable are named `luajit`.

LuaJIT 2.1 rewritten in Rust: the interpreter, every standard library, `string.buffer`, the `ffi`
library (x86-64 System V), LuaJIT bytecode dump/load, the Lua 5.1 C API, and the `luajit` command-line
program. It aims to behave exactly like upstream LuaJIT v2.1 built with `-DLUAJIT_DISABLE_JIT`
(GC64, FFI enabled). That covers output, error messages, bytecode, `luajit -bl` listings and
`string.dump` bytes.

There is **no JIT compiler**. `jit.status()` returns `false` and `jit.on()` raises
`JIT compiler permanently disabled by build option`, as upstream does in that configuration.

## Build and run

```sh
cargo build --release
target/release/luajit -e 'print(jit.version, require"ffi".arch)'
target/release/luajit script.lua args...
target/release/luajit -bl script.lua        # bytecode listing (embedded jit.bc)
target/release/luajit -b script.lua out.o   # bytecode object file (embedded jit.bcsave)
```

The build also produces `target/release/libluajit.so`. It exports the same 148
`lua_*`/`luaL_*`/`luaopen_*`/`luaJIT_*` symbols as upstream, so C code builds against the headers in
`include/` (copied unchanged from upstream). The `luajit` binary exports these symbols too, so C
modules loaded through `require` / `package.loadlib` (for example lua-cjson) work.

Only x86-64 Linux is supported. Install the current stable Rust toolchain and a system linker
to build. The only Rust dependency is `libc`.

## Layout

| Path | Contents | Upstream origin |
|---|---|---|
| `src/gc.rs`, `value.rs`, `lstr.rs`, `table.rs`, `func.rs`, `udata.rs`, `cdata.rs`, `thread.rs`, `state.rs` | object model, stop-the-world mark & sweep GC, interned strings, tables, VM state | lj_gc, lj_obj, lj_str, lj_tab, lj_func, lj_state |
| `src/bc.rs` | bytecode definitions | lj_bc.h |
| `src/parse/` | lexer and single-pass compiler (incl. the v2.1 syntax extensions) | lj_lex, lj_parse |
| `src/vm/` | interpreter, calls/errors/coroutines, metamethods, hooks, debug info, loading, native API | vm_x64.dasc, lj_meta, lj_err, lj_debug, lj_dispatch, lj_load |
| `src/bcdump/` | `\x1bLJ` bytecode writer/reader | lj_bcwrite, lj_bcread |
| `src/text/` | number scanning/formatting, `string.format`, Lua patterns, PRNG | lj_strscan, lj_strfmt*, lib_string, lj_prng |
| `src/stdlib/` | base, coroutine, package, table, io, os, string, math, debug, bit, jit (+`jit.util`, `jit.profile`, embedded `jit/*.lua`), string.buffer | lib_*.c, lj_serialize, lj_buf |
| `src/ffi/` | C types and declaration parser, conversions, cdata arithmetic, C calls, callbacks, `ffi.*` | lj_ctype, lj_cparse, lj_cconv, lj_carith, lj_cdata, lj_ccall, lj_ccallback, lj_clib, lib_ffi |
| `src/capi/` | `extern "C-unwind"` Lua 5.1 C API and lauxlib | lj_api, lib_aux, lib_init |
| `src/cli.rs` | the `luajit` frontend and REPL | luajit.c |

GC objects are reached through `Gc<T>` handles. A collection runs only at safe points: between
instructions, after a native function returns, or in an explicit `collectgarbage`. So a `Value`
kept only in a Rust local stays valid until that code calls back into Lua. Anything that must live
across such a call has to be on the Lua stack.

## Testing

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
LUAJIT_RS_GCSTRESS=1 target/debug/luajit script.lua  # full collection at every GC safe point
```

The regular suite uses committed golden outputs and needs no external LuaJIT checkout. Install
`gcc` and `g++` to run the C and C++ API tests; those tests skip when their compiler is absent.
Two optional tests use the [upstream LuaJIT test suite](https://github.com/LuaJIT/LuaJIT-test-cleanup).
To include them, point `LUAJIT_TEST_DIR` at that checkout's `test` directory:

```sh
LUAJIT_TEST_DIR=/absolute/path/to/LuaJIT-test-cleanup/test cargo test --locked -- --include-ignored
```

For live C API comparisons, fixture regeneration, and benchmarks, build upstream LuaJIT v2.1
with `make XCFLAGS=-DLUAJIT_DISABLE_JIT` and point `LUAJIT_REF` at its binary using an absolute path:

```sh
export LUAJIT_REF=/absolute/path/to/LuaJIT/src/luajit
cargo test --locked --test capi
bench/run.sh
```

You can also run the complete upstream suite with this interpreter:

```sh
cd /absolute/path/to/LuaJIT-test-cleanup/test
/absolute/path/to/lua-rs/target/release/luajit test.lua
```

The previous full upstream-suite run reported 383 passed and 3 failed, matching the reference
build on that setup.

The expected outputs under `tests/fixtures/` were generated with the reference binary; where a
fixture directory has a `gen.sh`, it regenerates them. In debug builds, stress mode also keeps freed
GC cells in quarantine, so a use-after-free panics instead of reading reused memory.

## Known differences from upstream

- No JIT compiler. Interpreter speed is about 1.2–4x slower than upstream's assembler interpreter,
  depending on the workload (`bench/`).
- The collector runs whole cycles at once rather than incrementally. Pacing and the meaning of
  `collectgarbage` results follow lj_gc.c, but byte counts (`collectgarbage"count"`) differ because
  object sizes differ.
- Nested Rust-level calls (natives calling back into Lua) are capped at 1000 and then raise
  `C stack overflow`. Upstream has no such limit and can crash instead.
- `pairs` order for string keys differs, as upstream randomises its string hash on every run. The
  order for number, boolean and integer keys matches upstream.

## Credits and license

This project is released under the [MIT license](LICENSE).

- [LuaJIT](https://github.com/LuaJIT/LuaJIT), Copyright (C) 2005-2026 Mike Pall, is the upstream
  project this port is based on. The headers in `include/` and Lua modules in `src/stdlib/jit/`
  come from LuaJIT and retain their upstream copyright notices.
- [Lua](https://www.lua.org/), by Lua.org and PUC-Rio, provides the language and original C API.
  Lua's copyright notice is retained in the bundled headers.
- [LuaJIT-test-cleanup](https://github.com/LuaJIT/LuaJIT-test-cleanup) supplies upstream conformance
  tests; the vendored C API test sources are recorded in `tests/capi/upstream/UPSTREAM`.
- [lua-cjson](https://github.com/mpx/lua-cjson) is vendored only for C API integration tests. Its
  original MIT license and source revision are retained in `tests/capi/lua-cjson/`.
