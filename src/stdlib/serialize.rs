//! Object de/serialization for `string.buffer`. Port of lj_serialize.c (exact upstream binary
//! format, little-endian host).

use crate::cdata::{CTID_COMPLEX_DOUBLE, CTID_INT64, CTID_UINT64};
use crate::error::LuaResult;
use crate::ffi::ctype::{CTF_UNSIGNED, ctype_iscomplex, ctype_isinteger};
use crate::gc::Gc;
use crate::lstr::LuaStr;
use crate::state::Vm;
use crate::table::{Table, hsize2hbits};
use crate::value::Value;
use crate::vm::{ErrMsg, fmt_msg};

use super::buffer::SBufExt;
use super::string::err_mem;

// Tags for the internal serialization format.
const SER_TAG_NIL: u32 = 0x00;
const SER_TAG_TRUE: u32 = 0x02;
const SER_TAG_NULL: u32 = 0x03;
const SER_TAG_LIGHTUD32: u32 = 0x04;
const SER_TAG_LIGHTUD64: u32 = 0x05;
const SER_TAG_INT: u32 = 0x06;
const SER_TAG_NUM: u32 = 0x07;
const SER_TAG_TAB: u32 = 0x08;
const SER_TAG_DICT_MT: u32 = SER_TAG_TAB + 6;
const SER_TAG_DICT_STR: u32 = SER_TAG_DICT_MT + 1;
const SER_TAG_INT64: u32 = 0x10;
const SER_TAG_UINT64: u32 = 0x11;
const SER_TAG_COMPLEX: u32 = 0x12;
const SER_TAG_STR: u32 = 0x20;

/// `LJ_SERIALIZE_DEPTH`.
const LJ_SERIALIZE_DEPTH: i32 = 100;

/// Canonical quiet NaN (`setnanV`).
const CANONICAL_NAN: u64 = 0xfff8_0000_0000_0000;

/// `!tvisnum(o)` for a raw 64-bit pattern (non-canonical NaNs that collide with NaN-tagged
/// values upstream).
fn fix_nan(u: u64) -> u64 {
    if u >= 0xfff9_0000_0000_0000 { CANONICAL_NAN } else { u }
}

/// `serialize_more`.
fn more(vm: &mut Vm, sbx: &SBufExt, sz: u32) -> LuaResult<()> {
    sbx.more(sz).map_err(|_| err_mem(vm))
}

/// `serialize_wu124` (space already reserved).
fn wu124(sbx: &SBufExt, v: u32) {
    if v < 0xe0 {
        sbx.wmem(&[v as u8]);
    } else if v < 0x1fe0 {
        let v = v - 0xe0;
        sbx.wmem(&[(0xe0 | (v >> 8)) as u8, v as u8]);
    } else {
        sbx.wmem(&[0xff]);
        sbx.wmem(&v.to_le_bytes());
    }
}

/// `serialize_ru124`: value and new position, `None` at end of buffer.
fn ru124(d: &[u8], mut p: usize) -> Option<(u32, usize)> {
    let mut v = *d.get(p)? as u32;
    p += 1;
    if v >= 0xe0 {
        if v != 0xff {
            let b = *d.get(p)? as u32;
            v = ((v & 0x1f) << 8) + b + 0xe0;
            p += 1;
        } else {
            let b = d.get(p..p + 4)?;
            v = u32::from_le_bytes(b.try_into().expect("4 bytes"));
            p += 4;
        }
    }
    Some((v, p))
}

/// Dictionary index stored by `lj_serialize_dict_prep_*` (`n->val.u32.lo`).
fn dict_index(v: Value) -> u32 {
    match v {
        Value::Num(n) => n.to_bits() as u32,
        _ => 0,
    }
}

/// `lj_serialize_dict_prep_str` / `lj_serialize_dict_prep_mt`: index the array part of the
/// dictionary in its hash part (once), storing the raw 0-based index bits as the value.
pub fn dict_prep(vm: &mut Vm, dict: Gc<Table>, mt: bool) -> LuaResult<()> {
    if dict.hmask() != 0 {
        return Ok(()); // A hash part means already prepared.
    }
    let len = dict.len() as u32;
    if len == 0 {
        return Ok(());
    }
    dict.resize(dict.asize(), hsize2hbits(len)).map_err(|e| vm.tab_error(e))?;
    let mut i = 1;
    while i <= len && i < dict.asize() {
        let o = dict.array_get(i);
        let ok = if mt { matches!(o, Value::Table(_)) } else { matches!(o, Value::Str(_)) };
        if ok {
            let dup = if mt { !dict.get(o).is_nil() } else { dict.has_slot(o) };
            if !dup {
                dict.set(o, Value::Num(f64::from_bits((i - 1) as u64))).map_err(|e| vm.tab_error(e))?;
            }
        } else if o != Value::Bool(false) {
            return Err(vm.err_caller(ErrMsg::BUFFER_BADOPT));
        }
        i += 1;
    }
    Ok(())
}

/// Write a string (`SER_TAG_STR + len`, bytes).
fn put_str(vm: &mut Vm, sbx: &SBufExt, s: Gc<LuaStr>) -> LuaResult<()> {
    let len = s.len() as u32;
    more(vm, sbx, 5 + len)?;
    wu124(sbx, SER_TAG_STR + len);
    sbx.wmem(s.as_bytes());
    Ok(())
}

/// `serialize_put`.
fn serialize_put(vm: &mut Vm, sbx: &SBufExt, o: Value) -> LuaResult<()> {
    match o {
        Value::Str(s) => put_str(vm, sbx, s)?,
        Value::Num(n) => {
            more(vm, sbx, 1 + 8)?;
            sbx.wmem(&[SER_TAG_NUM as u8]);
            sbx.wmem(&n.to_bits().to_le_bytes());
        }
        Value::Nil => {
            more(vm, sbx, 1)?;
            sbx.wmem(&[SER_TAG_NIL as u8]);
        }
        Value::Bool(b) => {
            more(vm, sbx, 1)?;
            sbx.wmem(&[(SER_TAG_NIL + 1 + b as u32) as u8]);
        }
        Value::Table(t) => {
            if sbx.depth.get() <= 0 {
                return Err(vm.err_caller(ErrMsg::BUFFER_DEPTH));
            }
            sbx.depth.set(sbx.depth.get() - 1);
            let (mut narray, mut nhash, mut one) = (0u32, 0u32, 2u32);
            if t.asize() > 0 {
                // Determine max. length of array part.
                let mut i = t.asize() as i64 - 1;
                while i >= 0 && t.array_get(i as u32).is_nil() {
                    i -= 1;
                }
                narray = (i + 1) as u32;
                if narray > 0 && t.array_get(0).is_nil() {
                    one = 4;
                }
            }
            if t.hmask() > 0 {
                // Count number of used hash slots.
                for i in 0..=t.hmask() {
                    nhash += !t.node_get(i).1.is_nil() as u32;
                }
            }
            // Write metatable index.
            if let (Some(dict_mt), Some(mt)) = (sbx.dict_mt.get(), t.metatable())
                && dict_mt.has_slot(Value::Table(mt))
            {
                let idx = dict_index(dict_mt.get(Value::Table(mt)));
                more(vm, sbx, 1 + 5)?;
                sbx.wmem(&[SER_TAG_DICT_MT as u8]);
                wu124(sbx, idx);
            }
            // Write number of array slots and hash slots.
            more(vm, sbx, 1 + 2 * 5)?;
            let tag = SER_TAG_TAB + (nhash > 0) as u32 + if narray > 0 { one } else { 0 };
            sbx.wmem(&[tag as u8]);
            if narray > 0 {
                wu124(sbx, narray);
            }
            if nhash > 0 {
                wu124(sbx, nhash);
            }
            // Write array entries.
            for i in (one >> 2)..narray {
                serialize_put(vm, sbx, t.array_get(i))?;
            }
            // Write hash entries.
            if nhash > 0 {
                let dict_str = sbx.dict_str.get();
                let mut i = t.hmask() as i64;
                while i >= 0 {
                    let (k, v) = t.node_get(i as u32);
                    i -= 1;
                    if v.is_nil() {
                        continue;
                    }
                    match (dict_str, k) {
                        (Some(d), Value::Str(s)) => {
                            if d.has_slot(k) {
                                let idx = dict_index(d.get(k));
                                more(vm, sbx, 1 + 5)?;
                                sbx.wmem(&[SER_TAG_DICT_STR as u8]);
                                wu124(sbx, idx);
                            } else {
                                put_str(vm, sbx, s)?;
                            }
                        }
                        _ => serialize_put(vm, sbx, k)?,
                    }
                    serialize_put(vm, sbx, v)?;
                    nhash -= 1;
                    if nhash == 0 {
                        break;
                    }
                }
            }
            sbx.depth.set(sbx.depth.get() + 1);
        }
        Value::CData(cd) => {
            let (info, size) = match vm.rt.ffi.ctype_raw {
                Some(f) => f(vm, cd.ctypeid()),
                None => (0, 0),
            };
            if ctype_isinteger(info) && size == 8 {
                more(vm, sbx, 1 + 8)?;
                let tag = if info & CTF_UNSIGNED != 0 { SER_TAG_UINT64 } else { SER_TAG_INT64 };
                sbx.wmem(&[tag as u8]);
                // SAFETY: an 8-byte integer cdata.
                sbx.wmem(unsafe { std::slice::from_raw_parts(cd.ptr(), 8) });
            } else if ctype_iscomplex(info) && size == 16 {
                more(vm, sbx, 1 + 16)?;
                sbx.wmem(&[SER_TAG_COMPLEX as u8]);
                // SAFETY: a complex double cdata.
                sbx.wmem(unsafe { std::slice::from_raw_parts(cd.ptr(), 16) });
            } else {
                return Err(bad_enc(vm, o)); // NYI other cdata.
            }
        }
        Value::LightUd(ud) => {
            more(vm, sbx, 1 + 8)?;
            if ud == 0 {
                sbx.wmem(&[SER_TAG_NULL as u8]);
            } else if ud as u64 <= u32::MAX as u64 {
                sbx.wmem(&[SER_TAG_LIGHTUD32 as u8]);
                sbx.wmem(&(ud as u32).to_le_bytes());
            } else {
                sbx.wmem(&[SER_TAG_LIGHTUD64 as u8]);
                sbx.wmem(&(ud as u64).to_le_bytes());
            }
        }
        _ => return Err(bad_enc(vm, o)), // NYI userdata etc.
    }
    Ok(())
}

fn bad_enc(vm: &mut Vm, o: Value) -> crate::error::LuaError {
    let m = fmt_msg(ErrMsg::BUFFER_BADENC, &[o.type_name().as_bytes()]);
    vm.err_callermsg(&m)
}

fn bad_dec(vm: &mut Vm, tp: u32) -> crate::error::LuaError {
    let m = format!("cannot deserialize tag 0x{tp:02x}");
    vm.err_callermsg(m.as_bytes())
}

fn bad_dictx(vm: &mut Vm, idx: u32) -> crate::error::LuaError {
    let m = fmt_msg(ErrMsg::BUFFER_BADDICTX, &[(idx as i32).to_string().as_bytes()]);
    vm.err_callermsg(&m)
}

fn eob(vm: &mut Vm) -> crate::error::LuaError {
    vm.err_caller(ErrMsg::BUFFER_EOB)
}

/// `serialize_get`: decode one value from `d` at `r`; returns it and the new position.
fn serialize_get(vm: &mut Vm, sbx: &SBufExt, d: &[u8], r: usize) -> LuaResult<(Value, usize)> {
    let Some((mut tp, mut r)) = ru124(d, r) else { return Err(eob(vm)) };
    let rem = |r: usize| d.len() - r;
    if tp >= SER_TAG_STR {
        let len = (tp - SER_TAG_STR) as usize;
        if len > rem(r) {
            return Err(eob(vm));
        }
        let s = vm.new_str(&d[r..r + len]);
        Ok((Value::Str(s), r + len))
    } else if tp == SER_TAG_INT {
        if rem(r) < 4 {
            return Err(eob(vm));
        }
        let x = i32::from_le_bytes(d[r..r + 4].try_into().expect("4 bytes"));
        Ok((Value::Num(x as f64), r + 4))
    } else if tp == SER_TAG_NUM {
        if rem(r) < 8 {
            return Err(eob(vm));
        }
        let u = u64::from_le_bytes(d[r..r + 8].try_into().expect("8 bytes"));
        Ok((Value::Num(f64::from_bits(fix_nan(u))), r + 8))
    } else if tp <= SER_TAG_TRUE {
        let v = match tp {
            SER_TAG_NIL => Value::Nil,
            SER_TAG_TRUE => Value::Bool(true),
            _ => Value::Bool(false),
        };
        Ok((v, r))
    } else if tp == SER_TAG_DICT_STR {
        let Some((idx, r2)) = ru124(d, r) else { return Err(eob(vm)) };
        let idx = idx.wrapping_add(1);
        match sbx.dict_str.get() {
            Some(dict) if idx < dict.asize() && matches!(dict.array_get(idx), Value::Str(_)) => {
                Ok((dict.array_get(idx), r2))
            }
            _ => Err(bad_dictx(vm, idx)),
        }
    } else if (SER_TAG_TAB..=SER_TAG_DICT_MT).contains(&tp) {
        let (mut narray, mut nhash) = (0u32, 0u32);
        let mut mt = None;
        if sbx.depth.get() <= 0 {
            return Err(vm.err_caller(ErrMsg::BUFFER_DEPTH));
        }
        sbx.depth.set(sbx.depth.get() - 1);
        if tp == SER_TAG_DICT_MT {
            let Some((idx, r2)) = ru124(d, r) else { return Err(eob(vm)) };
            r = r2;
            let idx = idx.wrapping_add(1);
            match sbx.dict_mt.get() {
                Some(dict) if idx < dict.asize() && matches!(dict.array_get(idx), Value::Table(_)) => {
                    mt = dict.array_get(idx).as_table();
                }
                _ => return Err(bad_dictx(vm, idx)),
            }
            let Some((t2, r2)) = ru124(d, r) else { return Err(eob(vm)) };
            tp = t2;
            r = r2;
            if !(SER_TAG_TAB..SER_TAG_DICT_MT).contains(&tp) {
                return Err(bad_dec(vm, tp));
            }
        }
        if tp >= SER_TAG_TAB + 2 {
            let Some((n, r2)) = ru124(d, r) else { return Err(eob(vm)) };
            narray = n;
            r = r2;
        }
        if tp & 1 != 0 {
            let Some((n, r2)) = ru124(d, r) else { return Err(eob(vm)) };
            nhash = n;
            r = r2;
        }
        let t = vm.new_table(narray, hsize2hbits(nhash))?;
        t.set_metatable(mt);
        let start = if tp >= SER_TAG_TAB + 4 { 1 } else { 0 };
        for i in start..narray {
            let (v, r2) = serialize_get(vm, sbx, d, r)?;
            r = r2;
            t.array_set(i, v);
        }
        while nhash > 0 {
            let (k, r2) = serialize_get(vm, sbx, d, r)?;
            r = r2;
            if !t.get(k).is_nil() {
                return Err(vm.err_caller(ErrMsg::BUFFER_DUPKEY));
            }
            vm.raw_set(t, k, Value::Nil)?;
            let (v, r2) = serialize_get(vm, sbx, d, r)?;
            r = r2;
            t.set(k, v).map_err(|e| vm.tab_error(e))?;
            nhash -= 1;
        }
        sbx.depth.set(sbx.depth.get() + 1);
        Ok((Value::Table(t), r))
    } else if (SER_TAG_INT64..=SER_TAG_COMPLEX).contains(&tp) {
        let sz = if tp == SER_TAG_COMPLEX { 16 } else { 8 };
        if rem(r) < sz {
            return Err(eob(vm));
        }
        if vm.ffi.is_none() {
            return Err(bad_dec(vm, tp));
        }
        let id = match tp {
            SER_TAG_INT64 => CTID_INT64,
            SER_TAG_UINT64 => CTID_UINT64,
            _ => CTID_COMPLEX_DOUBLE,
        };
        let cd = vm.new_cdata(id, sz, 8);
        let mut bytes = d[r..r + sz].to_vec();
        if sz == 16 {
            // Fix non-canonical NaNs.
            for half in bytes.chunks_mut(8) {
                let u = fix_nan(u64::from_le_bytes((&*half).try_into().expect("8 bytes")));
                half.copy_from_slice(&u.to_le_bytes());
            }
        }
        // SAFETY: the new cdata owns `sz` bytes.
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), cd.ptr(), sz) };
        Ok((Value::CData(cd), r + sz))
    } else if tp <= SER_TAG_LIGHTUD64 {
        let mut ud = 0usize;
        if tp == SER_TAG_LIGHTUD32 {
            if rem(r) < 4 {
                return Err(eob(vm));
            }
            ud = u32::from_le_bytes(d[r..r + 4].try_into().expect("4 bytes")) as usize;
            r += 4;
        } else if tp == SER_TAG_LIGHTUD64 {
            if rem(r) < 8 {
                return Err(eob(vm));
            }
            ud = u64::from_le_bytes(d[r..r + 8].try_into().expect("8 bytes")) as usize;
            r += 8;
        }
        Ok((Value::LightUd(ud), r))
    } else {
        Err(bad_dec(vm, tp))
    }
}

/// `lj_serialize_put`: encode `o` into the buffer.
pub fn put(vm: &mut Vm, sbx: &SBufExt, o: Value) -> LuaResult<()> {
    sbx.depth.set(LJ_SERIALIZE_DEPTH);
    serialize_put(vm, sbx, o)
}

/// The unread bytes of a buffer as a slice independent of the buffer borrow.
fn unread<'a>(sbx: &SBufExt) -> &'a [u8] {
    let n = sbx.len() as usize;
    if n == 0 {
        return &[];
    }
    // SAFETY: `r..w` stays valid and unmodified while decoding: decoding only allocates GC
    // objects and never writes to the buffer or runs Lua code.
    unsafe { std::slice::from_raw_parts(sbx.rptr(), n) }
}

/// `lj_serialize_get`: decode one value from the buffer and advance its read pointer.
pub fn get(vm: &mut Vm, sbx: &SBufExt) -> LuaResult<Value> {
    sbx.depth.set(LJ_SERIALIZE_DEPTH);
    let d = unread(sbx);
    let (v, r) = serialize_get(vm, sbx, d, 0)?;
    // SAFETY: `r <= len`.
    sbx.set_r(unsafe { sbx.rptr().add(r) });
    Ok(v)
}

/// `lj_serialize_encode`: stand-alone encoding to a string.
pub fn encode(vm: &mut Vm, o: Value) -> LuaResult<Gc<LuaStr>> {
    let sbx = SBufExt::new();
    put(vm, &sbx, o)?;
    Ok(vm.new_str(sbx.contents()))
}

/// `lj_serialize_decode`: stand-alone decoding of a string (must be consumed completely).
pub fn decode(vm: &mut Vm, s: Gc<LuaStr>) -> LuaResult<Value> {
    let sbx = SBufExt::new_cow(s.as_bytes().as_ptr(), s.len());
    sbx.depth.set(LJ_SERIALIZE_DEPTH);
    let (v, r) = serialize_get(vm, &sbx, s.as_bytes(), 0)?;
    if r != s.len() {
        return Err(vm.err_caller(ErrMsg::BUFFER_LEFTOV));
    }
    Ok(v)
}
