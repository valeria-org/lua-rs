//! C type conversions. Port of lj_cconv.c: C type ↔ C type, Lua value ↔ C type, bitfields
//! and aggregate initializers.
//!
//! All conversions work on raw memory through raw pointers, exactly like upstream: the
//! destination and source pointers must be valid for the sizes implied by the C types.
//! Unaligned access is used throughout (packed structs).

use std::ptr;

use crate::error::{LuaError, LuaResult};
use crate::state::Vm;
use crate::table::Table;
use crate::udata::{UDTYPE_BUFFER, UDTYPE_IO_FILE};
use crate::value::Value;
use crate::vm::{ErrMsg, fmt_msg};

use super::callback::callback_new;
use super::ctype::*;
use super::{FfiError, errmsg};

// -- Conversion flags --------------------------------------------------------------------------

pub const CCF_CAST: CTInfo = 0x0000_0001;
pub const CCF_FROMTV: CTInfo = 0x0000_0002;
pub const CCF_SAME: CTInfo = 0x0000_0004;
pub const CCF_IGNQUAL: CTInfo = 0x0000_0008;
pub const CCF_ARG_SHIFT: u32 = 8;
/// Internal (not upstream): report a conversion error without any position. Used for
/// callback results, whose error position is the end of the callback function.
pub(crate) const CCF_NOLOC: CTInfo = 0x0000_0010;

/// `CCF_ARG(n)`: report conversion errors as errors of argument `n`.
#[inline]
pub const fn ccf_arg(n: u32) -> CTInfo {
    n << CCF_ARG_SHIFT
}

#[inline]
const fn ccf_getarg(f: CTInfo) -> u32 {
    f >> CCF_ARG_SHIFT
}

// Compressed C type index. ORDER CCX.
const CCX_B: u32 = 0;
const CCX_I: u32 = 1;
const CCX_F: u32 = 2;
const CCX_C: u32 = 3;
const CCX_V: u32 = 4;
const CCX_P: u32 = 5;
const CCX_A: u32 = 6;
const CCX_S: u32 = 7;

/// Convert C type info to compressed C type index (`cconv_idx`).
#[inline]
fn cconv_idx(info: CTInfo) -> u32 {
    let idx = (info >> 26) & 15;
    ((0xf436_fff5_fff7_f021u64 >> (4 * idx)) & 15) as u32
}

const fn ccx(d: u32, s: u32) -> u32 {
    (d << 3) + s
}

// -- Raw memory helpers ------------------------------------------------------------------------

/// Read a `T` from possibly unaligned memory.
///
/// # Safety
/// `p` must be valid for reading `size_of::<T>()` bytes.
#[inline]
pub(crate) unsafe fn rd<T: Copy>(p: *const u8) -> T {
    // SAFETY: guaranteed by the caller.
    unsafe { ptr::read_unaligned(p as *const T) }
}

/// Write a `T` to possibly unaligned memory.
///
/// # Safety
/// `p` must be valid for writing `size_of::<T>()` bytes.
#[inline]
pub(crate) unsafe fn wr<T>(p: *mut u8, v: T) {
    // SAFETY: guaranteed by the caller.
    unsafe { ptr::write_unaligned(p as *mut T, v) }
}

/// `cdata_getptr`: read a pointer of size `sz` (4 or 8).
///
/// # Safety
/// `p` must be valid for reading `sz` bytes.
#[inline]
pub(crate) unsafe fn getptr(p: *const u8, sz: CTSize) -> *mut u8 {
    // SAFETY: guaranteed by the caller.
    unsafe { if sz == 4 { rd::<u32>(p) as usize as *mut u8 } else { rd::<*mut u8>(p) } }
}

/// `cdata_setptr`: store a pointer of size `sz` (4 or 8).
///
/// # Safety
/// `p` must be valid for writing `sz` bytes.
#[inline]
pub(crate) unsafe fn setptr(p: *mut u8, sz: CTSize, v: *const u8) {
    // SAFETY: guaranteed by the caller.
    unsafe {
        if sz == 4 {
            wr::<u32>(p, v as usize as u32)
        } else {
            wr::<*const u8>(p, v)
        }
    }
}

// -- Errors ------------------------------------------------------------------------------------

/// Format an FFI error message with `'%s'` arguments.
pub(crate) fn msgf(fmt: &'static str, args: &[&[u8]]) -> Vec<u8> {
    fmt_msg(ErrMsg(fmt), args)
}

/// Raise an FFI core error the way upstream throws it.
pub fn ffi_err(vm: &mut Vm, e: FfiError) -> LuaError {
    match e {
        FfiError::Msg(m) => vm.err_msgv(&m),
        FfiError::Caller(m) => vm.err_callermsg(&m),
        FfiError::ArgType { narg, expected } => vm.type_error(narg, expected),
    }
}

/// Error in the context of the caller or of argument `CCF_GETARG(flags)`.
fn err_ctx(vm: &mut Vm, msg: &[u8], flags: CTInfo) -> LuaError {
    let narg = ccf_getarg(flags);
    if narg != 0 {
        vm.arg_error(narg as i32, msg)
    } else if flags & CCF_NOLOC != 0 {
        vm.error_msg_bytes(msg)
    } else {
        vm.err_callermsg(msg)
    }
}

/// Bad conversion (`cconv_err_conv`).
fn err_conv(vm: &mut Vm, cts: &CTState, d: CTypeID, s: CTypeID, flags: CTInfo) -> LuaError {
    let dst = cts.repr(d, None);
    let src: Vec<u8> = if flags & CCF_FROMTV != 0 {
        let sinfo = cts.get(s).info;
        let t = if ctype_isnum(sinfo) {
            "number"
        } else if ctype_isarray(sinfo) {
            "string"
        } else {
            "nil"
        };
        t.as_bytes().to_vec()
    } else {
        cts.repr(s, None)
    };
    let m = msgf(errmsg::FFI_BADCONV, &[&src, &dst]);
    err_ctx(vm, &m, flags)
}

/// Bad conversion from a Lua value (`cconv_err_convtv`).
fn err_convtv(vm: &mut Vm, cts: &CTState, d: CTypeID, o: Value, flags: CTInfo) -> LuaError {
    let dst = cts.repr(d, None);
    let m = msgf(errmsg::FFI_BADCONV, &[o.type_name().as_bytes(), &dst]);
    err_ctx(vm, &m, flags)
}

/// Initializer overflow (`cconv_err_initov`).
fn err_initov(vm: &mut Vm, cts: &CTState, d: CTypeID) -> LuaError {
    let dst = cts.repr(d, None);
    let m = msgf(errmsg::FFI_INITOV, &[&dst]);
    vm.err_callermsg(&m)
}

// -- C type compatibility checks ---------------------------------------------------------------

/// Raw child type and qualifiers (`cconv_childqual`). Resolves enums, too.
fn childqual(cts: &CTState, id: CTypeID, qual: &mut CTInfo) -> CTypeID {
    let mut id = cts.child(id);
    loop {
        let ct = cts.get(id);
        if ctype_isattrib(ct.info) {
            if ctype_attrib(ct.info) == CTA_QUAL {
                *qual |= ct.size;
            }
        } else if !ctype_isenum(ct.info) {
            break;
        }
        id = cts.child(id);
    }
    *qual |= cts.get(id).info & CTF_QUAL;
    id
}

/// Check for compatible types when converting to a pointer (`lj_cconv_compatptr`).
pub fn compatptr(cts: &CTState, d: CTypeID, s: CTypeID, flags: CTInfo) -> bool {
    if flags & CCF_CAST != 0 || d == s {
        return true;
    }
    let (mut dqual, mut squal) = (0, 0);
    let d = childqual(cts, d, &mut dqual);
    let s = if !ctype_isstruct(cts.get(s).info) { childqual(cts, s, &mut squal) } else { s };
    let (dct, sct) = (*cts.get(d), *cts.get(s));
    if flags & CCF_SAME != 0 {
        if dqual != squal {
            return false; // Different qualifiers.
        }
    } else if flags & CCF_IGNQUAL == 0 {
        if (dqual & squal) != squal {
            return false; // Discarded qualifiers.
        }
        if ctype_isvoid(dct.info) || ctype_isvoid(sct.info) {
            return true; // Converting to/from void * is always ok.
        }
    }
    if ctype_type(dct.info) != ctype_type(sct.info) || dct.size != sct.size {
        return false; // Different type or different size.
    }
    if ctype_isnum(dct.info) {
        if (dct.info ^ sct.info) & (CTF_BOOL | CTF_FP) != 0 {
            return false; // Different numeric types.
        }
    } else if ctype_ispointer(dct.info) {
        // Check child types for compatibility.
        return compatptr(cts, d, s, flags | CCF_SAME);
    } else if ctype_isstruct(dct.info) {
        if d != s {
            return false; // Must be exact same type for struct/union.
        }
    }
    // NYI: structural equality of functions.
    true
}

// -- C type to C type conversion ---------------------------------------------------------------

/// Convert C type to C type (`lj_cconv_ct_ct`). Caveat: expects raw C types.
///
/// `dp`/`sp` must be valid for the sizes of `d`/`s`.
pub fn ct_ct(
    vm: &mut Vm,
    cts: &CTState,
    d: CTypeID,
    s: CTypeID,
    dp: *mut u8,
    sp: *const u8,
    flags: CTInfo,
) -> LuaResult<()> {
    match ct_ct_raw(cts, d, s, dp, sp, flags) {
        Ok(()) => Ok(()),
        Err((d, s)) => Err(err_conv(vm, cts, d, s, flags)),
    }
}

#[derive(Clone, Copy)]
enum Step {
    Done,
    IntInt,
    IntFp,
    FpInt,
    FpFp,
    Copy,
}

/// [`ct_ct`] without the error report: `Err((d, s))` is a bad conversion between the types
/// reported in the error message.
pub(crate) fn ct_ct_raw(
    cts: &CTState,
    mut d: CTypeID,
    mut s: CTypeID,
    dp: *mut u8,
    mut sp: *const u8,
    flags: CTInfo,
) -> Result<(), (CTypeID, CTypeID)> {
    let (dct, sct) = (*cts.get(d), *cts.get(s));
    let (mut dsize, mut ssize, mut dinfo, mut sinfo) = (dct.size, sct.size, dct.info, sct.info);
    let tmpptr: usize;

    debug_assert!(!ctype_isenum(dinfo) && !ctype_isenum(sinfo), "unresolved enum");
    debug_assert!(!ctype_isattrib(dinfo) && !ctype_isattrib(sinfo), "unstripped attribute");

    if ctype_type(dinfo) > CT_MAYCONVERT || ctype_type(sinfo) > CT_MAYCONVERT {
        return Err((d, s));
    }

    let store_bool = |b: u8, dsize: CTSize| {
        // SAFETY: dp is valid for dsize (1 or 4) bytes.
        unsafe {
            if dsize == 1 {
                *dp = b
            } else {
                wr::<i32>(dp, b as i32)
            }
        }
    };

    // SAFETY (whole function): the caller guarantees dp/sp are valid for the type sizes.
    let step = match ccx(cconv_idx(dinfo), cconv_idx(sinfo)) {
        // Destination is a bool.
        x if x == ccx(CCX_B, CCX_B) => {
            // Source operand is already normalized.
            store_bool(unsafe { *sp }, dsize);
            Step::Done
        }
        x if x == ccx(CCX_B, CCX_I) => {
            let mut b = 0u8;
            for i in 0..ssize as usize {
                b |= unsafe { *sp.add(i) };
            }
            store_bool((b != 0) as u8, dsize);
            Step::Done
        }
        x if x == ccx(CCX_B, CCX_F) => {
            let b = if ssize == 8 {
                unsafe { rd::<f64>(sp) != 0.0 }
            } else if ssize == 4 {
                unsafe { rd::<f32>(sp) != 0.0 }
            } else {
                return Err((d, s)); // NYI: long double.
            };
            store_bool(b as u8, dsize);
            Step::Done
        }

        // Destination is an integer.
        x if x == ccx(CCX_I, CCX_B) || x == ccx(CCX_I, CCX_I) => Step::IntInt,
        x if x == ccx(CCX_I, CCX_F) => Step::IntFp,
        x if x == ccx(CCX_I, CCX_C) => {
            s = cts.child(s);
            sinfo = cts.get(s).info;
            ssize = cts.get(s).size;
            Step::IntFp // Just convert re.
        }
        x if x == ccx(CCX_I, CCX_P) => {
            if flags & CCF_CAST == 0 {
                return Err((d, s));
            }
            sinfo = ctinfo(CT_NUM, CTF_UNSIGNED);
            Step::IntInt
        }
        x if x == ccx(CCX_I, CCX_A) => {
            if flags & CCF_CAST == 0 {
                return Err((d, s));
            }
            sinfo = ctinfo(CT_NUM, CTF_UNSIGNED);
            ssize = CTSIZE_PTR;
            tmpptr = sp as usize;
            sp = ptr::from_ref(&tmpptr).cast();
            Step::IntInt
        }

        // Destination is a floating-point number.
        x if x == ccx(CCX_F, CCX_B) || x == ccx(CCX_F, CCX_I) => Step::FpInt,
        x if x == ccx(CCX_F, CCX_F) => Step::FpFp,
        x if x == ccx(CCX_F, CCX_C) => {
            s = cts.child(s);
            sinfo = cts.get(s).info;
            ssize = cts.get(s).size;
            Step::FpFp // Ignore im, and convert from re.
        }

        // Destination is a complex number.
        x if x == ccx(CCX_C, CCX_I) => {
            d = cts.child(d);
            dinfo = cts.get(d).info;
            dsize = cts.get(d).size;
            unsafe { ptr::write_bytes(dp.add(dsize as usize), 0, dsize as usize) }; // Clear im.
            Step::FpInt // Convert to re.
        }
        x if x == ccx(CCX_C, CCX_F) => {
            d = cts.child(d);
            dinfo = cts.get(d).info;
            dsize = cts.get(d).size;
            unsafe { ptr::write_bytes(dp.add(dsize as usize), 0, dsize as usize) }; // Clear im.
            Step::FpFp
        }
        x if x == ccx(CCX_C, CCX_C) => {
            if dsize != ssize {
                // Different types: convert re/im separately.
                let dc = cts.child(d);
                let sc = cts.child(s);
                ct_ct_raw(cts, dc, sc, dp, sp, flags)?;
                let (dcs, scs) = (cts.get(dc).size as usize, cts.get(sc).size as usize);
                return ct_ct_raw(cts, dc, sc, unsafe { dp.add(dcs) }, unsafe { sp.add(scs) }, flags);
            }
            Step::Copy // Otherwise this is easy.
        }

        // Destination is a vector.
        x if x == ccx(CCX_V, CCX_I) || x == ccx(CCX_V, CCX_F) || x == ccx(CCX_V, CCX_C) => {
            let dc = cts.child(d);
            let esize = cts.get(dc).size;
            // First convert the scalar to the first element.
            ct_ct_raw(cts, dc, s, dp, sp, flags)?;
            // Then replicate it to the other elements (splat).
            let mut ofs = esize;
            while ofs < dsize {
                unsafe { ptr::copy_nonoverlapping(dp, dp.add(ofs as usize), esize as usize) };
                ofs += esize;
            }
            Step::Done
        }
        x if x == ccx(CCX_V, CCX_V) => {
            // Copy same-sized vectors, even for different lengths/element-types.
            if dsize != ssize {
                return Err((d, s));
            }
            Step::Copy
        }

        // Destination is a pointer.
        x if x == ccx(CCX_P, CCX_I) => {
            if flags & CCF_CAST == 0 {
                return Err((d, s));
            }
            dinfo = ctinfo(CT_NUM, CTF_UNSIGNED);
            Step::IntInt
        }
        x if x == ccx(CCX_P, CCX_F) => {
            if flags & CCF_CAST == 0 || flags & CCF_FROMTV == 0 {
                return Err((d, s));
            }
            // The signed conversion is cheaper. x64 really has 47 bit pointers.
            dinfo = ctinfo(CT_NUM, if dsize == 8 { 0 } else { CTF_UNSIGNED });
            Step::IntFp
        }
        x if x == ccx(CCX_P, CCX_P) => {
            if !compatptr(cts, d, s, flags) {
                return Err((d, s));
            }
            unsafe { setptr(dp, dsize, getptr(sp, ssize)) };
            Step::Done
        }
        x if x == ccx(CCX_P, CCX_A) || x == ccx(CCX_P, CCX_S) => {
            if !compatptr(cts, d, s, flags) {
                return Err((d, s));
            }
            unsafe { setptr(dp, dsize, sp) };
            Step::Done
        }

        // Destination is an array.
        x if x == ccx(CCX_A, CCX_A) => {
            if flags & CCF_CAST != 0
                || dct.info & CTF_VLA != 0
                || dsize != ssize
                || dct.size == CTSIZE_INVALID
                || !compatptr(cts, d, s, flags)
            {
                return Err((d, s));
            }
            Step::Copy
        }

        // Destination is a struct/union.
        x if x == ccx(CCX_S, CCX_S) => {
            if flags & CCF_CAST != 0 || dct.info & CTF_VLA != 0 || d != s {
                return Err((d, s)); // Must be exact same type.
            }
            Step::Copy
        }

        _ => return Err((d, s)),
    };

    match step {
        Step::Done => {}
        Step::Copy => {
            debug_assert_eq!(dsize, ssize, "value copy with different sizes");
            unsafe { ptr::copy(sp, dp, dsize as usize) };
        }
        Step::IntInt => conv_i_i(dp, dsize, sp, ssize, sinfo),
        Step::IntFp => {
            // Convert source to double.
            let n = if ssize == 8 {
                unsafe { rd::<f64>(sp) }
            } else if ssize == 4 {
                unsafe { rd::<f32>(sp) as f64 }
            } else {
                return Err((d, s)); // NYI: long double.
            };
            // Then convert double to integer.
            if dsize < 8 {
                let i = crate::text::num2i64(n); // Always convert via int64_t.
                unsafe {
                    match dsize {
                        4 => wr::<i32>(dp, i as i32),
                        2 => wr::<i16>(dp, i as i16),
                        _ => wr::<i8>(dp, i as i8),
                    }
                }
            } else if dsize == 8 {
                unsafe {
                    if dinfo & CTF_UNSIGNED != 0 {
                        wr::<u64>(dp, crate::text::num2u64(n))
                    } else {
                        wr::<i64>(dp, crate::text::num2i64(n))
                    }
                }
            } else {
                return Err((d, s)); // NYI: conversion to >64 bit integers.
            }
        }
        Step::FpInt => {
            // First convert source to double.
            let n = if ssize < 4 || (ssize == 4 && sinfo & CTF_UNSIGNED == 0) {
                let i: i32 = unsafe {
                    if ssize == 4 {
                        rd::<i32>(sp)
                    } else if sinfo & CTF_UNSIGNED == 0 {
                        if ssize == 2 { rd::<i16>(sp) as i32 } else { rd::<i8>(sp) as i32 }
                    } else if ssize == 2 {
                        rd::<u16>(sp) as i32
                    } else {
                        rd::<u8>(sp) as i32
                    }
                };
                i as f64
            } else if ssize == 4 {
                unsafe { rd::<u32>(sp) as f64 }
            } else if ssize == 8 {
                unsafe { if sinfo & CTF_UNSIGNED == 0 { rd::<i64>(sp) as f64 } else { rd::<u64>(sp) as f64 } }
            } else {
                return Err((d, s)); // NYI: conversion from >64 bit integers.
            };
            store_fp(dp, dsize, n).map_err(|()| (d, s))?;
        }
        Step::FpFp => {
            if ssize == dsize {
                unsafe { ptr::copy(sp, dp, dsize as usize) };
            } else {
                let n = if ssize == 8 {
                    unsafe { rd::<f64>(sp) }
                } else if ssize == 4 {
                    unsafe { rd::<f32>(sp) as f64 }
                } else {
                    return Err((d, s)); // NYI: long double.
                };
                store_fp(dp, dsize, n).map_err(|()| (d, s))?;
            }
        }
    }
    let _ = dinfo;
    Ok(())
}

/// Store a double as `double`/`float` (`Err` for long double).
fn store_fp(dp: *mut u8, dsize: CTSize, n: f64) -> Result<(), ()> {
    // SAFETY: dp is valid for dsize bytes (caller of ct_ct_raw).
    unsafe {
        match dsize {
            8 => wr::<f64>(dp, n),
            4 => wr::<f32>(dp, n as f32),
            _ => return Err(()), // NYI: long double.
        }
    }
    Ok(())
}

/// Integer to integer: zero-/sign-extend or truncate (little-endian).
fn conv_i_i(dp: *mut u8, dsize: CTSize, sp: *const u8, ssize: CTSize, sinfo: CTInfo) {
    // SAFETY: dp/sp are valid for dsize/ssize bytes (caller of ct_ct_raw).
    unsafe {
        if dsize > ssize {
            let fill = if sinfo & CTF_UNSIGNED == 0 && *sp.add(ssize as usize - 1) & 0x80 != 0 { 0xff } else { 0 };
            ptr::copy(sp, dp, ssize as usize);
            ptr::write_bytes(dp.add(ssize as usize), fill, (dsize - ssize) as usize);
        } else {
            ptr::copy(sp, dp, dsize as usize);
        }
    }
}

// -- C type to Lua value conversion ------------------------------------------------------------

/// Allocate a fixed-size cdata (`lj_cdata_new`).
pub fn cdata_new(vm: &mut Vm, id: CTypeID, sz: CTSize) -> crate::gc::Gc<crate::cdata::CData> {
    vm.new_cdata(id, sz as usize, 8)
}

/// Allocate a cdata holding a reference to `p` of type `id` (`lj_cdata_newref`).
pub fn cdata_newref(vm: &mut Vm, cts: &mut CTState, p: *const u8, id: CTypeID) -> LuaResult<Value> {
    let refid = cts.intern(ctinfo_ref(id), CTSIZE_PTR).map_err(|e| ffi_err(vm, e))?;
    let cd = cdata_new(vm, refid, CTSIZE_PTR);
    // SAFETY: the cdata owns 8 bytes.
    unsafe { wr::<*const u8>(cd.ptr(), p) };
    Ok(Value::CData(cd))
}

/// Convert C type to Lua value (`lj_cconv_tv_ct`). Caveat: expects the raw C type `s`;
/// `sid` is the type id used for references.
pub fn tv_ct(vm: &mut Vm, cts: &mut CTState, s: CTypeID, sid: CTypeID, sp: *const u8) -> LuaResult<Value> {
    let sct = *cts.get(s);
    let sinfo = sct.info;
    if ctype_isnum(sinfo) {
        if !ctype_isbool(sinfo) {
            if !(ctype_isinteger(sinfo) && sct.size > 4) {
                let mut n = 0f64;
                ct_ct(vm, cts, CTID_DOUBLE, s, ptr::from_mut(&mut n).cast(), sp, 0)?;
                // Numbers are NOT canonicalized here! Beware of uninitialized data.
                return Ok(Value::Num(n));
            }
        } else {
            // SAFETY: sp is valid for the bool's size (1 or 4).
            let b = unsafe { if sct.size == 1 { *sp != 0 } else { rd::<i32>(sp) != 0 } };
            return Ok(Value::Bool(b));
        }
    } else if ctype_isrefarray(sinfo) || ctype_isstruct(sinfo) {
        // Create reference.
        return cdata_newref(vm, cts, sp, sid);
    }
    // Copy value.
    let sz = sct.size;
    debug_assert!(sz != CTSIZE_INVALID, "value copy with invalid size");
    // Attributes are stripped, qualifiers are kept (but mostly ignored).
    let cd = cdata_new(vm, s, sz);
    // SAFETY: the cdata owns sz bytes; sp is valid for sz bytes.
    unsafe { ptr::copy_nonoverlapping(sp, cd.ptr(), sz as usize) };
    Ok(Value::CData(cd))
}

/// Convert bitfield to Lua value (`lj_cconv_tv_bf`).
pub fn tv_bf(vm: &mut Vm, cts: &CTState, s: CTypeID, sp: *const u8) -> LuaResult<Value> {
    let info = cts.get(s).info;
    debug_assert!(ctype_isbitfield(info), "bitfield expected");
    let csz = ctype_bitcsz(info);
    // NYI: packed bitfields may cause misaligned reads.
    // SAFETY: sp is valid for the container size.
    let val: u32 = unsafe {
        match csz {
            4 => rd::<u32>(sp),
            2 => rd::<u16>(sp) as u32,
            1 => rd::<u8>(sp) as u32,
            _ => {
                debug_assert!(false, "bad bitfield container size {csz}");
                0
            }
        }
    };
    // Check if a packed bitfield crosses a container boundary.
    let pos = ctype_bitpos(info);
    let bsz = ctype_bitbsz(info);
    if pos + bsz > 8 * csz {
        return Err(vm.err_callermsg(errmsg::FFI_NYIPACKBIT.as_bytes()));
    }
    if info & CTF_BOOL == 0 {
        let shift = 32 - bsz;
        if info & CTF_UNSIGNED == 0 {
            Ok(Value::Num(((val << (shift - pos)) as i32 >> shift) as f64))
        } else {
            Ok(Value::Num(((val << (shift - pos)) >> shift) as f64))
        }
    } else {
        debug_assert!(bsz == 1, "bad bool bitfield size");
        Ok(Value::Bool((val >> pos) & 1 != 0))
    }
}

// -- Lua value to C type conversion ------------------------------------------------------------

/// Field name of a ctype as a Lua string.
fn name_str(vm: &mut Vm, cts: &CTState, id: CTypeID) -> Option<crate::gc::Gc<crate::lstr::LuaStr>> {
    let n = cts.get(id).name;
    if !n.is_some() {
        return None;
    }
    Some(vm.new_str(cts.name_str(n)))
}

/// Convert table to array (`cconv_array_tab`).
fn array_tab(vm: &mut Vm, cts: &mut CTState, d: CTypeID, dp: *mut u8, t: crate::gc::Gc<Table>, flags: CTInfo) -> LuaResult<()> {
    let dc = cts.rawchild(d); // Array element type.
    let (size, esize) = (cts.get(d).size, cts.get(dc).size);
    let mut ofs: CTSize = 0;
    let mut i = 0i32;
    loop {
        let tv = t.get_int(i);
        if tv.is_nil() {
            if i == 0 {
                i += 1;
                continue; // Try again for 1-based tables.
            }
            break; // Stop at first nil.
        }
        if ofs >= size {
            return Err(err_initov(vm, cts, d));
        }
        // SAFETY: ofs < size, the destination holds `size` bytes.
        ct_tv(vm, cts, dc, unsafe { dp.add(ofs as usize) }, tv, flags)?;
        ofs += esize;
        i += 1;
    }
    if size != CTSIZE_INVALID {
        // Only fill up arrays with known size.
        // SAFETY: all offsets stay below `size`.
        unsafe {
            if ofs == esize {
                // Replicate a single element.
                while ofs < size {
                    ptr::copy_nonoverlapping(dp, dp.add(ofs as usize), esize as usize);
                    ofs += esize;
                }
            } else {
                // Otherwise fill the remainder with zero.
                ptr::write_bytes(dp.add(ofs as usize), 0, (size - ofs) as usize);
            }
        }
    }
    Ok(())
}

/// Convert table to sub-struct/union (`cconv_substruct_tab`).
fn substruct_tab(
    vm: &mut Vm,
    cts: &mut CTState,
    d: CTypeID,
    dp: *mut u8,
    t: crate::gc::Gc<Table>,
    ip: &mut i32,
    flags: CTInfo,
) -> LuaResult<()> {
    let dinfo = cts.get(d).info;
    let mut id = cts.get(d).sib as CTypeID;
    while id != 0 {
        let df = *cts.get(id);
        let fid = id;
        id = df.sib as CTypeID;
        if ctype_isfield(df.info) || ctype_isbitfield(df.info) {
            if !df.name.is_some() {
                continue; // Ignore unnamed fields.
            }
            let mut i = *ip;
            let iz = i;
            let tv;
            'get: {
                if i >= 0 {
                    loop {
                        let v = t.get_int(i);
                        if v.is_nil() {
                            if i == 0 {
                                i = 1;
                                continue; // 1-based tables.
                            }
                            if iz == 0 {
                                *ip = -1;
                                break; // Init named fields.
                            }
                            return Ok(()); // Stop at first nil.
                        }
                        *ip = i + 1;
                        tv = v;
                        break 'get;
                    }
                }
                let name = name_str(vm, cts, fid).expect("named field");
                let v = t.get_str(name);
                if v.is_nil() {
                    tv = Value::Nil;
                    break 'get;
                }
                tv = v;
            }
            if tv.is_nil() {
                continue;
            }
            // SAFETY: field offsets are within the struct.
            let fp = unsafe { dp.add(df.size as usize) };
            if ctype_isfield(df.info) {
                let ft = cts.rawchild(fid);
                ct_tv(vm, cts, ft, fp, tv, flags)?;
            } else {
                bf_tv(vm, cts, fid, fp, tv)?;
            }
            if dinfo & CTF_UNION != 0 {
                break;
            }
        } else if ctype_isxattrib(df.info, CTA_SUBTYPE) {
            let sub = cts.rawchild(fid);
            // SAFETY: field offsets are within the struct.
            substruct_tab(vm, cts, sub, unsafe { dp.add(df.size as usize) }, t, ip, flags)?;
        } // Ignore all other entries in the chain.
    }
    Ok(())
}

/// Convert Lua value to C type (`lj_cconv_ct_tv`). Caveat: expects the raw C type `d`.
/// `dp` must be valid for the size of `d`.
pub fn ct_tv(vm: &mut Vm, cts: &mut CTState, d: CTypeID, dp: *mut u8, o: Value, mut flags: CTInfo) -> LuaResult<()> {
    let mut d = d;
    let mut sid = CTID_P_VOID;
    let mut tmpptr: usize;
    let tmpnum: f64;
    let tmpbool: u8;
    let tmpconst: u32;
    let mut sp: *const u8;
    let s: CTypeID;
    match o {
        Value::Num(n) => {
            tmpnum = n;
            sp = ptr::from_ref(&tmpnum).cast();
            sid = CTID_DOUBLE;
            flags |= CCF_FROMTV;
            s = sid;
        }
        Value::CData(cd) => {
            sp = cd.ptr();
            sid = cd.ctypeid();
            let sct = *cts.get(sid);
            if ctype_isref(sct.info) {
                // Resolve reference for value.
                // SAFETY: reference cdata hold a pointer.
                sp = unsafe { rd::<*const u8>(sp) };
                sid = ctype_cid(sct.info);
            }
            let raw = cts.raw(sid);
            if ctype_isfunc(cts.get(raw).info) {
                sid = cts.intern(ctinfo(CT_PTR, CTALIGN_PTR | sid), CTSIZE_PTR).map_err(|e| ffi_err(vm, e))?;
                s = sid;
            } else {
                let s0 = if ctype_isenum(cts.get(raw).info) { cts.child(raw) } else { raw };
                if ctype_isenum(cts.get(d).info) {
                    d = cts.child(d);
                }
                return ct_ct(vm, cts, d, s0, dp, sp, flags);
            }
        }
        Value::Str(str) => {
            let dinfo = cts.get(d).info;
            if ctype_isenum(dinfo) {
                // Match string against enum constant.
                match cts.getfield(d, str.as_bytes()) {
                    Some((cct, _)) if ctype_isconstval(cts.get(cct).info) => {
                        debug_assert!(cts.get(d).size == 4, "only 32 bit enum supported");
                        tmpconst = cts.get(cct).size;
                        sp = ptr::from_ref(&tmpconst).cast();
                        sid = ctype_cid(cts.get(cct).info);
                    }
                    _ => return Err(err_convtv(vm, cts, d, o, flags)),
                }
            } else if ctype_isrefarray(dinfo) {
                // Copy string to array.
                let dc = *cts.get(cts.rawchild(d));
                let mut sz = str.len() as CTSize + 1;
                if !ctype_isinteger(dc.info) || dc.size != 1 {
                    return Err(err_convtv(vm, cts, d, o, flags));
                }
                let dsz = cts.get(d).size;
                if dsz != 0 && dsz < sz {
                    sz = dsz;
                }
                // SAFETY: the destination holds at least sz bytes (or is a VLA of unknown
                // size, as upstream); the string data is NUL-terminated.
                unsafe { ptr::copy_nonoverlapping(str.as_bytes_with_nul().as_ptr(), dp, sz as usize) };
                return Ok(());
            } else {
                // Otherwise pass it as a const char[].
                sp = str.as_bytes_with_nul().as_ptr();
                sid = CTID_A_CCHAR;
                flags |= CCF_FROMTV;
            }
            s = sid;
        }
        Value::Table(t) => {
            let dinfo = cts.get(d).info;
            if ctype_isarray(dinfo) {
                return array_tab(vm, cts, d, dp, t, flags);
            } else if ctype_isstruct(dinfo) {
                // Much simpler to clear the struct first.
                // SAFETY: dp is valid for the struct size.
                unsafe { ptr::write_bytes(dp, 0, cts.get(d).size as usize) };
                let mut i = 0;
                return substruct_tab(vm, cts, d, dp, t, &mut i, flags);
            } else {
                return Err(err_convtv(vm, cts, d, o, flags));
            }
        }
        Value::Bool(b) => {
            tmpbool = b as u8;
            sp = ptr::from_ref(&tmpbool);
            sid = CTID_BOOL;
            s = sid;
        }
        Value::Nil => {
            tmpptr = 0;
            sp = ptr::from_ref(&tmpptr).cast();
            flags |= CCF_FROMTV;
            s = sid;
        }
        Value::Udata(u) => {
            tmpptr = u.data_ptr() as usize;
            if u.udtype() == UDTYPE_IO_FILE {
                // SAFETY: io file userdata start with the FILE* pointer.
                tmpptr = unsafe { rd::<usize>(tmpptr as *const u8) };
            } else if u.udtype() == UDTYPE_BUFFER
                && let Some(f) = vm.rt.ffi.bufptr
            {
                tmpptr = f(&u) as usize;
            }
            sp = ptr::from_ref(&tmpptr).cast();
            s = sid;
        }
        Value::LightUd(p) => {
            tmpptr = p;
            sp = ptr::from_ref(&tmpptr).cast();
            s = sid;
        }
        Value::Func(f) => {
            return match callback_new(cts, d, None) {
                Ok(Some(p)) => {
                    let slot = cts.cb.ptr2slot(p);
                    let mm = super::state::state(vm).miscmap;
                    mm.set_int(slot as i32, Value::Func(f)).map_err(|e| vm.tab_error(e))?;
                    // SAFETY: dp is valid for a pointer-sized destination (checked by
                    // callback_new: d is a 64 bit pointer type).
                    unsafe { wr::<*const u8>(dp, p.cast()) };
                    Ok(())
                }
                Ok(None) => Err(err_convtv(vm, cts, d, o, flags)),
                Err(e) => Err(ffi_err(vm, e)),
            };
        }
        _ => return Err(err_convtv(vm, cts, d, o, flags)),
    }
    if ctype_isenum(cts.get(d).info) {
        d = cts.child(d);
    }
    ct_ct(vm, cts, d, s, dp, sp, flags)
}

/// Convert Lua value to bitfield (`lj_cconv_bf_tv`).
pub fn bf_tv(vm: &mut Vm, cts: &mut CTState, d: CTypeID, dp: *mut u8, o: Value) -> LuaResult<()> {
    let info = cts.get(d).info;
    debug_assert!(ctype_isbitfield(info), "bitfield expected");
    let mut val: u32 = 0;
    if info & CTF_BOOL != 0 {
        let mut tmpbool: u8 = 0;
        debug_assert!(ctype_bitbsz(info) == 1, "bad bool bitfield size");
        ct_tv(vm, cts, CTID_BOOL, &mut tmpbool, o, 0)?;
        val = tmpbool as u32;
    } else {
        let did = if info & CTF_UNSIGNED != 0 { CTID_UINT32 } else { CTID_INT32 };
        ct_tv(vm, cts, did, ptr::from_mut(&mut val).cast(), o, 0)?;
    }
    let pos = ctype_bitpos(info);
    let bsz = ctype_bitbsz(info);
    let csz = ctype_bitcsz(info);
    // Check if a packed bitfield crosses a container boundary.
    if pos + bsz > 8 * csz {
        return Err(vm.err_callermsg(errmsg::FFI_NYIPACKBIT.as_bytes()));
    }
    let mask = (((1u64 << bsz) - 1) as u32) << pos;
    let val = (val << pos) & mask;
    // NYI: packed bitfields may cause misaligned reads/writes.
    // SAFETY: dp is valid for the container size.
    unsafe {
        match csz {
            4 => wr::<u32>(dp, (rd::<u32>(dp) & !mask) | val),
            2 => wr::<u16>(dp, (rd::<u16>(dp) & !(mask as u16)) | val as u16),
            1 => wr::<u8>(dp, (rd::<u8>(dp) & !(mask as u8)) | val as u8),
            _ => debug_assert!(false, "bad bitfield container size {csz}"),
        }
    }
    Ok(())
}

// -- Initialize C type with Lua values ---------------------------------------------------------

/// Initialize an array with Lua values (`cconv_array_init`).
fn array_init(vm: &mut Vm, cts: &mut CTState, d: CTypeID, sz: CTSize, dp: *mut u8, o: &[Value]) -> LuaResult<()> {
    let dc = cts.rawchild(d); // Array element type.
    let esize = cts.get(dc).size;
    if (o.len() as u64) * (esize as u64) > sz as u64 {
        return Err(err_initov(vm, cts, d));
    }
    let mut ofs: CTSize = 0;
    for &v in o {
        // SAFETY: ofs + esize <= sz.
        ct_tv(vm, cts, dc, unsafe { dp.add(ofs as usize) }, v, 0)?;
        ofs += esize;
    }
    // SAFETY: all offsets stay below sz.
    unsafe {
        if ofs == esize {
            // Replicate a single element.
            while ofs < sz {
                ptr::copy_nonoverlapping(dp, dp.add(ofs as usize), esize as usize);
                ofs += esize;
            }
        } else {
            // Otherwise fill the remainder with zero.
            ptr::write_bytes(dp.add(ofs as usize), 0, (sz - ofs) as usize);
        }
    }
    Ok(())
}

/// Initialize a sub-struct/union with Lua values (`cconv_substruct_init`).
fn substruct_init(vm: &mut Vm, cts: &mut CTState, d: CTypeID, dp: *mut u8, o: &[Value], ip: &mut usize) -> LuaResult<()> {
    let dinfo = cts.get(d).info;
    let mut id = cts.get(d).sib as CTypeID;
    while id != 0 {
        let df = *cts.get(id);
        let fid = id;
        id = df.sib as CTypeID;
        if ctype_isfield(df.info) || ctype_isbitfield(df.info) {
            if !df.name.is_some() {
                continue; // Ignore unnamed fields.
            }
            let i = *ip;
            if i >= o.len() {
                break;
            }
            *ip = i + 1;
            // SAFETY: field offsets are within the struct.
            let fp = unsafe { dp.add(df.size as usize) };
            if ctype_isfield(df.info) {
                let ft = cts.rawchild(fid);
                ct_tv(vm, cts, ft, fp, o[i], 0)?;
            } else {
                bf_tv(vm, cts, fid, fp, o[i])?;
            }
            if dinfo & CTF_UNION != 0 {
                break;
            }
        } else if ctype_isxattrib(df.info, CTA_SUBTYPE) {
            let sub = cts.rawchild(fid);
            // SAFETY: field offsets are within the struct.
            substruct_init(vm, cts, sub, unsafe { dp.add(df.size as usize) }, o, ip)?;
            if dinfo & CTF_UNION != 0 {
                break;
            }
        } // Ignore all other entries in the chain.
    }
    Ok(())
}

/// Check whether to use a multi-value initializer (`lj_cconv_multi_init`).
/// This is true if an aggregate is to be initialized with a value. Valarrays are treated
/// as values here so `ct_tv` handles (V|C, I|F).
pub fn multi_init(cts: &CTState, d: CTypeID, o: Value) -> bool {
    let dinfo = cts.get(d).info;
    if !(ctype_isrefarray(dinfo) || ctype_isstruct(dinfo)) {
        return false; // Destination is not an aggregate.
    }
    match o {
        Value::Table(_) => false, // Initializer is not a value.
        Value::Str(_) if !ctype_isstruct(dinfo) => false,
        Value::CData(cd) if cts.rawref(cd.ctypeid()) == d => false, // Identical aggregates.
        _ => true,
    }
}

/// Initialize C type with Lua values (`lj_cconv_ct_init`). Caveat: expects the raw C type.
pub fn ct_init(vm: &mut Vm, cts: &mut CTState, d: CTypeID, sz: CTSize, dp: *mut u8, o: &[Value]) -> LuaResult<()> {
    let dinfo = cts.get(d).info;
    if o.is_empty() {
        // SAFETY: dp is valid for sz bytes.
        unsafe { ptr::write_bytes(dp, 0, sz as usize) };
        Ok(())
    } else if o.len() == 1 && !multi_init(cts, d, o[0]) {
        ct_tv(vm, cts, d, dp, o[0], 0)
    } else if ctype_isarray(dinfo) {
        // Also handles valarray init with len>1.
        array_init(vm, cts, d, sz, dp, o)
    } else if ctype_isstruct(dinfo) {
        // Much simpler to clear the struct first.
        // SAFETY: dp is valid for sz bytes.
        unsafe { ptr::write_bytes(dp, 0, sz as usize) };
        let mut i = 0;
        substruct_init(vm, cts, d, dp, o, &mut i)?;
        if i < o.len() {
            return Err(err_initov(vm, cts, d));
        }
        Ok(())
    } else {
        Err(err_initov(vm, cts, d))
    }
}
