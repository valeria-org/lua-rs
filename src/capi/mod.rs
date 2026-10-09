//! The Lua 5.1 / LuaJIT 2.1 C API, exported with C linkage from the `luajit` binary and from
//! `libluajit.so`: `lua_*` ([`lapi`]: port of lj_api.c plus the public API parts of lj_err.c,
//! lj_load.c, lj_state.c, lj_dispatch.c and lj_debug.c), `luaL_*` ([`lauxlib`]: lib_aux.c and
//! the `luaL_*` functions of lj_api.c/lj_err.c/lj_load.c/lj_debug.c), `luaopen_*`,
//! `luaL_openlibs` and `luaJIT_*` ([`linit`]: lib_init.c, lj_dispatch.c, lj_profile.c).
//!
//! # `lua_State*`
//! Every thread owns a heap-allocated [`LuaState`] handle (created on first use and freed with
//! the thread), so the pointer is stable for the thread's lifetime and `lua_tothread`/
//! `lua_pushthread` round-trip. A handle refers to its thread and to a per-VM cell holding the
//! current `*mut Vm`; that cell is refreshed from the live `&mut Vm` whenever control passes
//! into C (C function calls, readers, writers, hooks, panic functions), so the pointer C code
//! uses is always derived from the borrow of the code that called it.
//!
//! Every API function operates on the stack of its `L` argument: for the duration of the call
//! the VM's current thread is `L`'s thread. Stack indices are relative to the base of the
//! thread's current frame (a C function's arguments start at index 1); a thread without frames
//! has its base at slot 0 (main thread) or 2 (coroutines, as upstream's `lj_state_new`).
//!
//! # Errors
//! An error raised by an API function (or `lua_error`) unwinds through the C frames with
//! `std::panic::resume_unwind` and a private payload, and is caught where the VM called into
//! C ([`call_c`], readers, hooks, ...) and turned back into a [`LuaError`]. Without any such
//! catch point (an unprotected error in a host program), the panic function runs and the
//! process exits with `EXIT_FAILURE`, as upstream.
//!
//! # Rooting
//! As upstream, pointers handed to C (`lua_tostring`, `lua_touserdata`, ...) stay valid while
//! the value is reachable; the collector only runs at the API's GC check points (allocating
//! functions) and inside calls into Lua.

//!
//! # Safety
//! All exported functions follow the contracts of the Lua 5.1 reference manual: every
//! `lua_State*` must be a live handle of a live state, indices must be valid for the function,
//! and pointer arguments must be valid as documented there. Per-function `# Safety` sections
//! are omitted for that reason.

#![allow(non_camel_case_types, non_snake_case, clippy::missing_safety_doc)]

pub mod lapi;
pub mod lauxlib;
pub mod linit;
mod vararg;

use std::cell::Cell;
use std::ffi::{c_char, c_int, c_void};
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};

use crate::error::{ErrorKind, LuaError, LuaResult};
use crate::func::{CFunction, Function};
use crate::gc::Gc;
use crate::state::Vm;
use crate::table::Table;
use crate::thread::Thread;
use crate::value::Value;
use crate::vm::HookEvent;

pub use vararg::VaListTag;

/// `lua_Number`.
pub type lua_Number = f64;
/// `lua_Integer` (`ptrdiff_t`).
pub type lua_Integer = isize;
/// `lua_CFunction`.
pub type lua_CFunction = CFunction;
/// `lua_Reader`.
pub type lua_Reader = unsafe extern "C-unwind" fn(*mut lua_State, *mut c_void, *mut usize) -> *const c_char;
/// `lua_Writer`.
pub type lua_Writer = unsafe extern "C-unwind" fn(*mut lua_State, *const c_void, usize, *mut c_void) -> c_int;
/// `lua_Alloc`.
pub type lua_Alloc = unsafe extern "C-unwind" fn(*mut c_void, *mut c_void, usize, usize) -> *mut c_void;
/// `lua_Hook`.
pub type lua_Hook = unsafe extern "C-unwind" fn(*mut lua_State, *mut lua_Debug);
/// Wrapper for C function calls (`LUAJIT_MODE_WRAPCFUNC`): `wrapf(L, f)`.
pub type WrapFn = unsafe extern "C-unwind" fn(*mut lua_State, CFunction) -> c_int;

pub const LUA_REGISTRYINDEX: c_int = -10000;
pub const LUA_ENVIRONINDEX: c_int = -10001;
pub const LUA_GLOBALSINDEX: c_int = -10002;

pub const LUA_TNONE: c_int = -1;
pub const LUA_TNIL: c_int = 0;
pub const LUA_TBOOLEAN: c_int = 1;
pub const LUA_TLIGHTUSERDATA: c_int = 2;
pub const LUA_TNUMBER: c_int = 3;
pub const LUA_TSTRING: c_int = 4;
pub const LUA_TTABLE: c_int = 5;
pub const LUA_TFUNCTION: c_int = 6;
pub const LUA_TUSERDATA: c_int = 7;
pub const LUA_TTHREAD: c_int = 8;
/// Prototype (internal, as seen through `lua_type` on jit.util results).
pub const LUA_TPROTO: c_int = 9;
/// cdata (LuaJIT extension of `lua_type`).
pub const LUA_TCDATA: c_int = 10;

/// `LUA_IDSIZE` of luaconf.h.
pub const LUA_IDSIZE: usize = 60;
/// `LUAL_BUFFERSIZE` of luaconf.h (`BUFSIZ` is 8192 with glibc).
pub const LUAL_BUFFERSIZE: usize = 8192;
/// `LUAI_MAXCSTACK`.
pub const LUAI_MAXCSTACK: c_int = 8000;

/// Stack base of a coroutine without frames (`lj_state_new`: `stack + 1 + LJ_FR2`).
const CO_BASE: usize = 2;

/// `lua_Debug` (layout of lua.h). `i_ci` holds the frame index of the thread's frame vector.
#[repr(C)]
pub struct lua_Debug {
    pub event: c_int,
    pub name: *const c_char,
    pub namewhat: *const c_char,
    pub what: *const c_char,
    pub source: *const c_char,
    pub currentline: c_int,
    pub nups: c_int,
    pub linedefined: c_int,
    pub lastlinedefined: c_int,
    pub short_src: [c_char; LUA_IDSIZE],
    pub i_ci: c_int,
}

impl lua_Debug {
    fn zeroed() -> lua_Debug {
        lua_Debug {
            event: 0,
            name: std::ptr::null(),
            namewhat: std::ptr::null(),
            what: std::ptr::null(),
            source: std::ptr::null(),
            currentline: 0,
            nups: 0,
            linedefined: 0,
            lastlinedefined: 0,
            short_src: [0; LUA_IDSIZE],
            i_ci: 0,
        }
    }
}

/// `luaL_Buffer` (layout of lauxlib.h).
#[repr(C)]
pub struct luaL_Buffer {
    pub p: *mut c_char,
    pub lvl: c_int,
    pub L: *mut lua_State,
    pub buffer: [c_char; LUAL_BUFFERSIZE],
}

/// `luaL_Reg`.
#[repr(C)]
pub struct luaL_Reg {
    pub name: *const c_char,
    pub func: Option<CFunction>,
}

/// The C API handle of a thread (`lua_State`). Opaque to C.
pub struct LuaState {
    /// Per-VM cell holding the current `*mut Vm` (see the module docs).
    g: *const Cell<*mut Vm>,
    th: Gc<Thread>,
}

/// `lua_State` as seen from C.
pub type lua_State = LuaState;

/// Per-VM C API state, stored in the VM runtime.
pub struct CApiGlobal {
    /// Shared by all `LuaState` handles of the VM; boxed so its address is stable.
    vmref: Box<Cell<*mut Vm>>,
    /// `lua_atpanic` function (`g->panic`).
    pub(crate) panic: Option<CFunction>,
    /// `lua_getallocf`/`lua_setallocf`.
    pub(crate) allocf: Option<lua_Alloc>,
    pub(crate) allocd: *mut c_void,
    /// Number of active catch points for C API errors (calls into C made by the VM).
    catchers: u32,
    /// C hook installed with `lua_sethook`.
    pub(crate) hook: Option<lua_Hook>,
    /// `LUAJIT_MODE_WRAPCFUNC` wrapper and whether it is enabled.
    pub(crate) wrapf: Option<WrapFn>,
    pub(crate) wrap_on: bool,
    /// Set by `lua_yield`; checked when the C function returns.
    pub(crate) yield_pending: bool,
    /// The `Vm` was allocated by `lua_newstate` (freed by `lua_close`).
    pub(crate) owned: bool,
    /// Result buffer of `luaJIT_profile_dumpstack` (valid until the next call).
    pub(crate) dumpbuf: Vec<u8>,
}

impl Default for CApiGlobal {
    fn default() -> Self {
        CApiGlobal {
            vmref: Box::new(Cell::new(std::ptr::null_mut())),
            panic: None,
            allocf: None,
            allocd: std::ptr::null_mut(),
            catchers: 0,
            hook: None,
            wrapf: None,
            wrap_on: false,
            yield_pending: false,
            owned: false,
            dumpbuf: Vec::new(),
        }
    }
}

/// Private unwind payload carrying a Lua error through C frames.
struct Thrown(LuaError);

// SAFETY: the payload never leaves the OS thread that runs the VM; `Send` is only required by
// the `resume_unwind` signature.
unsafe impl Send for Thrown {}

/// Point the VM's shared handle cell at `vm` (the borrow C code will derive from).
#[inline]
fn refresh(vm: &mut Vm) -> *const Cell<*mut Vm> {
    let p: *mut Vm = vm;
    let cell: *const Cell<*mut Vm> = &*vm.rt.capi.vmref;
    // SAFETY: `cell` points into the boxed cell owned by `vm`.
    unsafe { (*cell).set(p) };
    cell
}

/// The `lua_State*` of thread `th` of `vm` (created on first use).
pub fn state_of(vm: &mut Vm, th: Gc<Thread>) -> *mut lua_State {
    let g = refresh(vm);
    let st = th.capi.get_or_init(|| Box::new(LuaState { g, th }));
    &**st as *const LuaState as *mut LuaState
}

/// VM and thread of a handle.
///
/// # Safety
/// `L` must be a live handle of a live VM.
#[inline]
pub(crate) unsafe fn ctx<'a>(L: *mut lua_State) -> (&'a mut Vm, Gc<Thread>) {
    // SAFETY: per the contract, the handle and its VM cell are live.
    unsafe {
        let st = &*L;
        (&mut *(*st.g).get(), st.th)
    }
}

/// Run an API operation on `L`'s thread: the VM's current thread is `L`'s thread during `f`;
/// an error is raised in C (see [`throw`]).
///
/// # Safety
/// `L` must be a live handle.
#[inline]
pub(crate) unsafe fn api<R>(L: *mut lua_State, f: impl FnOnce(&mut Vm, Gc<Thread>) -> LuaResult<R>) -> R {
    // SAFETY: forwarded contract.
    let (vm, th) = unsafe { ctx(L) };
    let old = vm.cur_thread;
    vm.cur_thread = th;
    let r = f(vm, th);
    vm.cur_thread = old;
    match r {
        Ok(v) => v,
        Err(e) => throw(vm, e),
    }
}

/// Raise `e` in C code: unwind to the innermost catch point, or call the panic function and
/// exit if there is none (`lj_err_throw` without a C frame to unwind to).
pub(crate) fn throw(vm: &mut Vm, e: LuaError) -> ! {
    if vm.rt.capi.catchers == 0 {
        let th = vm.cur_thread;
        vm.push(e.value);
        if let Some(p) = vm.rt.capi.panic {
            let L = state_of(vm, th);
            // SAFETY: calling the user's panic function with a valid state.
            unsafe { p(L.cast()) };
        }
        std::process::exit(libc::EXIT_FAILURE);
    }
    resume_unwind(Box::new(Thrown(e)))
}

/// Run `f` (which calls into C) as a catch point: a C API error raised inside comes back as
/// `Err`. Foreign panics keep unwinding.
pub(crate) fn protect<R>(vm: &mut Vm, f: impl FnOnce() -> R) -> LuaResult<R> {
    vm.rt.capi.catchers += 1;
    refresh(vm);
    let r = catch_unwind(AssertUnwindSafe(f));
    vm.rt.capi.catchers -= 1;
    match r {
        Ok(v) => Ok(v),
        Err(p) => match p.downcast::<Thrown>() {
            Ok(t) => Err(t.0),
            Err(p) => resume_unwind(p),
        },
    }
}

/// The VM's dispatcher for `NativeImpl::C` functions ([`crate::vm::CCallHook`]): run `f` with
/// the current thread's `lua_State*`, turn a C API error into `Err` and a `lua_yield` into a
/// yield. Returns the number of results (the top values of the frame).
pub fn call_c(vm: &mut Vm, f: CFunction) -> LuaResult<usize> {
    let th = vm.cur_thread;
    let L = state_of(vm, th);
    vm.rt.capi.yield_pending = false;
    let wrap = if vm.rt.capi.wrap_on { vm.rt.capi.wrapf } else { None };
    // SAFETY: calling a C function with a valid state, as the VM's FUNCC/FUNCCW would.
    let n = protect(vm, || unsafe {
        match wrap {
            Some(w) => w(L, f),
            None => f(L.cast()),
        }
    })?;
    if std::mem::take(&mut vm.rt.capi.yield_pending) {
        return Err(LuaError::new(ErrorKind::Yield, Value::Num(vm.rt.multres as f64)));
    }
    Ok(n.max(0) as usize)
}

/// The VM hook installed by `lua_sethook`: call the C hook with a `lua_Debug` whose `i_ci`
/// designates the hooked frame (`callhook` of lj_dispatch.c).
pub(crate) fn hook_trampoline(vm: &mut Vm, ev: HookEvent, line: i32) -> LuaResult<()> {
    let Some(h) = vm.rt.capi.hook else { return Ok(()) };
    let th = vm.cur_thread;
    let mut ar = lua_Debug::zeroed();
    ar.event = ev as c_int;
    ar.currentline = line;
    ar.i_ci = vm.get_stack(th, 0).map_or(0, |f| f.idx as c_int);
    let L = state_of(vm, th);
    // SAFETY: calling the user's hook with a valid state and debug record.
    protect(vm, || unsafe { h(L, &mut ar) })?;
    if std::mem::take(&mut vm.rt.capi.yield_pending) {
        return Err(LuaError::new(ErrorKind::Yield, Value::Num(vm.rt.multres as f64)));
    }
    Ok(())
}

// -- Stack addressing (index2adr) ---------------------------------------------------------------

/// Base slot of `th` as seen by the API.
#[inline]
pub(crate) fn base_of(vm: &Vm, th: Gc<Thread>) -> usize {
    match th.current_frame() {
        Some(f) => f.base as usize,
        None if th == vm.main_thread => 0,
        None => CO_BASE,
    }
}

/// Address of an API index (`index2adr`).
#[derive(Clone, Copy, Debug)]
pub(crate) enum Adr {
    Stack(usize),
    Registry,
    Globals,
    Env,
    Upval(Gc<Function>, usize),
    /// `niltv`: an invalid or absent index.
    None,
}

/// The running function of `th` (`curr_func`), if any.
#[inline]
pub(crate) fn curr_func(th: Gc<Thread>) -> Option<Gc<Function>> {
    th.current_frame().map(|f| f.func)
}

/// `getcurrenv`: environment of the running function, else of the thread.
pub(crate) fn getcurrenv(th: Gc<Thread>) -> Gc<Table> {
    curr_func(th).map_or_else(|| th.env(), |f| f.env())
}

pub(crate) fn index2adr(vm: &Vm, th: Gc<Thread>, idx: c_int) -> Adr {
    let base = base_of(vm, th);
    let top = th.top();
    if idx > 0 {
        let s = base + idx as usize - 1;
        if s < top { Adr::Stack(s) } else { Adr::None }
    } else if idx > LUA_REGISTRYINDEX {
        let s = top as i64 + idx as i64;
        if idx != 0 && s >= base as i64 { Adr::Stack(s as usize) } else { Adr::None }
    } else if idx == LUA_GLOBALSINDEX {
        Adr::Globals
    } else if idx == LUA_REGISTRYINDEX {
        Adr::Registry
    } else if idx == LUA_ENVIRONINDEX {
        Adr::Env
    } else {
        let n = (LUA_GLOBALSINDEX - idx) as usize;
        match curr_func(th) {
            Some(f) if !f.is_lua() && n <= f.nupvalues() => Adr::Upval(f, n - 1),
            _ => Adr::None,
        }
    }
}

/// Value at an address (`None` for `niltv`).
pub(crate) fn adr_get(vm: &Vm, th: Gc<Thread>, a: Adr) -> Option<Value> {
    match a {
        Adr::Stack(s) => Some(th.get(s)),
        Adr::Registry => Some(Value::Table(vm.registry)),
        Adr::Globals => Some(Value::Table(th.env())),
        Adr::Env => Some(Value::Table(getcurrenv(th))),
        Adr::Upval(f, i) => f.upvalue(i),
        Adr::None => None,
    }
}

/// Value at an index (`None` for an invalid/absent index).
#[inline]
pub(crate) fn idx_get(vm: &Vm, th: Gc<Thread>, idx: c_int) -> Option<Value> {
    adr_get(vm, th, index2adr(vm, th, idx))
}

/// Value at an index, nil for an invalid index.
#[inline]
pub(crate) fn idx_val(vm: &Vm, th: Gc<Thread>, idx: c_int) -> Value {
    idx_get(vm, th, idx).unwrap_or(Value::Nil)
}

/// Absolute stack slot of a stack index (`index2adr_stack`).
pub(crate) fn idx_stack(vm: &Vm, th: Gc<Thread>, idx: c_int) -> usize {
    match index2adr(vm, th, idx) {
        Adr::Stack(s) => s,
        a => panic!("C API: invalid stack index {idx} ({a:?})"),
    }
}

/// Convert a C string to a byte slice.
///
/// # Safety
/// `s` must be a valid NUL-terminated string that outlives the returned slice's use.
#[inline]
pub(crate) unsafe fn cstr<'a>(s: *const c_char) -> &'a [u8] {
    // SAFETY: per the contract.
    unsafe { std::ffi::CStr::from_ptr(s).to_bytes() }
}

/// Pointer to the NUL-terminated content of a Lua string (`strdata`).
#[inline]
pub(crate) fn strdata(s: Gc<crate::lstr::LuaStr>) -> *const c_char {
    s.as_bytes_with_nul().as_ptr().cast()
}
