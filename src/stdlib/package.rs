//! Package library. Port of lib_package.c (plus `luaL_findtable`/`luaL_pushmodule` of
//! lib_aux.c), dlopen variant.
//!
//! Upstream runs `luaopen_package` with the package table as its C environment, so the
//! loaders, `require` and `module` see the package table as their environment
//! (`LUA_ENVIRONINDEX`) and read `path`, `cpath`, `loaders` and `preload` through it.
//! `loadlib`, `searchpath` and `seeall` are created before that and keep the globals.

use std::ffi::{CStr, CString, c_void};

use crate::error::{ErrorKind, LuaResult};
use crate::func::{CFunction, FF_C, NativeImpl};
use crate::gc::Gc;
use crate::lstr::LuaStr;
use crate::parse::lex::cstr;
use crate::state::Vm;
use crate::table::Table;
use crate::value::Value;
use crate::vm::{ErrMsg, InfoTarget, fmt_msg};

use super::{LibFn, register_lib};

/// Error codes for `ll_loadfunc`.
const PACKAGE_ERR_LIB: i32 = 1;
const PACKAGE_ERR_FUNC: i32 = 2;
const PACKAGE_ERR_LOAD: i32 = 3;
const PACKAGE_LIB_FAIL: &[u8] = b"open";

const SYMPREFIX_CF: &[u8] = b"luaopen_";
const SYMPREFIX_BC: &[u8] = b"luaJIT_BC_";

/// `LUA_PATH_DEFAULT` / `LUA_CPATH_DEFAULT` of a default POSIX build (luaconf.h).
pub const LUA_PATH_DEFAULT: &str =
    "./?.lua;/usr/local/share/luajit-2.1/?.lua;/usr/local/share/lua/5.1/?.lua;/usr/local/share/lua/5.1/?/init.lua";
pub const LUA_CPATH_DEFAULT: &str = "./?.so;/usr/local/lib/lua/5.1/?.so;/usr/local/lib/lua/5.1/loadall.so";
/// `LUA_PATH_CONFIG`.
const LUA_PATH_CONFIG: &str = "/\n;\n?\n!\n-\n";
/// `LUA_LJDIR`: where the `jit.*` modules are installed upstream; embedded modules get
/// chunk names below it.
const LUA_LJDIR: &str = "/usr/local/share/luajit-2.1/";

/// `KEY_SENTINEL`: marks a module that is being loaded (a number with this bit pattern).
fn key_sentinel() -> Value {
    Value::Num(f64::from_bits(0x8100_0000_0000_0000 | b's' as u64))
}

fn is_sentinel(v: Value) -> bool {
    matches!(v, Value::Num(n) if n.to_bits() == 0x8100_0000_0000_0000 | b's' as u64)
}

/// `luaopen_package`.
pub fn open_package(vm: &mut Vm) -> LuaResult<()> {
    // luaL_newmetatable(L, "_LOADLIB") with __gc = unloadlib.
    let mt = vm.new_table(0, crate::table::hsize2hbits(2))?;
    let k = vm.new_str(b"_LOADLIB");
    vm.registry.set_str(k, Value::Table(mt)).map_err(|e| vm.tab_error(e))?;
    let gc = super::new_lib_fn(vm, lj_cf_package_unloadlib, FF_C, vec![]);
    let k = vm.new_str(b"__gc");
    vm.setindex(Value::Table(mt), Value::Str(k), Value::Func(gc))?;

    let funcs: &[LibFn] = &[
        ("loadlib", lj_cf_package_loadlib, FF_C),
        ("searchpath", lj_cf_package_searchpath, FF_C),
        ("seeall", lj_cf_package_seeall, FF_C),
    ];
    let pkg = register_lib(vm, "package", funcs)?;
    // From here on the "current environment" of luaopen_package is the package table.
    let loaders = vm.new_table(4, 0)?;
    let lf: [crate::func::NativeFn; 4] =
        [lj_cf_package_loader_preload, lj_cf_package_loader_lua, lj_cf_package_loader_c, lj_cf_package_loader_croot];
    for (i, f) in lf.into_iter().enumerate() {
        let f = vm.new_native(NativeImpl::Rust(f), FF_C, vec![], pkg);
        loaders.set_int(i as i32 + 1, Value::Func(f)).map_err(|e| vm.tab_error(e))?;
    }
    set_field(vm, pkg, b"loaders", Value::Table(loaders))?;
    let k = vm.new_str(b"LUA_NOENV");
    let noenv = vm.registry.get_str(k).is_truthy();
    setpath(vm, pkg, b"path", "LUA_PATH", LUA_PATH_DEFAULT, noenv)?;
    setpath(vm, pkg, b"cpath", "LUA_CPATH", LUA_CPATH_DEFAULT, noenv)?;
    let v = vm.new_str(LUA_PATH_CONFIG.as_bytes());
    set_field(vm, pkg, b"config", Value::Str(v))?;
    let reg = vm.registry;
    let loaded = find_table(vm, reg, b"_LOADED", 16)?.expect("registry._LOADED");
    set_field(vm, pkg, b"loaded", Value::Table(loaded))?;
    let preload = find_table(vm, reg, b"_PRELOAD", 4)?.expect("registry._PRELOAD");
    set_field(vm, pkg, b"preload", Value::Table(preload))?;
    let g = vm.cur_thread.env();
    for (name, f) in [("module", lj_cf_package_module as crate::func::NativeFn), ("require", lj_cf_package_require)] {
        let f = vm.new_native(NativeImpl::Rust(f), FF_C, vec![], pkg);
        set_field(vm, g, name.as_bytes(), Value::Func(f))?;
    }
    Ok(())
}

/// `lua_setfield(L, t, k)` on a table (metamethod-aware).
fn set_field(vm: &mut Vm, t: Gc<Table>, k: &[u8], v: Value) -> LuaResult<()> {
    let k = vm.new_str(k);
    vm.setindex(Value::Table(t), Value::Str(k), v)
}

/// `lua_getfield(L, idx, k)` (metamethod-aware).
fn get_field(vm: &mut Vm, o: Value, k: &[u8]) -> LuaResult<Value> {
    let k = vm.new_str(k);
    vm.index(o, Value::Str(k))
}

/// `luaL_gsub`: replace every occurrence of `p` (non-empty) in `s` by `r`.
fn gsub(s: &[u8], p: &[u8], r: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        if s[i..].starts_with(p) {
            out.extend_from_slice(r);
            i += p.len();
        } else {
            out.push(s[i]);
            i += 1;
        }
    }
    out
}

/// `luaL_findtable(L, t, fname, szhint)`: walk/create the dotted path `fname` below `t`.
/// Returns `None` if a component holds a non-table value.
pub fn find_table(vm: &mut Vm, t: Gc<Table>, fname: &[u8], szhint: u32) -> LuaResult<Option<Gc<Table>>> {
    let mut t = t;
    let mut rest = fname;
    loop {
        let (part, more) = match rest.iter().position(|&c| c == b'.') {
            Some(p) => (&rest[..p], Some(&rest[p + 1..])),
            None => (rest, None),
        };
        let k = vm.new_str(part);
        match t.get_str(k) {
            Value::Nil => {
                let hint = if more.is_some() { 1 } else { szhint };
                let nt = vm.new_table(0, crate::table::hsize2hbits(hint))?;
                vm.setindex(Value::Table(t), Value::Str(k), Value::Table(nt))?;
                t = nt;
            }
            Value::Table(nt) => t = nt,
            _ => return Ok(None),
        }
        match more {
            Some(r) => rest = r,
            None => return Ok(Some(t)),
        }
    }
}

/// `luaL_pushmodule(L, modname, sizehint)`: the module table `_LOADED[modname]`, created as a
/// global (dotted path) if needed.
pub fn push_module(vm: &mut Vm, modname: &[u8], sizehint: u32) -> LuaResult<Gc<Table>> {
    let reg = vm.registry;
    let loaded = find_table(vm, reg, b"_LOADED", 16)?.expect("registry._LOADED");
    let v = get_field(vm, Value::Table(loaded), modname)?;
    if let Value::Table(t) = v {
        return Ok(t);
    }
    let g = vm.cur_thread.env();
    match find_table(vm, g, modname, sizehint)? {
        Some(t) => {
            set_field(vm, loaded, modname, Value::Table(t))?;
            Ok(t)
        }
        None => {
            let m = fmt_msg(ErrMsg::BADMODN, &[modname]);
            Err(vm.err_callermsg(&m))
        }
    }
}

// -- Dynamic libraries (dlopen) ---------------------------------------------------------------

fn dlerror_bytes() -> Vec<u8> {
    // SAFETY: dlerror returns NULL or a valid C string.
    let e = unsafe { libc::dlerror() };
    if e.is_null() { Vec::new() } else { unsafe { CStr::from_ptr(e) }.to_bytes().to_vec() }
}

fn c_string(s: &[u8]) -> CString {
    CString::new(cstr(s)).expect("no interior NUL after cstr")
}

/// `ll_load`.
fn ll_load(path: &[u8], global: bool) -> Result<*mut c_void, Vec<u8>> {
    let p = c_string(path);
    let flags = libc::RTLD_NOW | if global { libc::RTLD_GLOBAL } else { libc::RTLD_LOCAL };
    // SAFETY: valid C string.
    let lib = unsafe { libc::dlopen(p.as_ptr(), flags) };
    if lib.is_null() { Err(dlerror_bytes()) } else { Ok(lib) }
}

/// `ll_sym`.
fn ll_sym(lib: *mut c_void, sym: &[u8]) -> Result<CFunction, Vec<u8>> {
    let s = c_string(sym);
    // SAFETY: `lib` is a live handle from dlopen.
    let f = unsafe { libc::dlsym(lib, s.as_ptr()) };
    if f.is_null() {
        Err(dlerror_bytes())
    } else {
        // SAFETY: the library promises a `lua_CFunction` under this name (as upstream casts).
        Ok(unsafe { std::mem::transmute::<*mut c_void, CFunction>(f) })
    }
}

/// `ll_bcsym`: address of embedded bytecode `sym` in `lib` (NULL = the program).
fn ll_bcsym(lib: *mut c_void, sym: &[u8]) -> *const u8 {
    let s = c_string(sym);
    let lib = if lib.is_null() { libc::RTLD_DEFAULT } else { lib };
    // SAFETY: valid handle or RTLD_DEFAULT.
    unsafe { libc::dlsym(lib, s.as_ptr()) as *const u8 }
}

/// Length of a bytecode dump at `p` (upstream reads it with an unbounded buffer size).
///
/// # Safety
/// `p` must point to a complete, well-formed `\x1bLJ` dump.
unsafe fn bc_dump_len(p: *const u8) -> usize {
    unsafe {
        let mut i = 0usize;
        let uleb = |i: &mut usize| -> usize {
            let mut v = 0usize;
            let mut sh = 0;
            loop {
                let b = *p.add(*i);
                *i += 1;
                v |= ((b & 0x7f) as usize) << sh;
                if b < 0x80 {
                    return v;
                }
                sh += 7;
            }
        };
        if *p != 0x1b || *p.add(1) != b'L' || *p.add(2) != b'J' {
            return 3;
        }
        i += 4;
        let flags = uleb(&mut i);
        if flags & crate::bcdump::BCDUMP_F_STRIP as usize == 0 {
            let n = uleb(&mut i);
            i += n;
        }
        loop {
            if *p.add(i) == 0 {
                return i + 1;
            }
            let n = uleb(&mut i);
            i += n;
        }
    }
}

/// `luaL_loadbuffer(L, bcdata, ~0, name)` for embedded bytecode.
fn load_bcdata(vm: &mut Vm, data: *const u8, name: &[u8]) -> LuaResult<Gc<crate::func::Function>> {
    // SAFETY: the symbol was exported as a bytecode dump (`luaJIT_BC_*` convention).
    let buf = unsafe { std::slice::from_raw_parts(data, bc_dump_len(data)) };
    vm.load(buf, cstr(name), None)
}

/// Pointer slot of a `_LOADLIB` userdata.
fn lib_slot(u: &crate::udata::Userdata) -> *mut *mut c_void {
    u.data_ptr() as *mut *mut c_void
}

/// `ll_register`: the `_LOADLIB` userdata for `path` (created if missing).
fn ll_register(vm: &mut Vm, path: &[u8]) -> LuaResult<Gc<crate::udata::Userdata>> {
    let mut key = b"LOADLIB: ".to_vec();
    key.extend_from_slice(cstr(path));
    let key = vm.new_str(&key);
    vm.push(Value::Str(key));
    if let Value::Udata(u) = vm.registry.get_str(key) {
        vm.pop();
        return Ok(u);
    }
    let env = vm.current_env();
    let u = vm.new_udata(std::mem::size_of::<*mut c_void>(), env);
    // SAFETY: fresh 8-byte aligned payload.
    unsafe { *lib_slot(&u) = std::ptr::null_mut() };
    let k = vm.new_str(b"_LOADLIB");
    let mt = vm.registry.get_str(k);
    u.set_metatable(mt.as_table());
    vm.push(Value::Udata(u));
    vm.registry.set_str(key, Value::Udata(u)).map_err(|e| vm.tab_error(e))?;
    vm.pop();
    vm.pop();
    Ok(u)
}

/// `mksymname`: `prefix` + module name after the `-` mark with dots replaced by `_`.
fn mksymname(modname: &[u8], prefix: &[u8]) -> Vec<u8> {
    let m = cstr(modname);
    let m = match m.iter().position(|&c| c == b'-') {
        Some(p) => &m[p + 1..],
        None => m,
    };
    let mut s = prefix.to_vec();
    s.extend_from_slice(&gsub(m, b".", b"_"));
    s
}

/// `ll_loadfunc`: on success pushes the function (or `true` for `*`), on error returns the
/// error code and message.
fn ll_loadfunc(vm: &mut Vm, path: &[u8], name: &[u8], r: bool) -> LuaResult<Result<(), (i32, Value)>> {
    if cstr(path).len() >= 4096 {
        let m = vm.new_str(b"path too long");
        return Ok(Err((PACKAGE_ERR_LIB, Value::Str(m))));
    }
    let reg = ll_register(vm, path)?;
    let slot = lib_slot(&reg);
    let star = name.first() == Some(&b'*');
    // SAFETY: the userdata payload holds the handle (or NULL).
    let mut lib = unsafe { *slot };
    if lib.is_null() {
        match ll_load(path, star) {
            Ok(l) => {
                lib = l;
                unsafe { *slot = l };
            }
            Err(m) => {
                let m = vm.new_str(&m);
                return Ok(Err((PACKAGE_ERR_LIB, Value::Str(m))));
            }
        }
    }
    if star {
        vm.push(Value::Bool(true));
        return Ok(Ok(()));
    }
    let sym = if r { cstr(name).to_vec() } else { mksymname(name, SYMPREFIX_CF) };
    match ll_sym(lib, &sym) {
        Ok(f) => {
            let env = vm.current_env();
            let f = vm.new_native(NativeImpl::C(f), FF_C, vec![], env);
            vm.push(Value::Func(f));
            Ok(Ok(()))
        }
        Err(m) => {
            let m = Value::Str(vm.new_str(&m));
            if !r {
                let bc = ll_bcsym(lib, &mksymname(name, SYMPREFIX_BC));
                if !bc.is_null() {
                    vm.push(m);
                    return match load_bcdata(vm, bc, name) {
                        Ok(f) => {
                            vm.push(Value::Func(f));
                            Ok(Ok(()))
                        }
                        Err(e) if e.kind == ErrorKind::Yield => Err(e),
                        Err(e) => {
                            vm.rt_clear_thrown();
                            Ok(Err((PACKAGE_ERR_LOAD, e.value)))
                        }
                    };
                }
            }
            Ok(Err((PACKAGE_ERR_FUNC, m)))
        }
    }
}

fn lj_cf_package_loadlib(vm: &mut Vm) -> LuaResult<usize> {
    let path = vm.check_str(1)?;
    let init = vm.check_str(2)?;
    match ll_loadfunc(vm, path.as_bytes(), init.as_bytes(), true)? {
        Ok(()) => Ok(1),
        Err((st, msg)) => {
            vm.push(Value::Nil);
            vm.push(msg);
            vm.push_str(if st == PACKAGE_ERR_LIB { PACKAGE_LIB_FAIL } else { b"init" });
            Ok(3)
        }
    }
}

/// `__gc` of `_LOADLIB` userdata.
fn lj_cf_package_unloadlib(vm: &mut Vm) -> LuaResult<usize> {
    let k = vm.new_str(b"_LOADLIB");
    let mt = vm.registry.get_str(k);
    let u = match vm.arg(1) {
        Value::Udata(u) if vm.nargs() >= 1 && mt.as_table().is_some() && u.metatable() == mt.as_table() => u,
        _ => return Err(vm.type_error(1, "_LOADLIB")),
    };
    let slot = lib_slot(&u);
    // SAFETY: payload holds the handle.
    unsafe {
        if !(*slot).is_null() {
            libc::dlclose(*slot);
        }
        *slot = std::ptr::null_mut();
    }
    Ok(0)
}

// -- Searchers --------------------------------------------------------------------------------

/// `readable`: `fopen(filename, "r")` succeeds.
fn readable(filename: &[u8]) -> bool {
    let p = c_string(filename);
    // SAFETY: valid C strings.
    let f = unsafe { libc::fopen(p.as_ptr(), c"r".as_ptr()) };
    if f.is_null() {
        return false;
    }
    unsafe { libc::fclose(f) };
    true
}

/// `searchpath`: the first readable file name, or the accumulated `no file` message.
fn searchpath(name: &[u8], path: &[u8], sep: &[u8], dirsep: &[u8]) -> Result<Vec<u8>, Vec<u8>> {
    let mut msg = Vec::new();
    let name = cstr(name);
    let (sep, dirsep) = (cstr(sep), cstr(dirsep));
    let name = if !sep.is_empty() { gsub(name, sep, dirsep) } else { name.to_vec() };
    let path = cstr(path);
    for tmpl in path.split(|&c| c == b';').filter(|t| !t.is_empty()) {
        let filename = gsub(tmpl, b"?", &name);
        if readable(&filename) {
            return Ok(filename);
        }
        msg.extend_from_slice(b"\n\tno file '");
        msg.extend_from_slice(&filename);
        msg.push(b'\'');
    }
    Err(msg)
}

fn lj_cf_package_searchpath(vm: &mut Vm) -> LuaResult<usize> {
    // Upstream checks these as arguments of one C call, which GCC evaluates right to left.
    let dirsep = vm.opt_str(4)?.map_or_else(|| b"/".to_vec(), |s| s.as_bytes().to_vec());
    let sep = vm.opt_str(3)?.map_or_else(|| b".".to_vec(), |s| s.as_bytes().to_vec());
    let path = vm.check_str(2)?;
    let name = vm.check_str(1)?;
    match searchpath(name.as_bytes(), path.as_bytes(), &sep, &dirsep) {
        Ok(f) => {
            vm.push_str(&f);
            Ok(1)
        }
        Err(m) => {
            vm.push(Value::Nil);
            vm.push_str(&m);
            Ok(2)
        }
    }
}

/// `findfile`: search `package[pname]` (through the function environment).
fn findfile(vm: &mut Vm, name: &[u8], pname: &str) -> LuaResult<Result<Vec<u8>, Vec<u8>>> {
    let env = vm.current_env();
    let v = get_field(vm, Value::Table(env), pname.as_bytes())?;
    let Some(path) = vm.tostring_opt(v) else {
        let m = format!("'package.{pname}' must be a string");
        return Err(vm.rt_error(m.as_bytes()));
    };
    Ok(searchpath(name, &path, b".", b"/"))
}

/// `loaderror`.
fn loaderror(vm: &mut Vm, filename: &[u8], err: Value) -> crate::error::LuaError {
    let a1 = vm.arg(1);
    let mut m = b"error loading module '".to_vec();
    m.extend_from_slice(cstr(&vm.tostring_opt(a1).unwrap_or_else(|| b"(null)".to_vec())));
    m.extend_from_slice(b"' from file '");
    m.extend_from_slice(cstr(filename));
    m.extend_from_slice(b"':\n\t");
    m.extend_from_slice(cstr(&vm.tostring_opt(err).unwrap_or_else(|| b"(null)".to_vec())));
    vm.rt_error(&m)
}

fn lj_cf_package_loader_lua(vm: &mut Vm) -> LuaResult<usize> {
    let name = vm.check_str(1)?;
    let filename = match findfile(vm, name.as_bytes(), "path")? {
        Ok(f) => f,
        Err(m) => {
            if let Some((src, file)) = super::jit::embedded_jit_module_entry(cstr(name.as_bytes())) {
                let mut cn = b"@".to_vec();
                cn.extend_from_slice(LUA_LJDIR.as_bytes());
                cn.extend_from_slice(file.as_bytes());
                return match vm.load(src, &cn, None) {
                    Ok(f) => vm.ret1(Value::Func(f)),
                    Err(e) if e.kind == ErrorKind::Yield => Err(e),
                    Err(e) => {
                        vm.rt_clear_thrown();
                        Err(loaderror(vm, &cn[1..], e.value))
                    }
                };
            }
            vm.push_str(&m);
            return Ok(1);
        }
    };
    match vm.load_file(Some(&filename), None) {
        Ok(f) => vm.ret1(Value::Func(f)),
        Err(e) if e.kind == ErrorKind::Yield => Err(e),
        Err(e) => {
            vm.rt_clear_thrown();
            vm.push(e.value);
            Err(loaderror(vm, &filename, e.value))
        }
    }
}

fn lj_cf_package_loader_c(vm: &mut Vm) -> LuaResult<usize> {
    let name = vm.check_str(1)?;
    let filename = match findfile(vm, name.as_bytes(), "cpath")? {
        Ok(f) => f,
        Err(m) => {
            vm.push_str(&m);
            return Ok(1);
        }
    };
    match ll_loadfunc(vm, &filename, name.as_bytes(), false)? {
        Ok(()) => Ok(1),
        Err((_, e)) => {
            vm.push(e);
            Err(loaderror(vm, &filename, e))
        }
    }
}

fn lj_cf_package_loader_croot(vm: &mut Vm) -> LuaResult<usize> {
    let name = vm.check_str(1)?;
    let nb = cstr(name.as_bytes());
    let Some(p) = nb.iter().position(|&c| c == b'.') else { return Ok(0) };
    let root = nb[..p].to_vec();
    vm.push_str(&root);
    let filename = match findfile(vm, &root, "cpath")? {
        Ok(f) => f,
        Err(m) => {
            vm.push_str(&m);
            return Ok(1);
        }
    };
    match ll_loadfunc(vm, &filename, name.as_bytes(), false)? {
        Ok(()) => Ok(1),
        Err((st, e)) => {
            if st != PACKAGE_ERR_FUNC {
                vm.push(e);
                return Err(loaderror(vm, &filename, e));
            }
            let mut m = b"\n\tno module '".to_vec();
            m.extend_from_slice(cstr(name.as_bytes()));
            m.extend_from_slice(b"' in file '");
            m.extend_from_slice(cstr(&filename));
            m.push(b'\'');
            vm.push_str(&m);
            Ok(1)
        }
    }
}

fn lj_cf_package_loader_preload(vm: &mut Vm) -> LuaResult<usize> {
    let name = vm.check_str(1)?;
    let env = vm.current_env();
    let preload = get_field(vm, Value::Table(env), b"preload")?;
    if !matches!(preload, Value::Table(_)) {
        return Err(vm.rt_error(b"'package.preload' must be a table"));
    }
    vm.push(preload);
    let v = get_field(vm, preload, cstr(name.as_bytes()))?;
    if !v.is_nil() {
        return vm.ret1(v);
    }
    let bc = ll_bcsym(std::ptr::null_mut(), &mksymname(name.as_bytes(), SYMPREFIX_BC));
    if !bc.is_null() {
        match load_bcdata(vm, bc, name.as_bytes()) {
            Ok(f) => return vm.ret1(Value::Func(f)),
            Err(e) if e.kind == ErrorKind::Yield => return Err(e),
            Err(_) => vm.rt_clear_thrown(),
        }
    }
    let mut m = b"\n\tno field package.preload['".to_vec();
    m.extend_from_slice(cstr(name.as_bytes()));
    m.extend_from_slice(b"']");
    vm.push_str(&m);
    Ok(1)
}

// -- require / module -------------------------------------------------------------------------

fn lj_cf_package_require(vm: &mut Vm) -> LuaResult<usize> {
    let name: Gc<LuaStr> = vm.check_str(1)?;
    vm.set_top(1);
    let nameb = cstr(name.as_bytes()).to_vec();
    let reg = Value::Table(vm.registry);
    let loaded = get_field(vm, reg, b"_LOADED")?;
    vm.push(loaded); // slot 2
    let v = get_field(vm, loaded, &nameb)?;
    if v.is_truthy() {
        if is_sentinel(v) {
            let mut m = b"loop or previous error loading module '".to_vec();
            m.extend_from_slice(&nameb);
            m.push(b'\'');
            return Err(vm.rt_error(&m));
        }
        return vm.ret1(v);
    }
    let env = vm.current_env();
    let loaders = get_field(vm, Value::Table(env), b"loaders")?;
    let Value::Table(lt) = loaders else {
        return Err(vm.rt_error(b"'package.loaders' must be a table"));
    };
    vm.push(loaders); // slot 3
    vm.push_str(b""); // slot 4: error message accumulator
    let mut i = 1;
    loop {
        let loader = lt.get_int(i);
        if loader.is_nil() {
            let acc = vm.arg(4);
            let acc = vm.tostring_opt(acc).unwrap_or_default();
            let mut m = b"module '".to_vec();
            m.extend_from_slice(&nameb);
            m.extend_from_slice(b"' not found:");
            m.extend_from_slice(cstr(&acc));
            return Err(vm.rt_error(&m));
        }
        vm.push(loader);
        vm.push_str(&nameb);
        vm.call(1, 1)?;
        let r = vm.arg(5);
        match r {
            Value::Func(_) => break,
            Value::Str(_) | Value::Num(_) => vm.concat(2)?,
            _ => {
                vm.pop();
            }
        }
        i += 1;
    }
    let f = vm.arg(5);
    vm.set_top(3);
    vm.push(f); // slot 4
    set_field_v(vm, loaded, &nameb, key_sentinel())?;
    vm.push_str(&nameb);
    vm.call(1, 1)?;
    let r = vm.arg(4);
    if !r.is_nil() {
        set_field_v(vm, loaded, &nameb, r)?;
    }
    let mut v = get_field(vm, loaded, &nameb)?;
    if is_sentinel(v) {
        v = Value::Bool(true);
        set_field_v(vm, loaded, &nameb, v)?;
    }
    vm.ret1(v)
}

/// `lua_setfield` on an arbitrary value.
fn set_field_v(vm: &mut Vm, o: Value, k: &[u8], v: Value) -> LuaResult<()> {
    let k = vm.new_str(k);
    vm.setindex(o, Value::Str(k), v)
}

fn lj_cf_package_module(vm: &mut Vm) -> LuaResult<usize> {
    let modname = vm.check_str(1)?;
    let lastarg = vm.nargs();
    let mn = cstr(modname.as_bytes()).to_vec();
    let m = push_module(vm, &mn, 1)?;
    vm.push(Value::Table(m));
    let nm = get_field(vm, Value::Table(m), b"_NAME")?;
    if nm.is_nil() {
        // modinit
        set_field(vm, m, b"_M", Value::Table(m))?;
        let s = vm.new_str(&mn);
        set_field(vm, m, b"_NAME", Value::Str(s))?;
        let dot = mn.iter().rposition(|&c| c == b'.').map_or(0, |p| p + 1);
        let s = vm.new_str(&mn[..dot]);
        set_field(vm, m, b"_PACKAGE", Value::Str(s))?;
    }
    // setfenv on the calling function.
    let th = vm.cur_thread;
    let caller = vm
        .get_stack(th, 1)
        .and_then(|fr| vm.get_info(InfoTarget::Frame(fr), b"f"))
        .and_then(|ar| ar.func)
        .filter(|f| f.is_lua());
    let Some(f) = caller else {
        return Err(vm.rt_error(b"'module' not called from a Lua function"));
    };
    f.set_env(m);
    for i in 2..=lastarg {
        let opt = vm.arg(i);
        vm.call_value(opt, &[Value::Table(m)], 0)?;
    }
    Ok(0)
}

fn lj_cf_package_seeall(vm: &mut Vm) -> LuaResult<usize> {
    let t = vm.check_tab(1)?;
    let mt = match t.metatable() {
        Some(mt) => mt,
        None => {
            let mt = vm.new_table(0, crate::table::hsize2hbits(1))?;
            t.set_metatable(Some(mt));
            mt
        }
    };
    vm.push(Value::Table(mt));
    let g = vm.cur_thread.env();
    let k = vm.mm_name(crate::bc::MM::Index);
    vm.setindex(Value::Table(mt), Value::Str(k), Value::Table(g))?;
    Ok(0)
}

/// `setpath`: `package[field]` from environment variable `envname` (`;;` expands to the
/// default) or the default.
fn setpath(vm: &mut Vm, pkg: Gc<Table>, field: &[u8], envname: &str, def: &str, noenv: bool) -> LuaResult<()> {
    let path = std::env::var_os(envname);
    let v = match path {
        Some(p) if !noenv => {
            use std::os::unix::ffi::OsStrExt;
            let p = gsub(p.as_bytes(), b";;", b";\x01;");
            gsub(&p, b"\x01", def.as_bytes())
        }
        _ => def.as_bytes().to_vec(),
    };
    let s = vm.new_str(&v);
    set_field(vm, pkg, field, Value::Str(s))
}
