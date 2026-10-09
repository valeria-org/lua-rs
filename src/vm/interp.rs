//! The bytecode interpreter: semantics of every LuaJIT 2.1 instruction as implemented by
//! vm_x64.dasc (`build_ins`) for the non-JIT, GC64/FR2, non-DUALNUM configuration, plus the
//! metamethod continuations (`lj_cont_ra`, `lj_cont_nop`, `lj_cont_cat`, `lj_cont_condt`,
//! `lj_cont_condf`).

use crate::bc::{Ins, MM, Op};
use crate::error::LuaResult;
use crate::func::{
    Function, KGc, PROTO_CLC_BITS, PROTO_CLCOUNT, PROTO_UV_IMMUTABLE, PROTO_UV_LOCAL, Proto,
};
use crate::gc::Gc;
use crate::state::Vm;
use crate::thread::{Cont, FrameKind, Thread};
use crate::value::Value;

use super::call::Flow;
use super::meta::{Meta, arith_op, modf, num2bit, num2int};
use super::{ffid, hook};

/// Marker in the control slot of a specialized `pairs`/`next` loop (`LJ_KEYINDEX`): the high
/// word tags the value, the low word is the traversal index.
const KEYINDEX_HI: u64 = 0xfffe_7fff;

#[inline]
fn keyindex(idx: u32) -> Value {
    Value::Num(f64::from_bits((KEYINDEX_HI << 32) | idx as u64))
}

#[inline]
fn as_keyindex(v: Value) -> Option<u32> {
    match v {
        Value::Num(n) if n.to_bits() >> 32 == KEYINDEX_HI => Some(n.to_bits() as u32),
        _ => None,
    }
}

#[inline]
fn kpri(d: u32) -> Value {
    match d {
        0 => Value::Nil,
        1 => Value::Bool(false),
        _ => Value::Bool(true),
    }
}

/// Index into upstream's `lj_obj_itypename` for ISTYPE checks.
fn itype_index(v: Value) -> u32 {
    match v {
        Value::Nil => 0,
        Value::Bool(false) => 1,
        Value::Bool(true) => 2,
        Value::LightUd(_) => 3,
        Value::Str(_) => 4,
        Value::Thread(_) => 6,
        Value::Func(_) => 8,
        Value::CData(_) => 10,
        Value::Table(_) => 11,
        Value::Udata(_) => 12,
        Value::Num(_) => 13,
    }
}

const ITYPENAME: [&str; 14] = [
    "nil", "boolean", "boolean", "userdata", "string", "upval", "thread", "proto", "function", "trace", "cdata",
    "table", "userdata", "number",
];

#[inline]
fn kstr(pt: &Proto, d: u32) -> Value {
    match pt.kgc[d as usize] {
        KGc::Str(s) => Value::Str(s),
        _ => Value::Nil,
    }
}

impl Vm {
    /// Apply a metamethod continuation to the (now top) Lua frame with result `res`.
    pub(crate) fn apply_cont(&mut self, th: Gc<Thread>, c: Cont, res: Value) -> LuaResult<Flow> {
        let fidx = th.frame_count() - 1;
        let fr = th.frame(fidx).expect("lua frame");
        let pt = fr.func.proto().expect("lua frame");
        let base = fr.base as usize;
        let pc = fr.pc as usize;
        let ins = pt.bc[pc - 1];
        match c {
            Cont::Ra => th.set(base + ins.a() as usize, res),
            Cont::Nop => {}
            Cont::CondT | Cont::CondF => {
                let jmp = pt.bc[pc];
                let mut npc = pc as i64 + 1;
                if res.is_truthy() == (c == Cont::CondT) {
                    npc += jmp.j() as i64;
                }
                th.set_frame_pc(fidx, npc as u32);
            }
            Cont::Cat { top } => {
                let t = base + top as usize;
                let b = base + ins.b() as usize;
                if t - 1 == b {
                    th.set(base + ins.a() as usize, res);
                } else {
                    th.set(t - 1, res);
                    match self.meta_cat(t - 1, t - 1 - b)? {
                        Meta::Done(()) => {
                            th.set(base + ins.a() as usize, th.get(b));
                            th.set_top(base + pt.framesize as usize);
                            self.gc_safe();
                        }
                        Meta::Call { cont: Cont::Cat { top: nt }, mo, a, b } => {
                            let rel = Cont::Cat { top: nt - base as u32 };
                            return self.mm_call(th, rel, mo, a, b, None);
                        }
                        Meta::Call { .. } => unreachable!("cat continuation"),
                    }
                }
            }
        }
        Ok(Flow::Continue)
    }

    /// Resolve `__call` for the object in `slot` (in place, `lj_meta_call`); returns the new
    /// argument count.
    fn resolve_call(&mut self, th: Gc<Thread>, slot: usize, nargs: usize) -> LuaResult<usize> {
        let mut nargs = nargs;
        loop {
            let o = th.get(slot);
            if let Value::Func(_) = o {
                return Ok(nargs);
            }
            let mo = self.meta_lookup(o, MM::Call);
            if !matches!(mo, Value::Func(_)) {
                return Err(self.err_optype(o, Some(slot), super::err::ErrMsg::OPCALL));
            }
            let top = slot + 2 + nargs;
            if top + 1 > th.stack_size() {
                self.grow_stack(th, 1)?;
            }
            for p in (slot + 3..=top).rev() {
                th.set(p, th.get(p - 1));
            }
            th.set(slot + 2, o);
            th.set(slot, mo);
            nargs += 1;
        }
    }

    /// Create a closure for prototype `pt` inside Lua function `parent` (`lj_func_newL_gc`).
    fn new_closure(&mut self, th: Gc<Thread>, pt: Gc<Proto>, parent: Gc<Function>, base: usize) -> Gc<Function> {
        let mut uvs = Vec::with_capacity(pt.uv.len());
        for &v in pt.uv.iter() {
            if v & PROTO_UV_LOCAL != 0 {
                let uv = self.find_upval(th, (base + (v & 0xff) as usize) as u32);
                uv.set_immutable(v & PROTO_UV_IMMUTABLE != 0);
                uvs.push(uv);
            } else {
                uvs.push(parent.upval(v as usize).expect("parent upvalue"));
            }
        }
        closure_count(&pt);
        self.new_lua_function(pt, uvs, parent.env())
    }

    /// Execute Lua frames on `th` until a run ends, a yield happens, or an error is raised.
    #[allow(clippy::neg_cmp_op_on_partial_ord)] // NaN semantics of ISGE/ISGT.
    pub(crate) fn interp(&mut self, th: Gc<Thread>) -> LuaResult<Flow> {
        'reload: loop {
            let fidx = th.frame_count() - 1;
            let fr = th.frame(fidx).expect("lua frame");
            let func = fr.func;
            let pt = func.proto().expect("lua frame");
            let base = fr.base as usize;
            let mut pc = fr.pc as usize;
            let fs = pt.framesize as usize;

            macro_rules! get {
                ($i:expr) => {
                    th.get(base + ($i) as usize)
                };
            }
            macro_rules! set {
                ($i:expr, $v:expr) => {{
                    let v = $v;
                    th.set(base + ($i) as usize, v)
                }};
            }
            macro_rules! save {
                () => {
                    th.set_frame_pc(fidx, pc as u32)
                };
            }
            macro_rules! flow {
                ($e:expr) => {{
                    save!();
                    match $e? {
                        Flow::Continue => continue 'reload,
                        f => return Ok(f),
                    }
                }};
            }
            macro_rules! flow_nosave {
                ($e:expr) => {{
                    match $e? {
                        Flow::Continue => continue 'reload,
                        f => return Ok(f),
                    }
                }};
            }
            macro_rules! branch {
                ($j:expr) => {
                    pc = (pc as i64 + ($j) as i64) as usize
                };
            }
            // Branch to the target of the JMP following a comparison if `cond`.
            macro_rules! cond_jump {
                ($cond:expr) => {{
                    let j = pt.bc[pc].j();
                    pc += 1;
                    if $cond {
                        branch!(j);
                    }
                }};
            }
            macro_rules! gc_point {
                () => {{
                    save!();
                    th.set_top(base + fs);
                    self.gc_safe();
                }};
            }

            loop {
                let ins: Ins = pt.bc[pc];
                pc += 1;
                let opb = ins.op_byte();
                let op = Op::from_u8(opb).expect("valid opcode");
                if self.rt.hook.mask != 0 && !self.rt.hook.active {
                    let m = self.rt.hook.mask;
                    let ins_hook = m & (hook::LUA_MASKLINE | hook::LUA_MASKCOUNT) != 0 && opb < Op::FUNCF as u8;
                    if ins_hook || (m & hook::LUA_MASKRET != 0 && op.is_ret()) {
                        save!();
                        self.hook_ins(th, fidx, &pt, pc as u32, ins_hook)?;
                    }
                }
                let a = ins.a();
                match op {
                    // -- Comparisons -----------------------------------------------------------
                    Op::ISLT | Op::ISGE | Op::ISLE | Op::ISGT => {
                        let (va, vd) = (get!(a), get!(ins.d()));
                        if let (Value::Num(x), Value::Num(y)) = (va, vd) {
                            let c = match op {
                                Op::ISLT => x < y,
                                Op::ISGE => !(x < y),
                                Op::ISLE => x <= y,
                                _ => !(x <= y),
                            };
                            cond_jump!(c);
                        } else {
                            save!();
                            match self.meta_comp(va, vd, opb - Op::ISLT as u8)? {
                                Meta::Done(c) => cond_jump!(c),
                                Meta::Call { cont, mo, a, b } => flow!(self.mm_call(th, cont, mo, a, b, None)),
                            }
                        }
                    }
                    Op::ISEQV | Op::ISNEV => {
                        let ne = op == Op::ISNEV;
                        let (va, vd) = (get!(a), get!(ins.d()));
                        match (va, vd) {
                            (Value::Num(x), Value::Num(y)) => cond_jump!((x == y) != ne),
                            (Value::CData(_), _) | (_, Value::CData(_)) => {
                                save!();
                                match self.meta_equal_cd(va, vd, ne) {
                                    None => cond_jump!(ne),
                                    Some(Meta::Call { cont, mo, a, b }) => flow!(self.mm_call(th, cont, mo, a, b, None)),
                                    Some(Meta::Done(_)) => unreachable!(),
                                }
                            }
                            _ if va.raw_equal(vd) => cond_jump!(!ne),
                            (Value::Table(_), Value::Table(_)) | (Value::Udata(_), Value::Udata(_)) => {
                                save!();
                                match self.meta_equal(va, vd, ne) {
                                    None => cond_jump!(ne),
                                    Some(Meta::Call { cont, mo, a, b }) => flow!(self.mm_call(th, cont, mo, a, b, None)),
                                    Some(Meta::Done(_)) => unreachable!(),
                                }
                            }
                            _ => cond_jump!(ne),
                        }
                    }
                    Op::ISEQS | Op::ISNES | Op::ISEQN | Op::ISNEN | Op::ISEQP | Op::ISNEP => {
                        let ne = opb & 1 != 0;
                        let va = get!(a);
                        let d = ins.d();
                        let k = match op {
                            Op::ISEQS | Op::ISNES => kstr(&pt, d),
                            Op::ISEQN | Op::ISNEN => Value::Num(pt.knum[d as usize]),
                            _ => kpri(d),
                        };
                        if let Value::CData(_) = va {
                            save!();
                            match self.meta_equal_cd(va, k, ne) {
                                None => cond_jump!(ne),
                                Some(Meta::Call { cont, mo, a, b }) => flow!(self.mm_call(th, cont, mo, a, b, None)),
                                Some(Meta::Done(_)) => unreachable!(),
                            }
                        } else {
                            cond_jump!(va.raw_equal(k) != ne);
                        }
                    }

                    // -- Unary test and copy ---------------------------------------------------
                    Op::ISTC | Op::ISFC | Op::IST | Op::ISF => {
                        let v = get!(ins.d());
                        let t = v.is_truthy();
                        let want = matches!(op, Op::ISTC | Op::IST);
                        if t == want {
                            if matches!(op, Op::ISTC | Op::ISFC) {
                                set!(a, v);
                            }
                            cond_jump!(true);
                        } else {
                            pc += 1;
                        }
                    }
                    Op::ISTYPE | Op::ISNUM => {
                        let v = get!(a);
                        let tp = ins.d();
                        let ok = if op == Op::ISNUM { matches!(v, Value::Num(_)) } else { itype_index(v) + 1 == tp };
                        if !ok {
                            save!();
                            th.set_top(base + fs);
                            let narg = a as usize + 1;
                            let idx = if op == Op::ISNUM { 13 } else { tp.wrapping_sub(1) as usize };
                            if idx >= 13 {
                                self.check_num(narg)?;
                            } else if idx == 4 {
                                self.check_str(narg)?;
                            } else {
                                return Err(self.type_error(narg as i32, ITYPENAME.get(idx).copied().unwrap_or("?")));
                            }
                        }
                    }

                    // -- Unary ops -------------------------------------------------------------
                    Op::MOV => set!(a, get!(ins.d())),
                    Op::NOT => set!(a, Value::Bool(get!(ins.d()).is_falsy())),
                    Op::UNM => {
                        let d = ins.d();
                        match get!(d) {
                            Value::Num(x) => set!(a, Value::Num(-x)),
                            o => {
                                save!();
                                let s = Some(base + d as usize);
                                match self.meta_arith(MM::Unm, o, o, s, s)? {
                                    Meta::Done(n) => set!(a, Value::Num(n)),
                                    Meta::Call { cont, mo, a, b } => flow!(self.mm_call(th, cont, mo, a, b, None)),
                                }
                            }
                        }
                    }
                    Op::LEN => {
                        let d = ins.d();
                        match get!(d) {
                            Value::Str(s) => set!(a, Value::Num(s.len() as f64)),
                            Value::Table(t) => set!(a, Value::Num(t.len() as f64)),
                            o => {
                                save!();
                                match self.meta_len(o, Some(base + d as usize))? {
                                    Meta::Done(v) => set!(a, v),
                                    Meta::Call { cont, mo, a, b } => flow!(self.mm_call(th, cont, mo, a, b, None)),
                                }
                            }
                        }
                    }

                    // -- Binary arithmetic -----------------------------------------------------
                    Op::ADDVN | Op::SUBVN | Op::MULVN | Op::DIVVN | Op::MODVN | Op::ADDNV | Op::SUBNV
                    | Op::MULNV | Op::DIVNV | Op::MODNV | Op::ADDVV | Op::SUBVV | Op::MULVV | Op::DIVVV
                    | Op::MODVV | Op::POW => {
                        let (bb, cc) = (ins.b(), ins.c());
                        let (rb, rc, bs, cs) = if op == Op::POW {
                            (get!(bb), get!(cc), Some(base + bb as usize), Some(base + cc as usize))
                        } else {
                            match (opb - Op::ADDVN as u8) / 5 {
                                0 => (get!(bb), Value::Num(pt.knum[cc as usize]), Some(base + bb as usize), None),
                                1 => (Value::Num(pt.knum[cc as usize]), get!(bb), None, Some(base + bb as usize)),
                                _ => (get!(bb), get!(cc), Some(base + bb as usize), Some(base + cc as usize)),
                            }
                        };
                        let mm = op.mm().expect("arith mm");
                        if let (Value::Num(x), Value::Num(y)) = (rb, rc) {
                            let r = match mm {
                                MM::Add => x + y,
                                MM::Sub => x - y,
                                MM::Mul => x * y,
                                MM::Div => x / y,
                                MM::Mod => modf(x, y),
                                _ => x.powf(y),
                            };
                            set!(a, Value::Num(r));
                        } else {
                            save!();
                            match self.meta_arith(mm, rb, rc, bs, cs)? {
                                Meta::Done(n) => set!(a, Value::Num(n)),
                                Meta::Call { cont, mo, a, b } => flow!(self.mm_call(th, cont, mo, a, b, None)),
                            }
                        }
                    }
                    Op::CAT => {
                        save!();
                        let (bb, cc) = (ins.b() as usize, ins.c() as usize);
                        match self.meta_cat(base + cc, cc - bb)? {
                            Meta::Done(()) => {
                                set!(a, get!(bb));
                                gc_point!();
                            }
                            Meta::Call { cont: Cont::Cat { top }, mo, a, b } => {
                                let c = Cont::Cat { top: top - base as u32 };
                                flow!(self.mm_call(th, c, mo, a, b, None))
                            }
                            Meta::Call { .. } => unreachable!(),
                        }
                    }

                    // -- Bit ops ---------------------------------------------------------------
                    Op::BNOT => {
                        let d = ins.d();
                        match get!(d) {
                            Value::Num(x) => set!(a, Value::Num(!num2bit(x) as f64)),
                            o => {
                                save!();
                                let s = Some(base + d as usize);
                                let r = self.meta_bitop(op, o, o, s, s)?;
                                set!(a, r);
                            }
                        }
                    }
                    Op::BAND | Op::BOR | Op::BXOR | Op::BSHL | Op::BSHR | Op::BSAR => {
                        let (bb, cc) = (ins.b(), ins.c());
                        let (rb, rc) = (get!(bb), get!(cc));
                        if let (Value::Num(x), Value::Num(y)) = (rb, rc) {
                            let (x, y) = (num2bit(x), num2bit(y));
                            let sh = (y as u32) & 31;
                            let r = match op {
                                Op::BAND => x & y,
                                Op::BOR => x | y,
                                Op::BXOR => x ^ y,
                                Op::BSHL => ((x as u32) << sh) as i32,
                                Op::BSHR => ((x as u32) >> sh) as i32,
                                _ => x >> sh,
                            };
                            set!(a, Value::Num(r as f64));
                        } else {
                            save!();
                            let r = self.meta_bitop(op, rb, rc, Some(base + bb as usize), Some(base + cc as usize))?;
                            set!(a, r);
                        }
                    }

                    // -- Constants -------------------------------------------------------------
                    Op::KSTR => set!(a, kstr(&pt, ins.d())),
                    Op::KCDATA => {
                        let v = match pt.kgc[ins.d() as usize] {
                            KGc::CData(c) => Value::CData(c),
                            _ => Value::Nil,
                        };
                        set!(a, v);
                    }
                    Op::KSHORT => set!(a, Value::Num(ins.d() as u16 as i16 as f64)),
                    Op::KNUM => set!(a, Value::Num(pt.knum[ins.d() as usize])),
                    Op::KPRI => set!(a, kpri(ins.d())),
                    Op::KNIL => {
                        for i in a..=ins.d() {
                            set!(i, Value::Nil);
                        }
                    }

                    // -- Upvalues and closures -------------------------------------------------
                    Op::UGET => set!(a, func.upval(ins.d() as usize).expect("upvalue").get()),
                    Op::USETV => func.upval(a as usize).expect("upvalue").set(get!(ins.d())),
                    Op::USETS => func.upval(a as usize).expect("upvalue").set(kstr(&pt, ins.d())),
                    Op::USETN => func.upval(a as usize).expect("upvalue").set(Value::Num(pt.knum[ins.d() as usize])),
                    Op::USETP => func.upval(a as usize).expect("upvalue").set(kpri(ins.d())),
                    Op::UCLO => {
                        th.close_upvals((base + a as usize) as u32);
                        branch!(ins.j());
                    }
                    Op::FNEW => {
                        gc_point!();
                        let KGc::Proto(cpt) = pt.kgc[ins.d() as usize] else { unreachable!("FNEW proto") };
                        let f = self.new_closure(th, cpt, func, base);
                        set!(a, Value::Func(f));
                    }

                    // -- Tables ----------------------------------------------------------------
                    Op::TNEW => {
                        gc_point!();
                        let d = ins.d();
                        let mut asize = d & 0x7ff;
                        if asize == 0x7ff {
                            asize = 0x801;
                        }
                        let t = self.new_table(asize, d >> 11)?;
                        set!(a, Value::Table(t));
                    }
                    Op::TDUP => {
                        gc_point!();
                        let KGc::Table(kt) = pt.kgc[ins.d() as usize] else { unreachable!("TDUP table") };
                        let t = self.dup_table(kt);
                        set!(a, Value::Table(t));
                    }
                    Op::GGET | Op::TGETV | Op::TGETS | Op::TGETB => {
                        let (o, k, oslot) = match op {
                            Op::GGET => (Value::Table(func.env()), kstr(&pt, ins.d()), None),
                            Op::TGETV => (get!(ins.b()), get!(ins.c()), Some(base + ins.b() as usize)),
                            Op::TGETS => (get!(ins.b()), kstr(&pt, ins.c()), Some(base + ins.b() as usize)),
                            _ => (get!(ins.b()), Value::Num(ins.c() as f64), Some(base + ins.b() as usize)),
                        };
                        if let Value::Table(t) = o {
                            let v = t.get(k);
                            if !v.is_nil() || self.fast_mm(t.metatable(), MM::Index).is_none() {
                                set!(a, v);
                                continue;
                            }
                        }
                        save!();
                        match self.meta_tget(o, k, oslot)? {
                            Meta::Done(v) => set!(a, v),
                            Meta::Call { cont, mo, a, b } => flow!(self.mm_call(th, cont, mo, a, b, None)),
                        }
                    }
                    Op::TGETR => {
                        let v = match get!(ins.b()) {
                            Value::Table(t) => {
                                let k = match get!(ins.c()) {
                                    Value::Num(n) => num2int(n),
                                    _ => i32::MIN,
                                };
                                t.get_int(k)
                            }
                            _ => Value::Nil,
                        };
                        set!(a, v);
                    }
                    Op::GSET | Op::TSETV | Op::TSETS | Op::TSETB => {
                        let (o, k, oslot) = match op {
                            Op::GSET => (Value::Table(func.env()), kstr(&pt, ins.d()), None),
                            Op::TSETV => (get!(ins.b()), get!(ins.c()), Some(base + ins.b() as usize)),
                            Op::TSETS => (get!(ins.b()), kstr(&pt, ins.c()), Some(base + ins.b() as usize)),
                            _ => (get!(ins.b()), Value::Num(ins.c() as f64), Some(base + ins.b() as usize)),
                        };
                        let v = get!(a);
                        if let Value::Table(t) = o
                            && !t.get(k).is_nil()
                        {
                            t.set_existing(k, v);
                            continue;
                        }
                        save!();
                        match self.meta_tset(o, k, v, oslot)? {
                            Meta::Done(()) => {}
                            Meta::Call { cont, mo, a, b } => flow!(self.mm_call(th, cont, mo, a, b, Some(v))),
                        }
                    }
                    Op::TSETR => {
                        if let Value::Table(t) = get!(ins.b()) {
                            let k = match get!(ins.c()) {
                                Value::Num(n) => num2int(n),
                                _ => i32::MIN,
                            };
                            let v = get!(a);
                            if t.set_int(k, v).is_err() {
                                save!();
                                return Err(self.err_msg(super::err::ErrMsg::TABOV));
                            }
                        }
                    }
                    Op::TSETM => {
                        let Value::Table(t) = get!(a - 1) else { unreachable!("TSETM table") };
                        let start = pt.knum[ins.d() as usize].to_bits() as u32 as i32;
                        let n = self.rt.multres;
                        if n > 0 {
                            let need = start as i64 + n as i64 - 1;
                            if need >= t.asize() as i64 {
                                save!();
                                if let Err(e) = t.reasize((need + 1) as u32) {
                                    return Err(self.tab_error(e));
                                }
                            }
                            for i in 0..n {
                                let _ = t.set_int(start.wrapping_add(i as i32), get!(a as usize + i));
                            }
                        }
                    }

                    // -- Calls and varargs -----------------------------------------------------
                    Op::CALL | Op::CALLM => {
                        let mut nargs = ins.c() as usize;
                        if op == Op::CALL {
                            nargs -= 1;
                        } else {
                            nargs += self.rt.multres;
                        }
                        let slot = base + a as usize;
                        if let Value::Func(f) = th.get(slot)
                            && !f.is_lua()
                        {
                            // Natives return here directly; keep the frame state.
                            save!();
                            match self.precall(th, slot, nargs, FrameKind::Lua, ins.b() as i32 - 1, true)? {
                                Flow::Continue => {
                                    if th.frame_count() - 1 == fidx {
                                        continue;
                                    }
                                    continue 'reload;
                                }
                                f => return Ok(f),
                            }
                        }
                        flow!(self.precall(th, slot, nargs, FrameKind::Lua, ins.b() as i32 - 1, true))
                    }
                    Op::CALLT | Op::CALLMT => {
                        let mut nargs = ins.d() as usize;
                        if op == Op::CALLT {
                            nargs -= 1;
                        } else {
                            nargs += self.rt.multres;
                        }
                        let slot = base + a as usize;
                        save!();
                        let nargs = self.resolve_call(th, slot, nargs)?;
                        let cur = th.pop_frame().expect("lua frame");
                        let dest = cur.call_slot() as usize;
                        for i in 0..nargs + 2 {
                            th.set(dest + i, th.get(slot + i));
                        }
                        th.set(dest + 1, Value::Nil);
                        th.set_top(dest + 2 + nargs);
                        flow_nosave!(self.precall(th, dest, nargs, cur.kind, cur.nres, true))
                    }
                    Op::ITERC => {
                        let slot = base + a as usize;
                        th.set(slot, th.get(slot - 3));
                        th.set(slot + 1, Value::Nil);
                        th.set(slot + 2, th.get(slot - 2));
                        th.set(slot + 3, th.get(slot - 1));
                        th.set_top(slot + 4);
                        flow!(self.precall(th, slot, 2, FrameKind::Lua, ins.b() as i32 - 1, true))
                    }
                    Op::ITERN => {
                        if let Some(idx) = as_keyindex(get!(a - 1))
                            && let Value::Table(t) = get!(a - 2)
                        {
                            let iterl = pt.bc[pc];
                            pc += 1;
                            if let Some((nidx, k, v)) = t.next_at(idx) {
                                set!(a, k);
                                set!(a + 1, v);
                                set!(a - 1, keyindex(nidx));
                                branch!(iterl.j());
                            }
                        } else {
                            // Despecialized (ISNEXT failed): behaves like ITERC.
                            let slot = base + a as usize;
                            th.set(slot, th.get(slot - 3));
                            th.set(slot + 1, Value::Nil);
                            th.set(slot + 2, th.get(slot - 2));
                            th.set(slot + 3, th.get(slot - 1));
                            th.set_top(slot + 4);
                            flow!(self.precall(th, slot, 2, FrameKind::Lua, ins.b() as i32 - 1, true))
                        }
                    }
                    Op::ISNEXT => {
                        let g = get!(a - 3);
                        if let Value::Func(f) = g
                            && f.ffid() == ffid::FF_NEXT
                            && matches!(get!(a - 2), Value::Table(_))
                            && get!(a - 1).is_nil()
                        {
                            set!(a - 1, keyindex(0));
                        }
                        branch!(ins.j());
                    }
                    Op::VARG => {
                        let nparams = ins.c() as usize;
                        let fr = th.frame(fidx).expect("frame");
                        let vstart = base - fr.vdelta as usize + nparams;
                        let vend = base - 2;
                        let nvar = vend.saturating_sub(vstart);
                        let dst = base + a as usize;
                        let b = ins.b() as usize;
                        if b == 0 {
                            if dst + nvar > th.max_stack() {
                                save!();
                                th.set_top(dst);
                                self.grow_stack(th, nvar)?;
                            }
                            for i in 0..nvar {
                                th.set(dst + i, th.get(vstart + i));
                            }
                            self.rt.multres = nvar;
                        } else {
                            for i in 0..b - 1 {
                                th.set(dst + i, if i < nvar { th.get(vstart + i) } else { Value::Nil });
                            }
                        }
                    }

                    // -- Returns ---------------------------------------------------------------
                    Op::RETM | Op::RET | Op::RET0 | Op::RET1 => {
                        let n = match op {
                            Op::RETM => ins.d() as usize + self.rt.multres,
                            _ => ins.d() as usize - 1,
                        };
                        flow!(self.ret(th, base + a as usize, n))
                    }

                    // -- Loops and branches ----------------------------------------------------
                    Op::FORI | Op::JFORI => {
                        let ra = base + a as usize;
                        if !matches!((th.get(ra), th.get(ra + 1), th.get(ra + 2)), (Value::Num(_), Value::Num(_), Value::Num(_))) {
                            save!();
                            self.meta_for(ra)?;
                        }
                        let (Value::Num(idx), Value::Num(stop), Value::Num(step)) = (th.get(ra), th.get(ra + 1), th.get(ra + 2))
                        else {
                            unreachable!()
                        };
                        th.set(ra + 3, Value::Num(idx));
                        let enter = if step.is_sign_negative() { idx >= stop } else { stop >= idx };
                        if !enter {
                            branch!(ins.j());
                        }
                    }
                    Op::FORL | Op::IFORL | Op::JFORL => {
                        let ra = base + a as usize;
                        let (Value::Num(idx), Value::Num(stop), Value::Num(step)) = (th.get(ra), th.get(ra + 1), th.get(ra + 2))
                        else {
                            unreachable!("FORL slots are numbers")
                        };
                        let idx = idx + step;
                        th.set(ra, Value::Num(idx));
                        th.set(ra + 3, Value::Num(idx));
                        let cont = if step.is_sign_negative() { idx >= stop } else { stop >= idx };
                        if cont {
                            branch!(ins.j());
                        }
                    }
                    Op::ITERL | Op::IITERL | Op::JITERL => {
                        let v = get!(a);
                        if !v.is_nil() {
                            set!(a - 1, v);
                            branch!(ins.j());
                        }
                    }
                    Op::LOOP | Op::ILOOP | Op::JLOOP => {}
                    Op::JMP => branch!(ins.j()),
                    Op::FUNCF | Op::IFUNCF | Op::JFUNCF | Op::FUNCV | Op::IFUNCV | Op::JFUNCV | Op::FUNCC | Op::FUNCCW => {
                        unreachable!("function header executed")
                    }
                }
            }
        }
    }
}

/// Saturating 3-bit closure counter of `func_newL`.
pub(crate) fn closure_count(pt: &Proto) {
    let count = pt.flags() as u32 + PROTO_CLCOUNT as u32;
    pt.set_flags((count - ((count >> PROTO_CLC_BITS) & PROTO_CLCOUNT as u32)) as u8);
}

#[allow(dead_code)]
fn _arith(mm: MM, x: f64, y: f64) -> f64 {
    arith_op(mm, x, y)
}
