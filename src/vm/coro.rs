//! Coroutines: creation, resume and yield. Port of the semantics of `ff_coroutine_resume` /
//! `ff_coroutine_wrap_aux` / `ff_coroutine_yield` (vm_x64.dasc), `ffh_resume` (lib_base.c),
//! `vm_resume` and `lj_state_new`.
//!
//! A resume is a nested interpreter run on the coroutine's thread that may yield
//! (`cframe_canyield`). A yield leaves the yielding native's frame on the coroutine's frame
//! stack; the next resume completes that frame with the resume arguments as its results.

use crate::error::{ErrorKind, LuaError, LuaResult};
use crate::func::Function;
use crate::gc::Gc;
use crate::state::Vm;
use crate::thread::{ErrFunc, FrameKind, Thread, ThreadStatus};
use crate::value::Value;

use super::call::RunEnd;
use super::err::ErrMsg;
use super::{LJ_MAX_CCALLS, Run};

/// Slot of the coroutine's main function in a fresh coroutine stack.
const CO_FUNC_SLOT: usize = 2;

impl Vm {
    /// `lua_newthread` + push of `f` (`coroutine.create`): a fresh coroutine whose environment
    /// is the current thread's.
    pub fn new_coroutine(&mut self, f: Gc<Function>) -> Gc<Thread> {
        let env = self.cur_thread.env();
        let co = self.new_thread(env);
        co.set(CO_FUNC_SLOT, Value::Func(f));
        co.set_top(CO_FUNC_SLOT + 1);
        co.set_status(ThreadStatus::Fresh);
        co
    }

    /// Why `co` cannot be resumed (`ffh_resume` checks), if it cannot.
    pub fn resume_check(&self, co: Gc<Thread>) -> Option<ErrMsg> {
        match co.status() {
            ThreadStatus::Running | ThreadStatus::Normal => Some(ErrMsg::CORUN),
            ThreadStatus::Dead => Some(ErrMsg::CODEAD),
            ThreadStatus::Fresh if co.top() <= CO_FUNC_SLOT => Some(ErrMsg::CODEAD),
            _ => None,
        }
    }

    /// Resume `co` with the top `nargs` values of the current frame as arguments (popped).
    ///
    /// * `Ok(Ok(n))`: the coroutine yielded or returned; its `n` values were pushed onto the
    ///   current stack.
    /// * `Ok(Err(v))`: the coroutine died with error value `v` (nothing pushed).
    /// * `Err(e)`: error in the context of the resumer (not resumable, stack overflow, C stack
    ///   overflow).
    pub fn resume(&mut self, co: Gc<Thread>, nargs: usize) -> LuaResult<Result<usize, Value>> {
        if let Some(em) = self.resume_check(co) {
            return Err(self.err_caller(em));
        }
        if self.rt.nccalls >= LJ_MAX_CCALLS {
            return Err(self.err_msg(ErrMsg::CCALLS));
        }
        let prev = self.cur_thread;
        let ptop = prev.top();
        let args = ptop - nargs;
        let fresh = co.status() == ThreadStatus::Fresh;
        // Copy the arguments (after making room on the coroutine's stack).
        let dst = if fresh { CO_FUNC_SLOT + 2 } else { co.top() };
        if co.max_stack().saturating_sub(dst) < nargs + 1 {
            co.set_top(dst);
            if let Err(so) = co.grow_stack(nargs + 1) {
                let e = self.stkov_error(co, so, false);
                let msg = self.error_text(e.value).unwrap_or_default();
                return Err(self.err_callermsg(&msg));
            }
        }
        for i in 0..nargs {
            co.set(dst + i, prev.get(args + i));
        }
        co.set_top(dst + nargs);
        prev.set_top(args);

        prev.set_status(ThreadStatus::Normal);
        co.set_status(ThreadStatus::Running);
        self.cur_thread = co;
        self.rt.nccalls += 1;
        let old_yield = std::mem::replace(&mut self.rt.can_yield, true);
        self.rt.runs.push(Run { hook_oldpc: None });
        let first = if fresh {
            co.set(CO_FUNC_SLOT + 1, Value::Nil);
            self.precall(co, CO_FUNC_SLOT, nargs, FrameKind::Cp(ErrFunc::None), -1, false)
        } else {
            self.ret(co, dst, nargs)
        };
        let r = self.run(co, 0, first, true);
        self.rt.runs.pop();
        self.rt.can_yield = old_yield;
        self.rt.nccalls -= 1;
        self.cur_thread = prev;
        prev.set_status(ThreadStatus::Running);

        match r {
            Ok(end) => {
                let (from, n) = match end {
                    RunEnd::Returned(n) => {
                        co.set_status(ThreadStatus::Dead);
                        (CO_FUNC_SLOT, n)
                    }
                    RunEnd::Yielded => {
                        co.set_status(ThreadStatus::Suspended);
                        let n = self.rt.multres;
                        (co.top() - n, n)
                    }
                };
                let ptop = prev.top();
                if prev.max_stack().saturating_sub(ptop) < n {
                    self.grow_stack(prev, n)?;
                }
                for i in 0..n {
                    prev.set(ptop + i, co.get(from + i));
                }
                prev.set_top(ptop + n);
                match end {
                    RunEnd::Returned(_) => co.set_top(CO_FUNC_SLOT),
                    // The next resume stores its arguments where the yielded values were.
                    RunEnd::Yielded => co.set_top(from),
                }
                Ok(Ok(n))
            }
            Err(e) => {
                co.set_status(ThreadStatus::Dead);
                Ok(Err(e.value))
            }
        }
    }

    /// True if the running code may yield (`cframe_canyield`, `coroutine.isyieldable`).
    pub fn can_yield(&self) -> bool {
        self.rt.can_yield
    }

    /// Yield the top `nresults` values of the current native's frame (`lua_yield`). Return
    /// the result from the native: `return vm.yield_(n)`. Raises
    /// `attempt to yield across C-call boundary` if not allowed.
    pub fn yield_(&mut self, nresults: usize) -> LuaResult<usize> {
        if !self.rt.can_yield {
            return Err(self.err_caller(ErrMsg::CYIELD));
        }
        self.rt.multres = nresults;
        Err(LuaError::new(ErrorKind::Yield, Value::Num(nresults as f64)))
    }
}
