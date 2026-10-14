//! The bytecode interpreter: semantics of every LuaJIT 2.1 instruction as implemented by
//! vm_x64.dasc (`build_ins`) for the non-JIT, GC64/FR2, non-DUALNUM configuration, plus the
//! metamethod continuations (`lj_cont_ra`, `lj_cont_nop`, `lj_cont_cat`, `lj_cont_condt`,
//! `lj_cont_condf`).

use crate::bc::{Ins, MM, Op};
use crate::error::{ErrorKind, LuaResult};
use crate::func::{
    Function, KGc, PROTO_CLC_BITS, PROTO_CLCOUNT, PROTO_UV_IMMUTABLE, PROTO_UV_LOCAL, Proto,
};
use crate::gc::Gc;
use crate::state::Vm;
use crate::thread::{Cont, Frame, FrameKind, Thread};
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
        Value::Proto(_) => 7,
        Value::CData(_) => 10,
        Value::Table(_) => 11,
        Value::Udata(_) => 12,
        Value::Num(_) => 13,
    }
}

const ITYPENAME: [&str; 14] = [
    "nil", "boolean", "boolean", "userdata", "string", "upval", "thread", "proto", "function",
    "trace", "cdata", "table", "userdata", "number",
];

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
            Cont::Hook { multres } => {
                self.rt.multres = multres as usize;
                self.rt.hook_skip = true;
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
                        Meta::Call {
                            cont: Cont::Cat { top: nt },
                            mo,
                            a,
                            b,
                        } => {
                            let rel = Cont::Cat {
                                top: nt - base as u32,
                            };
                            return self.mm_call(th, rel, mo, a, b, None);
                        }
                        Meta::Call { .. } => unreachable!("cat continuation"),
                    }
                }
            }
        }
        Ok(Flow::Continue)
    }

    /// A C hook yielded (`lua_yield` inside a line/count hook): add a pseudo-frame that
    /// re-executes the hooked instruction (at `pc - 1`) on resume (`lua_yield` hook path and
    /// `lj_cont_hook`). The yielded values are the top `n` values of the stack.
    fn hook_yield(
        &mut self,
        th: Gc<Thread>,
        fidx: usize,
        pc: usize,
        multres: usize,
        n: Value,
    ) -> Flow {
        th.set_frame_pc(fidx, pc as u32 - 1);
        let func = th.frame(fidx).expect("lua frame").func;
        let mut d = Frame::new(
            FrameKind::Cont(Cont::Hook {
                multres: multres as u32,
            }),
            func,
            th.top() as u32,
            0,
        );
        d.dummy = true;
        th.push_frame(d);
        self.rt.multres = n.as_num().map_or(0, |x| x as usize);
        Flow::Yield
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
    fn new_closure(
        &mut self,
        th: Gc<Thread>,
        pt: Gc<Proto>,
        parent: Gc<Function>,
        base: usize,
    ) -> Gc<Function> {
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

    /// Instruction/return/profile hook dispatch before executing the instruction at `pc - 1`
    /// (`vm_inshook`, `vm_rethook`, `vm_profhook`). Returns the flow to leave the interpreter
    /// with if a C hook yielded.
    #[cold]
    #[inline(never)]
    fn dispatch_hook(
        &mut self,
        th: Gc<Thread>,
        fidx: usize,
        pt: &Gc<Proto>,
        pc: usize,
        op: Op,
    ) -> LuaResult<Option<Flow>> {
        let m = self.rt.hook.mask;
        if m & hook::HOOK_PROFILE != 0
            && hook::PROFILE_PENDING.load(std::sync::atomic::Ordering::Relaxed)
        {
            th.set_frame_pc(fidx, pc as u32);
            self.hook_profile(th, fidx, pt, pc as u32)?;
        }
        if !self.rt.hook.active && !std::mem::take(&mut self.rt.hook_skip) {
            let ins_hook =
                m & (hook::LUA_MASKLINE | hook::LUA_MASKCOUNT) != 0 && (op as u8) < Op::FUNCF as u8;
            if ins_hook || (m & hook::LUA_MASKRET != 0 && op.is_ret()) {
                th.set_frame_pc(fidx, pc as u32);
                let multres = self.rt.multres;
                match self.hook_ins(th, fidx, pt, pc as u32, ins_hook) {
                    Err(e) if e.kind == ErrorKind::Yield => {
                        return Ok(Some(self.hook_yield(th, fidx, pc, multres, e.value)));
                    }
                    r => r?,
                }
            }
        }
        Ok(None)
    }

    /// FORI/FORL with non-number control slots (string operands, or after `debug.setlocal`):
    /// coerce in place (`lj_meta_for`) and return the numbers.
    #[cold]
    fn for_coerce(&mut self, th: Gc<Thread>, ra: usize) -> LuaResult<(f64, f64, f64)> {
        self.meta_for(ra)?;
        let num = |v: Value| v.as_num().expect("coerced by meta_for");
        Ok((num(th.get(ra)), num(th.get(ra + 1)), num(th.get(ra + 2))))
    }

    /// Execute Lua frames on `th` until a run ends, a yield happens, or an error is raised.
    #[allow(clippy::neg_cmp_op_on_partial_ord)] // NaN semantics of ISGE/ISGT.
    pub(crate) fn interp(&mut self, th: Gc<Thread>) -> LuaResult<Flow> {
        'reload: loop {
            let fidx = th.frame_count() - 1;
            let fr = th.frame(fidx).expect("lua frame");
            let func = fr.func;
            let pt = func.proto().expect("lua frame");
            // The frame keeps `func` and thereby `pt` alive while it runs, and prototypes are
            // immutable apart from their flags, so this view stays valid.
            let bc: &[Ins] = &pt.bc;
            let knum: &[f64] = &pt.knum;
            let base = fr.base as usize;
            let mut pc = fr.pc as usize;
            let fs = pt.framesize as usize;

            // Fast slot access through `fp`, a pointer to stack slot `base`, with `flen` slots
            // available from there. `refresh!` reloads both; it must run after anything that
            // may reallocate this thread's stack (growth, natives, hooks, finalizers at GC
            // points) before the next `slot!`/`get!`/`set!`. Helpers that run Lua code are
            // followed by `continue 'reload`, which refreshes.
            let mut fp: *mut Value;
            let mut flen: usize;
            macro_rules! refresh {
                () => {{
                    // SAFETY: short-lived access to obtain the buffer pointer and length.
                    let v = unsafe { th.stack_mut() };
                    fp = v.as_mut_ptr().wrapping_add(base);
                    flen = v.len().saturating_sub(base);
                }};
            }
            refresh!();
            // Reference to slot `base + i` (nil beyond the stack), valid until the next store.
            macro_rules! slot {
                ($i:expr) => {{
                    let i = ($i) as usize;
                    // SAFETY: `base + i` is below the stack length and the buffer was not
                    // reallocated since `refresh!`.
                    if i < flen {
                        unsafe { &*fp.add(i) }
                    } else {
                        &Value::Nil
                    }
                }};
            }
            macro_rules! get {
                ($i:expr) => {
                    *slot!($i)
                };
            }
            macro_rules! set {
                ($i:expr, $v:expr) => {{
                    let v = $v;
                    let i = ($i) as usize;
                    // SAFETY: as for `slot!`.
                    if i < flen {
                        unsafe { *fp.add(i) = v }
                    } else {
                        th.set(base + i, v)
                    }
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
                    pc = (pc as isize + ($j) as isize) as usize
                };
            }
            // Branch to the target of the JMP following a comparison if `cond`.
            macro_rules! cond_jump {
                ($cond:expr) => {{
                    let j = bc[pc].j();
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
                    refresh!();
                }};
            }
            macro_rules! kstr {
                ($d:expr) => {
                    match pt.kgc[($d) as usize] {
                        KGc::Str(s) => s,
                        _ => unreachable!("string constant"),
                    }
                };
            }
            // Result of a metamethod-aware helper: the value, or call the metamethod.
            macro_rules! mm_call {
                ($m:expr, $c:expr) => {
                    match $m {
                        Meta::Done(v) => v,
                        Meta::Call { cont, mo, a, b } => {
                            flow!(self.mm_call(th, cont, mo, a, b, $c))
                        }
                    }
                };
            }
            // Decoded current instruction, declared here so the macros below can refer to it.
            let mut ins: Ins;
            let mut op: Op;
            let mut a: u32;
            // Numeric comparison with fallback to `__lt`/`__le`.
            macro_rules! comp {
                ($f:expr) => {{
                    let (pa, pd) = (slot!(a), slot!(ins.d()));
                    if let Value::Num(x) = *pa
                        && let Value::Num(y) = *pd
                    {
                        cond_jump!(($f)(x, y));
                    } else {
                        let (va, vd) = (*pa, *pd);
                        save!();
                        let c = mm_call!(self.meta_comp(va, vd, op as u8 - Op::ISLT as u8)?, None);
                        cond_jump!(c);
                    }
                }};
            }
            macro_rules! eqv {
                ($ne:expr) => {{
                    let ne: bool = $ne;
                    let (pa, pd) = (slot!(a), slot!(ins.d()));
                    if let Value::Num(x) = *pa
                        && let Value::Num(y) = *pd
                    {
                        cond_jump!((x == y) != ne);
                    } else {
                        let (va, vd) = (*pa, *pd);
                        match (va, vd) {
                            (Value::CData(_), _) | (_, Value::CData(_)) => {
                                save!();
                                match self.meta_equal_cd(va, vd, ne) {
                                    None => cond_jump!(ne),
                                    Some(m) => {
                                        mm_call!(m, None);
                                    }
                                }
                            }
                            _ if va.raw_equal(vd) => cond_jump!(!ne),
                            (Value::Table(_), Value::Table(_))
                            | (Value::Udata(_), Value::Udata(_)) => {
                                save!();
                                match self.meta_equal(va, vd, ne) {
                                    None => cond_jump!(ne),
                                    Some(m) => {
                                        mm_call!(m, None);
                                    }
                                }
                            }
                            _ => cond_jump!(ne),
                        }
                    }
                }};
            }
            // Equality with a constant (string, number or primitive).
            macro_rules! eqk {
                ($ne:expr, $k:expr) => {{
                    let ne: bool = $ne;
                    let va = get!(a);
                    let k: Value = $k;
                    if let Value::CData(_) = va {
                        save!();
                        match self.meta_equal_cd(va, k, ne) {
                            None => cond_jump!(ne),
                            Some(m) => {
                                mm_call!(m, None);
                            }
                        }
                    } else {
                        cond_jump!(va.raw_equal(k) != ne);
                    }
                }};
            }
            // Test (and copy) D: jump if its truthiness is `$want`.
            macro_rules! truth {
                ($want:expr, $copy:expr) => {{
                    let v = get!(ins.d());
                    if v.is_truthy() == $want {
                        if $copy {
                            set!(a, v);
                        }
                        cond_jump!(true);
                    } else {
                        pc += 1;
                    }
                }};
            }
            macro_rules! arith_slow {
                ($mm:expr, $rb:expr, $rc:expr, $bs:expr, $cs:expr) => {{
                    save!();
                    let n = mm_call!(self.meta_arith($mm, $rb, $rc, $bs, $cs)?, None);
                    set!(a, Value::Num(n));
                }};
            }
            // Binary arithmetic: `vn` = variable op number constant, `nv` = constant op
            // variable, `vv` = two variables.
            macro_rules! arith {
                ($mm:expr, $f:expr, vn) => {{
                    let (bb, cc) = (ins.b(), ins.c());
                    let pb = slot!(bb);
                    let y = knum[cc as usize];
                    if let Value::Num(x) = *pb {
                        set!(a, Value::Num(($f)(x, y)));
                    } else {
                        arith_slow!($mm, *pb, Value::Num(y), Some(base + bb as usize), None)
                    }
                }};
                ($mm:expr, $f:expr, nv) => {{
                    let (bb, cc) = (ins.b(), ins.c());
                    let pb = slot!(bb);
                    let x = knum[cc as usize];
                    if let Value::Num(y) = *pb {
                        set!(a, Value::Num(($f)(x, y)));
                    } else {
                        arith_slow!($mm, Value::Num(x), *pb, None, Some(base + bb as usize))
                    }
                }};
                ($mm:expr, $f:expr, vv) => {{
                    let (bb, cc) = (ins.b(), ins.c());
                    let (pb, pc_) = (slot!(bb), slot!(cc));
                    if let Value::Num(x) = *pb
                        && let Value::Num(y) = *pc_
                    {
                        set!(a, Value::Num(($f)(x, y)));
                    } else {
                        arith_slow!(
                            $mm,
                            *pb,
                            *pc_,
                            Some(base + bb as usize),
                            Some(base + cc as usize)
                        )
                    }
                }};
            }
            macro_rules! bitop {
                ($f:expr) => {{
                    let (bb, cc) = (ins.b(), ins.c());
                    let (pb, pc_) = (slot!(bb), slot!(cc));
                    if let Value::Num(x) = *pb
                        && let Value::Num(y) = *pc_
                    {
                        let r: i32 = ($f)(num2bit(x), num2bit(y));
                        set!(a, Value::Num(r as f64));
                    } else {
                        let (rb, rc) = (*pb, *pc_);
                        save!();
                        let r = self.meta_bitop(
                            op,
                            rb,
                            rc,
                            Some(base + bb as usize),
                            Some(base + cc as usize),
                        )?;
                        set!(a, r);
                    }
                }};
            }
            // Table loads: `v` is the raw result of `t[k]` for `o` = table `t`; it is final
            // unless it is nil and the metatable has `__index`.
            macro_rules! tget_slow {
                ($o:expr, $k:expr, $oslot:expr) => {{
                    save!();
                    let v = mm_call!(self.meta_tget($o, $k, $oslot)?, None);
                    set!(a, v);
                }};
            }
            macro_rules! tget_done {
                ($t:expr, $v:expr, $o:expr, $k:expr) => {{
                    let v: Value = $v;
                    if !v.is_nil() {
                        set!(a, v);
                    } else {
                        match self.fast_mm($t.metatable(), MM::Index) {
                            None => set!(a, v),
                            Some(mo) => {
                                save!();
                                let v = mm_call!(self.meta_tget_index($o, mo, $k)?, None);
                                set!(a, v);
                            }
                        }
                    }
                }};
            }
            macro_rules! tset_slow {
                ($o:expr, $k:expr, $v:expr, $oslot:expr) => {{
                    save!();
                    let v = $v;
                    mm_call!(self.meta_tset($o, $k, v, $oslot)?, Some(v));
                }};
            }
            // `nil_ok` check of the table store fast paths: no `__newindex` for table `t`.
            macro_rules! no_newindex {
                ($t:expr) => {
                    || self.fast_mm($t.metatable(), MM::Newindex).is_none()
                };
            }
            macro_rules! fori {
                ($idx:expr, $stop:expr, $step:expr) => {{
                    let (idx, stop, step): (f64, f64, f64) = ($idx, $stop, $step);
                    set!(a + 3, Value::Num(idx));
                    let enter = if step.is_sign_negative() {
                        idx >= stop
                    } else {
                        stop >= idx
                    };
                    if !enter {
                        branch!(ins.j());
                    }
                }};
            }
            macro_rules! forl {
                ($idx:expr, $stop:expr, $step:expr) => {{
                    let (idx, stop, step): (f64, f64, f64) = ($idx, $stop, $step);
                    let idx = idx + step;
                    set!(a, Value::Num(idx));
                    set!(a + 3, Value::Num(idx));
                    let cont = if step.is_sign_negative() {
                        idx >= stop
                    } else {
                        stop >= idx
                    };
                    if cont {
                        branch!(ins.j());
                    }
                }};
            }

            loop {
                ins = bc[pc];
                pc += 1;
                op = Op::from_u8(ins.op_byte()).expect("valid opcode");
                if self.rt.hook.mask != 0 {
                    if let Some(f) = self.dispatch_hook(th, fidx, &pt, pc, op)? {
                        return Ok(f);
                    }
                    refresh!();
                }
                a = ins.a();
                match op {
                    // -- Comparisons -----------------------------------------------------------
                    Op::ISLT => comp!(|x: f64, y: f64| x < y),
                    Op::ISGE => comp!(|x: f64, y: f64| !(x < y)),
                    Op::ISLE => comp!(|x: f64, y: f64| x <= y),
                    Op::ISGT => comp!(|x: f64, y: f64| !(x <= y)),
                    Op::ISEQV => eqv!(false),
                    Op::ISNEV => eqv!(true),
                    Op::ISEQS => eqk!(false, Value::Str(kstr!(ins.d()))),
                    Op::ISNES => eqk!(true, Value::Str(kstr!(ins.d()))),
                    Op::ISEQN => eqk!(false, Value::Num(knum[ins.d() as usize])),
                    Op::ISNEN => eqk!(true, Value::Num(knum[ins.d() as usize])),
                    Op::ISEQP => eqk!(false, kpri(ins.d())),
                    Op::ISNEP => eqk!(true, kpri(ins.d())),

                    // -- Unary test and copy ---------------------------------------------------
                    Op::ISTC => truth!(true, true),
                    Op::ISFC => truth!(false, true),
                    Op::IST => truth!(true, false),
                    Op::ISF => truth!(false, false),
                    Op::ISTYPE | Op::ISNUM => {
                        let v = get!(a);
                        let tp = ins.d();
                        let ok = if op == Op::ISNUM {
                            matches!(v, Value::Num(_))
                        } else {
                            itype_index(v) + 1 == tp
                        };
                        if !ok {
                            save!();
                            th.set_top(base + fs);
                            let narg = a as usize + 1;
                            let idx = if op == Op::ISNUM {
                                13
                            } else {
                                tp.wrapping_sub(1) as usize
                            };
                            if idx >= 13 {
                                self.check_num(narg)?;
                            } else if idx == 4 {
                                self.check_str(narg)?;
                            } else {
                                return Err(self.type_error(
                                    narg as i32,
                                    ITYPENAME.get(idx).copied().unwrap_or("?"),
                                ));
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
                                let s = Some(base + d as usize);
                                arith_slow!(MM::Unm, o, o, s, s)
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
                                let v = mm_call!(self.meta_len(o, Some(base + d as usize))?, None);
                                set!(a, v);
                            }
                        }
                    }

                    // -- Binary arithmetic -----------------------------------------------------
                    Op::ADDVN => arith!(MM::Add, |x: f64, y: f64| x + y, vn),
                    Op::SUBVN => arith!(MM::Sub, |x: f64, y: f64| x - y, vn),
                    Op::MULVN => arith!(MM::Mul, |x: f64, y: f64| x * y, vn),
                    Op::DIVVN => arith!(MM::Div, |x: f64, y: f64| x / y, vn),
                    Op::MODVN => arith!(MM::Mod, modf, vn),
                    Op::ADDNV => arith!(MM::Add, |x: f64, y: f64| x + y, nv),
                    Op::SUBNV => arith!(MM::Sub, |x: f64, y: f64| x - y, nv),
                    Op::MULNV => arith!(MM::Mul, |x: f64, y: f64| x * y, nv),
                    Op::DIVNV => arith!(MM::Div, |x: f64, y: f64| x / y, nv),
                    Op::MODNV => arith!(MM::Mod, modf, nv),
                    Op::ADDVV => arith!(MM::Add, |x: f64, y: f64| x + y, vv),
                    Op::SUBVV => arith!(MM::Sub, |x: f64, y: f64| x - y, vv),
                    Op::MULVV => arith!(MM::Mul, |x: f64, y: f64| x * y, vv),
                    Op::DIVVV => arith!(MM::Div, |x: f64, y: f64| x / y, vv),
                    Op::MODVV => arith!(MM::Mod, modf, vv),
                    Op::POW => arith!(MM::Pow, f64::powf, vv),
                    Op::CAT => {
                        save!();
                        let (bb, cc) = (ins.b() as usize, ins.c() as usize);
                        match self.meta_cat(base + cc, cc - bb)? {
                            Meta::Done(()) => {
                                th.set(base + a as usize, th.get(base + bb));
                                gc_point!();
                            }
                            Meta::Call {
                                cont: Cont::Cat { top },
                                mo,
                                a,
                                b,
                            } => {
                                let c = Cont::Cat {
                                    top: top - base as u32,
                                };
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
                    Op::BAND => bitop!(|x: i32, y: i32| x & y),
                    Op::BOR => bitop!(|x: i32, y: i32| x | y),
                    Op::BXOR => bitop!(|x: i32, y: i32| x ^ y),
                    Op::BSHL => bitop!(|x: i32, y: i32| ((x as u32) << (y as u32 & 31)) as i32),
                    Op::BSHR => bitop!(|x: i32, y: i32| ((x as u32) >> (y as u32 & 31)) as i32),
                    Op::BSAR => bitop!(|x: i32, y: i32| x >> (y as u32 & 31)),

                    // -- Constants -------------------------------------------------------------
                    Op::KSTR => set!(a, Value::Str(kstr!(ins.d()))),
                    Op::KCDATA => {
                        let v = match pt.kgc[ins.d() as usize] {
                            KGc::CData(c) => Value::CData(c),
                            _ => Value::Nil,
                        };
                        set!(a, v);
                    }
                    Op::KSHORT => set!(a, Value::Num(ins.d() as u16 as i16 as f64)),
                    Op::KNUM => set!(a, Value::Num(knum[ins.d() as usize])),
                    Op::KPRI => set!(a, kpri(ins.d())),
                    Op::KNIL => {
                        for i in a..=ins.d() {
                            set!(i, Value::Nil);
                        }
                    }

                    // -- Upvalues and closures -------------------------------------------------
                    Op::UGET => set!(a, func.lua_upvals()[ins.d() as usize].get().get()),
                    Op::USETV => func.lua_upvals()[a as usize].get().set(get!(ins.d())),
                    Op::USETS => func.lua_upvals()[a as usize]
                        .get()
                        .set(Value::Str(kstr!(ins.d()))),
                    Op::USETN => func.lua_upvals()[a as usize]
                        .get()
                        .set(Value::Num(knum[ins.d() as usize])),
                    Op::USETP => func.lua_upvals()[a as usize].get().set(kpri(ins.d())),
                    Op::UCLO => {
                        th.close_upvals((base + a as usize) as u32);
                        branch!(ins.j());
                    }
                    Op::FNEW => {
                        gc_point!();
                        let KGc::Proto(cpt) = pt.kgc[ins.d() as usize] else {
                            unreachable!("FNEW proto")
                        };
                        let f = self.new_closure(th, cpt, func, base);
                        th.set(base + a as usize, Value::Func(f));
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
                        th.set(base + a as usize, Value::Table(t));
                    }
                    Op::TDUP => {
                        gc_point!();
                        let KGc::Table(kt) = pt.kgc[ins.d() as usize] else {
                            unreachable!("TDUP table")
                        };
                        let t = self.dup_table(kt);
                        th.set(base + a as usize, Value::Table(t));
                    }
                    Op::GGET => {
                        let k = kstr!(ins.d());
                        let t = func.env();
                        tget_done!(t, t.get_str(k), Value::Table(t), Value::Str(k))
                    }
                    Op::TGETS => {
                        let bb = ins.b();
                        let po = slot!(bb);
                        let k = kstr!(ins.c());
                        if let Value::Table(t) = *po {
                            tget_done!(t, t.get_str(k), *po, Value::Str(k))
                        } else {
                            tget_slow!(*po, Value::Str(k), Some(base + bb as usize))
                        }
                    }
                    Op::TGETV => {
                        let bb = ins.b();
                        let (po, pk) = (slot!(bb), slot!(ins.c()));
                        if let Value::Table(t) = *po {
                            let v = match *pk {
                                Value::Num(n) => t.get_num(n),
                                Value::Str(s) => t.get_str(s),
                                k => t.get(k),
                            };
                            tget_done!(t, v, *po, *pk)
                        } else {
                            tget_slow!(*po, *pk, Some(base + bb as usize))
                        }
                    }
                    Op::TGETB => {
                        let bb = ins.b();
                        let o = get!(bb);
                        let c = ins.c();
                        match o {
                            Value::Table(t) => {
                                tget_done!(t, t.get_int(c as i32), o, Value::Num(c as f64))
                            }
                            _ => tget_slow!(o, Value::Num(c as f64), Some(base + bb as usize)),
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
                    Op::GSET => {
                        let k = kstr!(ins.d());
                        let t = func.env();
                        let v = get!(a);
                        if !t.store_str(k, v, no_newindex!(t)) {
                            tset_slow!(Value::Table(t), Value::Str(k), v, None);
                        }
                    }
                    Op::TSETS => {
                        let bb = ins.b();
                        let o = get!(bb);
                        let k = kstr!(ins.c());
                        let v = get!(a);
                        if let Value::Table(t) = o
                            && t.store_str(k, v, no_newindex!(t))
                        {
                            continue;
                        }
                        tset_slow!(o, Value::Str(k), v, Some(base + bb as usize));
                    }
                    Op::TSETV => {
                        let bb = ins.b();
                        let (po, pk) = (slot!(bb), slot!(ins.c()));
                        let v = get!(a);
                        if let Value::Table(t) = *po {
                            let done = match *pk {
                                Value::Num(n) => {
                                    let k = num2int(n);
                                    (k as f64 == n && t.store_array(k as u32, v, no_newindex!(t)))
                                        || t.store(Value::Num(n), v, no_newindex!(t))
                                }
                                Value::Str(s) => t.store_str(s, v, no_newindex!(t)),
                                k => t.store(k, v, no_newindex!(t)),
                            };
                            if done {
                                continue;
                            }
                        }
                        tset_slow!(*po, *pk, v, Some(base + bb as usize));
                    }
                    Op::TSETB => {
                        let bb = ins.b();
                        let (o, v) = (get!(bb), get!(a));
                        let c = ins.c();
                        if let Value::Table(t) = o
                            && t.store_array(c, v, no_newindex!(t))
                        {
                            continue;
                        }
                        tset_slow!(o, Value::Num(c as f64), v, Some(base + bb as usize));
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
                        // The parser guarantees a table below the values.
                        let Value::Table(t) = get!(a - 1) else {
                            continue;
                        };
                        let start = knum[ins.d() as usize].to_bits() as u32 as i32;
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
                                let _ =
                                    t.set_int(start.wrapping_add(i as i32), get!(a as usize + i));
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
                        if let Value::Func(f) = get!(a) {
                            match &*f {
                                Function::Lua { proto: cpt, .. } => {
                                    // Fast path of `precall` for Lua functions (BC_FUNCF /
                                    // BC_FUNCV) without call hook or stack growth.
                                    let nres = ins.b() as i32 - 1;
                                    let cbase = slot + 2;
                                    // Frame-link slot: never a GC reference (see `precall`).
                                    set!(a as usize + 1, Value::Nil);
                                    let np = cpt.numparams as usize;
                                    if self.rt.hook.mask & hook::LUA_MASKCALL == 0 {
                                        if !cpt.is_vararg() {
                                            let ntop = cbase + cpt.framesize as usize;
                                            if ntop <= th.max_stack() {
                                                save!();
                                                for i in cbase + nargs..cbase + np {
                                                    th.set(i, Value::Nil);
                                                }
                                                let mut nf = Frame::new(
                                                    FrameKind::Lua,
                                                    f,
                                                    cbase as u32,
                                                    nres,
                                                );
                                                nf.pc = 1;
                                                th.push_frame(nf);
                                                th.set_top(ntop);
                                                continue 'reload;
                                            }
                                        } else {
                                            // Move the function and fixed parameters above the
                                            // varargs.
                                            let nbase = cbase + nargs + 2;
                                            let ntop = nbase + cpt.framesize as usize;
                                            if ntop <= th.max_stack() {
                                                save!();
                                                th.set(nbase - 2, Value::Func(f));
                                                th.set(nbase - 1, Value::Nil);
                                                for i in 0..np {
                                                    if i < nargs {
                                                        th.set(nbase + i, th.get(cbase + i));
                                                        th.set(cbase + i, Value::Nil);
                                                    } else {
                                                        th.set(nbase + i, Value::Nil);
                                                    }
                                                }
                                                let mut nf = Frame::new(
                                                    FrameKind::Lua,
                                                    f,
                                                    nbase as u32,
                                                    nres,
                                                );
                                                nf.vdelta = (nargs + 2) as u32;
                                                nf.pc = 1;
                                                th.push_frame(nf);
                                                th.set_top(ntop);
                                                continue 'reload;
                                            }
                                        }
                                    }
                                }
                                Function::Native { .. } => {
                                    // Natives return here directly; keep the frame state.
                                    save!();
                                    match self.precall(
                                        th,
                                        slot,
                                        nargs,
                                        FrameKind::Lua,
                                        ins.b() as i32 - 1,
                                        true,
                                    )? {
                                        Flow::Continue => {
                                            if th.frame_count() - 1 == fidx {
                                                refresh!();
                                                continue;
                                            }
                                            continue 'reload;
                                        }
                                        f => return Ok(f),
                                    }
                                }
                            }
                        }
                        flow!(self.precall(
                            th,
                            slot,
                            nargs,
                            FrameKind::Lua,
                            ins.b() as i32 - 1,
                            true
                        ))
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
                            let iterl = bc[pc];
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
                            flow!(self.precall(
                                th,
                                slot,
                                2,
                                FrameKind::Lua,
                                ins.b() as i32 - 1,
                                true
                            ))
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
                        let vdelta = th.frame(fidx).expect("lua frame").vdelta as usize;
                        let vstart = base - vdelta + nparams;
                        let vend = base - 2;
                        let nvar = vend.saturating_sub(vstart);
                        let dst = base + a as usize;
                        let b = ins.b() as usize;
                        if b == 0 {
                            if dst + nvar > th.max_stack() {
                                save!();
                                th.set_top(dst);
                                self.grow_stack(th, nvar)?;
                                refresh!();
                            }
                            for i in 0..nvar {
                                th.set(dst + i, th.get(vstart + i));
                            }
                            self.rt.multres = nvar;
                        } else {
                            for i in 0..b - 1 {
                                th.set(
                                    dst + i,
                                    if i < nvar {
                                        th.get(vstart + i)
                                    } else {
                                        Value::Nil
                                    },
                                );
                            }
                        }
                    }

                    // -- Returns ---------------------------------------------------------------
                    Op::RETM | Op::RET | Op::RET0 | Op::RET1 => {
                        let n = match op {
                            Op::RETM => ins.d() as usize + self.rt.multres,
                            _ => ins.d() as usize - 1,
                        };
                        let first = base + a as usize;
                        // Fast path of `ret` for a return to the calling Lua frame without
                        // stack growth.
                        let fr = th.frame(fidx).expect("lua frame");
                        if fr.kind == FrameKind::Lua {
                            let dest = fr.call_slot() as usize;
                            let cnt = if fr.nres >= 0 { fr.nres as usize } else { n };
                            let slen = base + flen;
                            if first + n <= slen && dest + cnt <= slen {
                                let stk = fp.wrapping_sub(base);
                                // SAFETY: `stk` is the start of the stack buffer (`flen` was
                                // derived from `base <= len`), all indexes are below its length
                                // (checked above, and `dest < first`), and it was not
                                // reallocated since `refresh!`. The ascending copy never
                                // overwrites unread sources because `dest < first`.
                                unsafe {
                                    for i in 0..n {
                                        *stk.add(dest + i) = *stk.add(first + i);
                                    }
                                    for i in n..cnt {
                                        *stk.add(dest + i) = Value::Nil;
                                    }
                                }
                                self.rt.multres = n;
                                th.pop_frame();
                                let caller = th.frame(fidx - 1).expect("lua caller");
                                let ctop = caller.base as usize
                                    + caller.func.proto().map_or(0, |p| p.framesize as usize);
                                th.set_top(ctop.max(dest + cnt));
                                continue 'reload;
                            }
                        }
                        flow!(self.ret(th, first, n))
                    }

                    // -- Loops and branches ----------------------------------------------------
                    Op::FORI | Op::JFORI => {
                        if let Value::Num(idx) = get!(a)
                            && let Value::Num(stop) = get!(a + 1)
                            && let Value::Num(step) = get!(a + 2)
                        {
                            fori!(idx, stop, step)
                        } else {
                            save!();
                            let (idx, stop, step) = self.for_coerce(th, base + a as usize)?;
                            fori!(idx, stop, step)
                        }
                    }
                    Op::FORL | Op::IFORL | Op::JFORL => {
                        // The slots can only be non-numbers after `debug.setlocal`.
                        if let Value::Num(idx) = get!(a)
                            && let Value::Num(stop) = get!(a + 1)
                            && let Value::Num(step) = get!(a + 2)
                        {
                            forl!(idx, stop, step)
                        } else {
                            save!();
                            let (idx, stop, step) = self.for_coerce(th, base + a as usize)?;
                            forl!(idx, stop, step)
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
                    Op::FUNCF
                    | Op::IFUNCF
                    | Op::JFUNCF
                    | Op::FUNCV
                    | Op::IFUNCV
                    | Op::JFUNCV
                    | Op::FUNCC
                    | Op::FUNCCW => {
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
