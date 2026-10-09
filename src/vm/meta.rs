//! Metamethod resolution and the slow paths of the arithmetic, comparison, indexing,
//! concatenation and length instructions. Port of lj_meta.c (plus `lj_vm_foldarith`,
//! `lj_strfmt_obj` and `lj_obj_ptr`).

use crate::bc::MM;
use crate::cdata::{CTID_INT64, CTID_UINT64};
use crate::error::LuaResult;
use crate::gc::Gc;
use crate::lstr::LuaStr;
use crate::state::Vm;
use crate::table::{TabError, Table};
use crate::thread::Cont;
use crate::value::Value;

use super::err::ErrMsg;

/// `__index`/`__newindex` chain limit (`LJ_MAX_IDXCHAIN`).
pub const LJ_MAX_IDXCHAIN: usize = 100;

/// Result of a metamethod-aware operation: either finished, or a metamethod must be called.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Meta<T> {
    Done(T),
    /// Call `mo(a, b)` (and for `__newindex` a third argument) with continuation `cont`.
    Call { cont: Cont, mo: Value, a: Value, b: Value },
}

/// `lj_vm_foldarith` for the arithmetic metamethod order (`MM_add` .. `MM_unm`).
#[inline]
pub fn arith_op(mm: MM, x: f64, y: f64) -> f64 {
    match mm {
        MM::Add => x + y,
        MM::Sub => x - y,
        MM::Mul => x * y,
        MM::Div => x / y,
        MM::Mod => modf(x, y),
        MM::Pow => x.powf(y),
        MM::Unm => -x,
        _ => unreachable!("not an arithmetic metamethod"),
    }
}

/// `lj_vm_floor` (`vm_floor_sse`): `floor` with SSE2 arithmetic only. Without SSE4.1 (the
/// baseline x86-64 target) `f64::floor` is a libm call. Exact for all inputs; relies on the
/// default round-to-nearest mode like upstream.
#[inline]
pub fn floor(x: f64) -> f64 {
    const TWO52: f64 = 4503599627370496.0;
    let ax = x.abs();
    if !(ax < TWO52) {
        return x; // Integral, infinite or NaN.
    }
    let sign = x.to_bits() & (1 << 63);
    let mut r = f64::from_bits(((ax + TWO52) - TWO52).to_bits() | sign);
    if r > x {
        r -= 1.0;
    }
    f64::from_bits(r.to_bits() | sign)
}

/// `lj_vm_modi` / `vm_mod`: `a - floor(a/b)*b`.
#[inline]
pub fn modf(a: f64, b: f64) -> f64 {
    a - floor(a / b) * b
}

/// `lj_num2bit`: number to 32-bit integer for bit operations (round-to-nearest-even via the
/// 2^52+2^51 bias, modulo 2^32).
#[inline]
pub fn num2bit(n: f64) -> i32 {
    (n + 6755399441055744.0).to_bits() as u32 as i32
}

/// `lj_num2int` on x86-64 (`cvttsd2si`): truncation; NaN and out-of-range give `i32::MIN`.
#[inline]
pub fn num2int(n: f64) -> i32 {
    if n > -2147483649.0 && n < 2147483648.0 { n as i32 } else { i32::MIN }
}

/// `str2num`: number or number-convertible string.
#[inline]
pub(crate) fn str2num(v: Value) -> Option<f64> {
    match v {
        Value::Num(n) => Some(n),
        Value::Str(s) => crate::text::strscan::scan_num(s.as_bytes()),
        _ => None,
    }
}

/// `lj_obj_ptr`: the address printed by `tostring` and `%p`.
pub fn obj_ptr(v: Value) -> usize {
    match v {
        Value::Udata(u) => {
            let p = u.data_ptr();
            if p.is_null() { u.addr() } else { p as usize }
        }
        Value::LightUd(p) => p,
        Value::CData(c) => c.ptr() as usize,
        v => v.gc_addr().unwrap_or(0),
    }
}

impl Vm {
    /// `lj_meta_lookup`: metamethod `mm` of any value (nil if none).
    #[inline]
    pub fn meta_lookup(&self, o: Value, mm: MM) -> Value {
        match self.metatable_of(o) {
            Some(mt) => mt.get_str(self.mm_name(mm)),
            None => Value::Nil,
        }
    }

    /// `lj_strfmt_obj` (the conversion `tostring` does without `__tostring`).
    pub fn tostring_basic(&mut self, o: Value) -> Gc<LuaStr> {
        match o {
            Value::Str(s) => s,
            Value::Num(n) => {
                let b = crate::text::strfmt::fmt_num(n);
                self.new_str(&b)
            }
            Value::Nil => self.new_str(b"nil"),
            Value::Bool(false) => self.new_str(b"false"),
            Value::Bool(true) => self.new_str(b"true"),
            _ => {
                let mut buf = Vec::with_capacity(32);
                buf.extend_from_slice(o.type_name().as_bytes());
                buf.extend_from_slice(b": ");
                match o {
                    Value::Func(f) if f.is_builtin() => {
                        buf.extend_from_slice(b"builtin#");
                        buf.extend_from_slice(f.ffid().to_string().as_bytes());
                    }
                    _ => crate::text::strfmt::put_ptr(&mut buf, obj_ptr(o)),
                }
                self.new_str(&buf)
            }
        }
    }

    /// `lj_meta_tget`: resolve `o[k]` through the `__index` chain. `slot` is the stack slot of
    /// `o` for error messages.
    pub(crate) fn meta_tget(&mut self, o: Value, k: Value, slot: Option<usize>) -> LuaResult<Meta<Value>> {
        let mut o = o;
        for lp in 0..LJ_MAX_IDXCHAIN {
            let mo;
            if let Value::Table(t) = o {
                let v = t.get(k);
                if !v.is_nil() {
                    return Ok(Meta::Done(v));
                }
                match self.fast_mm(t.metatable(), MM::Index) {
                    Some(m) => mo = m,
                    None => return Ok(Meta::Done(v)),
                }
            } else {
                mo = self.meta_lookup(o, MM::Index);
                if mo.is_nil() {
                    return Err(self.err_optype(o, if lp == 0 { slot } else { None }, ErrMsg::OPINDEX));
                }
            }
            if let Value::Func(_) = mo {
                return Ok(Meta::Call { cont: Cont::Ra, mo, a: o, b: k });
            }
            o = mo;
        }
        Err(self.err_msg(ErrMsg::GETLOOP))
    }

    /// Raw store with upstream error messages (`lj_tab_set` / `lj_tab_newkey`).
    pub(crate) fn raw_set(&mut self, t: Gc<Table>, k: Value, v: Value) -> LuaResult<()> {
        match t.set(k, v) {
            Ok(()) => Ok(()),
            Err(TabError::NilIndex) => Err(self.err_msg(ErrMsg::NILIDX)),
            Err(TabError::NanIndex) => Err(self.err_msg(ErrMsg::NANIDX)),
            Err(TabError::Overflow) => Err(self.err_msg(ErrMsg::TABOV)),
            Err(e) => Err(self.tab_error(e)),
        }
    }

    /// `lj_meta_tset`: perform `o[k] = v` through the `__newindex` chain. Returns the
    /// metamethod call (with `a = object`, `b = key`; the value is the third argument).
    pub(crate) fn meta_tset(&mut self, o: Value, k: Value, v: Value, slot: Option<usize>) -> LuaResult<Meta<()>> {
        let mut o = o;
        for lp in 0..LJ_MAX_IDXCHAIN {
            let mo;
            if let Value::Table(t) = o {
                if !t.get(k).is_nil() {
                    t.set_existing(k, v);
                    return Ok(Meta::Done(()));
                }
                match self.fast_mm(t.metatable(), MM::Newindex) {
                    Some(m) => mo = m,
                    None => {
                        t.set_nomm(0);
                        self.raw_set(t, k, v)?;
                        return Ok(Meta::Done(()));
                    }
                }
            } else {
                mo = self.meta_lookup(o, MM::Newindex);
                if mo.is_nil() {
                    return Err(self.err_optype(o, if lp == 0 { slot } else { None }, ErrMsg::OPINDEX));
                }
            }
            if let Value::Func(_) = mo {
                return Ok(Meta::Call { cont: Cont::Nop, mo, a: o, b: k });
            }
            o = mo;
        }
        Err(self.err_msg(ErrMsg::SETLOOP))
    }

    /// `lj_meta_arith`: coercion and arithmetic metamethods. `bslot`/`cslot` are the operand
    /// slots (for error messages).
    pub(crate) fn meta_arith(
        &mut self,
        mm: MM,
        rb: Value,
        rc: Value,
        bslot: Option<usize>,
        cslot: Option<usize>,
    ) -> LuaResult<Meta<f64>> {
        if let (Some(b), Some(c)) = (str2num(rb), str2num(rc)) {
            return Ok(Meta::Done(arith_op(mm, b, c)));
        }
        let mut mo = self.meta_lookup(rb, mm);
        if mo.is_nil() {
            mo = self.meta_lookup(rc, mm);
            if mo.is_nil() {
                let (o, s) = if str2num(rb).is_none() { (rb, bslot) } else { (rc, cslot) };
                return Err(self.err_optype(o, s, ErrMsg::OPARITH));
            }
        }
        Ok(Meta::Call { cont: Cont::Ra, mo, a: rb, b: rc })
    }

    /// `lj_carith_checkbit64` for one operand.
    fn checkbit64(&mut self, o: Value, slot: Option<usize>, id: &mut u32) -> LuaResult<u64> {
        match o {
            Value::CData(cd) => match self.rt.ffi.checkbit64 {
                Some(f) => f(self, cd, id),
                None => Err(self.err_optype(o, slot, ErrMsg::OPARITH)),
            },
            Value::Num(n) => Ok(num2bit(n) as i64 as u64),
            _ => Err(self.err_optype(o, slot, ErrMsg::OPARITH)),
        }
    }

    /// `lj_meta_bitop`: bit operators on non-numbers (cdata) or errors. `op` is the `Op`.
    pub(crate) fn meta_bitop(
        &mut self,
        op: crate::bc::Op,
        rb: Value,
        rc: Value,
        bslot: Option<usize>,
        cslot: Option<usize>,
    ) -> LuaResult<Value> {
        use crate::bc::Op;
        let (mut id, mut id_ignore) = (0u32, 0u32);
        let mut b = self.checkbit64(rb, bslot, &mut id)?;
        let shift = (op as u8) >= (Op::BSHL as u8);
        let mut c = if shift { self.checkbit64(rc, cslot, &mut id_ignore)? } else { self.checkbit64(rc, cslot, &mut id)? };
        if id != 0 {
            if let Value::Num(n) = rb {
                b = if id == CTID_UINT64 { crate::text::num2u64(n) } else { crate::text::num2i64(n) as u64 };
            }
            if let Value::Num(n) = rc {
                c = if id == CTID_UINT64 { crate::text::num2u64(n) } else { crate::text::num2i64(n) as u64 };
            }
        }
        let r = match op {
            Op::BNOT => !b,
            Op::BAND => b & c,
            Op::BOR => b | c,
            Op::BXOR => b ^ c,
            _ => {
                if id != 0 {
                    let sh = (c as i32 as u32) & 63;
                    match op {
                        Op::BSHL => b << sh,
                        Op::BSHR => b >> sh,
                        _ => ((b as i64) >> sh) as u64,
                    }
                } else {
                    let sh = (c as u32) & 31;
                    match op {
                        Op::BSHL => ((b as u32) << sh) as u64,
                        Op::BSHR => ((b as u32) >> sh) as u64,
                        _ => (((b as u32 as i32) >> sh) as u32) as u64,
                    }
                }
            }
        };
        if id != 0 {
            let cd = self.new_cdata(if id == CTID_UINT64 { CTID_UINT64 } else { CTID_INT64 }, 8, 8);
            // SAFETY: the cdata owns 8 bytes of 8-byte aligned storage.
            unsafe { (cd.ptr() as *mut u64).write(r) };
            Ok(Value::CData(cd))
        } else {
            Ok(Value::Num(r as u32 as i32 as f64))
        }
    }

    /// `lj_meta_len`: `__len` metamethod (Lua 5.1 rules: only called for non-strings,
    /// non-tables or tables without... — the interpreter handles strings and tables itself).
    pub(crate) fn meta_len(&mut self, o: Value, slot: Option<usize>) -> LuaResult<Meta<Value>> {
        let mo = self.meta_lookup(o, MM::Len);
        if mo.is_nil() {
            return Err(self.err_optype(o, slot, ErrMsg::OPLEN));
        }
        Ok(Meta::Call { cont: Cont::Ra, mo, a: o, b: Value::Nil })
    }

    /// `lj_meta_equal` for two different tables or two different userdata: `None` = result
    /// `ne` without metamethod.
    pub(crate) fn meta_equal(&mut self, o1: Value, o2: Value, ne: bool) -> Option<Meta<bool>> {
        let mt1 = self.metatable_of(o1);
        let mo = self.fast_mm(mt1, MM::Eq)?;
        let mt2 = self.metatable_of(o2);
        if mt1 != mt2 {
            let mo2 = self.fast_mm(mt2, MM::Eq)?;
            if !mo.raw_equal(mo2) {
                return None;
            }
        }
        Some(Meta::Call { cont: if ne { Cont::CondF } else { Cont::CondT }, mo, a: o1, b: o2 })
    }

    /// `lj_meta_equal_cd`: equality with a cdata operand always consults `__eq` of the cdata
    /// (`o1mm`) — `ne` selects the inverted condition. `None` = no metamethod: result `ne`.
    pub(crate) fn meta_equal_cd(&mut self, o1: Value, o2: Value, ne: bool) -> Option<Meta<bool>> {
        let o1mm = if matches!(o1, Value::CData(_)) { o1 } else { o2 };
        let mo = self.meta_lookup(o1mm, MM::Eq);
        if mo.is_nil() {
            return None;
        }
        Some(Meta::Call { cont: if ne { Cont::CondF } else { Cont::CondT }, mo, a: o1, b: o2 })
    }

    /// `lj_meta_comp`: ordered comparison of non-numbers. `op` bit 0 = inverted condition, bit 1
    /// = `<=` (ISLT=0, ISGE=1, ISLE=2, ISGT=3). Done(b) is the branch condition.
    pub(crate) fn meta_comp(&mut self, o1: Value, o2: Value, op: u8) -> LuaResult<Meta<bool>> {
        let (mut o1, mut o2, mut op) = (o1, o2, op);
        if matches!(o1, Value::CData(_)) || matches!(o2, Value::CData(_)) {
            let cont = if op & 1 != 0 { Cont::CondF } else { Cont::CondT };
            let mm = if op & 2 != 0 { MM::Le } else { MM::Lt };
            let mo = self.meta_lookup(if matches!(o1, Value::CData(_)) { o1 } else { o2 }, mm);
            if mo.is_nil() {
                return Err(self.err_comp(o1, o2));
            }
            return Ok(Meta::Call { cont, mo, a: o1, b: o2 });
        }
        let same = o1.vtype() == o2.vtype() || (matches!(o1, Value::Bool(_)) && matches!(o2, Value::Bool(_)));
        if !same {
            return Err(self.err_comp(o1, o2));
        }
        if let (Value::Str(a), Value::Str(b)) = (o1, o2) {
            let ord = a.as_bytes().cmp(b.as_bytes());
            let res = if op & 2 != 0 { ord.is_le() } else { ord.is_lt() };
            return Ok(Meta::Done(res ^ (op & 1 != 0)));
        }
        loop {
            let cont = if op & 1 != 0 { Cont::CondF } else { Cont::CondT };
            let mm = if op & 2 != 0 { MM::Le } else { MM::Lt };
            let mo = self.meta_lookup(o1, mm);
            let mo2 = self.meta_lookup(o2, mm);
            if mo.is_nil() || !mo.raw_equal(mo2) {
                if op & 2 != 0 {
                    std::mem::swap(&mut o1, &mut o2);
                    op ^= 3;
                    continue;
                }
                return Err(self.err_comp(o1, o2));
            }
            return Ok(Meta::Call { cont, mo, a: o1, b: o2 });
        }
    }

    /// `lj_meta_cat` over absolute stack slots `[top - left, top]`. Concatenates as many
    /// strings/numbers as possible from the top; returns `Done(final_slot)` when `left` reaches
    /// 0 (the result is in slot `top - left_initial`), or the `__concat` call for the pair at
    /// `(t-1, t)` with `Cont::Cat { top: t }` (relative slot computed by the caller).
    pub(crate) fn meta_cat(&mut self, top: usize, left: usize) -> LuaResult<Meta<()>> {
        let th = self.cur_thread;
        let mut top = top;
        let mut left = left;
        let iscat = |v: Value| matches!(v, Value::Str(_) | Value::Num(_));
        loop {
            let a = th.get(top - 1);
            let b = th.get(top);
            if !iscat(b) || !iscat(a) {
                let mut mo = self.meta_lookup(a, MM::Concat);
                if mo.is_nil() {
                    mo = self.meta_lookup(b, MM::Concat);
                    if mo.is_nil() {
                        let (o, s) = if iscat(a) { (b, top) } else { (a, top - 1) };
                        return Err(self.err_optype(o, Some(s), ErrMsg::OPCAT));
                    }
                }
                return Ok(Meta::Call { cont: Cont::Cat { top: top as u32 }, mo, a, b });
            }
            // Pick as many strings as possible from the top and concatenate them.
            let mut o = top;
            loop {
                o -= 1;
                left -= 1;
                if left == 0 || !iscat(th.get(o - 1)) {
                    break;
                }
            }
            let mut buf: Vec<u8> = Vec::new();
            for i in o..=top {
                match th.get(i) {
                    Value::Str(s) => buf.extend_from_slice(s.as_bytes()),
                    Value::Num(n) => crate::text::strfmt::put_num(&mut buf, n),
                    _ => unreachable!(),
                }
            }
            if buf.len() >= crate::lstr::MAX_STR {
                return Err(self.err_msg(ErrMsg::STROV));
            }
            let s = self.new_str(&buf);
            th.set(o, Value::Str(s));
            top = o;
            if left == 0 {
                return Ok(Meta::Done(()));
            }
        }
    }

    /// `lj_meta_for` coercion: convert the three `for` slots to numbers or raise the upstream
    /// error.
    pub(crate) fn meta_for(&mut self, slot: usize) -> LuaResult<()> {
        let th = self.cur_thread;
        for (i, em) in [ErrMsg::FORINIT, ErrMsg::FORLIM, ErrMsg::FORSTEP].into_iter().enumerate() {
            match str2num(th.get(slot + i)) {
                Some(n) => th.set(slot + i, Value::Num(n)),
                None => return Err(self.err_msg(em)),
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::floor;

    #[test]
    fn floor_matches_libm() {
        let mut xs = vec![
            0.0,
            -0.0,
            0.5,
            -0.5,
            1.0,
            -1.0,
            2.5,
            -2.5,
            2.7,
            -2.7,
            1e-300,
            -1e-300,
            4503599627370495.5,
            -4503599627370495.5,
            4503599627370496.0,
            -4503599627370497.0,
            1e300,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::MIN_POSITIVE,
            -f64::MIN_POSITIVE,
        ];
        let mut s = 0x1234_5678_9abc_def0u64;
        for _ in 0..100_000 {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            xs.push(f64::from_bits(s));
            xs.push((s % 2_000_001) as f64 / 1000.0 - 1000.0);
        }
        // NaNs are returned unchanged (like upstream), so only their NaN-ness is compared.
        for x in xs.into_iter().filter(|x| !x.is_nan()) {
            assert_eq!(floor(x).to_bits(), x.floor().to_bits(), "floor({x:e})");
        }
        assert!(floor(f64::NAN).is_nan());
    }
}
