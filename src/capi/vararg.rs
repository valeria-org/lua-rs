//! Variadic C API functions (`lua_pushfstring`, `luaL_error`) and `va_list` access, for the
//! x86-64 SysV ABI. Rust has no stable C-variadic function definitions, so the variadic entry
//! points are naked functions that spill the argument registers into a register save area and
//! build a `va_list` exactly like a C compiler's prologue, then call the `va_list` variants.
//! The formatter is `lj_strfmt_pushvf` (lj_strfmt.c).

use std::ffi::{c_char, c_int};

use crate::text::numfmt::put_fnum;
use crate::text::strfmt::{FmtItem, FormatParser, FormatType, SFormat, put_fxint, put_ptr};

use super::{api, cstr, lua_State, strdata};

/// The SysV `__va_list_tag`; a C `va_list` argument is a pointer to it.
#[repr(C)]
pub struct VaListTag {
    gp_offset: u32,
    fp_offset: u32,
    overflow_arg_area: *mut u8,
    reg_save_area: *mut u8,
}

impl VaListTag {
    /// `va_arg` for an integer-class argument (int, pointer, size_t, ...), as 64 raw bits.
    ///
    /// # Safety
    /// The `va_list` must be live and have such an argument next.
    unsafe fn gp(&mut self) -> u64 {
        // SAFETY: per the contract; the layout follows the SysV ABI.
        unsafe {
            if self.gp_offset < 48 {
                let v = (self.reg_save_area.add(self.gp_offset as usize) as *const u64).read();
                self.gp_offset += 8;
                v
            } else {
                let v = (self.overflow_arg_area as *const u64).read();
                self.overflow_arg_area = self.overflow_arg_area.add(8);
                v
            }
        }
    }

    /// `va_arg(ap, double)`.
    ///
    /// # Safety
    /// The `va_list` must be live and have a double argument next.
    unsafe fn fp(&mut self) -> f64 {
        // SAFETY: per the contract; the layout follows the SysV ABI.
        unsafe {
            if self.fp_offset < 176 {
                let v = (self.reg_save_area.add(self.fp_offset as usize) as *const f64).read();
                self.fp_offset += 16;
                v
            } else {
                let v = (self.overflow_arg_area as *const f64).read();
                self.overflow_arg_area = self.overflow_arg_area.add(8);
                v
            }
        }
    }
}

/// `lj_strfmt_pushvf` minus the push: format `fmt` with the `va_list` arguments.
///
/// # Safety
/// `fmt` must be a valid C string and `ap` a live `va_list` matching it.
unsafe fn format_va(fmt: &[u8], ap: &mut VaListTag) -> Vec<u8> {
    let mut out = Vec::new();
    for item in FormatParser::new(fmt) {
        match item {
            FmtItem::Lit(s) => out.extend_from_slice(s),
            FmtItem::Err(_) => out.push(b'?'),
            // SAFETY (all arms): reading the argument the conversion consumes, per the contract.
            FmtItem::Spec(sf) => match sf.ty() {
                FormatType::Int => {
                    let k = unsafe { ap.gp() } as u32 as i32;
                    put_fxint(&mut out, sf, k as i64 as u64);
                }
                FormatType::Uint => {
                    let k = unsafe { ap.gp() } as u32;
                    put_fxint(&mut out, sf, k as u64);
                }
                FormatType::Num => put_fnum(&mut out, SFormat::G14, unsafe { ap.fp() }),
                FormatType::Str => {
                    let p = unsafe { ap.gp() } as usize as *const c_char;
                    if p.is_null() {
                        out.extend_from_slice(b"(null)");
                    } else {
                        out.extend_from_slice(unsafe { cstr(p) });
                    }
                }
                FormatType::Char => out.push(unsafe { ap.gp() } as u8),
                FormatType::Ptr => put_ptr(&mut out, unsafe { ap.gp() } as usize),
                FormatType::Eof | FormatType::Err | FormatType::Lit => out.push(b'?'),
            },
        }
    }
    out
}

/// `lua_pushvfstring`.
///
/// # Safety
/// `L` must be a live state, `fmt` a C string and `argp` a live `va_list` matching `fmt`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lua_pushvfstring(
    L: *mut lua_State,
    fmt: *const c_char,
    argp: *mut VaListTag,
) -> *const c_char {
    // SAFETY: per the contract.
    let s = unsafe { format_va(cstr(fmt), &mut *argp) };
    // SAFETY: per the contract.
    unsafe {
        api(L, |vm, _| {
            vm.gc_safe();
            let s = vm.new_str(&s);
            vm.push(crate::value::Value::Str(s));
            Ok(strdata(s))
        })
    }
}

/// Second half of `luaL_error`: push the formatted message with the caller's position
/// (`lj_err_callermsg`); the naked entry then tail-calls `lua_error`.
///
/// # Safety
/// As [`lua_pushvfstring`].
unsafe extern "C-unwind" fn lual_error_push(
    L: *mut lua_State,
    fmt: *const c_char,
    argp: *mut VaListTag,
) {
    // SAFETY: per the contract.
    let s = unsafe { format_va(cstr(fmt), &mut *argp) };
    // SAFETY: per the contract.
    unsafe {
        api(L, |vm, _| {
            let e = vm.rt_error(&s);
            vm.push(e.value);
            Ok(())
        })
    }
}

/// Spill the argument registers and build a `va_list` (2 fixed integer arguments) at
/// `[rsp+176]`; `rdx` points to it. Frame: rbp-based, 208 bytes.
macro_rules! va_prologue {
    () => {
        concat!(
            "push rbp\n",
            "mov rbp, rsp\n",
            "sub rsp, 208\n",
            "mov [rsp], rdi\n",
            "mov [rsp+8], rsi\n",
            "mov [rsp+16], rdx\n",
            "mov [rsp+24], rcx\n",
            "mov [rsp+32], r8\n",
            "mov [rsp+40], r9\n",
            "test al, al\n",
            "je 2f\n",
            "movaps xmmword ptr [rsp+48], xmm0\n",
            "movaps xmmword ptr [rsp+64], xmm1\n",
            "movaps xmmword ptr [rsp+80], xmm2\n",
            "movaps xmmword ptr [rsp+96], xmm3\n",
            "movaps xmmword ptr [rsp+112], xmm4\n",
            "movaps xmmword ptr [rsp+128], xmm5\n",
            "movaps xmmword ptr [rsp+144], xmm6\n",
            "movaps xmmword ptr [rsp+160], xmm7\n",
            "2:\n",
            "mov dword ptr [rsp+176], 16\n",
            "mov dword ptr [rsp+180], 48\n",
            "lea rax, [rbp+16]\n",
            "mov [rsp+184], rax\n",
            "mov [rsp+192], rsp\n",
            "lea rdx, [rsp+176]\n",
        )
    };
}

/// `lua_pushfstring(L, fmt, ...)`.
///
/// # Safety
/// `L` must be a live state and the variadic arguments must match `fmt`.
#[unsafe(no_mangle)]
#[unsafe(naked)]
pub unsafe extern "C" fn lua_pushfstring(L: *mut lua_State, fmt: *const c_char) -> *const c_char {
    core::arch::naked_asm!(
        va_prologue!(),
        "call {f}",
        "leave",
        "ret",
        f = sym lua_pushvfstring,
    )
}

/// `luaL_error(L, fmt, ...)`: raise the formatted message with the position of the calling
/// Lua code. The error unwinds from `lua_error`, which is reached by a tail jump so that no
/// frame of this function is on the stack.
///
/// # Safety
/// `L` must be a live state and the variadic arguments must match `fmt`.
#[unsafe(no_mangle)]
#[unsafe(naked)]
pub unsafe extern "C" fn luaL_error(L: *mut lua_State, fmt: *const c_char) -> c_int {
    core::arch::naked_asm!(
        va_prologue!(),
        "call {f}",
        "mov rdi, [rsp]",
        "leave",
        "jmp {err}",
        f = sym lual_error_push,
        err = sym super::lapi::lua_error,
    )
}
