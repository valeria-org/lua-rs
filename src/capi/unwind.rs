//! Unwinding Lua errors through C frames (x86-64, DWARF unwinder of libgcc_s), modelled on
//! lj_err.c's `_Unwind_RaiseException`-based error handling.
//!
//! A C API error is raised as an exception of a private class with `_Unwind_RaiseException`.
//! It is caught by [`pcall_c`], an assembly trampoline whose frame has its own personality
//! routine: it lands Lua errors as well as foreign exceptions, and lets Rust panics and forced
//! unwinds pass. Since the exception object is not a Rust panic, C++ code may catch it with
//! `catch (...)` and end the catch normally, as it may with upstream LuaJIT.
//!
//! A foreign exception (a C++ exception escaping a C++ module) becomes the Lua error
//! `C++ exception` while it unwinds Lua frames, but the exception object is kept: if no Lua
//! catch point (`pcall`, coroutine resume, `lua_pcall`, ...) consumes the error before it
//! reaches C code again, it is raised again unchanged ([`reraise`]), so C++ code calling
//! `lua_call` can catch its own exception, like with upstream's `err_unwind` in the DWARF
//! personality of the VM frames.

use std::ffi::{c_int, c_void};

use crate::error::LuaError;

/// `struct _Unwind_Exception` (libgcc, 16-byte aligned).
#[repr(C, align(16))]
pub(crate) struct UnwindException {
    class: u64,
    cleanup: Option<unsafe extern "C" fn(c_int, *mut UnwindException)>,
    private_1: usize,
    private_2: usize,
}

/// Exception object of a Lua error.
#[repr(C)]
struct LuaException {
    hdr: UnwindException,
    err: LuaError,
}

/// Exception class of Lua errors raised by the C API.
const LUA_CLASS: u64 = u64::from_be_bytes(*b"LJRS\0LUA");
/// Exception class of Rust panics (std's `RUST_EXCEPTION_CLASS`).
const RUST_CLASS: u64 = u64::from_be_bytes(*b"MOZ\0RUST");

const UA_SEARCH_PHASE: c_int = 1;
const UA_FORCE_UNWIND: c_int = 8;
const URC_FATAL_PHASE1_ERROR: c_int = 3;
const URC_HANDLER_FOUND: c_int = 6;
const URC_INSTALL_CONTEXT: c_int = 7;
const URC_CONTINUE_UNWIND: c_int = 8;

unsafe extern "C-unwind" {
    fn _Unwind_RaiseException(e: *mut UnwindException) -> c_int;
}

unsafe extern "C" {
    fn _Unwind_DeleteException(e: *mut UnwindException);
    fn _Unwind_GetIP(ctx: *mut c_void) -> usize;
    fn _Unwind_GetRegionStart(ctx: *mut c_void) -> usize;
    fn _Unwind_GetLanguageSpecificData(ctx: *mut c_void) -> *const u32;
    fn _Unwind_SetGR(ctx: *mut c_void, reg: c_int, v: usize);
    fn _Unwind_SetIP(ctx: *mut c_void, ip: usize);
    /// `pcall_c(f, arg)`: call `f(arg)`; returns NULL, or the caught exception object.
    fn ljrs_capi_pcall(f: unsafe extern "C-unwind" fn(*mut c_void), arg: *mut c_void) -> *mut UnwindException;
}

// The trampoline. Its LSDA (read only by `personality`) holds the call range and the landing
// pad as offsets from the function start.
core::arch::global_asm!(
    ".text",
    ".p2align 4",
    ".globl ljrs_capi_pcall",
    ".hidden ljrs_capi_pcall",
    ".type ljrs_capi_pcall,@function",
    "ljrs_capi_pcall:",
    ".cfi_startproc",
    ".cfi_personality 0x1b, {pers}",
    ".cfi_lsda 0x1b, .Lljrs_capi_lsda",
    "push rbp",
    ".cfi_def_cfa_offset 16",
    ".cfi_offset rbp, -16",
    "mov rbp, rsp",
    ".cfi_def_cfa_register rbp",
    "mov rax, rdi",
    "mov rdi, rsi",
    ".Lljrs_capi_cb:",
    "call rax",
    ".Lljrs_capi_ce:",
    "xor eax, eax",
    ".Lljrs_capi_land:",
    "leave",
    ".cfi_def_cfa rsp, 8",
    "ret",
    ".cfi_endproc",
    ".size ljrs_capi_pcall, .-ljrs_capi_pcall",
    ".section .gcc_except_table,\"a\",@progbits",
    ".p2align 2",
    ".Lljrs_capi_lsda:",
    ".long .Lljrs_capi_cb - ljrs_capi_pcall",
    ".long .Lljrs_capi_ce - ljrs_capi_pcall",
    ".long .Lljrs_capi_land - ljrs_capi_pcall",
    ".text",
    pers = sym personality,
);

/// Personality routine of `ljrs_capi_pcall`.
unsafe extern "C" fn personality(version: c_int, actions: c_int, class: u64, exc: *mut UnwindException, ctx: *mut c_void) -> c_int {
    if version != 1 {
        return URC_FATAL_PHASE1_ERROR;
    }
    if actions & UA_FORCE_UNWIND != 0 || class == RUST_CLASS {
        return URC_CONTINUE_UNWIND;
    }
    // SAFETY: called by the unwinder for the trampoline's frame, whose LSDA has 3 words.
    unsafe {
        let lsda = _Unwind_GetLanguageSpecificData(ctx);
        let start = _Unwind_GetRegionStart(ctx);
        let ip = _Unwind_GetIP(ctx);
        let (b, e, land) = (start + *lsda as usize, start + *lsda.add(1) as usize, start + *lsda.add(2) as usize);
        if !(ip > b && ip <= e) {
            return URC_CONTINUE_UNWIND;
        }
        if actions & UA_SEARCH_PHASE != 0 {
            return URC_HANDLER_FOUND;
        }
        _Unwind_SetGR(ctx, 0, exc as usize);
        _Unwind_SetIP(ctx, land);
    }
    URC_INSTALL_CONTEXT
}

/// Cleanup of a Lua error exception that was caught and destroyed by foreign code.
unsafe extern "C" fn cleanup(_reason: c_int, exc: *mut UnwindException) {
    // SAFETY: allocated by `raise` as a `LuaException`.
    drop(unsafe { Box::from_raw(exc as *mut LuaException) });
}

/// Raise `e` through the C frames. Returns only if no handler was found.
pub(crate) fn raise(e: LuaError) -> LuaError {
    let x = Box::into_raw(Box::new(LuaException {
        hdr: UnwindException { class: LUA_CLASS, cleanup: Some(cleanup), private_1: 0, private_2: 0 },
        err: e,
    }));
    // SAFETY: a valid exception object; ownership passes to the catcher.
    unsafe {
        _Unwind_RaiseException(x as *mut UnwindException);
        Box::from_raw(x).err
    }
}

/// A caught foreign exception object, owned (deleted on drop).
pub(crate) struct Foreign(std::ptr::NonNull<UnwindException>);

impl Foreign {
    /// Raise the exception again through the C frames. Returns it only if no handler was found.
    pub(crate) fn reraise(self) -> Foreign {
        let x = self.0.as_ptr();
        std::mem::forget(self);
        // SAFETY: a landed exception object owned by us; ownership passes to the catcher, as
        // with any raised exception.
        unsafe { _Unwind_RaiseException(x) };
        Foreign(std::ptr::NonNull::new(x).expect("exception object"))
    }
}

impl Drop for Foreign {
    fn drop(&mut self) {
        // SAFETY: a landed exception object owned by us, not referenced elsewhere.
        unsafe { _Unwind_DeleteException(self.0.as_ptr()) };
    }
}

/// What a catch point received.
pub(crate) enum Caught {
    Lua(LuaError),
    /// A foreign (non-Lua, non-Rust) exception.
    Foreign(Foreign),
}

/// Call `f` behind the trampoline: `Ok` with its result, or the caught exception.
pub(crate) fn pcall_c<F: FnOnce() -> R, R>(f: F) -> Result<R, Caught> {
    struct Data<F, R> {
        f: Option<F>,
        r: Option<R>,
    }
    unsafe extern "C-unwind" fn call<F: FnOnce() -> R, R>(d: *mut c_void) {
        // SAFETY: `d` points to the `Data` of the caller below.
        let d = unsafe { &mut *(d as *mut Data<F, R>) };
        let f = d.f.take().expect("called once");
        d.r = Some(f());
    }
    let mut d: Data<F, R> = Data { f: Some(f), r: None };
    // SAFETY: `call` matches `Data<F, R>`; the trampoline returns normally or with an exception.
    let exc = unsafe { ljrs_capi_pcall(call::<F, R>, &mut d as *mut Data<F, R> as *mut c_void) };
    let Some(exc) = std::ptr::NonNull::new(exc) else {
        return Ok(d.r.take().expect("result"));
    };
    // SAFETY: `exc` is the landed exception object, now owned by us.
    unsafe {
        if exc.as_ref().class == LUA_CLASS {
            Err(Caught::Lua(Box::from_raw(exc.as_ptr() as *mut LuaException).err))
        } else {
            Err(Caught::Foreign(Foreign(exc)))
        }
    }
}
