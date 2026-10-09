//! Calls, returns, interpreter runs, error unwinding and error handlers.
//! Ports the frame handling of vm_x64.dasc (`ins_call`, `BC_FUNCF/FUNCV/FUNCC`, `vm_return`,
//! `vm_returnp`, `vmeta_call`), `lj_meta_call`, `lj_err_run`/`finderrfunc`/`err_unwind`
//! (lj_err.c), `lj_state_growstack` error paths, `gc_call_finalizer` (lj_gc.c) and `lua_close`.

use crate::bc::MM;
use crate::error::{ErrorKind, LuaError, LuaResult};
use crate::func::{Function, NativeImpl};
use crate::gc::Gc;
use crate::state::{GC_MAX_MEM, Vm};
use crate::thread::{Cont, ErrFunc, Frame, FrameKind, LJ_STACK_MIN, StackOverflow, Thread};
use crate::value::Value;

use super::err::{ErrMsg, fmt_msg};
use super::{LJ_MAX_CCALLS, Run, TAILCALL, ffid, hook};

/// What the interpreter does next after a call/return step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Flow {
    /// Continue with the (possibly new) top frame, which is a Lua frame.
    Continue,
    /// The run's entry frame returned this many results (at its call slot).
    Done(usize),
    /// The coroutine yielded (`rt.multres` values at the top of its stack).
    Yield,
}

/// Outcome of an interpreter run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RunEnd {
    Returned(usize),
    Yielded,
}

impl Vm {
    // -- Stack growth ---------------------------------------------------------------------------

    /// Error for a failed `lj_state_growstack`. `located`: add the position of the current
    /// frame (`lj_err_stkov`); a dummy frame gives none.
    pub(crate) fn stkov_error(&mut self, th: Gc<Thread>, so: StackOverflow, located: bool) -> LuaError {
        let msg = ErrMsg::STKOV.0.as_bytes();
        if so == StackOverflow::InErrorHandler || th.errerr() {
            let em = if so == StackOverflow::InErrorHandler { ErrMsg::STKOV } else { ErrMsg::ERRERR };
            let v = self.err_str(em);
            return LuaError::new(ErrorKind::Err, v);
        }
        if located {
            self.err_msgv(msg)
        } else {
            self.error_msg_bytes(msg)
        }
    }

    /// `lj_state_growstack(th, need)` with the upstream error.
    pub(crate) fn grow_stack(&mut self, th: Gc<Thread>, need: usize) -> LuaResult<()> {
        match th.grow_stack(need) {
            Ok(()) => Ok(()),
            Err(so) => Err(self.stkov_error(th, so, true)),
        }
    }

    /// `lj_state_checkstack`: ensure `need` free slots above `top`.
    pub(crate) fn check_stack_th(&mut self, th: Gc<Thread>, need: usize) -> LuaResult<()> {
        if th.max_stack().saturating_sub(th.top()) < need {
            self.grow_stack(th, need)?;
        }
        Ok(())
    }

    // -- Calls ----------------------------------------------------------------------------------

    /// Call the value in `func_slot` with `nargs` arguments starting at `func_slot + 2`
    /// (`vm_call_dispatch` + `ins_call`). `bytecode` = called by a `CALL*`/`ITER*` instruction
    /// (affects the error message for non-callable values). Lua callees get a new frame
    /// (`Flow::Continue`); natives run to completion here.
    pub(crate) fn precall(
        &mut self,
        th: Gc<Thread>,
        func_slot: usize,
        nargs: usize,
        kind: FrameKind,
        nres: i32,
        bytecode: bool,
    ) -> LuaResult<Flow> {
        let mut nargs = nargs;
        let f = loop {
            match th.get(func_slot) {
                Value::Func(f) => break f,
                o => {
                    // lj_meta_call: insert the object as first argument.
                    let mo = self.meta_lookup(o, MM::Call);
                    if !matches!(mo, Value::Func(_)) {
                        let e = self.err_optype_call(o, func_slot, bytecode);
                        if matches!(kind, FrameKind::Pcall { .. } | FrameKind::Cp(_))
                            && let Some(below) = th.current_frame()
                        {
                            let mut d = Frame::new(kind, below.func, (func_slot + 2) as u32, nres);
                            d.dummy = true;
                            th.push_frame(d);
                            th.set_top(func_slot + 2);
                        }
                        return Err(e);
                    }
                    let top = func_slot + 2 + nargs;
                    if top + 1 > th.stack_size() {
                        self.grow_stack(th, 1)?;
                    }
                    for p in (func_slot + 3..=top).rev() {
                        th.set(p, th.get(p - 1));
                    }
                    th.set(func_slot + 2, o);
                    th.set(func_slot, mo);
                    nargs += 1;
                }
            }
        };
        let base = func_slot + 2;
        th.push_frame(Frame::new(kind, f, base as u32, nres));
        if let Some(pt) = f.proto() {
            let fidx = th.frame_count() - 1;
            if pt.is_vararg() {
                // BC_FUNCV: move the function and fixed parameters above the varargs.
                let nbase = base + nargs + 2;
                while nbase + pt.framesize as usize > th.max_stack() {
                    th.set_top(base + nargs);
                    if let Err(so) = th.grow_stack(pt.framesize as usize + nargs + 2) {
                        th.pop_frame();
                        return Err(self.stkov_error(th, so, false));
                    }
                }
                th.set(nbase - 2, Value::Func(f));
                th.set(nbase - 1, Value::Nil);
                for i in 0..pt.numparams as usize {
                    if i < nargs {
                        th.set(nbase + i, th.get(base + i));
                        th.set(base + i, Value::Nil);
                    } else {
                        th.set(nbase + i, Value::Nil);
                    }
                }
                let mut fr = th.frame(fidx).expect("frame");
                fr.base = nbase as u32;
                fr.vdelta = (nargs + 2) as u32;
                fr.pc = 1;
                th.set_frame(fidx, fr);
                th.set_top(nbase + pt.framesize as usize);
            } else {
                // BC_FUNCF.
                while base + pt.framesize as usize > th.max_stack() {
                    th.set_top(base + nargs);
                    if let Err(so) = th.grow_stack(pt.framesize as usize) {
                        th.pop_frame();
                        return Err(self.stkov_error(th, so, false));
                    }
                }
                for i in nargs..pt.numparams as usize {
                    th.set(base + i, Value::Nil);
                }
                th.set_frame_pc(fidx, 1);
                th.set_top(base + pt.framesize as usize);
            }
            if self.rt.hook.mask & hook::LUA_MASKCALL != 0 && !self.rt.hook.active {
                self.hook_call(th, true)?;
            }
            Ok(Flow::Continue)
        } else {
            th.set_top(base + nargs);
            if th.max_stack().saturating_sub(base + nargs) < LJ_STACK_MIN {
                self.grow_stack(th, LJ_STACK_MIN)?;
            }
            if self.rt.hook.mask & hook::LUA_MASKCALL != 0 && !self.rt.hook.active {
                self.hook_call(th, false)?;
            }
            match f.ffid() {
                ffid::FF_PCALL | ffid::FF_XPCALL if f.is_builtin() => self.ff_pcall(th, f.ffid() == ffid::FF_XPCALL),
                _ => self.call_native(th),
            }
        }
    }

    /// `ff_pcall` / `ff_xpcall`: call the function with a protected frame (`FRAME_PCALL`)
    /// directly above the pcall frame, without a C-call boundary.
    fn ff_pcall(&mut self, th: Gc<Thread>, xpcall: bool) -> LuaResult<Flow> {
        let b = self.frame_base();
        let nargs = th.top() - b;
        self.check_any(1)?;
        let hook = self.rt.hook.active;
        if xpcall {
            self.check_func(2)?;
            // Swap function and handler; the handler stays in the xpcall frame's first slot.
            let f = th.get(b);
            th.set(b, th.get(b + 1));
            th.set(b + 1, f);
            for k in (2..nargs).rev() {
                th.set(b + k + 1, th.get(b + k));
            }
            th.set(b + 2, Value::Nil);
            th.set_top(b + nargs + 1);
            self.precall(th, b + 1, nargs - 2, FrameKind::Pcall { hook }, -1, false)
        } else {
            for k in (1..nargs).rev() {
                th.set(b + k + 1, th.get(b + k));
            }
            th.set(b + 1, Value::Nil);
            th.set_top(b + nargs + 1);
            self.precall(th, b, nargs - 1, FrameKind::Pcall { hook }, -1, false)
        }
    }

    /// `lj_err_optype_call`.
    fn err_optype_call(&mut self, o: Value, slot: usize, bytecode: bool) -> LuaError {
        if bytecode {
            self.err_optype(o, Some(slot), ErrMsg::OPCALL)
        } else {
            let m = fmt_msg(ErrMsg::BADCALL, &[o.type_name().as_bytes()]);
            self.error_msg_bytes(&m)
        }
    }

    /// Run the native function of the top frame and process its return.
    pub(crate) fn call_native(&mut self, th: Gc<Thread>) -> LuaResult<Flow> {
        let fr = th.current_frame().expect("native frame");
        let Function::Native { f, .. } = &*fr.func else { unreachable!("native frame") };
        super::hook::set_vmstate(super::hook::VMSTATE_C);
        let r = match *f {
            NativeImpl::Rust(nf) => nf(self),
            NativeImpl::C(cf) => match self.rt.c_call {
                Some(h) => h(self, cf),
                None => Err(self.error_msg_bytes(b"C function called without the C API")),
            },
        };
        super::hook::set_vmstate(super::hook::VMSTATE_INTERP);
        match r {
            Ok(TAILCALL) => {
                // FFH_TAILCALL: the callable was stored in the native's function slot.
                let fr = th.pop_frame().expect("native frame");
                let base = fr.base as usize;
                let nargs = th.top() - base;
                self.precall(th, base - 2, nargs, fr.kind, fr.nres, false)
            }
            Ok(n) => {
                self.gc_safe();
                let top = th.top();
                let n = n.min(top.saturating_sub(th.current_frame().map_or(0, |f| f.base as usize)));
                self.ret(th, top - n, n)
            }
            Err(e) if e.kind == ErrorKind::Yield => Ok(Flow::Yield),
            Err(e) => Err(e),
        }
    }

    /// Return `n` values starting at slot `first` from the top frame (`vm_return` /
    /// `BC_RET`). Results go to the frame's call slot, adjusted to the wanted count.
    pub(crate) fn ret(&mut self, th: Gc<Thread>, first: usize, n: usize) -> LuaResult<Flow> {
        let (mut first, mut n) = (first, n);
        loop {
            let fr = th.pop_frame().expect("frame to return from");
            match fr.kind {
                FrameKind::Lua => {
                    let dest = fr.call_slot() as usize;
                    for i in 0..n {
                        th.set(dest + i, th.get(first + i));
                    }
                    let mut cnt = n;
                    if fr.nres >= 0 {
                        let want = fr.nres as usize;
                        if dest + want > th.stack_size() {
                            th.set_top(dest + n);
                            self.grow_stack(th, want)?;
                        }
                        for i in n..want {
                            th.set(dest + i, Value::Nil);
                        }
                        cnt = want;
                    }
                    self.rt.multres = n;
                    let caller = th.current_frame().expect("lua caller");
                    let ctop = caller.base as usize + caller.func.proto().map_or(0, |p| p.framesize as usize);
                    th.set_top(ctop.max(dest + cnt));
                    return Ok(Flow::Continue);
                }
                FrameKind::Cont(c) => {
                    let res = if n > 0 { th.get(first) } else { Value::Nil };
                    let caller = th.current_frame().expect("lua caller");
                    let ctop = caller.base as usize + caller.func.proto().map_or(0, |p| p.framesize as usize);
                    th.set_top(ctop);
                    return self.apply_cont(th, c, res);
                }
                FrameKind::C | FrameKind::Cp(_) => {
                    let dest = fr.call_slot() as usize;
                    for i in 0..n {
                        th.set(dest + i, th.get(first + i));
                    }
                    let mut cnt = n;
                    if fr.nres >= 0 {
                        let want = fr.nres as usize;
                        if dest + want > th.max_stack() {
                            th.set_top(dest + n);
                            self.grow_stack(th, want)?;
                        }
                        for i in n..want {
                            th.set(dest + i, Value::Nil);
                        }
                        cnt = want;
                    }
                    th.set_top(dest + cnt);
                    return Ok(Flow::Done(cnt));
                }
                FrameKind::Pcall { .. } => {
                    // vm_returnp: prepend true and return from the pcall/xpcall frame below.
                    first -= 1;
                    th.set(first, Value::Bool(true));
                    n += 1;
                }
            }
        }
    }

    /// Call metamethod `mo(a, b[, c])` above the current Lua frame with continuation `cont`
    /// (`mmcall` + `vmeta_binop`).
    pub(crate) fn mm_call(
        &mut self,
        th: Gc<Thread>,
        cont: Cont,
        mo: Value,
        a: Value,
        b: Value,
        c: Option<Value>,
    ) -> LuaResult<Flow> {
        let fr = th.current_frame().expect("lua frame");
        let slot = fr.base as usize + fr.func.proto().expect("lua frame").framesize as usize;
        if slot + 6 > th.stack_size() {
            th.set_top(slot);
            self.grow_stack(th, 6)?;
        }
        th.set(slot, mo);
        th.set(slot + 1, Value::Nil);
        th.set(slot + 2, a);
        th.set(slot + 3, b);
        let nargs = if let Some(c) = c {
            th.set(slot + 4, c);
            3
        } else {
            2
        };
        th.set_top(slot + 2 + nargs);
        self.precall(th, slot, nargs, FrameKind::Cont(cont), 1, false)
    }

    // -- Runs -----------------------------------------------------------------------------------

    /// Drive the interpreter on `th` until the frame at index `entry` returns, starting from
    /// the result of an initial call/return step. Errors are caught by `pcall` frames of this
    /// run; otherwise the run's frames are unwound (except for coroutine runs, `resume`, whose
    /// frames stay for inspection) and the error is returned.
    pub(crate) fn run(&mut self, th: Gc<Thread>, entry: usize, first: LuaResult<Flow>, resume: bool) -> LuaResult<RunEnd> {
        let mut flow = first;
        loop {
            match flow {
                Ok(Flow::Continue) => flow = self.interp(th),
                Ok(Flow::Done(n)) => return Ok(RunEnd::Returned(n)),
                Ok(Flow::Yield) => return Ok(RunEnd::Yielded),
                Err(e) => flow = Ok(self.on_error(th, e, entry, resume)?),
            }
        }
    }

    /// Call the value at `func_slot` of `th` from Rust: a nested interpreter run and C-call
    /// boundary (`lj_vm_call` / `lj_vm_pcall`). Returns the number of results, which are
    /// stored from `func_slot` up (top = `func_slot + count`).
    pub(crate) fn call_at(&mut self, th: Gc<Thread>, func_slot: usize, nargs: usize, kind: FrameKind, nres: i32) -> LuaResult<usize> {
        if self.rt.nccalls >= LJ_MAX_CCALLS {
            return Err(self.err_msg(ErrMsg::CCALLS));
        }
        self.rt.nccalls += 1;
        let old_yield = std::mem::replace(&mut self.rt.can_yield, false);
        self.rt.runs.push(Run { hook_oldpc: None });
        let entry = th.frame_count();
        let oldst = super::hook::set_vmstate(super::hook::VMSTATE_INTERP);
        let first = self.precall(th, func_slot, nargs, kind, nres, false);
        let r = self.run(th, entry, first, false);
        super::hook::set_vmstate(oldst);
        self.rt.runs.pop();
        self.rt.can_yield = old_yield;
        self.rt.nccalls -= 1;
        match r {
            Ok(RunEnd::Returned(n)) => Ok(n),
            Ok(RunEnd::Yielded) => unreachable!("yield outside a resumable run"),
            Err(e) => Err(e),
        }
    }

    // -- Errors ---------------------------------------------------------------------------------

    /// `finderrfunc`: the active error handler slot for `th`, if any.
    fn find_errfunc(&self, th: Gc<Thread>) -> Option<usize> {
        let n = th.frame_count();
        for i in (0..n).rev() {
            let f = th.frame(i).expect("frame");
            match f.kind {
                FrameKind::Lua | FrameKind::C | FrameKind::Cont(_) | FrameKind::Cp(ErrFunc::Inherit) => {}
                FrameKind::Cp(ErrFunc::None) => return None,
                FrameKind::Cp(ErrFunc::Slot(s)) => return Some(s as usize),
                FrameKind::Pcall { .. } => {
                    let pf = th.frame(i.checked_sub(1)?)?;
                    return if pf.func.ffid() == ffid::FF_XPCALL { Some(pf.base as usize) } else { None };
                }
            }
        }
        None
    }

    /// `lj_err_run`: call the active error handler (if any) at the error site. Returns the
    /// error to throw.
    pub(crate) fn apply_errfunc(&mut self, th: Gc<Thread>, e: LuaError) -> LuaError {
        let Some(ef) = self.find_errfunc(th) else { return e };
        let errfunc = th.get(ef);
        if !matches!(errfunc, Value::Func(_)) || th.errerr() {
            let v = self.err_str(ErrMsg::ERRERR);
            return LuaError::new(ErrorKind::Err, v);
        }
        let mut msg = e.value;
        if th.max_stack().saturating_sub(th.top()) < 2 * LJ_STACK_MIN
            && let Err(so) = th.grow_stack(2 * LJ_STACK_MIN)
        {
            if so == StackOverflow::InErrorHandler {
                let v = self.err_str(ErrMsg::STKOV);
                return LuaError::new(ErrorKind::Err, v);
            }
            let s = self.addloc(th, self.top_frame_idx(), ErrMsg::STKOV.0.as_bytes());
            msg = Value::Str(self.new_str(&s));
        }
        let top = th.top();
        th.set(top, errfunc);
        th.set(top + 1, Value::Nil);
        th.set(top + 2, msg);
        th.set_top(top + 3);
        th.set_errerr(true);
        let r = self.call_at(th, top, 1, FrameKind::C, 1);
        th.set_errerr(false);
        match r {
            Ok(_) => {
                let v = th.get(top);
                th.set_top(top);
                LuaError::new(ErrorKind::Run, v)
            }
            Err(e2) => {
                th.set_top(top);
                e2
            }
        }
    }

    /// Handle an error raised inside a run (`lj_err_throw` + `err_unwind`): apply the error
    /// handler, then unwind to the innermost `pcall` frame of this run (returning its
    /// `false, msg` results) or out of the run.
    pub(crate) fn on_error(&mut self, th: Gc<Thread>, e: LuaError, entry: usize, resume: bool) -> LuaResult<Flow> {
        let mut e = e;
        if e.kind == ErrorKind::Yield {
            e = self.err_msg(ErrMsg::CYIELD);
        }
        if e.kind == ErrorKind::Run && self.rt.thrown.is_none_or(|t| !t.raw_equal(e.value)) {
            e = self.apply_errfunc(th, e);
        }
        self.rt.thrown = Some(e.value);
        let n = th.frame_count();
        for k in (entry + 1..n).rev() {
            let f = th.frame(k).expect("frame");
            if let FrameKind::Pcall { hook } = f.kind {
                let pf = th.frame(k - 1).expect("pcall frame");
                let pbase = pf.base as usize;
                th.close_upvals(pbase as u32);
                th.truncate_frames(k);
                th.set(pbase, Value::Bool(false));
                th.set(pbase + 1, e.value);
                th.set_top(pbase + 2);
                th.relimit_stack();
                th.set_errerr(false);
                if !hook {
                    self.rt.hook.active = false;
                }
                self.rt.thrown = None;
                return self.ret(th, pbase, 2);
            }
        }
        if resume {
            // Coroutine boundary: keep the frames (the dead coroutine can be inspected).
            self.rt.hook.active = false;
            th.set_errerr(false);
            return Err(e);
        }
        if let Some(ef) = th.frame(entry) {
            let slot = ef.call_slot();
            th.close_upvals(slot);
            th.truncate_frames(entry);
            th.set_top(slot as usize);
        }
        th.relimit_stack();
        th.set_errerr(false);
        Err(e)
    }

    // -- GC safe points and finalizers ----------------------------------------------------------

    /// GC safe point: run a GC step if due, then pending finalizers. The caller must have set
    /// the current thread's top to cover all live slots.
    pub fn gc_safe(&mut self) {
        if self.gc_check() {
            self.run_finalizers();
        }
    }

    /// Run all queued finalizers (`gc_call_finalizer`): protected, with hooks disabled and GC
    /// steps suppressed. Errors are reported through the `ERRFIN` VM event.
    pub fn run_finalizers(&mut self) {
        if self.rt.in_finalizers || !self.has_pending_finalizers() {
            return;
        }
        self.rt.in_finalizers = true;
        while let Some((f, o)) = self.take_finalizer() {
            let oldh = self.rt.hook.active;
            self.rt.hook.active = true;
            let oldt = std::mem::replace(&mut self.gc.threshold, GC_MAX_MEM);
            let cur = self.cur_thread;
            let th = self.vm_thread();
            self.cur_thread = th;
            let top = th.top();
            let r = if th.max_stack().saturating_sub(top) < 4 {
                self.grow_stack(th, 4).map(|_| 0)
            } else {
                Ok(0)
            };
            let r = r.and_then(|_| {
                th.set(top, f);
                th.set(top + 1, Value::Nil);
                th.set(top + 2, o);
                th.set_top(top + 3);
                self.call_at(th, top, 1, FrameKind::Cp(ErrFunc::Inherit), 0)
            });
            self.rt.hook.active = oldh;
            self.gc.threshold = oldt;
            th.set_top(top);
            if let Err(e) = r {
                self.rt.thrown = None;
                self.vmevent_errfin(e.value);
            }
            self.cur_thread = cur;
        }
        self.rt.in_finalizers = false;
    }

    /// `lua_close` minus freeing: close the main thread's upvalues and run every pending
    /// finalizer (userdata `__gc` and FFI finalizers), repeating up to 10 times as upstream.
    pub fn close(&mut self) {
        self.cur_thread = self.main_thread;
        let th = self.main_thread;
        th.close_upvals(0);
        th.truncate_frames(0);
        th.set_top(0);
        for _ in 0..10 {
            self.queue_all_finalizers();
            if !self.has_pending_finalizers() {
                break;
            }
            self.run_finalizers();
        }
    }
}
