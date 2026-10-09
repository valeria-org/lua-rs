//! Debugging and introspection. Port of lj_debug.c (frames, line numbers, variable and
//! function names, `lua_getinfo`/`lua_getlocal`/`lua_setlocal`, `luaL_traceback`).
//!
//! Frame levels count from the top of a thread's frame vector: level 0 is the running
//! function (for a native calling these helpers: the native itself), level 1 its caller, etc.

use crate::bc::{BCMode, Ins, MM, Op};
use crate::func::{Function, PROTO_VARARG, Proto, VARNAME_STR, VarName};
use crate::gc::Gc;
use crate::state::Vm;
use crate::thread::{Frame, FrameKind, Thread};
use crate::value::Value;

/// A frame of some thread, as returned by [`Vm::get_stack`] (upstream `lua_Debug.i_ci`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FrameRef {
    pub thread: Gc<Thread>,
    /// Index into the thread's frame vector (0 = bottom).
    pub idx: usize,
}

/// Result of [`Vm::get_info`] (`lua_Debug`). Fields not requested keep their defaults.
#[derive(Clone, Debug, Default)]
pub struct DebugInfo {
    /// `S`: chunk name (`"=[C]"` for natives).
    pub source: Vec<u8>,
    /// `S`: shortened source (`lj_debug_shortname`).
    pub short_src: Vec<u8>,
    /// `S`: first line, -1 for natives.
    pub linedefined: i32,
    /// `S`: last line, -1 for natives.
    pub lastlinedefined: i32,
    /// `S`: `"Lua"`, `"main"` or `"C"`.
    pub what: &'static str,
    /// `l`: current line or -1.
    pub currentline: i32,
    /// `u`: number of upvalues.
    pub nups: u32,
    /// `u` (extended info): number of fixed parameters / vararg flag.
    pub nparams: u32,
    pub isvararg: bool,
    /// `n`: function name and kind (`"global"`, `"local"`, `"method"`, `"field"`,
    /// `"upvalue"`, `"metamethod"` or `""`).
    pub name: Option<Vec<u8>>,
    pub namewhat: &'static str,
    /// `f`: the function.
    pub func: Option<Gc<Function>>,
    /// `L`: table of active lines (`nil` for natives).
    pub activelines: Option<Value>,
}

/// Target of [`Vm::get_info`].
#[derive(Clone, Copy, Debug)]
pub enum InfoTarget {
    /// A frame (`lua_getstack` result).
    Frame(FrameRef),
    /// A function value (`">"` option of `lua_getinfo`).
    Func(Gc<Function>),
}

/// Number of frames for the leading and trailing part of a traceback.
const TRACEBACK_LEVELS1: i32 = 12;
const TRACEBACK_LEVELS2: i32 = 10;

/// `lj_debug_line`: line of bytecode position `pc` (0 if there is no line info).
pub fn debug_line(pt: &Proto, pc: u32) -> u32 {
    let n = pt.bc.len() as u32;
    if pc <= n && !pt.lineinfo.is_empty() {
        if pc == n {
            return pt.firstline + pt.numline;
        }
        return pt.lineinfo[pc as usize];
    }
    0
}

/// `debug_framepc`: position of the current instruction of frame `idx` (None for natives).
pub(crate) fn framepc(th: Gc<Thread>, idx: usize) -> Option<u32> {
    let f = th.frame(idx)?;
    if !f.func.is_lua() || f.pc == 0 {
        return None;
    }
    // The top Lua frame of a thread that is not running has no saved PC (`cframe == NULL`),
    // e.g. a coroutine that died with an error raised by its own bytecode.
    if idx + 1 == th.frame_count()
        && matches!(th.status(), crate::thread::ThreadStatus::Dead | crate::thread::ThreadStatus::Suspended)
    {
        return None;
    }
    Some(f.pc - 1)
}

/// `debug_frameline`: current line of frame `idx`, -1 if unknown.
pub(crate) fn frameline(th: Gc<Thread>, idx: usize) -> i32 {
    match (framepc(th, idx), th.frame(idx)) {
        (Some(pc), Some(f)) => debug_line(&f.func.proto().expect("lua frame"), pc) as i32,
        _ => -1,
    }
}

fn varname_bytes(n: VarName) -> Vec<u8> {
    match n {
        VarName::Internal(i) => VARNAME_STR.get(i as usize).copied().unwrap_or("").as_bytes().to_vec(),
        VarName::Str(s) => s.as_bytes().to_vec(),
    }
}

/// `debug_varname`: name of the `slot`-th active local at `pc`.
pub(crate) fn varname(pt: &Proto, pc: u32, mut slot: u32) -> Option<Vec<u8>> {
    for v in pt.varinfo.iter() {
        if v.startpc > pc {
            break;
        }
        if pc < v.endpc {
            if slot == 0 {
                return Some(varname_bytes(v.name));
            }
            slot -= 1;
        }
    }
    None
}

/// `lj_debug_uvname`: name of upvalue `idx` (empty if stripped).
pub fn uvname(pt: &Proto, idx: usize) -> Vec<u8> {
    pt.uvnames.get(idx).map(|s| s.as_bytes().to_vec()).unwrap_or_default()
}

fn kstr(pt: &Proto, idx: u32) -> Vec<u8> {
    match pt.kgc.get(idx as usize) {
        Some(crate::func::KGc::Str(s)) => s.as_bytes().to_vec(),
        _ => Vec::new(),
    }
}

/// `lj_debug_slotname`: deduce the name of the object in `slot` at instruction `ip`.
pub(crate) fn slotname(pt: &Proto, ip: u32, mut slot: u32) -> Option<(&'static str, Vec<u8>)> {
    let bc: &[Ins] = &pt.bc;
    let mut ip = ip as usize;
    'restart: loop {
        if let Some(n) = varname(pt, ip as u32, slot) {
            return Some(("local", n));
        }
        while ip > 1 {
            ip -= 1;
            let ins = bc[ip];
            let Some(op) = Op::from_u8(ins.op_byte()) else { return None };
            let ra = ins.a();
            match op.mode_a() {
                BCMode::Base => {
                    if slot >= ra && (op != Op::KNIL || slot <= ins.d()) {
                        return None;
                    }
                }
                BCMode::Dst if ra == slot => match op {
                    Op::MOV => {
                        slot = ins.d();
                        continue 'restart;
                    }
                    Op::GGET => return Some(("global", kstr(pt, ins.d()))),
                    Op::TGETS => {
                        let name = kstr(pt, ins.c());
                        if ip > 1 {
                            let p = bc[ip - 1];
                            if p.op_byte() == Op::MOV as u8 && p.a() == ra + 2 && p.d() == ins.b() {
                                return Some(("method", name));
                            }
                        }
                        return Some(("field", name));
                    }
                    Op::UGET => return Some(("upvalue", uvname(pt, ins.d() as usize))),
                    _ => return None,
                },
                _ => {}
            }
        }
        return None;
    }
}

/// `lj_debug_funcname`: name of the function running in frame `idx`, from its call site.
pub(crate) fn funcname(vm: &Vm, th: Gc<Thread>, idx: usize) -> Option<(&'static str, Vec<u8>)> {
    if idx == 0 {
        return None;
    }
    let pf = th.frame(idx - 1)?;
    let pc = framepc(th, idx - 1)?;
    let pt = pf.func.proto()?;
    let ins = *pt.bc.get(pc as usize)?;
    let op = Op::from_u8(ins.op_byte())?;
    match op.mm() {
        Some(MM::Call) => {
            let mut slot = ins.a();
            if op == Op::ITERC {
                slot = slot.wrapping_sub(3);
            }
            slotname(&pt, pc, slot)
        }
        Some(mm) => Some(("metamethod", vm.mm_name(mm).as_bytes().to_vec())),
        None => None,
    }
}

/// `lj_debug_shortname`.
pub fn shortname(name: &[u8], line: u32) -> Vec<u8> {
    crate::parse::debug_shortname(name, line)
}

/// Address printed for a native function (`fn->c.f`).
pub(crate) fn native_addr(f: &Function) -> usize {
    match f {
        Function::Native { f: crate::func::NativeImpl::Rust(p), .. } => *p as *const () as usize,
        Function::Native { f: crate::func::NativeImpl::C(p), .. } => *p as *const () as usize,
        Function::Lua { .. } => 0,
    }
}

/// Slot index of the frame-link slot of the frame above `idx` (upstream `nextframe`), or the
/// thread's `top + 1` for the top frame.
fn next_link(th: Gc<Thread>, idx: usize) -> usize {
    match th.frame(idx + 1) {
        Some(n) => n.call_slot() as usize + 1,
        None => th.top() + 1,
    }
}

impl Vm {
    /// `lua_getstack`: the frame at `level` of `th` (0 = running function).
    pub fn get_stack(&self, th: Gc<Thread>, level: i32) -> Option<FrameRef> {
        if level < 0 {
            return None;
        }
        let mut l = level;
        for idx in (0..th.frame_count()).rev() {
            if th.frame(idx).is_some_and(|f| f.dummy) {
                continue;
            }
            if l == 0 {
                return Some(FrameRef { thread: th, idx });
            }
            l -= 1;
        }
        None
    }

    /// Number of frames of `th` (what `lua_getstack(L, -10, ar)` leaves in `ar.i_ci`).
    pub fn stack_depth(&self, th: Gc<Thread>) -> i32 {
        (0..th.frame_count()).filter(|&i| th.frame(i).is_some_and(|f| !f.dummy)).count() as i32
    }

    /// Level of a frame (inverse of [`Vm::get_stack`]).
    pub fn frame_level(&self, fr: FrameRef) -> i32 {
        (fr.idx + 1..fr.thread.frame_count()).filter(|&i| fr.thread.frame(i).is_some_and(|f| !f.dummy)).count() as i32
    }

    /// `lj_debug_getinfo`. `what` holds option letters `S l u n f L`; returns `None` for an
    /// unknown option. `ext` adds `nparams`/`isvararg` to `u` (`lua_getinfo` extension).
    pub fn get_info(&mut self, target: InfoTarget, what: &[u8]) -> Option<DebugInfo> {
        let (fr, func) = match target {
            InfoTarget::Frame(fr) => (Some(fr), fr.thread.frame(fr.idx)?.func),
            InfoTarget::Func(f) => (None, f),
        };
        let mut ar = DebugInfo { currentline: -1, linedefined: -1, lastlinedefined: -1, ..Default::default() };
        let (mut opt_f, mut opt_l) = (false, false);
        for &c in what {
            match c {
                b'S' => {
                    if let Some(pt) = func.proto() {
                        let name = pt.chunkname;
                        ar.source = name.as_bytes().to_vec();
                        ar.short_src = shortname(name.as_bytes(), pt.firstline);
                        ar.linedefined = pt.firstline as i32;
                        ar.lastlinedefined = (pt.firstline + pt.numline) as i32;
                        ar.what = if pt.firstline != 0 || pt.numline == 0 { "Lua" } else { "main" };
                    } else {
                        ar.source = b"=[C]".to_vec();
                        ar.short_src = b"[C]".to_vec();
                        ar.linedefined = -1;
                        ar.lastlinedefined = -1;
                        ar.what = "C";
                    }
                }
                b'l' => ar.currentline = fr.map_or(-1, |fr| frameline(fr.thread, fr.idx)),
                b'u' => {
                    ar.nups = func.nupvalues() as u32;
                    if let Some(pt) = func.proto() {
                        ar.nparams = pt.numparams as u32;
                        ar.isvararg = pt.flags() & PROTO_VARARG != 0;
                    } else {
                        ar.nparams = 0;
                        ar.isvararg = true;
                    }
                }
                b'n' => match fr.and_then(|fr| funcname(self, fr.thread, fr.idx)) {
                    Some((w, n)) => {
                        ar.namewhat = w;
                        ar.name = Some(n);
                    }
                    None => {
                        ar.namewhat = "";
                        ar.name = None;
                    }
                },
                b'f' => opt_f = true,
                b'L' => opt_l = true,
                _ => return None,
            }
        }
        if opt_f {
            ar.func = Some(func);
        }
        if opt_l {
            ar.activelines = Some(match func.proto() {
                Some(pt) => {
                    let t = self.empty_table();
                    if !pt.lineinfo.is_empty() {
                        for i in 1..pt.bc.len() {
                            let line = pt.lineinfo[i];
                            let _ = t.set(Value::Num(line as f64), Value::Bool(true));
                        }
                    }
                    Value::Table(t)
                }
                None => Value::Nil,
            });
        }
        Some(ar)
    }

    /// `debug_localname`: name and stack slot of local `n` of a frame (negative `n` = varargs).
    fn localname(&self, fr: FrameRef, n: i32) -> Option<(Vec<u8>, usize)> {
        let th = fr.thread;
        let f: Frame = th.frame(fr.idx)?;
        let pc = framepc(th, fr.idx);
        let mut nextframe = next_link(th, fr.idx);
        let mut frame = f.base as usize - 1;
        if n < 0 {
            let pc = pc?;
            let pt = f.func.proto()?;
            if pt.flags() & PROTO_VARARG == 0 {
                return None;
            }
            let slot1 = pt.numparams as usize + (-(n as i64)) as usize;
            if f.vdelta != 0 {
                let _ = pc;
                nextframe = frame;
                frame -= f.vdelta as usize;
            }
            if frame + slot1 + 1 < nextframe {
                return Some((b"(*vararg)".to_vec(), frame + slot1));
            }
            return None;
        }
        let slot1 = n as usize;
        if let (Some(pc), Some(pt)) = (pc, f.func.proto())
            && slot1 >= 1
            && let Some(name) = varname(&pt, pc, slot1 as u32 - 1)
        {
            return Some((name, frame + slot1));
        }
        if slot1 > 0 && frame + slot1 + 1 < nextframe {
            return Some((b"(*temporary)".to_vec(), frame + slot1));
        }
        None
    }

    /// `lua_getlocal`: name and value of local `n` of a frame.
    pub fn get_local(&self, fr: FrameRef, n: i32) -> Option<(Vec<u8>, Value)> {
        self.localname(fr, n).map(|(name, slot)| (name, fr.thread.get(slot)))
    }

    /// `lua_setlocal`: assign local `n` of a frame; returns its name.
    pub fn set_local(&self, fr: FrameRef, n: i32, v: Value) -> Option<Vec<u8>> {
        let (name, slot) = self.localname(fr, n)?;
        if slot < fr.thread.stack_size() {
            fr.thread.set(slot, v);
        }
        Some(name)
    }

    /// `lua_getlocal(L, NULL, n)`: name of parameter/local `n` of a Lua function at its start.
    pub fn local_name_of(&self, f: Gc<Function>, n: i32) -> Option<Vec<u8>> {
        let pt = f.proto()?;
        if n < 1 {
            return None;
        }
        varname(&pt, 0, n as u32 - 1)
    }

    /// `lj_debug_uvnamev`: name and value of upvalue `idx` (0-based) of a function value.
    /// Native upvalues have an empty name.
    pub fn upvalue_info(&self, f: Value, idx: usize) -> Option<(Vec<u8>, Value)> {
        let Value::Func(f) = f else { return None };
        match f.proto() {
            Some(pt) => {
                if idx < pt.sizeuv() {
                    Some((uvname(&pt, idx), f.upvalue(idx)?))
                } else {
                    None
                }
            }
            None => f.upvalue(idx).map(|v| (Vec::new(), v)),
        }
    }

    /// `lua_setupvalue`: returns the upvalue's name, or `None` if out of range.
    pub fn set_upvalue_info(&self, f: Value, idx: usize, v: Value) -> Option<Vec<u8>> {
        let (name, _) = self.upvalue_info(f, idx)?;
        f.as_func()?.set_upvalue(idx, v);
        Some(name)
    }

    /// `luaL_traceback(L, L1, msg, level)` for thread `th`.
    pub fn traceback(&mut self, th: Gc<Thread>, msg: Option<&[u8]>, level: i32) -> Vec<u8> {
        let mut out = Vec::new();
        if let Some(m) = msg {
            out.extend_from_slice(m);
            out.push(b'\n');
        }
        out.extend_from_slice(b"stack traceback:");
        let mut level = level;
        let mut lim = TRACEBACK_LEVELS1;
        loop {
            let Some(fr) = self.get_stack(th, level) else { break };
            level += 1;
            if level > lim {
                if self.get_stack(th, level + TRACEBACK_LEVELS2).is_none() {
                    level -= 1;
                } else {
                    out.extend_from_slice(b"\n\t...");
                    level = self.stack_depth(th) - TRACEBACK_LEVELS2;
                }
                lim = i32::MAX;
                continue;
            }
            let ar = self.get_info(InfoTarget::Frame(fr), b"Snlf").expect("valid options");
            let f = ar.func.expect("f option");
            if f.is_builtin() && ar.namewhat.is_empty() {
                out.extend_from_slice(format!("\n\t[builtin#{}]:", f.ffid()).as_bytes());
            } else {
                out.extend_from_slice(b"\n\t");
                out.extend_from_slice(&ar.short_src);
                out.push(b':');
            }
            if ar.currentline > 0 {
                out.extend_from_slice(format!("{}:", ar.currentline).as_bytes());
            }
            if !ar.namewhat.is_empty() {
                out.extend_from_slice(b" in function '");
                out.extend_from_slice(crate::parse::lex::cstr(ar.name.as_deref().unwrap_or(b"")));
                out.push(b'\'');
            } else if ar.what.starts_with('m') {
                out.extend_from_slice(b" in main chunk");
            } else if ar.what.starts_with('C') {
                out.extend_from_slice(b" at ");
                crate::text::strfmt::put_ptr(&mut out, native_addr(&f));
            } else {
                out.extend_from_slice(b" in function <");
                out.extend_from_slice(&ar.short_src);
                out.extend_from_slice(format!(":{}>", ar.linedefined).as_bytes());
            }
        }
        out
    }

    /// `lj_debug_addloc`: prefix `msg` with `chunk:line:` of frame `idx` if it is a Lua frame
    /// with a known line.
    pub(crate) fn addloc(&self, th: Gc<Thread>, idx: Option<usize>, msg: &[u8]) -> Vec<u8> {
        if let Some(idx) = idx
            && let Some(f) = th.frame(idx)
            && let Some(pt) = f.func.proto()
        {
            let line = frameline(th, idx);
            if line >= 0 {
                let mut out = shortname(pt.chunkname.as_bytes(), pt.firstline);
                out.extend_from_slice(format!(":{line}: ").as_bytes());
                out.extend_from_slice(msg);
                return out;
            }
        }
        msg.to_vec()
    }

    /// `luaL_where(L, level)`: `chunk:line: ` of the frame at `level`, or `""`.
    pub fn where_(&self, level: i32) -> Vec<u8> {
        let th = self.cur_thread;
        let idx = self.get_stack(th, level).map(|f| f.idx);
        self.addloc(th, idx, b"")
    }
}

/// True if frame `idx` of `th` was called from a Lua frame's bytecode (Lua or Cont link), i.e.
/// has a caller position for error messages (`lj_err_callermsg`).
pub(crate) fn caller_frame(th: Gc<Thread>, idx: usize) -> Option<usize> {
    let f = th.frame(idx)?;
    match f.kind {
        FrameKind::Lua | FrameKind::Cont(_) if idx > 0 => Some(idx - 1),
        _ => None,
    }
}
