//! C data arithmetic. Port of lj_carith.c: pointer arithmetic, 64 bit integer arithmetic,
//! comparisons, the ctype metamethod fallback and the 64 bit helpers used by the bit library.

use crate::bc::MM;
use crate::cdata::CData;
use crate::error::LuaResult;
use crate::gc::Gc;
use crate::state::Vm;
use crate::value::Value;

use super::cconv::{self, cdata_new, ffi_err, msgf, rd, wr};
use super::ctype::*;
use super::errmsg;
use super::state::state;

/// Binary operands of an operator converted to ctypes (`CDArith`).
struct CDArith {
    p: [*const u8; 2],
    ct: [Option<CTypeID>; 2],
}

/// Check arguments for arithmetic metamethods (`carith_checkarg`). `nums` provides the
/// storage for number operands. Returns false if an operand is not convertible.
#[allow(clippy::needless_range_loop)] // `i` indexes both operands and the stack.
fn checkarg(vm: &mut Vm, cts: &mut CTState, ca: &mut CDArith, nums: &mut [f64; 2]) -> LuaResult<bool> {
    if vm.nargs() < 2 {
        return Err(vm.type_error(1, "cdata"));
    }
    let mut ok = true;
    for i in 0..2 {
        let o = vm.arg(i + 1);
        match o {
            Value::CData(cd) => {
                let id = cd.ctypeid();
                let mut ct = cts.raw(id);
                let mut p = cd.ptr() as *const u8;
                let info = cts.get(ct).info;
                if ctype_isptr(info) {
                    // SAFETY: pointer cdata hold a pointer of their size.
                    p = unsafe { cconv::getptr(p, cts.get(ct).size) };
                    if ctype_isref(info) {
                        ct = cts.rawchild(ct);
                    }
                } else if ctype_isfunc(info) {
                    // SAFETY: function cdata hold the function pointer.
                    p = unsafe { rd::<*const u8>(p) };
                    ct = cts.intern(ctinfo(CT_PTR, CTALIGN_PTR | id), CTSIZE_PTR).map_err(|e| ffi_err(vm, e))?;
                }
                if ctype_isenum(cts.get(ct).info) {
                    ct = cts.child(ct);
                }
                ca.ct[i] = Some(ct);
                ca.p[i] = p;
            }
            Value::Num(n) => {
                nums[i] = n;
                ca.ct[i] = Some(CTID_DOUBLE);
                ca.p[i] = std::ptr::from_ref(&nums[i]).cast();
            }
            Value::Nil => {
                ca.ct[i] = Some(CTID_P_VOID);
                ca.p[i] = std::ptr::null();
            }
            Value::Str(s) => {
                let o2 = vm.arg(2 - i);
                ca.ct[i] = None;
                ca.p[i] = s.as_bytes_with_nul().as_ptr();
                ok = false;
                if let Value::CData(cd2) = o2 {
                    let ct = cts.raw(cd2.ctypeid());
                    if ctype_isenum(cts.get(ct).info) {
                        match cts.getfield(ct, s.as_bytes()) {
                            Some((cct, _)) if ctype_isconstval(cts.get(cct).info) => {
                                ca.ct[i] = Some(cts.child(cct));
                                nums[i] = f64::from_bits(cts.get(cct).size as u64);
                                ca.p[i] = std::ptr::from_ref(&nums[i]).cast();
                                ok = true;
                            }
                            _ => {
                                ca.ct[1 - i] = Some(ct); // Use enum to improve error message.
                                ca.p[1 - i] = std::ptr::null();
                                break;
                            }
                        }
                    }
                }
            }
            _ => {
                ca.ct[i] = None;
                ca.p[i] = std::ptr::without_provenance(1); // To make it unequal.
                ok = false;
            }
        }
    }
    Ok(ok)
}

/// Pointer arithmetic (`carith_ptr`).
#[allow(clippy::needless_late_init)]
fn carith_ptr(vm: &mut Vm, cts: &mut CTState, ca: &CDArith, mm: MM) -> LuaResult<Option<Value>> {
    let (Some(ctp0), Some(ct1)) = (ca.ct[0], ca.ct[1]) else { return Ok(None) };
    let mut ctp = ctp0;
    let mut pp = ca.p[0];
    let idx: isize;
    let i0 = cts.get(ctp).info;
    let i1 = cts.get(ct1).info;
    if ctype_isptr(i0) || ctype_isrefarray(i0) {
        if matches!(mm, MM::Sub | MM::Eq | MM::Lt | MM::Le) && (ctype_isptr(i1) || ctype_isrefarray(i1)) {
            let pp2 = ca.p[1];
            if mm == MM::Eq {
                // Pointer equality. Incompatible pointers are ok.
                return Ok(Some(Value::Bool(pp == pp2)));
            }
            if !cconv::compatptr(cts, ctp, ct1, cconv::CCF_IGNQUAL) {
                return Ok(None);
            }
            return Ok(Some(match mm {
                MM::Sub => {
                    // Pointer difference.
                    let sz = cts.size(ctype_cid(i0)); // Element size.
                    if sz == 0 || sz == CTSIZE_INVALID {
                        return Ok(None);
                    }
                    let diff = (pp as isize).wrapping_sub(pp2 as isize) / (sz as i32 as isize);
                    Value::Num(diff as f64)
                }
                MM::Lt => Value::Bool((pp as usize) < (pp2 as usize)),
                _ => Value::Bool((pp as usize) <= (pp2 as usize)),
            }));
        }
        if !(matches!(mm, MM::Add | MM::Sub) && ctype_isnum(i1)) {
            return Ok(None);
        }
        let mut x: isize = 0;
        cconv::ct_ct(vm, cts, CTID_INT_PSZ, ct1, std::ptr::from_mut(&mut x).cast(), ca.p[1], 0)?;
        idx = if mm == MM::Sub { x.wrapping_neg() } else { x };
    } else if mm == MM::Add && ctype_isnum(i0) && (ctype_isptr(i1) || ctype_isrefarray(i1)) {
        // Swap pointer and index.
        ctp = ct1;
        pp = ca.p[1];
        let mut x: isize = 0;
        cconv::ct_ct(vm, cts, CTID_INT_PSZ, ctp0, std::ptr::from_mut(&mut x).cast(), ca.p[0], 0)?;
        idx = x;
    } else {
        return Ok(None);
    }
    let cid = ctype_cid(cts.get(ctp).info);
    let sz = cts.size(cid); // Element size.
    if sz == CTSIZE_INVALID {
        return Ok(None);
    }
    let pp = pp.wrapping_offset(idx.wrapping_mul(sz as i32 as isize)); // Compute pointer + index.
    let id = cts.intern(ctinfo(CT_PTR, CTALIGN_PTR | cid), CTSIZE_PTR).map_err(|e| ffi_err(vm, e))?;
    let cd = cdata_new(vm, id, CTSIZE_PTR);
    // SAFETY: the cdata owns 8 bytes.
    unsafe { wr::<*const u8>(cd.ptr(), pp) };
    Ok(Some(Value::CData(cd)))
}

/// 64 bit integer arithmetic (`carith_int64`).
fn carith_int64(vm: &mut Vm, cts: &mut CTState, ca: &CDArith, mm: MM) -> LuaResult<Option<Value>> {
    let (Some(c0), Some(c1)) = (ca.ct[0], ca.ct[1]) else { return Ok(None) };
    let (t0, t1) = (*cts.get(c0), *cts.get(c1));
    if !(ctype_isnum(t0.info) && t0.size <= 8 && ctype_isnum(t1.info) && t1.size <= 8) {
        return Ok(None);
    }
    let id = if (t0.info & CTF_UNSIGNED != 0 && t0.size == 8) || (t1.info & CTF_UNSIGNED != 0 && t1.size == 8) {
        CTID_UINT64
    } else {
        CTID_INT64
    };
    let (mut u0, mut u1) = (0u64, 0u64);
    cconv::ct_ct(vm, cts, id, c0, std::ptr::from_mut(&mut u0).cast(), ca.p[0], 0)?;
    if mm != MM::Unm {
        cconv::ct_ct(vm, cts, id, c1, std::ptr::from_mut(&mut u1).cast(), ca.p[1], 0)?;
    }
    let signed = id == CTID_INT64;
    match mm {
        MM::Eq => return Ok(Some(Value::Bool(u0 == u1))),
        MM::Lt => return Ok(Some(Value::Bool(if signed { (u0 as i64) < (u1 as i64) } else { u0 < u1 }))),
        MM::Le => return Ok(Some(Value::Bool(if signed { (u0 as i64) <= (u1 as i64) } else { u0 <= u1 }))),
        _ => {}
    }
    let r = match mm {
        MM::Add => u0.wrapping_add(u1),
        MM::Sub => u0.wrapping_sub(u1),
        MM::Mul => u0.wrapping_mul(u1),
        MM::Div => {
            if signed {
                divi64(u0 as i64, u1 as i64) as u64
            } else {
                divu64(u0, u1)
            }
        }
        MM::Mod => {
            if signed {
                modi64(u0 as i64, u1 as i64) as u64
            } else {
                modu64(u0, u1)
            }
        }
        MM::Pow => {
            if signed {
                powi64(u0 as i64, u1 as i64) as u64
            } else {
                powu64(u0, u1)
            }
        }
        MM::Unm => (!u0).wrapping_add(1),
        _ => {
            debug_assert!(false, "bad metamethod {mm:?}");
            0
        }
    };
    let cd = cdata_new(vm, id, 8);
    // SAFETY: the cdata owns 8 bytes.
    unsafe { wr::<u64>(cd.ptr(), r) };
    Ok(Some(Value::CData(cd)))
}

/// `lj_ctype_meta`: metamethod `mm` of the ctype `id` set by `ffi.metatype` (or of the
/// shared callback metatable for pointers to functions).
pub fn ctype_meta(vm: &mut Vm, cts: &CTState, id: CTypeID, mm: MM) -> Option<Value> {
    let mt = match cts.meta_key(id) {
        None => {
            let e = vm.empty_str;
            state(vm).miscmap.get_str(e)
        }
        Some(id) => state(vm).miscmap.get(Value::Num(-(id as i32) as f64)),
    };
    if let Value::Table(t) = mt {
        let v = t.get_str(vm.mm_name(mm));
        if !v.is_nil() {
            return Some(v);
        }
    }
    None
}

/// The ctype metamethod lookup for an operand (pointers use their element type).
fn operand_meta(vm: &mut Vm, cts: &CTState, o: Value, mm: MM) -> Option<Value> {
    let Value::CData(cd) = o else { return None };
    let mut id = cd.ctypeid();
    let ct = cts.raw(id);
    if ctype_isptr(cts.get(ct).info) {
        id = ctype_cid(cts.get(ct).info);
    }
    ctype_meta(vm, cts, id, mm)
}

/// Handle ctype arithmetic metamethods (`lj_carith_meta`).
#[allow(clippy::needless_range_loop)] // `i` indexes both operands and the stack.
fn carith_meta(vm: &mut Vm, cts: &CTState, ca: &CDArith, mm: MM) -> LuaResult<usize> {
    let mut tv = operand_meta(vm, cts, vm.arg(1), mm);
    if tv.is_none() && vm.nargs() >= 2 {
        tv = operand_meta(vm, cts, vm.arg(2), mm);
    }
    if let Some(tv) = tv {
        return vm.tailcall(tv);
    }
    if mm == MM::Eq {
        // Equality checks never raise an error.
        return vm.ret1(Value::Bool(ca.p[0] == ca.p[1]));
    }
    let mut repr: [Vec<u8>; 2] = [Vec::new(), Vec::new()];
    let (mut isenum, mut isstr) = (-1i32, -1i32);
    for i in 0..2 {
        let o = vm.arg(i + 1);
        match (ca.ct[i], o) {
            (Some(ct), Value::CData(_)) => {
                if ctype_isenum(cts.get(ct).info) {
                    isenum = i as i32;
                }
                repr[i] = cts.repr(ct, None);
            }
            _ => {
                if let Value::Str(_) = o {
                    isstr = i as i32;
                }
                repr[i] = o.type_name().as_bytes().to_vec();
            }
        }
    }
    if (isenum ^ isstr) == 1 {
        let m = msgf(errmsg::FFI_BADCONV, &[&repr[isstr as usize], &repr[isenum as usize]]);
        return Err(vm.err_callermsg(&m));
    }
    let em = match mm {
        MM::Len => errmsg::FFI_BADLEN,
        MM::Concat => errmsg::FFI_BADCONCAT,
        _ if (mm as u8) < (MM::Add as u8) => errmsg::FFI_BADCOMP,
        _ => errmsg::FFI_BADARITH,
    };
    let m = msgf(em, &[&repr[0], &repr[1]]);
    Err(vm.err_callermsg(&m))
}

/// Arithmetic operators for cdata (`lj_carith_op`), run as the metamethod native with the
/// operands as arguments.
pub fn carith_op(vm: &mut Vm, mm: MM) -> LuaResult<usize> {
    let st = state(vm);
    let mut cts = st.cts.borrow_mut();
    let mut ca = CDArith { p: [std::ptr::null(); 2], ct: [None; 2] };
    let mut nums = [0f64; 2];
    if checkarg(vm, &mut cts, &mut ca, &mut nums)? && mm != MM::Len && mm != MM::Concat {
        let r = match carith_int64(vm, &mut cts, &ca, mm)? {
            Some(v) => Some(v),
            None => carith_ptr(vm, &mut cts, &ca, mm)?,
        };
        if let Some(v) = r {
            return vm.ret1(v);
        }
    }
    carith_meta(vm, &cts, &ca, mm)
}

// -- 64 bit bit operations helpers -------------------------------------------------------------

/// `lj_carith_shift64`: `op` is 0..4 for shl, shr, sar, rol, ror.
pub fn shift64(x: u64, sh: i32, op: u32) -> u64 {
    let sh = (sh & 63) as u32;
    match op {
        0 => x << sh,
        1 => x >> sh,
        2 => ((x as i64) >> sh) as u64,
        3 => x.rotate_left(sh),
        _ => x.rotate_right(sh),
    }
}

/// Convert a cdata operand of a 64 bit bit operation (the cdata part of
/// `lj_carith_check64`/`lj_carith_checkbit64`).
fn cdata_bit64(vm: &mut Vm, cd: Gc<CData>, id: &mut u32, flags: CTInfo) -> LuaResult<u64> {
    let st = state(vm);
    let cts = st.cts.borrow();
    let mut sp = cd.ptr() as *const u8;
    let mut sid = cd.ctypeid();
    let s = cts.get(sid);
    if ctype_isref(s.info) {
        // SAFETY: reference cdata hold a pointer.
        sp = unsafe { rd::<*const u8>(sp) };
        sid = ctype_cid(s.info);
    }
    let mut s = cts.raw(sid);
    if ctype_isenum(cts.get(s).info) {
        s = cts.child(s);
    }
    let sct = cts.get(s);
    if (sct.info & (CTMASK_NUM | CTF_BOOL | CTF_FP | CTF_UNSIGNED)) == ctinfo(CT_NUM, CTF_UNSIGNED) && sct.size == 8 {
        *id = CTID_UINT64; // Use uint64_t, since it has the highest rank.
    } else if *id == 0 {
        *id = CTID_INT64; // Use int64_t, unless already set.
    }
    let mut x = 0u64;
    cconv::ct_ct(vm, &cts, *id, s, std::ptr::from_mut(&mut x).cast(), sp, flags)?;
    Ok(x)
}

/// `lj_carith_checkbit64` for a cdata operand (installed as `FfiHooks::checkbit64`).
pub fn checkbit64(vm: &mut Vm, cd: Gc<CData>, id: &mut u32) -> LuaResult<u64> {
    cdata_bit64(vm, cd, id, 0)
}

/// The cdata branch of `lj_carith_check64` (installed as `FfiHooks::check64`).
pub fn check64(vm: &mut Vm, cd: Gc<CData>, narg: u32, id: &mut u32) -> LuaResult<u64> {
    cdata_bit64(vm, cd, id, cconv::ccf_arg(narg))
}

// -- 64 bit integer arithmetic helpers ---------------------------------------------------------

/// Unsigned 64 bit division (`lj_carith_divu64`).
pub fn divu64(a: u64, b: u64) -> u64 {
    a.checked_div(b).unwrap_or(1 << 63)
}

/// Signed 64 bit division (`lj_carith_divi64`).
pub fn divi64(a: i64, b: i64) -> i64 {
    if b == 0 || (a == i64::MIN && b == -1) { i64::MIN } else { a / b }
}

/// Unsigned 64 bit modulo (`lj_carith_modu64`).
pub fn modu64(a: u64, b: u64) -> u64 {
    if b == 0 { 1 << 63 } else { a % b }
}

/// Signed 64 bit modulo (`lj_carith_modi64`).
pub fn modi64(a: i64, b: i64) -> i64 {
    if b == 0 {
        i64::MIN
    } else if a == i64::MIN && b == -1 {
        0
    } else {
        a % b
    }
}

/// Unsigned 64 bit x^k (`lj_carith_powu64`).
pub fn powu64(mut x: u64, mut k: u64) -> u64 {
    if k == 0 {
        return 1;
    }
    while k & 1 == 0 {
        x = x.wrapping_mul(x);
        k >>= 1;
    }
    let mut y = x;
    k >>= 1;
    if k != 0 {
        loop {
            x = x.wrapping_mul(x);
            if k == 1 {
                break;
            }
            if k & 1 != 0 {
                y = y.wrapping_mul(x);
            }
            k >>= 1;
        }
        y = y.wrapping_mul(x);
    }
    y
}

/// Signed 64 bit x^k (`lj_carith_powi64`).
pub fn powi64(x: i64, k: i64) -> i64 {
    if k == 0 {
        return 1;
    }
    if k < 0 {
        return match x {
            0 => i64::MAX,
            1 => 1,
            -1 => {
                if k & 1 != 0 {
                    -1
                } else {
                    1
                }
            }
            _ => 0,
        };
    }
    powu64(x as u64, k as u64) as i64
}
