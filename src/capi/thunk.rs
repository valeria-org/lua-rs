//! C entry points for Rust natives, so that `lua_tocfunction` can return a `lua_CFunction` for
//! every non-Lua function, as upstream does for its C-implemented library functions. A thunk
//! is a small x86-64 stub that passes the native's address to [`native_adapter`], which runs
//! the native on `L`'s current frame like a direct call of the C function would. Thunks are
//! created once per native, each in its own page (never written again once executable, so
//! other threads may run existing thunks while new ones are made), owned by a process-wide
//! table.

use std::collections::HashMap;
use std::ffi::c_int;
use std::sync::Mutex;

use crate::error::ErrorKind;
use crate::func::{CFunction, NativeFn};
use crate::value::Value;
use crate::vm::{ErrMsg, TAILCALL};

use super::{api, base_of, lua_State};

/// Bytes of thunk code.
const THUNK_SIZE: usize = 22;
const PAGE_SIZE: usize = 4096;

/// Native address → thunk address.
static THUNKS: Mutex<Option<HashMap<usize, usize>>> = Mutex::new(None);

/// Run the Rust native `f` with `L`'s current frame as its arguments (the thunk target).
unsafe extern "C-unwind" fn native_adapter(L: *mut lua_State, f: usize) -> c_int {
    // SAFETY: the thunk passes the address of a `NativeFn`.
    let f = unsafe { std::mem::transmute::<usize, NativeFn>(f) };
    // SAFETY: `L` is a live state (the thunk is called like a C function).
    unsafe {
        api(L, |vm, th| {
            let base = base_of(vm, th);
            let fslot = base.checked_sub(2);
            let saved = fslot.map(|s| th.get(s));
            match f(vm) {
                Ok(TAILCALL) => {
                    // The native stored a callable to tail-call with the current arguments in
                    // the frame's function slot: call it here and return all its results.
                    let s = fslot.expect("tail call needs a frame");
                    let callee = th.get(s);
                    th.set(s, saved.unwrap_or(Value::Nil));
                    let top = th.top();
                    vm.check_stack(top - base + 1, None)?;
                    vm.push(callee);
                    for i in base..top {
                        vm.push(th.get(i));
                    }
                    Ok(vm.call(top - base, -1)? as c_int)
                }
                Ok(n) => Ok(n as c_int),
                Err(e) if e.kind == ErrorKind::Yield => Err(vm.err_msg(ErrMsg::CYIELD)),
                Err(e) => Err(e),
            }
        })
    }
}

/// The C entry point of native `f`.
pub(crate) fn thunk_of(f: NativeFn) -> CFunction {
    let key = f as *const () as usize;
    let mut t = THUNKS.lock().unwrap_or_else(|e| e.into_inner());
    let map = t.get_or_insert_with(HashMap::new);
    if let Some(&p) = map.get(&key) {
        // SAFETY: `p` is a thunk with the C function ABI.
        return unsafe { std::mem::transmute::<usize, CFunction>(p) };
    }
    let adapter = native_adapter as *const () as usize;
    let mut c = Vec::with_capacity(THUNK_SIZE);
    c.extend_from_slice(&[0x48, 0xbe]); // mov rsi, imm64
    c.extend_from_slice(&key.to_le_bytes());
    c.extend_from_slice(&[0x48, 0xb8]); // mov rax, imm64
    c.extend_from_slice(&adapter.to_le_bytes());
    c.extend_from_slice(&[0xff, 0xe0]); // jmp rax
    // SAFETY: a private anonymous page, written before it becomes executable.
    unsafe {
        let p = libc::mmap(
            std::ptr::null_mut(),
            PAGE_SIZE,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
            -1,
            0,
        );
        assert!(p != libc::MAP_FAILED, "cannot allocate thunk memory");
        std::ptr::copy_nonoverlapping(c.as_ptr(), p as *mut u8, c.len());
        let r = libc::mprotect(p, PAGE_SIZE, libc::PROT_READ | libc::PROT_EXEC);
        assert!(r == 0, "cannot make thunk memory executable");
        map.insert(key, p as usize);
        std::mem::transmute::<*mut libc::c_void, CFunction>(p)
    }
}
