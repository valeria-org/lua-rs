//! Debug hooks. Port of the hook parts of lj_dispatch.c (`lua_sethook`, `callhook`,
//! `lj_dispatch_ins`, `lj_dispatch_call`, `cur_topslot`) and the `vm_inshook`/`vm_rethook`/
//! `vm_callhook` dispatch rules of vm_x64.dasc.
//!
//! Semantics: hooks are global. Instruction hooks (line/count) run before every bytecode
//! except function headers; return hooks before `RET*` of Lua functions only; call hooks on
//! entry of every function (Lua and native). While a hook runs, no hooks fire. A line event
//! fires when the line changes, on a backward jump (or re-execution of the same pc), or when
//! the previously hooked instruction in the same interpreter run belonged to another
//! function (e.g. after returning from a Lua function, or after a native called with a call
//! hook active).

use crate::bc::{Ins, Op};
use crate::error::LuaResult;
use crate::func::Proto;
use crate::gc::Gc;
use crate::state::Vm;
use crate::thread::Thread;

use super::debug::debug_line;

pub const LUA_MASKCALL: u8 = 1;
pub const LUA_MASKRET: u8 = 2;
pub const LUA_MASKLINE: u8 = 4;
pub const LUA_MASKCOUNT: u8 = 8;
const HOOK_EVENTMASK: u8 = 0x0f;

/// Hook event (`LUA_HOOK*`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HookEvent {
    Call = 0,
    Ret = 1,
    Line = 2,
    Count = 3,
}

impl HookEvent {
    /// Name passed to Lua hook functions by `debug.sethook` (`hooknames`).
    pub fn name(self) -> &'static str {
        match self {
            HookEvent::Call => "call",
            HookEvent::Ret => "return",
            HookEvent::Line => "line",
            HookEvent::Count => "count",
        }
    }
}

/// A hook function (`lua_Hook`). Called with the hooked function's frame on top of the
/// current thread (level 0 for [`Vm::get_stack`]); `currentline` is the new line for line
/// events and -1 otherwise. May call into Lua (through `Vm::call`); errors propagate.
pub type HookFn = fn(&mut Vm, HookEvent, i32) -> LuaResult<()>;

#[derive(Clone, Copy, Default)]
pub(crate) struct HookState {
    pub func: Option<HookFn>,
    /// `LUA_MASK*` bits (0 when no hook is set).
    pub mask: u8,
    pub count: i32,
    pub cstart: i32,
    /// A hook (or finalizer / VM event handler) is running (`HOOK_ACTIVE`).
    pub active: bool,
}

impl Vm {
    /// `lua_sethook`.
    pub fn set_hook(&mut self, func: Option<HookFn>, mask: u8, count: i32) {
        let mut mask = mask & HOOK_EVENTMASK;
        let mut func = func;
        if func.is_none() || mask == 0 {
            mask = 0;
            func = None;
        }
        let h = &mut self.rt.hook;
        h.func = func;
        h.count = count;
        h.cstart = count;
        h.mask = mask;
    }

    /// `lua_gethook`, `lua_gethookmask`, `lua_gethookcount`.
    pub fn get_hook(&self) -> (Option<HookFn>, u8, i32) {
        (self.rt.hook.func, self.rt.hook.mask, self.rt.hook.cstart)
    }

    /// True while a hook is running (hooks are not re-entered).
    pub fn hook_active(&self) -> bool {
        self.rt.hook.active
    }

    /// `callhook`.
    fn callhook(&mut self, th: Gc<Thread>, event: HookEvent, line: i32) -> LuaResult<()> {
        let Some(f) = self.rt.hook.func else { return Ok(()) };
        if self.rt.hook.active {
            return Ok(());
        }
        self.check_stack_th(th, 1 + crate::thread::LJ_STACK_MIN)?;
        self.rt.hook.active = true;
        let multres = self.rt.multres;
        let r = f(self, event, line);
        self.rt.multres = multres;
        self.rt.hook.active = false;
        r
    }

    /// `lj_dispatch_call` for the frame just entered.
    pub(crate) fn hook_call(&mut self, th: Gc<Thread>, is_lua: bool) -> LuaResult<()> {
        if !is_lua && let Some(r) = self.rt.runs.last_mut() {
            // The C function's fake PC becomes the cframe PC (`vm_callhook`).
            r.hook_oldpc = None;
        }
        self.callhook(th, HookEvent::Call, -1)
    }

    /// `vm_inshook`/`vm_rethook` + `lj_dispatch_ins`, before executing the instruction at
    /// `pc - 1` of frame `fidx`. `ins_hook`: line/count dispatch (else return dispatch only).
    pub(crate) fn hook_ins(&mut self, th: Gc<Thread>, fidx: usize, pt: &Gc<Proto>, pc: u32, ins_hook: bool) -> LuaResult<()> {
        let mask = self.rt.hook.mask;
        if ins_hook {
            self.rt.hook.count = self.rt.hook.count.wrapping_sub(1);
            if self.rt.hook.count != 0 && mask & LUA_MASKLINE == 0 {
                return Ok(());
            }
        }
        let base = th.frame(fidx).expect("frame").base as usize;
        let ptaddr = Gc::as_ptr(*pt) as usize;
        let oldpc = match self.rt.runs.last_mut() {
            Some(r) => r.hook_oldpc.replace((ptaddr, pc)),
            None => None,
        };
        let slots = cur_topslot(pt, pc, self.rt.multres);
        th.set_top(base + slots);
        if mask & LUA_MASKCOUNT != 0 && self.rt.hook.count == 0 {
            self.rt.hook.count = self.rt.hook.cstart;
            self.callhook(th, HookEvent::Count, -1)?;
            th.set_top(base + slots);
        }
        if mask & LUA_MASKLINE != 0 {
            let npc = pc - 1;
            let line = debug_line(pt, npc);
            let fire = match oldpc {
                Some((p, opc)) if p == ptaddr => {
                    let o = opc.wrapping_sub(1);
                    pc <= opc || o >= pt.bc.len() as u32 || line != debug_line(pt, o)
                }
                _ => true,
            };
            if fire {
                self.callhook(th, HookEvent::Line, line as i32)?;
                th.set_top(base + slots);
            }
        }
        if mask & LUA_MASKRET != 0 && Op::from_u8(pt.bc[pc as usize - 1].op_byte()).is_some_and(|o| o.is_ret()) {
            self.callhook(th, HookEvent::Ret, -1)?;
        }
        Ok(())
    }
}

/// `cur_topslot`: used stack slots of the current frame at `pc`.
fn cur_topslot(pt: &Proto, pc: u32, multres: usize) -> usize {
    let mut ins: Ins = pt.bc[pc as usize - 1];
    if ins.op_byte() == Op::UCLO as u8 {
        ins = pt.bc[(pc as i64 + ins.j() as i64) as usize];
    }
    let nres = multres + 1;
    match Op::from_u8(ins.op_byte()) {
        Some(Op::CALLM | Op::CALLMT) => ins.a() as usize + ins.c() as usize + nres + 1,
        Some(Op::RETM) => ins.a() as usize + ins.d() as usize + nres - 1,
        Some(Op::TSETM) => ins.a() as usize + nres - 1,
        _ => pt.framesize as usize,
    }
}
