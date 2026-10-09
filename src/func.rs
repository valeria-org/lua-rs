//! Function prototypes, closures and upvalues (`GCproto`, `GCfunc`, `GCupval` of lj_obj.h;
//! object layout of lj_func.c).

use std::cell::Cell;
use std::fmt;

use crate::bc::Ins;
use crate::cdata::CData;
use crate::error::LuaResult;
use crate::gc::{Gc, GcBox, GcKind, GcObject, Marker, Trace};
use crate::lstr::LuaStr;
use crate::state::Vm;
use crate::table::Table;
use crate::thread::Thread;
use crate::value::Value;

// -- Prototype flags (lj_obj.h) -----------------------------------------------------------------

/// Has child prototypes.
pub const PROTO_CHILD: u8 = 0x01;
/// Vararg function.
pub const PROTO_VARARG: u8 = 0x02;
/// Uses BC_KCDATA for FFI datatypes.
pub const PROTO_FFI: u8 = 0x04;
/// JIT disabled for this function.
pub const PROTO_NOJIT: u8 = 0x08;
/// Patched bytecode with ILOOP etc.
pub const PROTO_ILOOP: u8 = 0x10;
/// Already emitted a return (parser only).
pub const PROTO_HAS_RETURN: u8 = 0x20;
/// Need to fixup emitted returns (parser only).
pub const PROTO_FIXUP_RETURN: u8 = 0x40;
/// Uses bit operator bytecodes.
pub const PROTO_BITOP: u8 = 0x80;
/// Base of saturating 3 bit closure counter.
pub const PROTO_CLCOUNT: u8 = 0x20;
pub const PROTO_CLC_BITS: u8 = 3;
/// Polymorphic threshold.
pub const PROTO_CLC_POLY: u8 = 3 * PROTO_CLCOUNT;

/// Upvalue refers to a local slot of the enclosing function (else: parent upvalue index).
pub const PROTO_UV_LOCAL: u16 = 0x8000;
/// Immutable upvalue.
pub const PROTO_UV_IMMUTABLE: u16 = 0x4000;

/// Internal variable names (`VARNAMEDEF` in lj_debug.h). 0 is the varinfo terminator.
pub const VARNAME_END: u8 = 0;
pub const VARNAME_FOR_IDX: u8 = 1;
pub const VARNAME_FOR_STOP: u8 = 2;
pub const VARNAME_FOR_STEP: u8 = 3;
pub const VARNAME_FOR_GEN: u8 = 4;
pub const VARNAME_FOR_STATE: u8 = 5;
pub const VARNAME_FOR_CTL: u8 = 6;
pub const VARNAME_MAX: u8 = 7;
/// Display strings of the internal variable names, indexed by `VARNAME_*`.
pub const VARNAME_STR: [&str; 7] =
    ["", "(for index)", "(for limit)", "(for step)", "(for generator)", "(for state)", "(for control)"];

/// Builtin id of Lua functions.
pub const FF_LUA: u8 = 0;
/// Builtin id of plain C/Rust functions; builtins (`lj_ffdef.h`) have ids > `FF_C`.
pub const FF_C: u8 = 1;

/// A collectable constant of a prototype.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum KGc {
    Str(Gc<LuaStr>),
    Proto(Gc<Proto>),
    /// Template table for TDUP.
    Table(Gc<Table>),
    /// 64-bit integer / complex constant for KCDATA.
    CData(Gc<CData>),
}

/// Name of a local variable in the debug info.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum VarName {
    /// One of the internal `VARNAME_*` ids (1..VARNAME_MAX).
    Internal(u8),
    Str(Gc<LuaStr>),
}

/// Local variable extent: live for `startpc <= pc < endpc`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VarInfo {
    pub name: VarName,
    pub startpc: u32,
    pub endpc: u32,
}

/// Function prototype (`GCproto`). Built once by the parser or bytecode loader.
pub struct Proto {
    /// Bytecode; `bc[0]` is the FUNCF/FUNCV header.
    pub bc: Box<[Ins]>,
    /// Collectable constants, indexed directly by the D operand (reverse of upstream's memory
    /// order).
    pub kgc: Box<[KGc]>,
    /// Number constants.
    pub knum: Box<[f64]>,
    /// Upvalue descriptors (`PROTO_UV_LOCAL | slot`, or parent upvalue index; may carry
    /// `PROTO_UV_IMMUTABLE`).
    pub uv: Box<[u16]>,
    pub numparams: u8,
    pub framesize: u8,
    /// `PROTO_*` flags (mutable: the closure counter and NOJIT bits change at runtime).
    pub flags: Cell<u8>,
    pub chunkname: Gc<LuaStr>,
    pub firstline: u32,
    pub numline: u32,
    /// Absolute source line of each instruction (`lineinfo[i]` for `bc[i]`, so `lineinfo[0]` is
    /// the header line), or empty if stripped.
    pub lineinfo: Box<[u32]>,
    /// Upvalue names, or empty if stripped.
    pub uvnames: Box<[Gc<LuaStr>]>,
    /// Local variable info, or empty if stripped.
    pub varinfo: Box<[VarInfo]>,
}

impl Proto {
    #[inline]
    pub fn flags(&self) -> u8 {
        self.flags.get()
    }
    #[inline]
    pub fn set_flags(&self, f: u8) {
        self.flags.set(f);
    }
    #[inline]
    pub fn is_vararg(&self) -> bool {
        self.flags.get() & PROTO_VARARG != 0
    }
    #[inline]
    pub fn sizeuv(&self) -> usize {
        self.uv.len()
    }
    /// Source line of instruction `pc`, if line info is present.
    #[inline]
    pub fn line(&self, pc: usize) -> Option<u32> {
        self.lineinfo.get(pc).copied()
    }
}

impl fmt::Debug for Proto {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Proto({:?}:{}, {} ins)", self.chunkname, self.firstline, self.bc.len())
    }
}

impl Trace for Proto {
    /// `gc_traverse_proto`.
    fn trace(&self, m: &mut Marker) {
        m.mark(self.chunkname);
        for k in self.kgc.iter() {
            match *k {
                KGc::Str(s) => m.mark(s),
                KGc::Proto(p) => m.mark(p),
                KGc::Table(t) => m.mark(t),
                KGc::CData(c) => m.mark(c),
            }
        }
        for &s in self.uvnames.iter() {
            m.mark(s);
        }
        for v in self.varinfo.iter() {
            if let VarName::Str(s) = v.name {
                m.mark(s);
            }
        }
    }

    fn size(&self) -> usize {
        std::mem::size_of::<GcBox<Proto>>()
            + self.bc.len() * 4
            + self.kgc.len() * std::mem::size_of::<KGc>()
            + self.knum.len() * 8
            + self.uv.len() * 2
            + self.lineinfo.len() * 4
            + self.uvnames.len() * 8
            + self.varinfo.len() * std::mem::size_of::<VarInfo>()
    }
}

impl GcObject for Proto {
    const KIND: GcKind = GcKind::Proto;
}

// -- Upvalues -----------------------------------------------------------------------------------

/// State of an upvalue.
#[derive(Clone, Copy, Debug)]
pub enum UpState {
    /// Refers to a live stack slot of `thread`.
    Open { thread: Gc<Thread>, slot: u32 },
    /// Holds its own value.
    Closed(Value),
}

/// Upvalue object (`GCupval`).
pub struct UpVal {
    state: Cell<UpState>,
    immutable: Cell<bool>,
}

impl fmt::Debug for UpVal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "UpVal({:?})", self.state.get())
    }
}

impl UpVal {
    pub fn new_open(thread: Gc<Thread>, slot: u32) -> UpVal {
        UpVal { state: Cell::new(UpState::Open { thread, slot }), immutable: Cell::new(false) }
    }
    pub fn new_closed(v: Value) -> UpVal {
        UpVal { state: Cell::new(UpState::Closed(v)), immutable: Cell::new(false) }
    }
    #[inline]
    pub fn state(&self) -> UpState {
        self.state.get()
    }
    #[inline]
    pub fn is_open(&self) -> bool {
        matches!(self.state.get(), UpState::Open { .. })
    }
    /// Stack slot of an open upvalue.
    #[inline]
    pub fn slot(&self) -> Option<u32> {
        match self.state.get() {
            UpState::Open { slot, .. } => Some(slot),
            UpState::Closed(_) => None,
        }
    }
    #[inline]
    pub fn get(&self) -> Value {
        match self.state.get() {
            UpState::Open { thread, slot } => thread.get(slot as usize),
            UpState::Closed(v) => v,
        }
    }
    #[inline]
    pub fn set(&self, v: Value) {
        match self.state.get() {
            UpState::Open { thread, slot } => thread.set(slot as usize, v),
            UpState::Closed(_) => self.state.set(UpState::Closed(v)),
        }
    }
    /// Close an open upvalue, capturing the current slot value.
    pub fn close(&self) {
        if let UpState::Open { thread, slot } = self.state.get() {
            self.state.set(UpState::Closed(thread.get(slot as usize)));
        }
    }
    #[inline]
    pub fn immutable(&self) -> bool {
        self.immutable.get()
    }
    #[inline]
    pub fn set_immutable(&self, b: bool) {
        self.immutable.set(b);
    }
}

impl Trace for UpVal {
    /// An open upvalue marks the value in its slot but not its thread (upstream `gc_mark_uv`):
    /// a coroutine reachable only through upvalues of closures it created is collected, and the
    /// collector closes its open upvalues first (`lj_state_free`), see `Vm::collect`.
    fn trace(&self, m: &mut Marker) {
        match self.state.get() {
            UpState::Open { thread, slot } => m.mark_value(thread.get(slot as usize)),
            UpState::Closed(v) => m.mark_value(v),
        }
    }
    fn size(&self) -> usize {
        std::mem::size_of::<GcBox<UpVal>>()
    }
}

impl GcObject for UpVal {
    const KIND: GcKind = GcKind::UpVal;
}

// -- Functions ----------------------------------------------------------------------------------

/// Rust native function: arguments are the current frame's slots; pushes results and returns
/// their count.
pub type NativeFn = fn(&mut Vm) -> LuaResult<usize>;
/// C API function (`lua_CFunction`); the argument is the `lua_State*`.
pub type CFunction = unsafe extern "C-unwind" fn(*mut core::ffi::c_void) -> core::ffi::c_int;

/// Implementation of a native function.
#[derive(Clone, Copy)]
pub enum NativeImpl {
    Rust(NativeFn),
    C(CFunction),
}

impl fmt::Debug for NativeImpl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NativeImpl::Rust(p) => write!(f, "Rust({:p})", *p as *const ()),
            NativeImpl::C(p) => write!(f, "C({:p})", *p as *const ()),
        }
    }
}

/// A closure (`GCfunc`).
#[derive(Debug)]
pub enum Function {
    Lua {
        proto: Gc<Proto>,
        /// Upvalue objects; cells because `debug.upvaluejoin` replaces them.
        upvals: Box<[Cell<Gc<UpVal>>]>,
        env: Cell<Gc<Table>>,
    },
    Native {
        f: NativeImpl,
        /// `FF_C` for plain natives, upstream builtin id (`lj_ffdef.h`) for builtins.
        ffid: u8,
        upvals: Box<[Cell<Value>]>,
        env: Cell<Gc<Table>>,
    },
}

impl Function {
    pub fn new_lua(proto: Gc<Proto>, upvals: Vec<Gc<UpVal>>, env: Gc<Table>) -> Function {
        Function::Lua { proto, upvals: upvals.into_iter().map(Cell::new).collect(), env: Cell::new(env) }
    }
    pub fn new_native(f: NativeImpl, ffid: u8, upvals: Vec<Value>, env: Gc<Table>) -> Function {
        Function::Native { f, ffid, upvals: upvals.into_iter().map(Cell::new).collect(), env: Cell::new(env) }
    }

    #[inline]
    pub fn is_lua(&self) -> bool {
        matches!(self, Function::Lua { .. })
    }
    /// `FF_LUA` for Lua functions, else the native's ffid.
    #[inline]
    pub fn ffid(&self) -> u8 {
        match self {
            Function::Lua { .. } => FF_LUA,
            Function::Native { ffid, .. } => *ffid,
        }
    }
    /// True for builtins (`isffunc`): printed as `function: builtin#N`.
    #[inline]
    pub fn is_builtin(&self) -> bool {
        self.ffid() > FF_C
    }
    #[inline]
    pub fn proto(&self) -> Option<Gc<Proto>> {
        match self {
            Function::Lua { proto, .. } => Some(*proto),
            Function::Native { .. } => None,
        }
    }
    #[inline]
    pub fn env(&self) -> Gc<Table> {
        match self {
            Function::Lua { env, .. } | Function::Native { env, .. } => env.get(),
        }
    }
    #[inline]
    pub fn set_env(&self, t: Gc<Table>) {
        match self {
            Function::Lua { env, .. } | Function::Native { env, .. } => env.set(t),
        }
    }
    /// Number of upvalues.
    #[inline]
    pub fn nupvalues(&self) -> usize {
        match self {
            Function::Lua { upvals, .. } => upvals.len(),
            Function::Native { upvals, .. } => upvals.len(),
        }
    }
    /// Upvalue object `i` of a Lua function.
    #[inline]
    pub fn upval(&self, i: usize) -> Option<Gc<UpVal>> {
        match self {
            Function::Lua { upvals, .. } => upvals.get(i).map(Cell::get),
            Function::Native { .. } => None,
        }
    }
    /// Upvalue objects of a Lua function (empty for natives).
    #[inline]
    pub fn lua_upvals(&self) -> &[Cell<Gc<UpVal>>] {
        match self {
            Function::Lua { upvals, .. } => upvals,
            Function::Native { .. } => &[],
        }
    }
    /// Replace upvalue object `i` of a Lua function (`debug.upvaluejoin`).
    pub fn set_upval(&self, i: usize, uv: Gc<UpVal>) {
        if let Function::Lua { upvals, .. } = self {
            upvals[i].set(uv);
        }
    }
    /// Value of upvalue `i` (either kind of function).
    pub fn upvalue(&self, i: usize) -> Option<Value> {
        match self {
            Function::Lua { upvals, .. } => upvals.get(i).map(|u| u.get().get()),
            Function::Native { upvals, .. } => upvals.get(i).map(Cell::get),
        }
    }
    /// Set the value of upvalue `i`; returns false if out of range.
    pub fn set_upvalue(&self, i: usize, v: Value) -> bool {
        match self {
            Function::Lua { upvals, .. } => upvals.get(i).map(|u| u.get().set(v)).is_some(),
            Function::Native { upvals, .. } => upvals.get(i).map(|c| c.set(v)).is_some(),
        }
    }
}

impl Trace for Function {
    /// `gc_traverse_func`.
    fn trace(&self, m: &mut Marker) {
        match self {
            Function::Lua { proto, upvals, env } => {
                m.mark(env.get());
                m.mark(*proto);
                for u in upvals.iter() {
                    m.mark(u.get());
                }
            }
            Function::Native { upvals, env, .. } => {
                m.mark(env.get());
                for u in upvals.iter() {
                    m.mark_value(u.get());
                }
            }
        }
    }
    fn size(&self) -> usize {
        std::mem::size_of::<GcBox<Function>>()
            + match self {
                Function::Lua { upvals, .. } => upvals.len() * 8,
                Function::Native { upvals, .. } => upvals.len() * 16,
            }
    }
}

impl GcObject for Function {
    const KIND: GcKind = GcKind::Func;
}
