//! Standard libraries. Registration helpers follow lj_lib.c (`lj_lib_register`) and
//! lib_init.c (`luaL_openlibs`).

pub mod base;
pub mod coroutine;
pub mod debug;
pub mod jit;
pub mod package;

use crate::error::LuaResult;
use crate::func::{NativeFn, NativeImpl};
use crate::gc::Gc;
use crate::state::Vm;
use crate::table::Table;
use crate::value::Value;

/// One library function: name, implementation, builtin id (`vm::ffid::FF_*`, or `FF_C`).
pub type LibFn = (&'static str, NativeFn, u8);

/// `luaL_openlibs`: open every standard library. Library modules add their `open_*` call
/// here.
pub fn open_libs(vm: &mut Vm) -> LuaResult<()> {
    base::open_base(vm)?;
    package::open_package(vm)?;
    debug::open_debug(vm)?;
    jit::open_jit(vm)?;
    Ok(())
}

/// `registry._LOADED` (created on demand).
pub fn loaded_table(vm: &mut Vm) -> Gc<Table> {
    let key = vm.new_str(b"_LOADED");
    match vm.registry.get_str(key) {
        Value::Table(t) => t,
        _ => {
            let t = vm.new_table(0, 4).expect("small table");
            let _ = vm.registry.set_str(key, Value::Table(t));
            t
        }
    }
}

/// Create a native function for a library (environment = the globals table, as natives
/// registered by `luaL_openlibs` get).
pub fn new_lib_fn(vm: &mut Vm, f: NativeFn, ffid: u8, upvals: Vec<Value>) -> Gc<crate::func::Function> {
    let env = vm.globals;
    vm.new_native(NativeImpl::Rust(f), ffid, upvals, env)
}

/// `lj_lib_register` / `luaL_register`: set `funcs` into library table `name` (the globals
/// table for `"_G"`), creating it if needed, store it as global `name` and in
/// `package.loaded[name]`. Returns the table.
pub fn register_lib(vm: &mut Vm, name: &str, funcs: &[LibFn]) -> LuaResult<Gc<Table>> {
    let loaded = loaded_table(vm);
    let key = vm.new_str(name.as_bytes());
    let t = if name == "_G" {
        vm.globals
    } else {
        match loaded.get_str(key) {
            Value::Table(t) => t,
            _ => {
                let t = vm.new_table(0, crate::table::hsize2hbits(funcs.len() as u32))?;
                let _ = vm.globals.set_str(key, Value::Table(t));
                t
            }
        }
    };
    let _ = loaded.set_str(key, Value::Table(t));
    for &(fname, f, ffid) in funcs {
        let func = new_lib_fn(vm, f, ffid, Vec::new());
        let k = vm.new_str(fname.as_bytes());
        let _ = t.set_str(k, Value::Func(func));
    }
    Ok(t)
}

unsafe extern "C" {
    #[link_name = "stdout"]
    static mut C_STDOUT: *mut libc::FILE;
}

/// The C library's `stdout` stream. `print` writes through it so its output interleaves
/// correctly with `io.write` (which uses the same `FILE*`, like upstream).
pub fn c_stdout() -> *mut libc::FILE {
    // SAFETY: reading the glibc `stdout` pointer, initialized before main.
    unsafe { C_STDOUT }
}

/// `fwrite(s, 1, len, stdout)`.
pub fn write_stdout(s: &[u8]) {
    if !s.is_empty() {
        // SAFETY: `s` is a valid buffer; `stdout` is a valid stream.
        unsafe { libc::fwrite(s.as_ptr() as *const libc::c_void, 1, s.len(), c_stdout()) };
    }
}
