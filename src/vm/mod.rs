//! The interpreter and everything that runs Lua code: bytecode execution (port of the semantics
//! of vm_x64.dasc), calls/returns and frames, metamethods (lj_meta.c), closures and upvalues
//! (lj_func.c), errors and unwinding (lj_err.c), coroutines (lj_state.c / lib_base.c resume),
//! hooks (lj_dispatch.c), VM events (lj_vmevent.c), debug information (lj_debug.c), chunk
//! loading (lj_load.c) and the native API used by all library code ([`api`]).
//!
//! # Execution model
//! Every call frame lives on the thread's frame vector ([`crate::thread::Frame`]); Lua→Lua
//! calls, tail calls, metamethod calls made by bytecode, `pcall`/`xpcall` and iterator calls
//! never recurse in Rust, so `coroutine.yield` works across all of them. A Rust native that
//! calls back into Lua (`Vm::call`, `Vm::pcall`, metamethod-aware API helpers) starts a nested
//! interpreter run and thereby a C-call boundary: yielding across it raises
//! `attempt to yield across C-call boundary`.

pub mod api;
pub mod debug;
pub mod ffid;

mod call;
mod coro;
mod err;
pub mod hook;
mod interp;
mod load;
mod meta;
mod vmevent;

pub use api::TAILCALL;
pub use debug::{DebugInfo, FrameRef};
pub use err::{ErrMsg, fmt_msg};
pub use meta::{modf, num2bit, num2int, obj_ptr};
pub use debug::InfoTarget;
pub use hook::{HookEvent, HookFn, LUA_MASKCALL, LUA_MASKCOUNT, LUA_MASKLINE, LUA_MASKRET};
pub use vmevent::{VMEVENT_ERRFIN, VMEVENTS_REGKEY, vmevent_hash};

use crate::cdata::CData;
use crate::error::LuaResult;
use crate::func::CFunction;
use crate::gc::Gc;
use crate::state::Vm;

/// Maximum nesting of Rust-level re-entries into the VM (native → `Vm::call`, coroutine
/// resumes, metamethods called from natives, error handlers, hooks, finalizers). Exceeding it
/// raises "C stack overflow". Upstream has no such limit and crashes when the C stack runs
/// out; this keeps the Rust stack bounded instead.
pub const LJ_MAX_CCALLS: u32 = 1000;

/// Dispatcher for `NativeImpl::C` functions, installed by the C API: it must run the C
/// function with a `lua_State*` for `vm`, convert a `lua_error` unwind into `Err`, and return
/// the number of results the C function returned (the results are the top values of the
/// current frame, like for Rust natives).
pub type CCallHook = fn(&mut Vm, CFunction) -> LuaResult<usize>;

/// `FfiHooks::checkbit64` signature.
pub type CheckBit64Fn = fn(&mut Vm, Gc<CData>, &mut u32) -> LuaResult<u64>;
/// `FfiHooks::tonumber` signature.
pub type CdataToNumberFn = fn(&mut Vm, Gc<CData>) -> LuaResult<Option<f64>>;

/// Hooks the FFI library installs so the core VM can handle cdata where upstream calls into
/// the FFI directly.
#[derive(Clone, Copy, Default)]
pub struct FfiHooks {
    /// `lj_carith_checkbit64` for a cdata operand of a bit operator: convert the cdata to 64
    /// bits; set `*id` to `CTID_UINT64` for uint64 operands, or to `CTID_INT64` if it is still 0.
    /// Errors with upstream's messages for non-integer cdata.
    pub checkbit64: Option<CheckBit64Fn>,
    /// `tonumber(cdata)`: the number for numeric/complex/enum cdata, `None` otherwise.
    pub tonumber: Option<CdataToNumberFn>,
}

/// One activation of the interpreter (upstream: a C frame / `cframe`).
#[derive(Clone, Copy, Debug)]
pub(crate) struct Run {
    /// Last bytecode position seen by the instruction hook in this run (`cframe_pc`):
    /// (proto address, pc index). `None` = "outside of bytecode".
    pub hook_oldpc: Option<(usize, u32)>,
}

/// VM-side runtime state stored in [`Vm::rt`].
#[derive(Default)]
pub struct Runtime {
    /// Hook function and mask (`g->hookf`, `g->hookmask`, `g->hookcount`, `g->hookcstart`).
    pub(crate) hook: hook::HookState,
    /// True if the innermost run was entered by `coroutine.resume` and no C-call boundary was
    /// crossed since (`cframe_canyield`).
    pub(crate) can_yield: bool,
    /// Current nesting depth of Rust-level VM entries.
    pub(crate) nccalls: u32,
    /// Stack of active interpreter runs.
    pub(crate) runs: Vec<Run>,
    /// Number of results of the last multi-result call/vararg (`MULTRES`).
    pub(crate) multres: usize,
    /// Finalizers are currently being run (prevents re-entrant draining).
    pub(crate) in_finalizers: bool,
    /// Value of the last error whose handler was already applied (`lj_err_run` ran), so that
    /// outer runs rethrow it without calling the handler again. Rooted.
    pub(crate) thrown: Option<crate::value::Value>,
    /// Dedicated thread for finalizers and VM event handlers (`vmthread(g)`). Rooted.
    pub(crate) vmthread: Option<Gc<crate::thread::Thread>>,
    /// Dispatcher for C functions (installed by the C API).
    pub c_call: Option<CCallHook>,
    /// FFI integration points.
    pub ffi: FfiHooks,
}

impl Runtime {
    /// Mark the GC values held by the runtime (called from the root marking of `Vm`).
    pub fn trace(&self, m: &mut crate::gc::Marker) {
        if let Some(v) = self.thrown {
            m.mark_value(v);
        }
        if let Some(t) = self.vmthread {
            m.mark(t);
        }
    }
}

impl Vm {
    /// The VM's internal thread used for finalizers and VM events (created on first use).
    pub(crate) fn vm_thread(&mut self) -> Gc<crate::thread::Thread> {
        match self.rt.vmthread {
            Some(t) => t,
            None => {
                let t = self.new_thread(self.globals);
                t.set_status(crate::thread::ThreadStatus::Normal);
                self.rt.vmthread = Some(t);
                t
            }
        }
    }

    /// `luaL_newstate`: an empty state plus the default VM event handler that reports errors
    /// in `__gc` finalizers on stderr (`ERROR in finalizer: ...`). Libraries are opened with
    /// [`crate::stdlib::open_libs`].
    pub fn new_state() -> Vm {
        let mut vm = Vm::new();
        vmevent::install_default_handlers(&mut vm);
        vm
    }
}
