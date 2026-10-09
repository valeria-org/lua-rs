//! `luaopen_*` and `luaL_openlibs` (port of lib_init.c and the `luaopen_*` entry points of the
//! lib_*.c files), `luaJIT_setmode` and the version symbol (lj_dispatch.c).

use std::ffi::{c_char, c_int, c_void};

use crate::func::FF_C;
use crate::state::Vm;
use crate::stdlib::{LIB_LOAD, LIB_MODULES, loaded_table, new_lib_fn};
use crate::value::Value;
use crate::vm::ErrMsg;

use super::*;

/// Open the `package.loaded` library `name` and push its table(s) (`luaopen_base` pushes `_G`
/// and `coroutine`). Returns the number of pushed values.
fn open_loaded(vm: &mut Vm, name: &str) -> LuaResult<c_int> {
    let Some(&(_, open)) = LIB_LOAD.iter().find(|(n, _)| *n == name) else {
        let m = format!("library '{name}' is not available");
        return Err(vm.error_msg_bytes(m.as_bytes()));
    };
    open(vm)?;
    let loaded = loaded_table(vm);
    if name.is_empty() {
        let g = vm.globals;
        vm.push(Value::Table(g));
        let k = vm.new_str(b"coroutine");
        let co = loaded.get_str(k);
        vm.push(co);
        Ok(2)
    } else {
        let k = vm.new_str(name.as_bytes());
        let t = loaded.get_str(k);
        vm.push(t);
        Ok(1)
    }
}

/// Run the `luaopen_*`-style native of module `name` ([`LIB_MODULES`]) and keep its results.
fn open_module(vm: &mut Vm, name: &str) -> LuaResult<c_int> {
    let Some(&(_, f)) = LIB_MODULES.iter().find(|(n, _)| *n == name) else {
        let m = format!("library '{name}' is not available");
        return Err(vm.error_msg_bytes(m.as_bytes()));
    };
    let func = new_lib_fn(vm, f, FF_C, Vec::new());
    vm.push(Value::Func(func));
    Ok(vm.call(0, -1)? as c_int)
}

macro_rules! luaopen {
    ($($(#[$m:meta])* $fname:ident => $how:ident($lib:literal);)*) => {$(
        $(#[$m])*
        #[unsafe(no_mangle)]
        pub unsafe extern "C-unwind" fn $fname(L: *mut lua_State) -> c_int {
            // SAFETY: `L` is a live state.
            unsafe { api(L, |vm, _| $how(vm, $lib)) }
        }
    )*};
}

luaopen! {
    /// `luaopen_base`: base library and `coroutine`; returns `_G` and the coroutine table.
    luaopen_base => open_loaded("");
    /// `luaopen_package`.
    luaopen_package => open_loaded("package");
    /// `luaopen_table`.
    luaopen_table => open_loaded("table");
    /// `luaopen_io`.
    luaopen_io => open_loaded("io");
    /// `luaopen_os`.
    luaopen_os => open_loaded("os");
    /// `luaopen_string`.
    luaopen_string => open_loaded("string");
    /// `luaopen_math`.
    luaopen_math => open_loaded("math");
    /// `luaopen_debug`.
    luaopen_debug => open_loaded("debug");
    /// `luaopen_bit`.
    luaopen_bit => open_loaded("bit");
    /// `luaopen_jit`.
    luaopen_jit => open_loaded("jit");
    /// `luaopen_ffi`: returns the ffi module (not registered as a global).
    luaopen_ffi => open_module("ffi");
    /// `luaopen_string_buffer`: returns the string.buffer module.
    luaopen_string_buffer => open_module("string.buffer");
}

/// `luaL_openlibs`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn luaL_openlibs(L: *mut lua_State) {
    // SAFETY: `L` is a live state.
    unsafe { api(L, |vm, _| crate::stdlib::open_libs(vm)) }
}

const LUAJIT_MODE_MASK: c_int = 0x00ff;
const LUAJIT_MODE_ENGINE: c_int = 0;
const LUAJIT_MODE_FUNC: c_int = 2;
const LUAJIT_MODE_ALLFUNC: c_int = 3;
const LUAJIT_MODE_ALLSUBFUNC: c_int = 4;
const LUAJIT_MODE_WRAPCFUNC: c_int = 0x10;
const LUAJIT_MODE_ON: c_int = 0x0100;

/// `luaJIT_setmode` of a build without the JIT compiler: turning the engine or function JIT
/// modes on fails, turning them off succeeds; `LUAJIT_MODE_WRAPCFUNC` installs/removes the C
/// function call wrapper (a light userdata at `idx`).
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn luaJIT_setmode(L: *mut lua_State, idx: c_int, mode: c_int) -> c_int {
    // SAFETY: `L` is a live state.
    unsafe {
        api(L, |vm, th| {
            if vm.rt.in_finalizers {
                return Err(vm.err_caller(ErrMsg::NOGCMM));
            }
            Ok(match mode & LUAJIT_MODE_MASK {
                LUAJIT_MODE_ENGINE | LUAJIT_MODE_FUNC | LUAJIT_MODE_ALLFUNC | LUAJIT_MODE_ALLSUBFUNC => {
                    (mode & LUAJIT_MODE_ON == 0) as c_int
                }
                LUAJIT_MODE_WRAPCFUNC => {
                    if mode & LUAJIT_MODE_ON != 0 {
                        if idx == 0 {
                            return Ok(0);
                        }
                        let base = base_of(vm, th);
                        let slot = if idx > 0 { base + idx as usize - 1 } else { (th.top() as i64 + idx as i64) as usize };
                        let Value::LightUd(p) = th.get(slot) else { return Ok(0) };
                        if p == 0 {
                            return Ok(0);
                        }
                        // SAFETY: the user passes a `lua_CFunction` wrapper as light userdata.
                        vm.rt.capi.wrapf = Some(std::mem::transmute::<usize, WrapFn>(p));
                        vm.rt.capi.wrap_on = true;
                    } else {
                        vm.rt.capi.wrap_on = false;
                    }
                    1
                }
                _ => 0,
            })
        })
    }
}

/// Version symbol (`LUAJIT_VERSION_SYM`): referenced by programs to enforce a link-time
/// version check.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn luaJIT_version_2_1_1788856981() {}

/// `luaJIT_profile_callback`.
pub type luaJIT_profile_callback = unsafe extern "C-unwind" fn(*mut c_void, *mut lua_State, c_int, c_int);

/// `luaJIT_profile_start` (lj_profile.c): sample with the given mode (`i<ms>` interval) and
/// call `cb(data, L, samples, vmstate)` from the interpreter, with `L` the profiled thread.
/// A Lua error raised by the callback is discarded (the callback runs with hooks disabled, at a
/// point where the profiled code cannot be unwound).
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn luaJIT_profile_start(L: *mut lua_State, mode: *const c_char, cb: luaJIT_profile_callback, data: *mut c_void) {
    // SAFETY: `L` is a live state; `mode` is a C string.
    unsafe {
        let mode = cstr(mode);
        api(L, |vm, _| {
            let cb: crate::stdlib::jit::ProfileCallback = Box::new(move |vm: &mut Vm, samples: i32, vmstate: u8| {
                let th = vm.cur_thread;
                let L = state_of(vm, th);
                let _ = protect(vm, || cb(data, L, samples, vmstate as c_int));
            });
            crate::stdlib::jit::profile_start(vm, mode, cb);
            Ok(())
        })
    }
}

/// `luaJIT_profile_stop`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn luaJIT_profile_stop(L: *mut lua_State) {
    // SAFETY: `L` is a live state.
    unsafe {
        api(L, |vm, _| {
            crate::stdlib::jit::profile_stop(vm);
            Ok(())
        })
    }
}

/// `luaJIT_profile_dumpstack`: compact stack dump of `L` (`lj_debug_dumpstack`). The result
/// stays valid until the next call.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn luaJIT_profile_dumpstack(L: *mut lua_State, fmt: *const c_char, depth: c_int, len: *mut usize) -> *const c_char {
    // SAFETY: `L` is a live state; `fmt` is a C string; `len` is valid.
    unsafe {
        let fmt = cstr(fmt);
        api(L, |vm, th| {
            let s = crate::stdlib::jit::profile_dumpstack(vm, th, fmt, depth);
            vm.rt.capi.dumpbuf = s;
            *len = vm.rt.capi.dumpbuf.len();
            Ok(vm.rt.capi.dumpbuf.as_ptr().cast())
        })
    }
}
