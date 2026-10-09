//! C data management. Port of lj_cdata.c: allocation of variable-size cdata, finalizer
//! registration, indexing of cdata by Lua values and the getters/setters.

use crate::cdata::CData;
use crate::error::LuaResult;
use crate::gc::Gc;
use crate::state::Vm;
use crate::value::Value;

use super::cconv::{self, rd};
use super::ctype::*;
use super::errmsg;

/// Allocate an arbitrary cdata object (`lj_cdata_newx`): VLAs and over-aligned types get
/// their alignment.
pub fn cdata_newx(vm: &mut Vm, id: CTypeID, sz: CTSize, info: CTInfo) -> Gc<CData> {
    vm.new_cdata(id, sz as usize, 1usize << ctype_align(info))
}

/// True if the cdata was allocated with a variable length (`cdataisv` for VLA/VLS types).
pub fn cdata_isv(cts: &CTState, cd: Gc<CData>) -> bool {
    let ct = cts.get(cts.raw(cd.ctypeid()));
    ctype_isvltype(ct.info) || ctype_align(ct.info) > CT_MEMALIGN
}

/// `lj_cdata_setfin`: set (`fin` non-nil) or clear the finalizer of a cdata.
pub fn cdata_setfin(vm: &mut Vm, cd: Gc<CData>, fin: Value) -> LuaResult<()> {
    let Some(t) = vm.ffi_fin else { return Ok(()) };
    if t.metatable().is_some() {
        // Add cdata to finalizer table, if still enabled.
        t.set(Value::CData(cd), fin).map_err(|e| vm.tab_error(e))?;
        cd.set_has_finalizer(!fin.is_nil());
    }
    Ok(())
}

/// Index C data by a Lua value (`lj_cdata_index`). Returns the C type, the pointer and the
/// collected qualifiers; bit 0 of the qualifiers is set if the lookup failed (then the type
/// is the resolved raw type for the metamethod lookup).
pub fn cdata_index(vm: &mut Vm, cts: &CTState, cd: Gc<CData>, key: Value) -> LuaResult<(CTypeID, *mut u8, CTInfo)> {
    let mut p = cd.ptr();
    let mut ct = cd.ctypeid();
    let mut qual: CTInfo = 0;

    // Resolve reference for cdata object.
    if ctype_isref(cts.get(ct).info) {
        // SAFETY: reference cdata hold a pointer.
        p = unsafe { rd::<*mut u8>(p) };
        ct = cts.child(ct);
    }

    loop {
        // Skip attributes and collect qualifiers.
        while ctype_isattrib(cts.get(ct).info) {
            if ctype_attrib(cts.get(ct).info) == CTA_QUAL {
                qual |= cts.get(ct).size;
            }
            ct = cts.child(ct);
        }
        debug_assert!(!ctype_isref(cts.get(ct).info), "bad ref of ref");
        let info = cts.get(ct).info;

        let idx: Option<isize> = match key {
            Value::Num(n) => Some(crate::text::num2i64(n) as isize),
            Value::CData(cdk) => {
                // Integer cdata key.
                let mut ctk = cts.raw(cdk.ctypeid());
                if ctype_isenum(cts.get(ctk).info) {
                    ctk = cts.child(ctk);
                }
                if ctype_isinteger(cts.get(ctk).info) {
                    let mut idx: isize = 0;
                    cconv::ct_ct(vm, cts, CTID_INT_PSZ, ctk, std::ptr::from_mut(&mut idx).cast(), cdk.ptr(), 0)?;
                    Some(idx)
                } else {
                    None
                }
            }
            _ => None,
        };
        if let Some(idx) = idx {
            if ctype_ispointer(info) {
                let sz = cts.size(ctype_cid(info)); // Element size.
                if sz == CTSIZE_INVALID {
                    return Err(vm.err_callermsg(errmsg::FFI_INVSIZE.as_bytes()));
                }
                let mut idx = idx;
                if ctype_isptr(info) {
                    // SAFETY: pointer cdata hold a pointer of their size.
                    p = unsafe { cconv::getptr(p, cts.get(ct).size) };
                } else if info & (CTF_VECTOR | CTF_COMPLEX) != 0 {
                    if info & CTF_COMPLEX != 0 {
                        idx &= 1;
                    }
                    qual |= CTF_CONST; // Valarray elements are constant.
                }
                let p = p.wrapping_offset(idx.wrapping_mul(sz as i32 as isize));
                return Ok((ct, p, qual));
            }
        } else if let Value::Str(name) = key {
            // String key.
            if ctype_isstruct(info) {
                if let Some((fct, ofs, q)) = cts.getfieldq(ct, name.as_bytes()) {
                    return Ok((fct, p.wrapping_add(ofs as usize), qual | q));
                }
            } else if ctype_iscomplex(info) {
                let nb = name.as_bytes();
                if nb.len() == 2 {
                    qual |= CTF_CONST; // Complex fields are constant.
                    if nb == b"re" {
                        return Ok((ct, p, qual));
                    } else if nb == b"im" {
                        return Ok((ct, p.wrapping_add((cts.get(ct).size >> 1) as usize), qual));
                    }
                }
            } else if cd.ctypeid() == CTID_CTYPEID {
                // Allow indexing a (pointer to) struct constructor to get constants.
                // SAFETY: ctype cdata hold a CTypeID.
                let mut sct = cts.raw(unsafe { rd::<CTypeID>(p) });
                if ctype_isptr(cts.get(sct).info) {
                    sct = cts.rawchild(sct);
                }
                if ctype_isstruct(cts.get(sct).info)
                    && let Some((fct, _)) = cts.getfield(sct, name.as_bytes())
                    && ctype_isconstval(cts.get(fct).info)
                {
                    return Ok((fct, p, qual));
                }
                ct = sct; // Allow resolving metamethods for constructors, too.
            }
        }
        let info = cts.get(ct).info;
        if ctype_isptr(info) && ctype_isstruct(cts.get(cts.rawchild(ct)).info) {
            // Automatically perform '->'.
            // SAFETY: pointer cdata hold a pointer of their size.
            p = unsafe { cconv::getptr(p, cts.get(ct).size) };
            ct = cts.child(ct);
            continue;
        }
        qual |= 1; // Lookup failed.
        return Ok((ct, p, qual)); // But return the resolved raw type.
    }
}

/// Get a constant value as a Lua number (`cdata_getconst`).
fn getconst(cts: &CTState, ct: CTypeID) -> Value {
    let ctt = cts.get(cts.child(ct));
    debug_assert!(ctype_isinteger(ctt.info) && ctt.size <= 4, "only 32 bit const supported");
    // Constants are already zero-extended/sign-extended to 32 bits.
    let v = cts.get(ct).size;
    if ctt.info & CTF_UNSIGNED != 0 && (v as i32) < 0 { Value::Num(v as f64) } else { Value::Num(v as i32 as f64) }
}

/// Get C data value and convert it to a Lua value (`lj_cdata_get`).
pub(crate) fn cdata_get(vm: &mut Vm, cts: &mut CTState, s: CTypeID, sp: *mut u8) -> LuaResult<Value> {
    let sinfo = cts.get(s).info;
    if ctype_isconstval(sinfo) {
        return Ok(getconst(cts, s));
    } else if ctype_isbitfield(sinfo) {
        return cconv::tv_bf(vm, cts, s, sp);
    }
    // Get child type of pointer/array/field.
    debug_assert!(ctype_ispointer(sinfo) || ctype_isfield(sinfo), "pointer or field expected");
    let mut sid = ctype_cid(sinfo);
    let mut sp = sp;
    // Resolve reference for field.
    if ctype_isref(cts.get(sid).info) {
        // SAFETY: reference fields hold a pointer.
        sp = unsafe { rd::<*mut u8>(sp) };
        sid = ctype_cid(cts.get(sid).info);
    }
    // Skip attributes.
    let mut s = sid;
    while ctype_isattrib(cts.get(s).info) {
        s = cts.child(s);
    }
    cconv::tv_ct(vm, cts, s, sid, sp)
}

/// Convert a Lua value and store it into C data (`lj_cdata_set`).
pub(crate) fn cdata_set(vm: &mut Vm, cts: &mut CTState, d: CTypeID, dp: *mut u8, o: Value, qual: CTInfo) -> LuaResult<()> {
    let dinfo = cts.get(d).info;
    let mut qual = qual;
    if ctype_isconstval(dinfo) {
        return Err(vm.err_callermsg(errmsg::FFI_WRCONST.as_bytes()));
    } else if ctype_isbitfield(dinfo) {
        if (dinfo | qual) & CTF_CONST != 0 {
            return Err(vm.err_callermsg(errmsg::FFI_WRCONST.as_bytes()));
        }
        return cconv::bf_tv(vm, cts, d, dp, o);
    }
    // Get child type of pointer/array/field.
    debug_assert!(ctype_ispointer(dinfo) || ctype_isfield(dinfo), "pointer or field expected");
    let mut d = cts.child(d);
    let mut dp = dp;
    // Resolve reference for field.
    if ctype_isref(cts.get(d).info) {
        // SAFETY: reference fields hold a pointer.
        dp = unsafe { rd::<*mut u8>(dp) };
        d = cts.child(d);
    }
    // Skip attributes and collect qualifiers.
    loop {
        let ct = cts.get(d);
        if ctype_isattrib(ct.info) {
            if ctype_attrib(ct.info) == CTA_QUAL {
                qual |= ct.size;
            }
        } else {
            break;
        }
        d = cts.child(d);
    }
    debug_assert!(ctype_hassize(cts.get(d).info), "store to ctype without size");
    debug_assert!(!ctype_isvoid(cts.get(d).info), "store to void type");
    if (cts.get(d).info | qual) & CTF_CONST != 0 {
        return Err(vm.err_callermsg(errmsg::FFI_WRCONST.as_bytes()));
    }
    cconv::ct_tv(vm, cts, d, dp, o, 0)
}
