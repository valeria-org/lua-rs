//! Global VM state, allocation entry points and the collector driver.
//! Port of the state setup of lj_state.c and the cycle/pacing logic of lj_gc.c, adapted to a
//! stop-the-world collector (one full mark & sweep per completed "cycle").

use std::any::Any;
use std::collections::VecDeque;

use crate::bc::{MM, MM_MAX};
use crate::cdata::CData;
use crate::error::{ErrorKind, LuaError};
use crate::func::{Function, NativeImpl, Proto, UpVal};
use crate::gc::{GC_FINALIZED, GC_MARKED, Gc, GcObject, Heap, Marker, Trace};
use crate::lstr::{LuaStr, StrTab};
use crate::table::{TabError, Table};
use crate::thread::Thread;
use crate::udata::{UdExt, Userdata};
use crate::value::{VType, Value};

/// Min. global table size in hash bits (`LJ_MIN_GLOBAL`).
pub const LJ_MIN_GLOBAL: u32 = 6;
/// Min. registry size in hash bits (`LJ_MIN_REGISTRY`).
pub const LJ_MIN_REGISTRY: u32 = 2;
/// Default GC pause (`LUAI_GCPAUSE`).
pub const GC_PAUSE_DEFAULT: u32 = 200;
/// Default GC step multiplier (`LUAI_GCMUL`).
pub const GC_STEPMUL_DEFAULT: u32 = 200;
/// Work unit of one GC step (`GCSTEPSIZE`).
pub const GC_STEPSIZE: usize = 1024;
/// "Infinite" threshold used to stop the collector (`LJ_MAX_MEM`).
pub const GC_MAX_MEM: usize = 1 << 47;
/// Estimated per-object sweep cost (`GCSWEEPCOST`).
const GC_SWEEPCOST: usize = 10;
/// Fixed string seed: hashing and string ids are deterministic.
const STR_SEED: u64 = 0x9e37_79b9_7f4a_7c15;

/// Rust-side state attached to the VM and traced as a GC root (used by the FFI).
pub trait VmExt: Any + Trace {}

impl<T: Any + Trace> VmExt for T {}

/// Collector parameters and pacing state (`g->gc`).
#[derive(Clone, Debug)]
pub struct GcState {
    /// Collection starts when the heap total reaches this many bytes.
    pub threshold: usize,
    pub pause: u32,
    pub stepmul: u32,
    debt: usize,
    estimate: usize,
    /// Mark work of the last cycle: bytes of the reachable objects upstream charges for
    /// traversal (see [`crate::gc::Swept::traversal`]).
    traversal: usize,
    /// Work credited towards the next full cycle by incremental steps.
    work: usize,
    /// Number of completed cycles.
    pub cycles: u64,
    /// GC stress mode, a debugging aid for rooting bugs: enabled by setting the environment
    /// variable `LUAJIT_RS_GCSTRESS` to a value other than `0` (read once in [`Vm::new`]).
    /// Every GC safe point ([`Vm::gc_check`]) then runs a full collection unless the collector
    /// is stopped, so a value held across a safe point without being rooted is freed right
    /// away. In debug builds freed objects are also quarantined
    /// ([`crate::gc::Heap::enable_quarantine`]), which turns a later use into a deterministic
    /// panic. Costs one predictable branch per safe point when off.
    pub stress: bool,
}

/// Read the `LUAJIT_RS_GCSTRESS` switch (see [`GcState::stress`]).
fn gc_stress_from_env() -> bool {
    std::env::var_os("LUAJIT_RS_GCSTRESS").is_some_and(|v| !v.is_empty() && v != "0")
}

/// The global state.
pub struct Vm {
    pub heap: Heap,
    pub strtab: StrTab,
    /// Registry table (`LUA_REGISTRYINDEX`).
    pub registry: Gc<Table>,
    /// Globals (`LUA_GLOBALSINDEX`, environment of the main thread).
    pub globals: Gc<Table>,
    pub main_thread: Gc<Thread>,
    /// Currently running thread.
    pub cur_thread: Gc<Thread>,
    /// Metatables for base types (`GCROOT_BASEMT`), indexed by `VType`.
    pub basemt: [Option<Gc<Table>>; VType::COUNT],
    mmname: [Gc<LuaStr>; MM_MAX],
    /// The empty string (fixed).
    pub empty_str: Gc<LuaStr>,
    /// Objects resurrected for finalization, oldest first (`mmudata`). Rooted. The VM drains it
    /// with `take_finalizer` at safe points.
    pub pending_finalizers: VecDeque<Value>,
    /// FFI finalizer table (`GCROOT_FFI_FIN`): cdata key → finalizer. Keys are weak but the table
    /// is never cleared; set by the FFI library.
    pub ffi_fin: Option<Gc<Table>>,
    /// FFI state (ctype tables etc.), traced as a root.
    pub ffi: Option<Box<dyn VmExt>>,
    /// Default io input/output file userdata (`GCROOT_IO_INPUT/OUTPUT`).
    pub io_input: Value,
    pub io_output: Value,
    pub gc: GcState,
    /// Interpreter runtime state (hooks, run stack, C-call depth, FFI/C API dispatch hooks).
    pub rt: crate::vm::Runtime,
}

impl Default for Vm {
    fn default() -> Self {
        Vm::new()
    }
}

impl Vm {
    /// Empty state: registry, globals, main thread, fixed metamethod names; no libraries.
    pub fn new() -> Vm {
        let mut heap = Heap::new();
        let stress = gc_stress_from_env();
        if stress {
            heap.enable_quarantine();
        }
        let mut strtab = StrTab::new(STR_SEED);
        let empty_str = strtab.intern(&mut heap, b"");
        empty_str.fix();
        let mmname = MM::ALL.map(|mm| {
            let s = strtab.intern(&mut heap, mm.name().as_bytes());
            s.fix();
            s
        });
        let globals = heap.alloc(Table::new(0, LJ_MIN_GLOBAL).expect("small table"));
        let registry = heap.alloc(Table::new(0, LJ_MIN_REGISTRY).expect("small table"));
        let main_thread = heap.alloc(Thread::new(globals));
        main_thread.set_status(crate::thread::ThreadStatus::Running);
        let total = heap.total();
        Vm {
            heap,
            strtab,
            registry,
            globals,
            main_thread,
            cur_thread: main_thread,
            basemt: [None; VType::COUNT],
            mmname,
            empty_str,
            pending_finalizers: VecDeque::new(),
            ffi_fin: None,
            ffi: None,
            io_input: Value::Nil,
            io_output: Value::Nil,
            rt: crate::vm::Runtime::default(),
            gc: GcState {
                threshold: 4 * total,
                pause: GC_PAUSE_DEFAULT,
                stepmul: GC_STEPMUL_DEFAULT,
                debt: 0,
                estimate: total,
                traversal: total,
                work: 0,
                cycles: 0,
                stress,
            },
        }
    }

    // -- Allocation (never collects) --

    /// Allocate any heap object.
    #[inline]
    pub fn alloc<T: GcObject>(&mut self, v: T) -> Gc<T> {
        self.heap.alloc(v)
    }

    /// Intern a byte string (`lj_str_new`).
    #[inline]
    pub fn new_str(&mut self, s: &[u8]) -> Gc<LuaStr> {
        self.strtab.intern(&mut self.heap, s)
    }

    /// Runtime error carrying a message string.
    pub fn msg_error(&mut self, msg: &str) -> LuaError {
        let s = self.new_str(msg.as_bytes());
        LuaError::new(ErrorKind::Run, Value::Str(s))
    }

    /// Convert a table error into a runtime error with the upstream message (no position).
    pub fn tab_error(&mut self, e: TabError) -> LuaError {
        self.msg_error(e.msg())
    }

    /// `lj_tab_new`: `asize` array slots (non-inclusive) and `2^hbits` hash slots.
    pub fn new_table(&mut self, asize: u32, hbits: u32) -> Result<Gc<Table>, LuaError> {
        match Table::new(asize, hbits) {
            Ok(t) => Ok(self.heap.alloc(t)),
            Err(e) => Err(self.tab_error(e)),
        }
    }

    /// `lua_createtable` sizing.
    pub fn new_table_ah(&mut self, narr: u32, nrec: u32) -> Result<Gc<Table>, LuaError> {
        match Table::new_ah(narr, nrec) {
            Ok(t) => Ok(self.heap.alloc(t)),
            Err(e) => Err(self.tab_error(e)),
        }
    }

    /// New empty table.
    pub fn empty_table(&mut self) -> Gc<Table> {
        self.heap.alloc(Table::new(0, 0).expect("empty table"))
    }

    /// TDUP: copy a template table.
    pub fn dup_table(&mut self, t: Gc<Table>) -> Gc<Table> {
        let d = t.dup();
        self.heap.alloc(d)
    }

    /// Raw userdata with `len` zeroed bytes (`lua_newuserdata`).
    pub fn new_udata(&mut self, len: usize, env: Gc<Table>) -> Gc<Userdata> {
        self.heap.alloc(Userdata::new_raw(len, env))
    }

    /// Userdata with a Rust payload.
    pub fn new_udata_ext(&mut self, ext: Box<dyn UdExt>, udtype: u8, env: Gc<Table>) -> Gc<Userdata> {
        self.heap.alloc(Userdata::new_ext(ext, udtype, env))
    }

    pub fn new_proto(&mut self, p: Proto) -> Gc<Proto> {
        self.heap.alloc(p)
    }

    pub fn new_lua_function(&mut self, proto: Gc<Proto>, upvals: Vec<Gc<UpVal>>, env: Gc<Table>) -> Gc<Function> {
        self.heap.alloc(Function::new_lua(proto, upvals, env))
    }

    pub fn new_native(&mut self, f: NativeImpl, ffid: u8, upvals: Vec<Value>, env: Gc<Table>) -> Gc<Function> {
        self.heap.alloc(Function::new_native(f, ffid, upvals, env))
    }

    pub fn new_upval_closed(&mut self, v: Value) -> Gc<UpVal> {
        self.heap.alloc(UpVal::new_closed(v))
    }

    /// Find or create the open upvalue of `thread` for `slot` (`lj_func_finduv`).
    pub fn find_upval(&mut self, thread: Gc<Thread>, slot: u32) -> Gc<UpVal> {
        let heap = &mut self.heap;
        thread.find_upval(slot, || heap.alloc(UpVal::new_open(thread, slot)))
    }

    /// New coroutine with environment `env` (`lj_state_new`).
    pub fn new_thread(&mut self, env: Gc<Table>) -> Gc<Thread> {
        self.heap.alloc(Thread::new(env))
    }

    /// Cdata owning `size` zeroed bytes.
    pub fn new_cdata(&mut self, ctypeid: u32, size: usize, align: usize) -> Gc<CData> {
        self.heap.alloc(CData::new(ctypeid, size, align))
    }

    /// Cdata referring to external memory.
    pub fn new_cdata_external(&mut self, ctypeid: u32, ptr: *mut u8, size: usize) -> Gc<CData> {
        self.heap.alloc(CData::new_external(ctypeid, ptr, size))
    }

    // -- Metatables --

    /// Interned metamethod name (`mmname_str`).
    #[inline]
    pub fn mm_name(&self, mm: MM) -> Gc<LuaStr> {
        self.mmname[mm as usize]
    }

    /// `lj_meta_fastg`: metamethod `mm` of metatable `mt`, using the negative cache.
    #[inline]
    pub fn fast_mm(&self, mt: Option<Gc<Table>>, mm: MM) -> Option<Value> {
        mt.and_then(|t| t.meta_fast(mm, self.mm_name(mm)))
    }

    #[inline]
    pub fn basemt(&self, t: VType) -> Option<Gc<Table>> {
        self.basemt[t as usize]
    }
    #[inline]
    pub fn set_basemt(&mut self, t: VType, mt: Option<Gc<Table>>) {
        self.basemt[t as usize] = mt;
    }

    /// Metatable of any value (own metatable for tables/userdata, base metatable otherwise).
    pub fn metatable_of(&self, v: Value) -> Option<Gc<Table>> {
        match v {
            Value::Table(t) => t.metatable(),
            Value::Udata(u) => u.metatable(),
            v => self.basemt(v.vtype()),
        }
    }

    // -- Collector --

    /// Heap size in bytes (`collectgarbage("count") * 1024`).
    #[inline]
    pub fn gc_count_bytes(&mut self) -> usize {
        self.heap.total()
    }

    /// `lj_gc_check`: run a GC step if the threshold is reached (in stress mode: a full
    /// collection unless the collector is stopped). Call only at safe points.
    /// Returns true if a step ran.
    #[inline]
    pub fn gc_check(&mut self) -> bool {
        if self.gc.stress {
            return self.gc_stress_collect();
        }
        if self.heap.total() >= self.gc.threshold {
            self.gc_step_once();
            true
        } else {
            false
        }
    }

    #[cold]
    #[inline(never)]
    fn gc_stress_collect(&mut self) -> bool {
        if !self.gc_is_running() {
            return false;
        }
        self.gc_full();
        true
    }

    /// `lj_gc_fullgc`. Finalizers are queued, not run.
    pub fn gc_full(&mut self) {
        self.collect();
        self.gc.debt = 0;
        self.gc.work = 0;
        self.gc.threshold = (self.gc.estimate / 100) * self.gc.pause as usize;
    }

    /// `lua_gc(L, LUA_GCSTEP, kb)`: returns true if a cycle finished.
    pub fn gc_step(&mut self, kb: i32) -> bool {
        // Negative sizes wrap to a huge byte count upstream, i.e. threshold 0.
        let a = if kb < 0 { usize::MAX } else { (kb as usize) << 10 };
        let total = self.heap.total();
        self.gc.threshold = total.saturating_sub(a);
        while self.heap.total() >= self.gc.threshold {
            if self.gc_step_once() > 0 {
                return true;
            }
        }
        false
    }

    /// `LUA_GCSTOP`.
    pub fn gc_stop(&mut self) {
        self.gc.threshold = GC_MAX_MEM;
    }
    /// `LUA_GCRESTART` (`data == -1`: restart with the pause-derived threshold).
    pub fn gc_restart(&mut self, data: i32) {
        let total = self.heap.total();
        self.gc.threshold = if data == -1 { (total / 100) * self.gc.pause as usize } else { total };
    }
    /// `LUA_GCISRUNNING`.
    pub fn gc_is_running(&self) -> bool {
        self.gc.threshold != GC_MAX_MEM
    }
    /// `LUA_GCSETPAUSE`: returns the old value.
    pub fn gc_set_pause(&mut self, p: u32) -> u32 {
        std::mem::replace(&mut self.gc.pause, p)
    }
    /// `LUA_GCSETSTEPMUL`: returns the old value.
    pub fn gc_set_stepmul(&mut self, m: u32) -> u32 {
        std::mem::replace(&mut self.gc.stepmul, m)
    }

    /// `lj_gc_step`: perform one unit of GC work. Incremental work is credited until it covers
    /// the cost of a full cycle, which then runs atomically. Returns 1 if a cycle finished,
    /// 0 if more work is owed, -1 otherwise.
    fn gc_step_once(&mut self) -> i32 {
        let mut lim = (GC_STEPSIZE / 100) * self.gc.stepmul as usize;
        if lim == 0 {
            lim = GC_MAX_MEM;
        }
        let total = self.heap.total();
        if total > self.gc.threshold {
            self.gc.debt += total - self.gc.threshold;
        }
        self.gc.work = self.gc.work.saturating_add(lim);
        if self.gc.work >= self.cycle_cost() {
            self.collect();
            self.gc.work = 0;
            self.gc.debt = 0;
            self.gc.threshold = (self.gc.estimate / 100) * self.gc.pause as usize;
            return 1;
        }
        if self.gc.debt < GC_STEPSIZE {
            self.gc.threshold = total + GC_STEPSIZE;
            -1
        } else {
            self.gc.debt -= GC_STEPSIZE;
            self.gc.threshold = total;
            0
        }
    }

    /// Work of one incremental cycle as upstream's `gc_onestep` charges it: traversing the
    /// reachable tables/functions/prototypes/threads (`propagatemark`; estimated by the last
    /// cycle's), then `GCSWEEPCOST` per string hash chain and per other object (garbage
    /// included). Garbage bytes cost nothing, so heap growth in allocation-heavy loops stays
    /// proportional to the live heap.
    fn cycle_cost(&self) -> usize {
        let strings = self.strtab.len();
        let others = self.heap.object_count().saturating_sub(strings);
        self.gc.traversal + (self.strtab.chains() + others) * GC_SWEEPCOST
    }

    fn mark_roots(&self, m: &mut Marker) {
        m.mark(self.main_thread);
        m.mark(self.cur_thread);
        m.mark(self.registry);
        m.mark(self.globals);
        m.mark(self.empty_str);
        for &s in &self.mmname {
            m.mark(s);
        }
        for mt in self.basemt.iter().flatten() {
            m.mark(*mt);
        }
        for &v in &self.pending_finalizers {
            m.mark_value(v);
        }
        if let Some(t) = self.ffi_fin {
            m.mark(t);
        }
        if let Some(f) = &self.ffi {
            f.trace(m);
        }
        m.mark_value(self.io_input);
        m.mark_value(self.io_output);
        self.rt.trace(m);
    }

    /// `lj_gc_separateudata`: queue unreachable (or, with `all`, every) userdata with a `__gc`
    /// metamethod for finalization, newest first, and mark them. Each userdata is considered
    /// once in its lifetime.
    fn separate_udata(&mut self, m: &mut Marker, all: bool) {
        let cands: Vec<Gc<Userdata>> = self
            .heap
            .objects()
            .rev()
            .filter_map(|o| o.downcast::<Userdata>())
            .filter(|u| (all || !u.header().is_marked()) && !u.header().is_finalized())
            .collect();
        for u in cands {
            u.header().set_flag(GC_FINALIZED);
            if self.fast_mm(u.metatable(), MM::Gc).is_some() {
                self.pending_finalizers.push_back(Value::Udata(u));
                m.mark(u);
            }
        }
    }

    /// One complete stop-the-world cycle.
    fn collect(&mut self) {
        let oldst = crate::vm::hook::set_vmstate(crate::vm::hook::VMSTATE_GC);
        self.collect_inner();
        crate::vm::hook::set_vmstate(oldst);
    }

    fn collect_inner(&mut self) {
        let mut m = Marker::new(Some(self.mm_name(MM::Mode)), self.ffi_fin);
        self.mark_roots(&mut m);
        m.propagate();
        self.separate_udata(&mut m, false);
        m.propagate();
        // All marking done: clear weak tables (gc_clearweak / gc_mayclear).
        for (t, mode) in m.take_weak() {
            t.clear_weak(mode, |v, is_val| match v {
                Value::Str(s) => {
                    // Strings are not weak references, but must be kept alive.
                    s.header().set_flag(GC_MARKED);
                    false
                }
                Value::Udata(u) => !u.header().is_marked() || (is_val && u.header().is_finalized()),
                Value::Table(o) => !o.header().is_marked(),
                Value::Func(o) => !o.header().is_marked(),
                Value::Thread(o) => !o.header().is_marked(),
                Value::CData(o) => !o.header().is_marked(),
                Value::Proto(o) => !o.header().is_marked(),
                Value::Nil | Value::Bool(_) | Value::Num(_) | Value::LightUd(_) => false,
            });
        }
        // Resurrect unreachable cdata with finalizers (lj_cdata_free during sweep).
        let fin: Vec<Gc<CData>> = self
            .heap
            .objects()
            .rev()
            .filter_map(|o| o.downcast::<CData>())
            .filter(|c| !c.header().is_marked() && c.has_finalizer())
            .collect();
        for c in fin {
            c.header().set_flag(GC_MARKED | GC_FINALIZED);
            self.pending_finalizers.push_back(Value::CData(c));
        }
        self.strtab.purge();
        let swept = self.heap.sweep();
        self.gc.estimate = swept.live;
        self.gc.traversal = swept.traversal;
        self.gc.cycles += 1;
    }

    /// Prepare finalization of everything at state close (`lua_close`): queue every userdata
    /// with `__gc` not yet finalized and every cdata with a registered finalizer, and disable
    /// further cdata finalizer registration (the finalizer table loses its metatable).
    pub fn queue_all_finalizers(&mut self) {
        let mut m = Marker::new(None, None);
        self.separate_udata(&mut m, true);
        // Clear the marks set above; they are only meaningful during a collection.
        for o in self.heap.objects() {
            o.header().clear_flag(GC_MARKED);
        }
        if let Some(t) = self.ffi_fin {
            t.set_metatable(None);
            for i in (0..t.node_count()).rev() {
                if let (Value::CData(c), v) = t.node_get(i)
                    && !v.is_nil()
                {
                    self.pending_finalizers.push_back(Value::CData(c));
                }
            }
        }
    }

    /// True if finalizers are waiting to run.
    #[inline]
    pub fn has_pending_finalizers(&self) -> bool {
        !self.pending_finalizers.is_empty()
    }

    /// Dequeue the next finalizer to run (`gc_finalize`): returns `(finalizer, object)`; the VM
    /// calls `finalizer(object)` with hooks disabled and GC steps suppressed. Objects whose
    /// finalizer vanished meanwhile are skipped.
    pub fn take_finalizer(&mut self) -> Option<(Value, Value)> {
        while let Some(o) = self.pending_finalizers.pop_front() {
            match o {
                Value::CData(c) => {
                    c.set_has_finalizer(false);
                    if let Some(t) = self.ffi_fin {
                        let f = t.get(o);
                        if !f.is_nil() {
                            t.set_existing(o, Value::Nil);
                            return Some((f, o));
                        }
                    }
                }
                Value::Udata(u) => {
                    if let Some(f) = self.fast_mm(u.metatable(), MM::Gc) {
                        return Some((f, o));
                    }
                }
                _ => {}
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(vm: &mut Vm, x: &str) -> Value {
        Value::Str(vm.new_str(x.as_bytes()))
    }

    #[test]
    fn interning_identity_via_vm() {
        let mut vm = Vm::new();
        let a = vm.new_str(b"foo");
        let b = vm.new_str(b"foo");
        assert_eq!(a, b);
        assert_eq!(vm.mm_name(MM::Index).as_bytes(), b"__index");
        assert_eq!(vm.new_str(b"__gc"), vm.mm_name(MM::Gc));
        assert_eq!(vm.new_str(b""), vm.empty_str);
    }

    #[test]
    fn frees_unreachable_keeps_rooted() {
        let mut vm = Vm::new();
        vm.gc_full();
        let base_objs = vm.heap.object_count();
        let base_bytes = vm.gc_count_bytes();
        let kept = vm.empty_table();
        let key = s(&mut vm, "kept");
        vm.globals.set(key, Value::Table(kept)).unwrap();
        let inner = vm.new_str(b"inner string");
        kept.set_int(1, Value::Str(inner)).unwrap();
        for i in 0..100 {
            let t = vm.empty_table();
            let k = s(&mut vm, &format!("garbage{i}"));
            t.set(k, Value::Bool(true)).unwrap();
        }
        assert!(vm.heap.object_count() > base_objs + 200);
        vm.gc_full();
        // Survivors: the kept table, its key and the inner string.
        assert_eq!(vm.heap.object_count(), base_objs + 3);
        assert!(vm.gc_count_bytes() > base_bytes);
        assert_eq!(vm.globals.get(key), Value::Table(kept));
        assert_eq!(kept.get_int(1), Value::Str(inner));
        assert!(vm.strtab.find(b"garbage5").is_none());
        assert_eq!(vm.strtab.find(b"inner string"), Some(inner));
        // Values on a thread stack below top are roots; above top they are not.
        let th = vm.main_thread;
        let a = vm.empty_table();
        let b = vm.empty_table();
        th.set(0, Value::Table(a));
        th.set(1, Value::Table(b));
        th.set_top(1);
        let before = vm.heap.object_count();
        vm.gc_full();
        assert_eq!(vm.heap.object_count(), before - 1);
        assert_eq!(th.get(0), Value::Table(a));
        assert_eq!(th.get(1), Value::Nil);
    }

    #[test]
    fn weak_tables() {
        for (mode, key_cleared, val_cleared) in [("k", true, false), ("v", false, true), ("kv", true, true)] {
            let mut vm = Vm::new();
            let wt = vm.empty_table();
            let mt = vm.empty_table();
            let mode_key = Value::Str(vm.mm_name(MM::Mode));
            let mode_val = s(&mut vm, mode);
            mt.set(mode_key, mode_val).unwrap();
            wt.set_metatable(Some(mt));
            let k = s(&mut vm, "wt");
            vm.globals.set(k, Value::Table(wt)).unwrap();
            // [garbage table] = 1     -> cleared if keys are weak
            let gk = vm.empty_table();
            wt.set(Value::Table(gk), Value::Num(1.0)).unwrap();
            // ["v"] = garbage table    -> cleared if values are weak
            let gv = vm.empty_table();
            let vk = s(&mut vm, "v");
            wt.set(vk, Value::Table(gv)).unwrap();
            // [1] = garbage table (array part)
            let ga = vm.empty_table();
            wt.set_int(1, Value::Table(ga)).unwrap();
            // ["str"] = "string value": strings are never cleared.
            let sk = s(&mut vm, "str");
            let sv = s(&mut vm, "string value");
            wt.set(sk, sv).unwrap();
            // [rooted] = rooted: never cleared.
            let rooted = vm.empty_table();
            let rk = s(&mut vm, "rooted");
            vm.registry.set(rk, Value::Table(rooted)).unwrap();
            wt.set(Value::Table(rooted), Value::Table(rooted)).unwrap();

            vm.gc_full();
            assert_eq!(wt.get(Value::Table(gk)).is_nil(), key_cleared, "mode {mode}");
            assert_eq!(wt.get(vk).is_nil(), val_cleared, "mode {mode}");
            assert_eq!(wt.get_int(1).is_nil(), val_cleared, "mode {mode}");
            assert_eq!(wt.get(sk), sv, "mode {mode}");
            assert_eq!(wt.get(Value::Table(rooted)), Value::Table(rooted), "mode {mode}");
            // The string value survived the sweep.
            assert_eq!(vm.strtab.find(b"string value"), sv.as_str());
            // Non-weak parts kept their objects alive.
            if !key_cleared {
                assert_eq!(wt.get(Value::Table(gk)), Value::Num(1.0));
            }
            if !val_cleared {
                assert_eq!(wt.get(vk), Value::Table(gv));
                assert!(gv.is_empty());
            }
        }
    }

    #[test]
    fn userdata_finalizer_once() {
        let mut vm = Vm::new();
        let mt = vm.empty_table();
        let gcname = Value::Str(vm.mm_name(MM::Gc));
        let fin = vm.new_native(NativeImpl::Rust(|_| Ok(0)), crate::func::FF_C, vec![], vm.globals);
        mt.set(gcname, Value::Func(fin)).unwrap();
        let k = s(&mut vm, "mt");
        vm.registry.set(k, Value::Table(mt)).unwrap();
        let k2 = s(&mut vm, "fin");
        vm.registry.set(k2, Value::Func(fin)).unwrap();

        let u = vm.new_udata(32, vm.globals);
        u.set_metatable(Some(mt));
        // Also reference the userdata from a weak-valued table: finalized userdata is cleared
        // from values (gc_mayclear).
        let wt = vm.empty_table();
        let wmt = vm.empty_table();
        let mode_key = Value::Str(vm.mm_name(MM::Mode));
        let v = s(&mut vm, "v");
        wmt.set(mode_key, v).unwrap();
        wt.set_metatable(Some(wmt));
        wt.set_int(1, Value::Udata(u)).unwrap();
        let k3 = s(&mut vm, "wt");
        vm.registry.set(k3, Value::Table(wt)).unwrap();
        // A userdata without __gc is just freed.
        let plain = vm.new_udata(8, vm.globals);
        let plain_addr = plain.addr();

        vm.gc_full();
        assert_eq!(vm.pending_finalizers.len(), 1);
        assert_eq!(vm.pending_finalizers[0], Value::Udata(u));
        assert!(wt.get_int(1).is_nil());
        assert!(vm.heap.objects().all(|o| o.downcast::<Userdata>().is_none_or(|x| x.addr() != plain_addr)));
        // Still queued: survives further cycles without being queued twice.
        vm.gc_full();
        assert_eq!(vm.pending_finalizers.len(), 1);
        let (f, o) = vm.take_finalizer().unwrap();
        assert_eq!(f, Value::Func(fin));
        assert_eq!(o, Value::Udata(u));
        assert!(vm.take_finalizer().is_none());
        let n = vm.heap.object_count();
        vm.gc_full();
        assert!(vm.pending_finalizers.is_empty());
        assert_eq!(vm.heap.object_count(), n - 1);
    }

    #[test]
    fn cdata_finalizer_hook() {
        let mut vm = Vm::new();
        let fint = vm.empty_table();
        let fmt = vm.empty_table();
        let mode_key = Value::Str(vm.mm_name(MM::Mode));
        let kmode = s(&mut vm, "k");
        fmt.set(mode_key, kmode).unwrap();
        fint.set_metatable(Some(fmt));
        vm.ffi_fin = Some(fint);
        let fin = vm.new_native(NativeImpl::Rust(|_| Ok(0)), crate::func::FF_C, vec![], vm.globals);
        let cd = vm.new_cdata(crate::cdata::CTID_INT64, 8, 8);
        cd.set_has_finalizer(true);
        fint.set(Value::CData(cd), Value::Func(fin)).unwrap();
        let other = vm.new_cdata(crate::cdata::CTID_INT64, 8, 8);
        let other_addr = other.addr();
        vm.gc_full();
        assert_eq!(vm.pending_finalizers.len(), 1);
        assert!(vm.heap.objects().all(|o| o.downcast::<CData>().is_none_or(|x| x.addr() != other_addr)));
        let (f, o) = vm.take_finalizer().unwrap();
        assert_eq!((f, o), (Value::Func(fin), Value::CData(cd)));
        assert!(fint.get(Value::CData(cd)).is_nil());
        vm.gc_full();
        assert!(vm.pending_finalizers.is_empty());
        assert!(vm.heap.objects().all(|o| o.downcast::<CData>().is_none()));
    }

    #[test]
    fn pacing_and_step() {
        let mut vm = Vm::new();
        assert!(vm.gc_is_running());
        vm.gc_stop();
        assert!(!vm.gc_is_running());
        vm.gc_restart(0);
        assert!(vm.gc_is_running());
        assert_eq!(vm.gc_set_pause(100), 200);
        assert_eq!(vm.gc_set_stepmul(0), 200);
        // stepmul 0: one step completes a full cycle.
        assert!(vm.gc_step(0));
        vm.gc_set_stepmul(200);
        // Default stepmul: a step eventually finishes a cycle.
        let mut done = false;
        for _ in 0..10_000 {
            if vm.gc_step(0) {
                done = true;
                break;
            }
        }
        assert!(done);
        // Allocation drives gc_check.
        let before = vm.gc.cycles;
        for _ in 0..100_000 {
            vm.empty_table();
            vm.gc_check();
        }
        assert!(vm.gc.cycles > before);
        assert!(vm.heap.object_count() < 100_000);
    }

    #[test]
    fn upvalues_and_threads() {
        let mut vm = Vm::new();
        let th = vm.new_thread(vm.globals);
        let k = s(&mut vm, "co");
        vm.registry.set(k, Value::Thread(th)).unwrap();
        th.set(3, Value::Num(42.0));
        th.set_top(4);
        let u1 = vm.find_upval(th, 3);
        let u2 = vm.find_upval(th, 3);
        assert_eq!(u1, u2);
        let u0 = vm.find_upval(th, 1);
        assert_eq!(th.open_upval_count(), 2);
        assert_eq!(u1.get(), Value::Num(42.0));
        u1.set(Value::Num(7.0));
        assert_eq!(th.get(3), Value::Num(7.0));
        vm.gc_full();
        th.close_upvals(2);
        assert_eq!(th.open_upval_count(), 1);
        assert!(!u1.is_open() && u0.is_open());
        th.set(3, Value::Nil);
        assert_eq!(u1.get(), Value::Num(7.0));
        assert!(th.grow_stack(100).is_ok());
        assert!(th.max_stack() >= 100);
        assert_eq!(th.grow_stack(70000), Err(crate::thread::StackOverflow::Overflow));
        assert_eq!(th.grow_stack(70000), Err(crate::thread::StackOverflow::InErrorHandler));
        th.set_top(0);
        th.relimit_stack();
        assert_eq!(th.max_stack(), crate::thread::LJ_STACK_MAX);
    }
}
