//! Library registration from upstream's generated `lj_lib_init_*` byte strings. Port of
//! `lj_lib_register`, `lj_lib_prereg` and `lj_lib_postreg` (lj_lib.c).
//!
//! The init strings (copied verbatim from the reference build's `lj_libdef.h`) encode, in
//! upstream's registration order, the builtin id of the first function, the hash size of the
//! library table, every function name with its kind (C function / assembler fast function /
//! fast function sharing the previous handler), the Lua-coded builtins (`LJLIB_LUA`, as
//! stripped bytecode) and the `LJLIB_PUSH`/`LJLIB_SET` constants. Interpreting them gives
//! library tables with exactly upstream's layout, `pairs` order and `builtin#N` ids.

use crate::error::LuaResult;
use crate::func::{NativeFn, NativeImpl};
use crate::gc::Gc;
use crate::state::Vm;
use crate::table::{Table, hsize2hbits};
use crate::value::Value;
use crate::vm::ErrMsg;

const LIBINIT_LENMASK: u8 = 0x3f;
const LIBINIT_TAGMASK: u8 = 0xc0;
const LIBINIT_CF: u8 = 0x00;
const LIBINIT_STRING: u8 = 0xc0;
const LIBINIT_LUA: u8 = 0xf9;
const LIBINIT_SET: u8 = 0xfa;
const LIBINIT_NUMBER: u8 = 0xfb;
const LIBINIT_COPY: u8 = 0xfc;
const LIBINIT_LASTCL: u8 = 0xfd;
const LIBINIT_FFID: u8 = 0xfe;
const LIBINIT_END: u8 = 0xff;

/// `lib_create_table`: `registry._LOADED[libname]` (created as global `libname` with `hsize`
/// hash slots if missing), or a fresh table for `None`.
fn lib_create_table(vm: &mut Vm, libname: Option<&str>, hsize: u32) -> LuaResult<Gc<Table>> {
    let Some(name) = libname else {
        return vm.new_table(0, hsize2hbits(hsize));
    };
    let loaded = super::loaded_table(vm);
    let key = vm.new_str(name.as_bytes());
    if let Value::Table(t) = loaded.get_str(key) {
        return Ok(t);
    }
    let t = match vm.globals.get_str(key) {
        Value::Nil => {
            let t = vm.new_table(0, hsize2hbits(hsize))?;
            vm.globals.set_str(key, Value::Table(t)).map_err(|e| vm.tab_error(e))?;
            t
        }
        Value::Table(t) => t,
        _ => {
            let m = crate::vm::fmt_msg(ErrMsg::BADMODN, &[name.as_bytes()]);
            return Err(vm.rt_error(&m));
        }
    };
    loaded.set_str(key, Value::Table(t)).map_err(|e| vm.tab_error(e))?;
    Ok(t)
}

/// `lj_lib_register`. `init` is the upstream `lj_lib_init_<name>` string; `funcs` holds one
/// implementation per function entry of the string, in order (including the `LJLIB_ASM_`
/// entries, which upstream implements by sharing the previous C handler and which have their
/// own fast-path semantics here). `stack` plays the role of the Lua stack upstream uses for
/// `LJLIB_PUSH`: values pushed by the caller beforehand can be referenced with `top-N`
/// copies, and the library table is pushed onto it. Returns the library table.
pub fn lib_register(
    vm: &mut Vm,
    libname: Option<&str>,
    init: &[u8],
    funcs: &[NativeFn],
    stack: &mut Vec<Value>,
) -> LuaResult<Gc<Table>> {
    let mut env = vm.cur_thread.env();
    let mut ffid = init[0];
    let tab = lib_create_table(vm, libname, init[2] as u32)?;
    stack.push(Value::Table(tab));
    let tpos = stack.len();
    tab.set_nomm(0);
    let mut p = 3;
    let mut fi = 0;
    let mut ofn = None;
    loop {
        let tag = init[p];
        p += 1;
        let len = (tag & LIBINIT_LENMASK) as usize;
        if tag & LIBINIT_TAGMASK != LIBINIT_STRING {
            let upvals = stack.split_off(tpos);
            let f = funcs[fi];
            fi += 1;
            let func = vm.new_native(NativeImpl::Rust(f), ffid, upvals, env);
            ffid = ffid.wrapping_add(1);
            debug_assert!(tag & LIBINIT_TAGMASK != LIBINIT_CF || len > 0);
            if len > 0 {
                let k = vm.new_str(&init[p..p + len]);
                tab.set_str(k, Value::Func(func)).map_err(|e| vm.tab_error(e))?;
            }
            p += len;
            ofn = Some(func);
        } else {
            match tag {
                LIBINIT_LUA => p = lib_read_lfunc(vm, init, p, tab)?,
                LIBINIT_SET => {
                    let key = stack.pop().expect("libinit SET key");
                    let val = stack.pop().expect("libinit SET value");
                    match key {
                        Value::Str(s) if s.is_empty() => {
                            let Value::Table(t) = val else { unreachable!("libinit env must be a table") };
                            env = t;
                        }
                        _ => vm.raw_set(tab, key, val)?,
                    }
                }
                LIBINIT_NUMBER => {
                    let b: [u8; 8] = init[p..p + 8].try_into().expect("8 bytes");
                    stack.push(Value::Num(f64::from_le_bytes(b)));
                    p += 8;
                }
                LIBINIT_COPY => {
                    let n = init[p] as usize;
                    p += 1;
                    stack.push(stack[stack.len() - n]);
                }
                LIBINIT_LASTCL => stack.push(Value::Func(ofn.expect("libinit LASTCL"))),
                LIBINIT_FFID => ffid = ffid.wrapping_add(1),
                LIBINIT_END => break,
                _ => {
                    let s = vm.new_str(&init[p..p + len]);
                    stack.push(Value::Str(s));
                    p += len;
                }
            }
        }
    }
    debug_assert_eq!(fi, funcs.len(), "libinit: function table size mismatch");
    Ok(tab)
}

/// `lib_read_lfunc`: one Lua-coded builtin (name, then a stripped prototype).
fn lib_read_lfunc(vm: &mut Vm, init: &[u8], mut p: usize, tab: Gc<Table>) -> LuaResult<usize> {
    let len = init[p] as usize;
    p += 1;
    let name = vm.new_str(&init[p..p + len]);
    p += len;
    let (mut pt, n) = crate::bcdump::read_proto(vm, &init[p..], name)?;
    pt.firstline = !0;
    let pt = vm.new_proto(pt);
    let env = vm.cur_thread.env();
    let f = vm.new_lua_function(pt, Vec::new(), env);
    tab.set_str(name, Value::Func(f)).map_err(|e| vm.tab_error(e))?;
    Ok(p + n)
}

/// `registry._PRELOAD` (created on demand, like `luaL_findtable(..., "_PRELOAD", 4)`).
pub fn preload_table(vm: &mut Vm) -> LuaResult<Gc<Table>> {
    let key = vm.new_str(b"_PRELOAD");
    match vm.registry.get_str(key) {
        Value::Table(t) => Ok(t),
        _ => {
            let t = vm.new_table(0, hsize2hbits(4))?;
            vm.registry.set_str(key, Value::Table(t)).map_err(|e| vm.tab_error(e))?;
            Ok(t)
        }
    }
}

/// `lj_lib_prereg`: register `f` (environment `env`) as `registry._PRELOAD[name]`.
pub fn lib_prereg(vm: &mut Vm, name: &str, f: NativeFn, env: Gc<Table>) -> LuaResult<()> {
    let pre = preload_table(vm)?;
    let func = vm.new_native(NativeImpl::Rust(f), crate::func::FF_C, Vec::new(), env);
    let k = vm.new_str(name.as_bytes());
    pre.set_str(k, Value::Func(func)).map_err(|e| vm.tab_error(e))
}

/// `lj_lib_postreg`: create builtin `f`, store it as field `name` of the running loader's
/// environment (the parent library table) and return it.
pub fn lib_postreg(vm: &mut Vm, f: NativeFn, ffid: u8, name: &str) -> LuaResult<usize> {
    let env = vm.current_env();
    let func = vm.new_native(NativeImpl::Rust(f), ffid, Vec::new(), env);
    let k = vm.new_str(name.as_bytes());
    env.set_str(k, Value::Func(func)).map_err(|e| vm.tab_error(e))?;
    vm.ret1(Value::Func(func))
}
