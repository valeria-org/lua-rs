//! `lua_*` functions. Port of lj_api.c and of the public API functions of lj_err.c
//! (`lua_atpanic`, `lua_error`), lj_load.c (`lua_load*`, `lua_dump`), lj_state.c
//! (`lua_newstate`, `lua_close`, `lua_newthread`), lj_dispatch.c (hooks) and lj_debug.c
//! (`lua_getstack`, `lua_getinfo`, `lua_getlocal`, `lua_setlocal`).

use std::ffi::{c_char, c_int, c_void};

use crate::error::{ErrorKind, LuaError, LuaResult};
use crate::func::{FF_C, Function, NativeImpl};
use crate::gc::Gc;
use crate::state::Vm;
use crate::thread::{Thread, ThreadStatus};
use crate::value::Value;
use crate::vm::{ErrMsg, FrameRef, HookFn, InfoTarget, fmt_msg, obj_ptr};

use super::*;

/// `LJ_MAX_UDATA`.
const LJ_MAX_UDATA: usize = 0x7fff_ff00;
/// Slot of a coroutine's main function (`coroutine.create` layout).
const CO_FUNC_SLOT: usize = 2;

// -- Helpers ------------------------------------------------------------------------------------

/// Ensure `n` free slots above `th`'s top (`lj_state_checkstack`).
fn checkstack(vm: &mut Vm, th: Gc<Thread>, n: usize) -> LuaResult<()> {
    vm.check_stack_th(th, n)
}

/// Store `v` at an API index (`copy_slot`).
fn copy_slot(vm: &mut Vm, th: Gc<Thread>, v: Value, idx: c_int) -> LuaResult<()> {
    match index2adr(vm, th, idx) {
        Adr::Stack(s) => th.set(s, v),
        Adr::Globals => {
            if let Value::Table(t) = v {
                th.set_env(t);
            }
        }
        Adr::Env => match curr_func(th) {
            Some(f) => {
                if let Value::Table(t) = v {
                    f.set_env(t);
                }
            }
            None => return Err(vm.err_msg(ErrMsg::NOENV)),
        },
        Adr::Registry => {
            if let Value::Table(t) = v {
                vm.registry = t;
            }
        }
        Adr::Upval(f, i) => {
            f.set_upvalue(i, v);
        }
        Adr::None => panic!("C API: invalid stack index {idx}"),
    }
    Ok(())
}

/// The value of a valid index (`index2adr_check`).
fn idx_check(vm: &Vm, th: Gc<Thread>, idx: c_int) -> Value {
    match idx_get(vm, th, idx) {
        Some(v) => v,
        None => panic!("C API: invalid stack index {idx}"),
    }
}

/// The table at an index (API contract: it must be one).
fn idx_tab(vm: &Vm, th: Gc<Thread>, idx: c_int) -> Gc<crate::table::Table> {
    match idx_val(vm, th, idx) {
        Value::Table(t) => t,
        v => panic!("C API: stack slot {idx} is not a table ({})", v.type_name()),
    }
}

/// Replace the value at an index after an in-place number → string conversion.
fn idx_store(vm: &Vm, th: Gc<Thread>, idx: c_int, v: Value) {
    match index2adr(vm, th, idx) {
        Adr::Stack(s) => th.set(s, v),
        Adr::Upval(f, i) => {
            f.set_upvalue(i, v);
        }
        _ => {}
    }
}

/// `lj_strfmt_number` in place: the string for a number at `idx` (stored back).
pub(crate) fn tostr_inplace(vm: &mut Vm, th: Gc<Thread>, idx: c_int, n: f64) -> Gc<crate::lstr::LuaStr> {
    vm.gc_safe();
    let s = vm.new_str(&crate::text::numfmt::fmt_num(n));
    idx_store(vm, th, idx, Value::Str(s));
    s
}

/// Canonical NaN (`setnanV`).
#[inline]
fn canon(n: f64) -> f64 {
    if n.is_nan() { f64::from_bits(0xfff8_0000_0000_0000) } else { n }
}

/// `lua_type` of a value.
#[allow(unreachable_patterns)]
pub(crate) fn type_of(v: Value) -> c_int {
    match v {
        Value::Nil => LUA_TNIL,
        Value::Bool(_) => LUA_TBOOLEAN,
        Value::LightUd(_) => LUA_TLIGHTUSERDATA,
        Value::Num(_) => LUA_TNUMBER,
        Value::Str(_) => LUA_TSTRING,
        Value::Table(_) => LUA_TTABLE,
        Value::Func(_) => LUA_TFUNCTION,
        Value::Udata(_) => LUA_TUSERDATA,
        Value::Thread(_) => LUA_TTHREAD,
        Value::CData(_) => LUA_TCDATA,
        _ => LUA_TPROTO,
    }
}

/// `lj_obj_typename`, indexed by `LUA_T* + 1`.
pub(crate) const TYPENAMES: [&std::ffi::CStr; 12] =
    [c"no value", c"nil", c"boolean", c"userdata", c"number", c"string", c"table", c"function", c"userdata", c"thread", c"proto", c"cdata"];

/// Number conversion of a value (`lj_strscan_num` for strings).
#[inline]
fn tonum(v: Value) -> Option<f64> {
    match v {
        Value::Num(n) => Some(n),
        Value::Str(s) => crate::text::strscan::scan_num(s.as_bytes()),
        _ => None,
    }
}

/// Run `f` with `th` as a running thread. A call on a thread other than the running one marks
/// the running thread `Normal` and `th` `Running` for the duration (upstream: `th` has a C
/// frame then, so it cannot be resumed meanwhile).
fn on_thread<R>(vm: &mut Vm, th: Gc<Thread>, old: Gc<Thread>, f: impl FnOnce(&mut Vm) -> R) -> R {
    if th == old {
        return f(vm);
    }
    let (ost, tst) = (old.status(), th.status());
    if ost == ThreadStatus::Running {
        old.set_status(ThreadStatus::Normal);
    }
    th.set_status(ThreadStatus::Running);
    let r = f(vm);
    th.set_status(tst);
    old.set_status(ost);
    r
}

/// Static C string for well-known debug strings, else an interned string's data.
pub(crate) fn name_ptr(vm: &mut Vm, s: &[u8]) -> *const c_char {
    const STATIC: [&std::ffi::CStr; 18] = [
        c"",
        c"Lua",
        c"C",
        c"main",
        c"global",
        c"local",
        c"method",
        c"field",
        c"upvalue",
        c"metamethod",
        c"for iterator",
        c"(*temporary)",
        c"(*vararg)",
        c"(for index)",
        c"(for limit)",
        c"(for step)",
        c"(for generator)",
        c"(for state)",
    ];
    if let Some(c) = STATIC.iter().find(|c| c.to_bytes() == s) {
        return c.as_ptr();
    }
    if s == b"(for control)" {
        return c"(for control)".as_ptr();
    }
    let st = vm.new_str(s);
    strdata(st)
}

// -- State management ---------------------------------------------------------------------------

/// Default allocator (`luaL_newstate`'s, as `LUAJIT_USE_SYSMALLOC`'s `mem_alloc`).
pub(crate) unsafe extern "C-unwind" fn l_alloc(_ud: *mut c_void, ptr: *mut c_void, _osize: usize, nsize: usize) -> *mut c_void {
    // SAFETY: standard realloc/free contract of lua_Alloc.
    unsafe {
        if nsize == 0 {
            libc::free(ptr);
            std::ptr::null_mut()
        } else {
            libc::realloc(ptr, nsize)
        }
    }
}

/// Allocate a VM on the heap for the C API.
pub(crate) fn new_boxed_state(vm: Vm, allocf: lua_Alloc, allocd: *mut c_void) -> *mut lua_State {
    // Owned by the C API from here on: `lua_close` frees it (`Box::from_raw`).
    let p = Box::into_raw(Box::new(vm));
    // SAFETY: freshly allocated, uniquely referenced here.
    let vm: &mut Vm = unsafe { &mut *p };
    vm.rt.c_call = Some(call_c);
    vm.rt.capi.allocf = Some(allocf);
    vm.rt.capi.allocd = allocd;
    vm.rt.capi.owned = true;
    let th = vm.main_thread;
    state_of(vm, th)
}

/// `lua_newstate`. The allocator is recorded (`lua_getallocf`); the VM's own objects are
/// allocated with the Rust allocator.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lua_newstate(f: lua_Alloc, ud: *mut c_void) -> *mut lua_State {
    new_boxed_state(Vm::new(), f, ud)
}

/// `lua_close`: run all finalizers; free the state if it was created through the C API.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lua_close(L: *mut lua_State) {
    // SAFETY: `L` is a live state.
    let (vm, _) = unsafe { ctx(L) };
    vm.close();
    if vm.rt.capi.owned {
        // SAFETY: allocated by `new_boxed_state`; no handle is used after lua_close.
        drop(unsafe { Box::from_raw(vm as *mut Vm) });
    }
}

/// `lua_newthread`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lua_newthread(L: *mut lua_State) -> *mut lua_State {
    // SAFETY: `L` is a live state.
    unsafe {
        api(L, |vm, th| {
            vm.gc_safe();
            let t = vm.new_thread(th.env());
            t.set_top(CO_FUNC_SLOT);
            vm.push(Value::Thread(t));
            Ok(state_of(vm, t))
        })
    }
}

/// `lua_atpanic`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lua_atpanic(L: *mut lua_State, panicf: Option<lua_CFunction>) -> Option<lua_CFunction> {
    // SAFETY: `L` is a live state.
    let (vm, _) = unsafe { ctx(L) };
    std::mem::replace(&mut vm.rt.capi.panic, panicf)
}

/// `lua_status`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lua_status(L: *mut lua_State) -> c_int {
    // SAFETY: `L` is a live state.
    let (_, th) = unsafe { ctx(L) };
    match th.status() {
        ThreadStatus::Suspended => crate::error::LUA_YIELD,
        // A coroutine that died with an error keeps its frames.
        ThreadStatus::Dead if th.frame_count() > 0 => crate::error::LUA_ERRRUN,
        _ => 0,
    }
}

/// `lua_version`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lua_version(_L: *mut lua_State) -> *const lua_Number {
    static VERSION: lua_Number = 501.0;
    &VERSION
}

// -- Stack manipulation -------------------------------------------------------------------------

/// `lua_checkstack`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lua_checkstack(L: *mut lua_State, size: c_int) -> c_int {
    // SAFETY: `L` is a live state.
    unsafe {
        api(L, |vm, th| {
            let used = (th.top() - base_of(vm, th)) as c_int;
            if size > LUAI_MAXCSTACK || used + size > LUAI_MAXCSTACK {
                return Ok(0);
            }
            if size > 0 && checkstack(vm, th, size as usize).is_err() {
                return Ok(0);
            }
            Ok(1)
        })
    }
}

/// `lua_xmove`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lua_xmove(from: *mut lua_State, to: *mut lua_State, n: c_int) {
    if from == to {
        return;
    }
    // SAFETY: both are live states of the same VM.
    let (_, tth) = unsafe { ctx(to) };
    // SAFETY: as above.
    unsafe {
        api(from, |vm, fth| {
            let n = n as usize;
            checkstack(vm, tth, n)?;
            let f = fth.top() - n;
            let t = tth.top();
            for i in 0..n {
                tth.set(t + i, fth.get(f + i));
            }
            tth.set_top(t + n);
            fth.set_top(f);
            Ok(())
        })
    }
}

/// `lua_gettop`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lua_gettop(L: *mut lua_State) -> c_int {
    // SAFETY: `L` is a live state.
    let (vm, th) = unsafe { ctx(L) };
    (th.top() - base_of(vm, th)) as c_int
}

/// `lua_settop`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lua_settop(L: *mut lua_State, idx: c_int) {
    // SAFETY: `L` is a live state.
    unsafe {
        api(L, |vm, th| {
            let base = base_of(vm, th);
            let top = th.top();
            if idx >= 0 {
                let nt = base + idx as usize;
                if nt > top {
                    checkstack(vm, th, nt - top)?;
                    for i in top..nt {
                        th.set(i, Value::Nil);
                    }
                }
                th.set_top(nt);
            } else {
                th.set_top((top as i64 + idx as i64 + 1) as usize);
            }
            Ok(())
        })
    }
}

/// `lua_remove`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lua_remove(L: *mut lua_State, idx: c_int) {
    // SAFETY: `L` is a live state.
    unsafe {
        api(L, |vm, th| {
            let p = idx_stack(vm, th, idx);
            let top = th.top();
            for i in p + 1..top {
                th.set(i - 1, th.get(i));
            }
            th.set_top(top - 1);
            Ok(())
        })
    }
}

/// `lua_insert`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lua_insert(L: *mut lua_State, idx: c_int) {
    // SAFETY: `L` is a live state.
    unsafe {
        api(L, |vm, th| {
            let p = idx_stack(vm, th, idx);
            let top = th.top();
            let v = th.get(top - 1);
            for i in (p + 1..top).rev() {
                th.set(i, th.get(i - 1));
            }
            th.set(p, v);
            Ok(())
        })
    }
}

/// `lua_replace`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lua_replace(L: *mut lua_State, idx: c_int) {
    // SAFETY: `L` is a live state.
    unsafe {
        api(L, |vm, th| {
            let v = th.get(th.top() - 1);
            copy_slot(vm, th, v, idx)?;
            th.set_top(th.top() - 1);
            Ok(())
        })
    }
}

/// `lua_copy`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lua_copy(L: *mut lua_State, fromidx: c_int, toidx: c_int) {
    // SAFETY: `L` is a live state.
    unsafe {
        api(L, |vm, th| {
            let v = idx_val(vm, th, fromidx);
            copy_slot(vm, th, v, toidx)
        })
    }
}

/// `lua_pushvalue`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lua_pushvalue(L: *mut lua_State, idx: c_int) {
    // SAFETY: `L` is a live state.
    unsafe {
        api(L, |vm, th| {
            let v = idx_val(vm, th, idx);
            vm.push(v);
            Ok(())
        })
    }
}

// -- Stack getters ------------------------------------------------------------------------------

/// `lua_type`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lua_type(L: *mut lua_State, idx: c_int) -> c_int {
    // SAFETY: `L` is a live state.
    let (vm, th) = unsafe { ctx(L) };
    match idx_get(vm, th, idx) {
        Some(v) => type_of(v),
        None => LUA_TNONE,
    }
}

/// `lua_typename`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lua_typename(_L: *mut lua_State, t: c_int) -> *const c_char {
    TYPENAMES.get((t + 1) as usize).map_or(std::ptr::null(), |s| s.as_ptr())
}

/// `lua_iscfunction`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lua_iscfunction(L: *mut lua_State, idx: c_int) -> c_int {
    // SAFETY: `L` is a live state.
    let (vm, th) = unsafe { ctx(L) };
    matches!(idx_val(vm, th, idx), Value::Func(f) if !f.is_lua()) as c_int
}

/// `lua_isnumber`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lua_isnumber(L: *mut lua_State, idx: c_int) -> c_int {
    // SAFETY: `L` is a live state.
    let (vm, th) = unsafe { ctx(L) };
    tonum(idx_val(vm, th, idx)).is_some() as c_int
}

/// `lua_isstring`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lua_isstring(L: *mut lua_State, idx: c_int) -> c_int {
    // SAFETY: `L` is a live state.
    let (vm, th) = unsafe { ctx(L) };
    matches!(idx_val(vm, th, idx), Value::Str(_) | Value::Num(_)) as c_int
}

/// `lua_isuserdata`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lua_isuserdata(L: *mut lua_State, idx: c_int) -> c_int {
    // SAFETY: `L` is a live state.
    let (vm, th) = unsafe { ctx(L) };
    matches!(idx_val(vm, th, idx), Value::Udata(_) | Value::LightUd(_)) as c_int
}

/// `lua_rawequal`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lua_rawequal(L: *mut lua_State, idx1: c_int, idx2: c_int) -> c_int {
    // SAFETY: `L` is a live state.
    let (vm, th) = unsafe { ctx(L) };
    match (idx_get(vm, th, idx1), idx_get(vm, th, idx2)) {
        (Some(a), Some(b)) => a.raw_equal(b) as c_int,
        _ => 0,
    }
}

/// `lua_equal`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lua_equal(L: *mut lua_State, idx1: c_int, idx2: c_int) -> c_int {
    // SAFETY: `L` is a live state.
    unsafe {
        api(L, |vm, th| match (idx_get(vm, th, idx1), idx_get(vm, th, idx2)) {
            (Some(a), Some(b)) => Ok(vm.equal(a, b)? as c_int),
            _ => Ok(0),
        })
    }
}

/// `lua_lessthan`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lua_lessthan(L: *mut lua_State, idx1: c_int, idx2: c_int) -> c_int {
    // SAFETY: `L` is a live state.
    unsafe {
        api(L, |vm, th| match (idx_get(vm, th, idx1), idx_get(vm, th, idx2)) {
            (Some(a), Some(b)) => Ok(vm.less_than(a, b)? as c_int),
            _ => Ok(0),
        })
    }
}

/// `lua_tonumber`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lua_tonumber(L: *mut lua_State, idx: c_int) -> lua_Number {
    // SAFETY: `L` is a live state.
    let (vm, th) = unsafe { ctx(L) };
    tonum(idx_val(vm, th, idx)).unwrap_or(0.0)
}

/// `lua_tonumberx`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lua_tonumberx(L: *mut lua_State, idx: c_int, ok: *mut c_int) -> lua_Number {
    // SAFETY: `L` is a live state.
    let (vm, th) = unsafe { ctx(L) };
    let r = tonum(idx_val(vm, th, idx));
    if !ok.is_null() {
        // SAFETY: caller-provided output pointer.
        unsafe { *ok = r.is_some() as c_int };
    }
    r.unwrap_or(0.0)
}

/// `lua_tointeger`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lua_tointeger(L: *mut lua_State, idx: c_int) -> lua_Integer {
    // SAFETY: `L` is a live state.
    let (vm, th) = unsafe { ctx(L) };
    tonum(idx_val(vm, th, idx)).map_or(0, |n| crate::text::num2i64(n) as lua_Integer)
}

/// `lua_tointegerx`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lua_tointegerx(L: *mut lua_State, idx: c_int, ok: *mut c_int) -> lua_Integer {
    // SAFETY: `L` is a live state.
    let (vm, th) = unsafe { ctx(L) };
    let r = tonum(idx_val(vm, th, idx));
    if !ok.is_null() {
        // SAFETY: caller-provided output pointer.
        unsafe { *ok = r.is_some() as c_int };
    }
    r.map_or(0, |n| crate::text::num2i64(n) as lua_Integer)
}

/// `lua_toboolean`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lua_toboolean(L: *mut lua_State, idx: c_int) -> c_int {
    // SAFETY: `L` is a live state.
    let (vm, th) = unsafe { ctx(L) };
    idx_val(vm, th, idx).is_truthy() as c_int
}

/// `lua_tolstring`: numbers are converted in place.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lua_tolstring(L: *mut lua_State, idx: c_int, len: *mut usize) -> *const c_char {
    // SAFETY: `L` is a live state.
    unsafe {
        api(L, |vm, th| {
            let s = match idx_val(vm, th, idx) {
                Value::Str(s) => s,
                Value::Num(n) => tostr_inplace(vm, th, idx, n),
                _ => {
                    if !len.is_null() {
                        *len = 0;
                    }
                    return Ok(std::ptr::null());
                }
            };
            if !len.is_null() {
                *len = s.len();
            }
            Ok(strdata(s))
        })
    }
}

/// `lua_objlen`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lua_objlen(L: *mut lua_State, idx: c_int) -> usize {
    // SAFETY: `L` is a live state.
    unsafe {
        api(L, |vm, th| {
            Ok(match idx_val(vm, th, idx) {
                Value::Str(s) => s.len(),
                Value::Table(t) => t.len(),
                Value::Udata(u) => u.len(),
                Value::Num(n) => tostr_inplace(vm, th, idx, n).len(),
                _ => 0,
            })
        })
    }
}

/// `lua_tocfunction`: the C function of a C closure; for a Rust native without upvalues
/// (library functions, builtins) a C entry point that runs it on the caller's frame, like a
/// direct call of upstream's C implementation. NULL for Lua functions and for Rust natives
/// with upvalues (their Rust code reads its own upvalues, which a direct call cannot supply).
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lua_tocfunction(L: *mut lua_State, idx: c_int) -> Option<lua_CFunction> {
    // SAFETY: `L` is a live state.
    let (vm, th) = unsafe { ctx(L) };
    match idx_val(vm, th, idx) {
        Value::Func(f) => match &*f {
            Function::Native { f: NativeImpl::C(cf), .. } => Some(*cf),
            Function::Native { f: NativeImpl::Rust(nf), upvals, .. } if upvals.is_empty() => Some(super::thunk::thunk_of(*nf)),
            Function::Native { .. } => None,
            Function::Lua { .. } => None,
        },
        _ => None,
    }
}

/// `lua_touserdata`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lua_touserdata(L: *mut lua_State, idx: c_int) -> *mut c_void {
    // SAFETY: `L` is a live state.
    let (vm, th) = unsafe { ctx(L) };
    match idx_val(vm, th, idx) {
        Value::Udata(u) => u.data_ptr().cast(),
        Value::LightUd(p) => p as *mut c_void,
        _ => std::ptr::null_mut(),
    }
}

/// `lua_tothread`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lua_tothread(L: *mut lua_State, idx: c_int) -> *mut lua_State {
    // SAFETY: `L` is a live state.
    let (vm, th) = unsafe { ctx(L) };
    match idx_val(vm, th, idx) {
        Value::Thread(t) => state_of(vm, t),
        _ => std::ptr::null_mut(),
    }
}

/// `lua_topointer` (`lj_obj_ptr`).
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lua_topointer(L: *mut lua_State, idx: c_int) -> *const c_void {
    // SAFETY: `L` is a live state.
    let (vm, th) = unsafe { ctx(L) };
    match idx_val(vm, th, idx) {
        Value::Nil | Value::Bool(_) | Value::Num(_) => std::ptr::null(),
        v => obj_ptr(v) as *const c_void,
    }
}

// -- Stack setters (object creation) ------------------------------------------------------------

/// `lua_pushnil`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lua_pushnil(L: *mut lua_State) {
    // SAFETY: `L` is a live state.
    unsafe {
        api(L, |vm, _| {
            vm.push(Value::Nil);
            Ok(())
        })
    }
}

/// `lua_pushnumber`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lua_pushnumber(L: *mut lua_State, n: lua_Number) {
    // SAFETY: `L` is a live state.
    unsafe {
        api(L, |vm, _| {
            vm.push(Value::Num(canon(n)));
            Ok(())
        })
    }
}

/// `lua_pushinteger`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lua_pushinteger(L: *mut lua_State, n: lua_Integer) {
    // SAFETY: `L` is a live state.
    unsafe {
        api(L, |vm, _| {
            vm.push(Value::Num(n as f64));
            Ok(())
        })
    }
}

/// `lua_pushlstring`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lua_pushlstring(L: *mut lua_State, s: *const c_char, len: usize) {
    // SAFETY: `s` points to `len` bytes (or `len` is 0).
    let b: &[u8] = if len == 0 { &[] } else { unsafe { std::slice::from_raw_parts(s.cast(), len) } };
    // SAFETY: `L` is a live state.
    unsafe {
        api(L, |vm, _| {
            vm.gc_safe();
            vm.push_str(b);
            Ok(())
        })
    }
}

/// `lua_pushstring` (NULL pushes nil).
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lua_pushstring(L: *mut lua_State, s: *const c_char) {
    // SAFETY: `L` is a live state; `s` is NULL or a C string.
    unsafe {
        api(L, |vm, _| {
            if s.is_null() {
                vm.push(Value::Nil);
            } else {
                vm.gc_safe();
                vm.push_str(cstr(s));
            }
            Ok(())
        })
    }
}

/// `lua_pushcclosure`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lua_pushcclosure(L: *mut lua_State, f: lua_CFunction, n: c_int) {
    // SAFETY: `L` is a live state.
    unsafe {
        api(L, |vm, th| {
            vm.gc_safe();
            let n = n.max(0) as usize;
            let top = th.top();
            let ups: Vec<Value> = (top - n..top).map(|i| th.get(i)).collect();
            let env = getcurrenv(th);
            let func = vm.new_native(NativeImpl::C(f), FF_C, ups, env);
            th.set_top(top - n);
            vm.push(Value::Func(func));
            Ok(())
        })
    }
}

/// `lua_pushboolean`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lua_pushboolean(L: *mut lua_State, b: c_int) {
    // SAFETY: `L` is a live state.
    unsafe {
        api(L, |vm, _| {
            vm.push(Value::Bool(b != 0));
            Ok(())
        })
    }
}

/// `lua_pushlightuserdata`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lua_pushlightuserdata(L: *mut lua_State, p: *mut c_void) {
    // SAFETY: `L` is a live state.
    unsafe {
        api(L, |vm, _| {
            vm.push(Value::LightUd(p as usize));
            Ok(())
        })
    }
}

/// `lua_pushthread`: returns 1 for the main thread.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lua_pushthread(L: *mut lua_State) -> c_int {
    // SAFETY: `L` is a live state.
    unsafe {
        api(L, |vm, th| {
            vm.push(Value::Thread(th));
            Ok((th == vm.main_thread) as c_int)
        })
    }
}

/// `lua_createtable`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lua_createtable(L: *mut lua_State, narr: c_int, nrec: c_int) {
    // SAFETY: `L` is a live state.
    unsafe {
        api(L, |vm, _| {
            vm.gc_safe();
            let t = vm.new_table_ah(narr as u32, nrec as u32)?;
            vm.push(Value::Table(t));
            Ok(())
        })
    }
}

/// `lua_newuserdata`: zeroed, 16-byte aligned payload, stable for the object's lifetime.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lua_newuserdata(L: *mut lua_State, size: usize) -> *mut c_void {
    // SAFETY: `L` is a live state.
    unsafe {
        api(L, |vm, th| {
            vm.gc_safe();
            if size > LJ_MAX_UDATA {
                return Err(vm.err_msg(ErrMsg::UDATAOV));
            }
            let u = vm.new_udata(size, getcurrenv(th));
            vm.push(Value::Udata(u));
            Ok(u.data_ptr().cast())
        })
    }
}

/// `lua_concat`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lua_concat(L: *mut lua_State, n: c_int) {
    // SAFETY: `L` is a live state.
    unsafe { api(L, |vm, _| vm.concat(n.max(0) as usize)) }
}

// -- Object getters -----------------------------------------------------------------------------

/// `lua_gettable`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lua_gettable(L: *mut lua_State, idx: c_int) {
    // SAFETY: `L` is a live state.
    unsafe {
        api(L, |vm, th| {
            let t = idx_check(vm, th, idx);
            let k = th.get(th.top() - 1);
            let v = vm.index(t, k)?;
            th.set(th.top() - 1, v);
            Ok(())
        })
    }
}

/// `lua_getfield`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lua_getfield(L: *mut lua_State, idx: c_int, k: *const c_char) {
    // SAFETY: `L` is a live state; `k` is a C string.
    unsafe {
        api(L, |vm, th| {
            let t = idx_check(vm, th, idx);
            let key = vm.new_str(cstr(k));
            let v = vm.index(t, Value::Str(key))?;
            vm.push(v);
            Ok(())
        })
    }
}

/// `lua_rawget`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lua_rawget(L: *mut lua_State, idx: c_int) {
    // SAFETY: `L` is a live state.
    unsafe {
        api(L, |vm, th| {
            let t = idx_tab(vm, th, idx);
            let k = th.get(th.top() - 1);
            th.set(th.top() - 1, t.get(k));
            Ok(())
        })
    }
}

/// `lua_rawgeti`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lua_rawgeti(L: *mut lua_State, idx: c_int, n: c_int) {
    // SAFETY: `L` is a live state.
    unsafe {
        api(L, |vm, th| {
            let t = idx_tab(vm, th, idx);
            vm.push(t.get_int(n));
            Ok(())
        })
    }
}

/// `lua_getmetatable`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lua_getmetatable(L: *mut lua_State, idx: c_int) -> c_int {
    // SAFETY: `L` is a live state.
    unsafe {
        api(L, |vm, th| {
            let v = idx_val(vm, th, idx);
            match vm.metatable_of(v) {
                Some(mt) => {
                    vm.push(Value::Table(mt));
                    Ok(1)
                }
                None => Ok(0),
            }
        })
    }
}

/// `lua_getfenv`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lua_getfenv(L: *mut lua_State, idx: c_int) {
    // SAFETY: `L` is a live state.
    unsafe {
        api(L, |vm, th| {
            let v = match idx_check(vm, th, idx) {
                Value::Func(f) => Value::Table(f.env()),
                Value::Udata(u) => Value::Table(u.env()),
                Value::Thread(t) => Value::Table(t.env()),
                _ => Value::Nil,
            };
            vm.push(v);
            Ok(())
        })
    }
}

/// `lua_next`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lua_next(L: *mut lua_State, idx: c_int) -> c_int {
    // SAFETY: `L` is a live state.
    unsafe {
        api(L, |vm, th| {
            let t = idx_tab(vm, th, idx);
            let top = th.top();
            match t.next(th.get(top - 1)) {
                Ok(Some((k, v))) => {
                    th.set(top - 1, k);
                    vm.push(v);
                    Ok(1)
                }
                Ok(None) => {
                    th.set_top(top - 1);
                    Ok(0)
                }
                Err(_) => Err(vm.err_msg(ErrMsg::NEXTIDX)),
            }
        })
    }
}

/// `lua_getupvalue`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lua_getupvalue(L: *mut lua_State, idx: c_int, n: c_int) -> *const c_char {
    // SAFETY: `L` is a live state.
    unsafe {
        api(L, |vm, th| {
            let f = idx_val(vm, th, idx);
            if n < 1 {
                return Ok(std::ptr::null());
            }
            match vm.upvalue_info(f, (n - 1) as usize) {
                Some((name, v)) => {
                    vm.push(v);
                    Ok(name_ptr(vm, &name))
                }
                None => Ok(std::ptr::null()),
            }
        })
    }
}

/// `lua_setupvalue`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lua_setupvalue(L: *mut lua_State, idx: c_int, n: c_int) -> *const c_char {
    // SAFETY: `L` is a live state.
    unsafe {
        api(L, |vm, th| {
            let f = idx_val(vm, th, idx);
            if n < 1 {
                return Ok(std::ptr::null());
            }
            let v = th.get(th.top() - 1);
            match vm.set_upvalue_info(f, (n - 1) as usize, v) {
                Some(name) => {
                    th.set_top(th.top() - 1);
                    Ok(name_ptr(vm, &name))
                }
                None => Ok(std::ptr::null()),
            }
        })
    }
}

/// `lua_upvalueid`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lua_upvalueid(L: *mut lua_State, idx: c_int, n: c_int) -> *mut c_void {
    // SAFETY: `L` is a live state.
    let (vm, th) = unsafe { ctx(L) };
    let Value::Func(f) = idx_val(vm, th, idx) else { return std::ptr::null_mut() };
    let n = (n - 1) as usize;
    match &*f {
        Function::Lua { upvals, .. } => upvals.get(n).map_or(std::ptr::null_mut(), |u| Gc::as_ptr(u.get()).cast()),
        Function::Native { upvals, .. } => upvals.get(n).map_or(std::ptr::null_mut(), |c| c.as_ptr().cast()),
    }
}

/// `lua_upvaluejoin`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lua_upvaluejoin(L: *mut lua_State, idx1: c_int, n1: c_int, idx2: c_int, n2: c_int) {
    // SAFETY: `L` is a live state.
    let (vm, th) = unsafe { ctx(L) };
    if let (Value::Func(f1), Value::Func(f2)) = (idx_val(vm, th, idx1), idx_val(vm, th, idx2))
        && let Some(uv) = f2.upval((n2 - 1) as usize)
        && (n1 - 1) >= 0
        && ((n1 - 1) as usize) < f1.nupvalues()
    {
        f1.set_upval((n1 - 1) as usize, uv);
    }
}

// -- Object setters -----------------------------------------------------------------------------

/// `lua_settable`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lua_settable(L: *mut lua_State, idx: c_int) {
    // SAFETY: `L` is a live state.
    unsafe {
        api(L, |vm, th| {
            let t = idx_check(vm, th, idx);
            let top = th.top();
            let (k, v) = (th.get(top - 2), th.get(top - 1));
            vm.setindex(t, k, v)?;
            th.set_top(top - 2);
            Ok(())
        })
    }
}

/// `lua_setfield`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lua_setfield(L: *mut lua_State, idx: c_int, k: *const c_char) {
    // SAFETY: `L` is a live state; `k` is a C string.
    unsafe {
        api(L, |vm, th| {
            let t = idx_check(vm, th, idx);
            let key = vm.new_str(cstr(k));
            let top = th.top();
            let v = th.get(top - 1);
            vm.setindex(t, Value::Str(key), v)?;
            th.set_top(top - 1);
            Ok(())
        })
    }
}

/// `lua_rawset`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lua_rawset(L: *mut lua_State, idx: c_int) {
    // SAFETY: `L` is a live state.
    unsafe {
        api(L, |vm, th| {
            let t = idx_tab(vm, th, idx);
            let top = th.top();
            vm.rawset(t, th.get(top - 2), th.get(top - 1))?;
            th.set_top(top - 2);
            Ok(())
        })
    }
}

/// `lua_rawseti`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lua_rawseti(L: *mut lua_State, idx: c_int, n: c_int) {
    // SAFETY: `L` is a live state.
    unsafe {
        api(L, |vm, th| {
            let t = idx_tab(vm, th, idx);
            let top = th.top();
            t.set_int(n, th.get(top - 1)).map_err(|e| vm.tab_error(e))?;
            th.set_top(top - 1);
            Ok(())
        })
    }
}

/// `lua_setmetatable`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lua_setmetatable(L: *mut lua_State, idx: c_int) -> c_int {
    // SAFETY: `L` is a live state.
    unsafe {
        api(L, |vm, th| {
            let o = idx_check(vm, th, idx);
            let top = th.top();
            let mt = match th.get(top - 1) {
                Value::Table(t) => Some(t),
                _ => None,
            };
            match o {
                Value::Table(t) => t.set_metatable(mt),
                Value::Udata(u) => u.set_metatable(mt),
                v => vm.set_basemt(v.vtype(), mt),
            }
            th.set_top(top - 1);
            Ok(1)
        })
    }
}

/// `lua_setfenv`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lua_setfenv(L: *mut lua_State, idx: c_int) -> c_int {
    // SAFETY: `L` is a live state.
    unsafe {
        api(L, |vm, th| {
            let o = idx_check(vm, th, idx);
            let top = th.top();
            let Value::Table(t) = th.get(top - 1) else { panic!("C API: top stack slot is not a table") };
            let r = match o {
                Value::Func(f) => {
                    f.set_env(t);
                    1
                }
                Value::Udata(u) => {
                    u.set_env(t);
                    1
                }
                Value::Thread(x) => {
                    x.set_env(t);
                    1
                }
                _ => 0,
            };
            th.set_top(top - 1);
            Ok(r)
        })
    }
}

// -- Calls --------------------------------------------------------------------------------------

/// `lua_call`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lua_call(L: *mut lua_State, nargs: c_int, nresults: c_int) {
    // SAFETY: `L` is a live state.
    let (vm, _) = unsafe { ctx(L) };
    let old = vm.cur_thread;
    // SAFETY: `L` is a live state.
    unsafe { api(L, |vm, th| on_thread(vm, th, old, |vm| vm.call(nargs as usize, nresults).map(|_| ()))) }
}

/// Finish a protected call: on error, restore the hook state and leave the error object on
/// the stack; return the status.
fn pcall_status(vm: &mut Vm, oldh: bool, r: LuaResult<usize>) -> c_int {
    match r {
        Ok(_) => 0,
        Err(e) => {
            vm.rt.hook.active = oldh;
            vm.push(e.value);
            e.status()
        }
    }
}

/// `lua_pcall`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lua_pcall(L: *mut lua_State, nargs: c_int, nresults: c_int, errfunc: c_int) -> c_int {
    // SAFETY: `L` is a live state.
    let (vm, _) = unsafe { ctx(L) };
    let old = vm.cur_thread;
    // SAFETY: `L` is a live state.
    unsafe {
        api(L, |vm, th| {
            let ef = if errfunc == 0 { None } else { Some(idx_stack(vm, th, errfunc)) };
            let oldh = vm.rt.hook.active;
            let r = on_thread(vm, th, old, |vm| vm.pcall(nargs as usize, nresults, ef));
            Ok(pcall_status(vm, oldh, r))
        })
    }
}

/// `lua_cpcall`: call `func` with a light userdata `ud`, protected, no results.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lua_cpcall(L: *mut lua_State, func: lua_CFunction, ud: *mut c_void) -> c_int {
    // SAFETY: `L` is a live state.
    let (vm, _) = unsafe { ctx(L) };
    let old = vm.cur_thread;
    // SAFETY: `L` is a live state.
    unsafe {
        api(L, |vm, th| {
            let env = getcurrenv(th);
            let f = vm.new_native(NativeImpl::C(func), FF_C, Vec::new(), env);
            vm.push(Value::Func(f));
            vm.push(Value::LightUd(ud as usize));
            let oldh = vm.rt.hook.active;
            let r = on_thread(vm, th, old, |vm| vm.pcall(1, 0, None));
            Ok(pcall_status(vm, oldh, r))
        })
    }
}

/// Push the result of a load: the function, or the error object (returning its status).
/// Then run the GC check (`lj_gc_check` after `lj_vm_cpcall` in `lua_loadx`): only once the
/// result is on the stack, since it is not reachable from anywhere else.
pub(crate) fn load_status(vm: &mut Vm, r: LuaResult<Value>) -> c_int {
    let st = match r {
        Ok(f) => {
            vm.push(f);
            0
        }
        Err(e) => {
            vm.rt_clear_thrown();
            vm.push(e.value);
            e.status()
        }
    };
    vm.gc_safe();
    st
}

/// `lua_loadx`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lua_loadx(
    L: *mut lua_State,
    reader: lua_Reader,
    data: *mut c_void,
    chunkname: *const c_char,
    mode: *const c_char,
) -> c_int {
    // SAFETY: `L` is a live state; the strings are NULL or C strings.
    unsafe {
        let name: &[u8] = if chunkname.is_null() { b"?" } else { cstr(chunkname) };
        let mode = (!mode.is_null()).then(|| cstr(mode));
        api(L, |vm, th| {
            let r = vm.load_reader(
                |vm| {
                    let L = state_of(vm, th);
                    let mut size = 0usize;
                    let p = protect(vm, || reader(L, data, &mut size))?;
                    if p.is_null() || size == 0 {
                        Ok(None)
                    } else {
                        Ok(Some(std::slice::from_raw_parts(p.cast::<u8>(), size).to_vec()))
                    }
                },
                name,
                mode,
            );
            Ok(load_status(vm, r))
        })
    }
}

/// `lua_load`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lua_load(L: *mut lua_State, reader: lua_Reader, data: *mut c_void, chunkname: *const c_char) -> c_int {
    // SAFETY: forwarded contract.
    unsafe { lua_loadx(L, reader, data, chunkname, std::ptr::null()) }
}

/// `lua_dump`: dump the Lua function at the top (FR2 format, not stripped).
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lua_dump(L: *mut lua_State, writer: lua_Writer, data: *mut c_void) -> c_int {
    // SAFETY: `L` is a live state.
    unsafe {
        api(L, |vm, th| {
            let Value::Func(f) = th.get(th.top() - 1) else { return Ok(1) };
            let Some(pt) = f.proto() else { return Ok(1) };
            let b = crate::bcdump::write(vm, pt, false);
            let L = state_of(vm, th);
            protect(vm, || writer(L, b.as_ptr().cast(), b.len(), data))
        })
    }
}

// -- Coroutines ---------------------------------------------------------------------------------

/// `lua_isyieldable`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lua_isyieldable(L: *mut lua_State) -> c_int {
    // SAFETY: `L` is a live state.
    let (vm, _) = unsafe { ctx(L) };
    vm.can_yield() as c_int
}

/// `lua_yield`: `return lua_yield(L, n)` from a C function yields the top `n` values.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lua_yield(L: *mut lua_State, nresults: c_int) -> c_int {
    // SAFETY: `L` is a live state.
    unsafe {
        api(L, |vm, th| {
            if !vm.can_yield() {
                return Err(vm.err_msg(ErrMsg::CYIELD));
            }
            let n = nresults.max(0) as usize;
            let base = base_of(vm, th);
            let f = th.top() - n;
            // Regular yield: move the results down. A yield from a hook keeps them on top
            // (the interpreter adds a pseudo-frame, see `Cont::Hook`).
            if !vm.rt.hook.active && f > base {
                for i in 0..n {
                    th.set(base + i, th.get(f + i));
                }
                th.set_top(base + n);
            }
            vm.rt.multres = n;
            vm.rt.capi.yield_pending = true;
            Ok(-1)
        })
    }
}

/// `lua_resume`: start or continue the coroutine `L` with the top `narg` values of its stack.
/// Its results (or the error object) are left on its stack.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lua_resume(L: *mut lua_State, narg: c_int) -> c_int {
    // SAFETY: `L` is a live state.
    let (vm, co) = unsafe { ctx(L) };
    let prev = vm.cur_thread;
    let nargs = narg.max(0) as usize;
    let st = co.status();
    let fresh = st == ThreadStatus::Fresh || (st == ThreadStatus::Dead && co.frame_count() == 0);
    if co == prev || !(fresh || st == ThreadStatus::Suspended) {
        co.set_top(base_of(vm, co));
        let s = vm.err_str(ErrMsg::COSUSP);
        co.push(s);
        return crate::error::LUA_ERRRUN;
    }
    if let Err(e) = checkstack(vm, prev, nargs + 1) {
        co.push(e.value);
        return e.status();
    }
    // Move the arguments to the resumer's stack (where `Vm::resume` takes them from).
    let top = co.top();
    let ptop = prev.top();
    for i in 0..nargs {
        prev.set(ptop + i, co.get(top - nargs + i));
    }
    prev.set_top(ptop + nargs);
    if fresh {
        let fslot = top - nargs - 1;
        co.set(CO_FUNC_SLOT, co.get(fslot));
        co.set_top(CO_FUNC_SLOT + 1);
        co.set_status(ThreadStatus::Fresh);
    } else {
        co.set_top(top - nargs);
    }
    let r = vm.resume(co, nargs);
    match r {
        Ok(Ok(n)) => {
            let ptop = prev.top();
            if let Err(e) = checkstack(vm, co, n) {
                prev.set_top(ptop - n);
                co.push(e.value);
                return e.status();
            }
            let ctop = co.top();
            for i in 0..n {
                co.set(ctop + i, prev.get(ptop - n + i));
            }
            co.set_top(ctop + n);
            prev.set_top(ptop - n);
            if co.status() == ThreadStatus::Suspended { crate::error::LUA_YIELD } else { 0 }
        }
        Ok(Err(v)) => {
            co.push(v);
            crate::error::LUA_ERRRUN
        }
        Err(e) => {
            vm.rt_clear_thrown();
            co.push(e.value);
            e.status()
        }
    }
}

// -- GC and memory management -------------------------------------------------------------------

/// `lua_gc`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lua_gc(L: *mut lua_State, what: c_int, data: c_int) -> c_int {
    // SAFETY: `L` is a live state.
    unsafe {
        api(L, |vm, _| {
            Ok(match what {
                0 => {
                    vm.gc_stop();
                    0
                }
                1 => {
                    vm.gc_restart(data);
                    0
                }
                2 => {
                    vm.gc_collect();
                    0
                }
                3 => (vm.gc_count_bytes() >> 10) as c_int,
                4 => (vm.gc_count_bytes() & 0x3ff) as c_int,
                5 => vm.gc_step_kb(data) as c_int,
                6 => vm.gc_set_pause(data as u32) as c_int,
                7 => vm.gc_set_stepmul(data as u32) as c_int,
                9 => vm.gc_is_running() as c_int,
                _ => -1,
            })
        })
    }
}

/// `lua_getallocf`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lua_getallocf(L: *mut lua_State, ud: *mut *mut c_void) -> lua_Alloc {
    // SAFETY: `L` is a live state.
    let (vm, _) = unsafe { ctx(L) };
    if !ud.is_null() {
        // SAFETY: caller-provided output pointer.
        unsafe { *ud = vm.rt.capi.allocd };
    }
    vm.rt.capi.allocf.unwrap_or(l_alloc)
}

/// `lua_setallocf`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lua_setallocf(L: *mut lua_State, f: lua_Alloc, ud: *mut c_void) {
    // SAFETY: `L` is a live state.
    let (vm, _) = unsafe { ctx(L) };
    vm.rt.capi.allocf = Some(f);
    vm.rt.capi.allocd = ud;
}

// -- Errors -------------------------------------------------------------------------------------

/// `lua_error`: raise the value at the top of the stack.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lua_error(L: *mut lua_State) -> c_int {
    // SAFETY: `L` is a live state.
    unsafe {
        api(L, |vm, th| {
            let v = th.get(th.top() - 1);
            Err::<c_int, LuaError>(vm.error(v))
        })
    }
}

// -- Debug API ----------------------------------------------------------------------------------

/// The frame designated by `ar->i_ci`.
fn frame_of(th: Gc<Thread>, ar: *const lua_Debug) -> Option<FrameRef> {
    // SAFETY: `ar` is a valid lua_Debug.
    let i = unsafe { (*ar).i_ci };
    (i >= 0 && (i as usize) < th.frame_count()).then_some(FrameRef { thread: th, idx: i as usize })
}

/// `lua_getstack`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lua_getstack(L: *mut lua_State, level: c_int, ar: *mut lua_Debug) -> c_int {
    // SAFETY: `L` is a live state; `ar` is valid.
    let (vm, th) = unsafe { ctx(L) };
    match vm.get_stack(th, level) {
        Some(fr) => {
            // SAFETY: as above.
            unsafe { (*ar).i_ci = fr.idx as c_int };
            1
        }
        None => {
            // SAFETY: as above.
            unsafe { (*ar).i_ci = vm.stack_depth(th) };
            0
        }
    }
}

/// `lua_getinfo`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lua_getinfo(L: *mut lua_State, what: *const c_char, ar: *mut lua_Debug) -> c_int {
    // SAFETY: `L` is a live state; `what` is a C string; `ar` is valid.
    unsafe {
        api(L, |vm, th| {
            let what = cstr(what);
            let (target, opts) = if what.first() == Some(&b'>') {
                let top = th.top() - 1;
                let Value::Func(f) = th.get(top) else { panic!("C API: function expected for lua_getinfo('>')") };
                th.set_top(top);
                (InfoTarget::Func(f), &what[1..])
            } else {
                match frame_of(th, ar) {
                    Some(fr) => (InfoTarget::Frame(fr), what),
                    None => return Ok(0),
                }
            };
            let Some(info) = vm.get_info(target, opts) else { return Ok(0) };
            let a = &mut *ar;
            for &c in opts {
                match c {
                    b'S' => {
                        a.source = match info.func.or(match target {
                            InfoTarget::Func(f) => Some(f),
                            InfoTarget::Frame(fr) => fr.thread.frame(fr.idx).map(|f| f.func),
                        }) {
                            Some(f) => match f.proto() {
                                Some(pt) => strdata(pt.chunkname),
                                None => c"=[C]".as_ptr(),
                            },
                            None => c"=[C]".as_ptr(),
                        };
                        let n = info.short_src.len().min(LUA_IDSIZE - 1);
                        for (d, &s) in a.short_src.iter_mut().zip(&info.short_src[..n]) {
                            *d = s as c_char;
                        }
                        a.short_src[n] = 0;
                        a.linedefined = info.linedefined;
                        a.lastlinedefined = info.lastlinedefined;
                        a.what = name_ptr(vm, info.what.as_bytes());
                    }
                    b'l' => a.currentline = info.currentline,
                    b'u' => a.nups = info.nups as c_int,
                    b'n' => {
                        a.name = match &info.name {
                            Some(n) => name_ptr(vm, n),
                            None => std::ptr::null(),
                        };
                        a.namewhat = name_ptr(vm, info.namewhat.as_bytes());
                    }
                    _ => {}
                }
            }
            if let Some(f) = info.func {
                vm.push(Value::Func(f));
            }
            if let Some(l) = info.activelines {
                vm.push(l);
            }
            Ok(1)
        })
    }
}

/// `lua_getlocal`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lua_getlocal(L: *mut lua_State, ar: *const lua_Debug, n: c_int) -> *const c_char {
    // SAFETY: `L` is a live state; `ar` is NULL or valid.
    unsafe {
        api(L, |vm, th| {
            if ar.is_null() {
                let Value::Func(f) = th.get(th.top() - 1) else { return Ok(std::ptr::null()) };
                return Ok(match vm.local_name_of(f, n) {
                    Some(name) => name_ptr(vm, &name),
                    None => std::ptr::null(),
                });
            }
            let Some(fr) = frame_of(th, ar) else { return Ok(std::ptr::null()) };
            match vm.get_local(fr, n) {
                Some((name, v)) => {
                    vm.push(v);
                    Ok(name_ptr(vm, &name))
                }
                None => Ok(std::ptr::null()),
            }
        })
    }
}

/// `lua_setlocal`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lua_setlocal(L: *mut lua_State, ar: *const lua_Debug, n: c_int) -> *const c_char {
    // SAFETY: `L` is a live state; `ar` is valid.
    unsafe {
        api(L, |vm, th| {
            let top = th.top() - 1;
            let v = th.get(top);
            let name = frame_of(th, ar).and_then(|fr| vm.set_local(fr, n, v));
            th.set_top(top);
            Ok(match name {
                Some(name) => name_ptr(vm, &name),
                None => std::ptr::null(),
            })
        })
    }
}

/// `lua_sethook`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lua_sethook(L: *mut lua_State, func: Option<lua_Hook>, mask: c_int, count: c_int) -> c_int {
    // SAFETY: `L` is a live state.
    let (vm, _) = unsafe { ctx(L) };
    let mask = (mask & 0x0f) as u8;
    let func = if mask == 0 { None } else { func };
    vm.rt.capi.hook = func;
    vm.set_hook(func.map(|_| hook_trampoline as HookFn), mask, count);
    1
}

/// `lua_gethook`: the C hook (NULL if none, or if the hook was set from Lua).
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lua_gethook(L: *mut lua_State) -> Option<lua_Hook> {
    // SAFETY: `L` is a live state.
    let (vm, _) = unsafe { ctx(L) };
    match vm.get_hook().0 {
        Some(f) if f as usize == hook_trampoline as HookFn as usize => vm.rt.capi.hook,
        _ => None,
    }
}

/// `lua_gethookmask`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lua_gethookmask(L: *mut lua_State) -> c_int {
    // SAFETY: `L` is a live state.
    let (vm, _) = unsafe { ctx(L) };
    vm.get_hook().1 as c_int
}

/// `lua_gethookcount`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lua_gethookcount(L: *mut lua_State) -> c_int {
    // SAFETY: `L` is a live state.
    let (vm, _) = unsafe { ctx(L) };
    vm.get_hook().2
}

/// Error message for `luaL_checkstack` and friends.
pub(crate) fn stkovm(vm: &mut Vm, msg: &[u8]) -> LuaError {
    let m = fmt_msg(ErrMsg::STKOVM, &[msg]);
    vm.err_callermsg(&m)
}

/// Raise a runtime error with a plain message (no position).
#[allow(dead_code)]
pub(crate) fn plain_error(vm: &mut Vm, msg: &[u8]) -> LuaError {
    LuaError::new(ErrorKind::Run, Value::Str(vm.new_str(msg)))
}
