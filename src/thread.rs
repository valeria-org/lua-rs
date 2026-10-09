//! Coroutine threads (`lua_State`): value stack, frames, open upvalues.
//! Stack sizing ported from lj_state.c.

use std::cell::{Cell, UnsafeCell};
use std::fmt;

use crate::func::UpVal;
use crate::gc::{self, Gc, GcBox, GcKind, GcObject, Marker, Trace};
use crate::table::Table;
use crate::value::Value;

/// Min. stack size (`LUA_MINSTACK`).
pub const LJ_STACK_MIN: usize = 20;
/// Max. stack size (`LUAI_MAXSTACK`).
pub const LJ_STACK_MAX: usize = 65500;
/// Starting stack size.
pub const LJ_STACK_START: usize = 2 * LJ_STACK_MIN;
/// Extra stack space for metamethod calls (`5 + 3*LJ_FR2`).
pub const LJ_STACK_EXTRA: usize = 5 + 3;
/// Max. size including the overflow reserve.
pub const LJ_STACK_MAXEX: usize = LJ_STACK_MAX + 1 + LJ_STACK_EXTRA;
/// Extra space granted to error handlers after a stack overflow.
pub const LJ_STACK_ERREX: usize = 1 + 2 * LJ_STACK_MIN;

/// Coroutine status.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ThreadStatus {
    /// Created, not yet resumed.
    Fresh,
    Running,
    Suspended,
    /// Resumed another coroutine.
    Normal,
    Dead,
}

/// Kind of a call frame.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FrameKind {
    Lua,
    Native,
    /// Protected call boundary (pcall/xpcall).
    Pcall,
    /// Continuation of a metamethod or other internal call.
    Cont,
}

/// A call frame record. Initial layout; the VM owns its evolution.
#[derive(Clone, Copy, Debug)]
pub struct Frame {
    pub kind: FrameKind,
    /// The called function (kept alive by the frame).
    pub func: Value,
    /// First argument / local slot.
    pub base: u32,
    /// Saved bytecode position (Lua frames).
    pub pc: u32,
    /// Expected number of results (-1 = multiple).
    pub nresults: i32,
}

impl Frame {
    pub fn new(kind: FrameKind, func: Value, base: u32) -> Frame {
        Frame { kind, func, base, pc: 0, nresults: -1 }
    }
}

/// Outcome of a failed stack growth request (`lj_state_growstack`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StackOverflow {
    /// Raise "stack overflow" (the stack was enlarged by `LJ_STACK_ERREX` for the handler).
    Overflow,
    /// Overflow while already using the error reserve: raise LUA_ERRERR without a handler.
    InErrorHandler,
}

/// A thread object.
pub struct Thread {
    /// Slots; `len()` is upstream's `stacksize` (incl. `LJ_STACK_EXTRA + 1`).
    stack: UnsafeCell<Vec<Value>>,
    top: Cell<usize>,
    frames: UnsafeCell<Vec<Frame>>,
    status: Cell<ThreadStatus>,
    /// Open upvalues sorted by slot (ascending).
    openupval: UnsafeCell<Vec<Gc<UpVal>>>,
    env: Cell<Gc<Table>>,
}

impl fmt::Debug for Thread {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Thread(status={:?}, top={}, stacksize={})", self.status.get(), self.top.get(), self.stack_size())
    }
}

impl Thread {
    /// New thread with the upstream starting stack (`stack_init`) and the given environment.
    pub fn new(env: Gc<Table>) -> Thread {
        Thread {
            stack: UnsafeCell::new(vec![Value::Nil; LJ_STACK_START + LJ_STACK_EXTRA]),
            top: Cell::new(0),
            frames: UnsafeCell::new(Vec::new()),
            status: Cell::new(ThreadStatus::Fresh),
            openupval: UnsafeCell::new(Vec::new()),
            env: Cell::new(env),
        }
    }

    #[inline]
    fn stack_ref(&self) -> &Vec<Value> {
        // SAFETY: no `&mut` to the stack outlives a single method call (see `stack_mut`).
        unsafe { &*self.stack.get() }
    }

    /// Exclusive access to the stack vector (interpreter fast paths).
    ///
    /// # Safety
    /// The caller must not hold any other reference obtained from `stack_mut`/`stack_slice`, and
    /// must not call other methods of this thread while the reference is alive.
    #[inline]
    #[allow(clippy::mut_from_ref)]
    pub unsafe fn stack_mut(&self) -> &mut Vec<Value> {
        // SAFETY: exclusivity is the caller's obligation.
        unsafe { &mut *self.stack.get() }
    }

    /// Exclusive access to the frame stack.
    ///
    /// # Safety
    /// Same contract as `stack_mut`, for the frame vector.
    #[inline]
    #[allow(clippy::mut_from_ref)]
    pub unsafe fn frames_mut(&self) -> &mut Vec<Frame> {
        // SAFETY: exclusivity is the caller's obligation.
        unsafe { &mut *self.frames.get() }
    }

    /// Slot value (nil beyond the allocated stack).
    #[inline]
    pub fn get(&self, i: usize) -> Value {
        self.stack_ref().get(i).copied().unwrap_or(Value::Nil)
    }
    /// Store into slot `i < stack_size()`.
    #[inline]
    pub fn set(&self, i: usize, v: Value) {
        // SAFETY: short-lived exclusive access, no re-entrancy.
        unsafe { (&mut *self.stack.get())[i] = v };
    }
    #[inline]
    pub fn top(&self) -> usize {
        self.top.get()
    }
    #[inline]
    pub fn set_top(&self, t: usize) {
        self.top.set(t);
    }
    /// Push at `top` (caller guarantees space, as with upstream's `incr_top` after a check).
    #[inline]
    pub fn push(&self, v: Value) {
        let t = self.top.get();
        self.set(t, v);
        self.top.set(t + 1);
    }
    /// Allocated slots (`stacksize`).
    #[inline]
    pub fn stack_size(&self) -> usize {
        self.stack_ref().len()
    }
    /// Usable limit (`maxstack`): slots above it are the metamethod reserve.
    #[inline]
    pub fn max_stack(&self) -> usize {
        self.stack_size() - LJ_STACK_EXTRA - 1
    }

    /// `resizestack`: make the usable size `n`.
    fn resize_stack(&self, n: usize) {
        let realsize = n + 1 + LJ_STACK_EXTRA;
        // SAFETY: short-lived exclusive access, no re-entrancy.
        let st = unsafe { &mut *self.stack.get() };
        let old = st.capacity();
        st.resize(realsize, Value::Nil);
        st.shrink_to_fit();
        gc::account((st.capacity() as isize - old as isize) * std::mem::size_of::<Value>() as isize);
    }

    /// Ensure `need` free slots above `top` (`lj_state_checkstack`).
    pub fn check_stack(&self, need: usize) -> Result<(), StackOverflow> {
        if self.max_stack().saturating_sub(self.top.get()) < need {
            self.grow_stack(need)
        } else {
            Ok(())
        }
    }

    /// `lj_state_growstack`. On overflow the stack is still enlarged so an error handler can run.
    pub fn grow_stack(&self, need: usize) -> Result<(), StackOverflow> {
        let size = self.stack_size();
        let mut n = size + need;
        if n < LJ_STACK_MAX {
            if n < 2 * size {
                n = (2 * size).min(LJ_STACK_MAX);
            }
            self.resize_stack(n);
            Ok(())
        } else if size <= LJ_STACK_MAXEX {
            self.resize_stack(LJ_STACK_MAX + LJ_STACK_ERREX);
            Err(StackOverflow::Overflow)
        } else {
            Err(StackOverflow::InErrorHandler)
        }
    }

    /// `lj_state_relimitstack`: undo the overflow reserve after unwinding.
    pub fn relimit_stack(&self) {
        if self.stack_size() > LJ_STACK_MAXEX && self.top.get() < LJ_STACK_MAX - 1 - LJ_STACK_ERREX {
            self.resize_stack(LJ_STACK_MAX);
        }
    }

    /// `lj_state_shrinkstack`: shrink when less than a quarter is used.
    pub fn shrink_stack(&self, used: usize) {
        let size = self.stack_size();
        if size > LJ_STACK_MAXEX {
            return;
        }
        if 4 * used < size && 2 * (LJ_STACK_START + LJ_STACK_EXTRA) < size {
            self.resize_stack(size >> 1);
        }
    }

    // -- Frames --

    #[inline]
    pub fn frame_count(&self) -> usize {
        // SAFETY: short-lived shared access.
        unsafe { (*self.frames.get()).len() }
    }
    #[inline]
    pub fn frame(&self, i: usize) -> Option<Frame> {
        // SAFETY: short-lived shared access.
        unsafe { (&*self.frames.get()).get(i).copied() }
    }
    #[inline]
    pub fn current_frame(&self) -> Option<Frame> {
        // SAFETY: short-lived shared access.
        unsafe { (*self.frames.get()).last().copied() }
    }
    pub fn push_frame(&self, f: Frame) {
        // SAFETY: short-lived exclusive access, no re-entrancy.
        let fr = unsafe { &mut *self.frames.get() };
        let old = fr.capacity();
        fr.push(f);
        if fr.capacity() != old {
            gc::account(((fr.capacity() - old) * std::mem::size_of::<Frame>()) as isize);
        }
    }
    pub fn pop_frame(&self) -> Option<Frame> {
        // SAFETY: short-lived exclusive access, no re-entrancy.
        unsafe { (*self.frames.get()).pop() }
    }
    pub fn set_frame(&self, i: usize, f: Frame) {
        // SAFETY: short-lived exclusive access, no re-entrancy.
        unsafe { (&mut *self.frames.get())[i] = f };
    }

    // -- Status / env --

    #[inline]
    pub fn status(&self) -> ThreadStatus {
        self.status.get()
    }
    #[inline]
    pub fn set_status(&self, s: ThreadStatus) {
        self.status.set(s);
    }
    #[inline]
    pub fn env(&self) -> Gc<Table> {
        self.env.get()
    }
    #[inline]
    pub fn set_env(&self, t: Gc<Table>) {
        self.env.set(t);
    }

    // -- Open upvalues (lj_func_finduv / lj_func_closeuv) --

    /// Find or create the open upvalue for `slot`. `alloc` creates a new upvalue object.
    pub fn find_upval(&self, slot: u32, alloc: impl FnOnce() -> Gc<UpVal>) -> Gc<UpVal> {
        // SAFETY: short-lived exclusive access; `alloc` does not touch this thread's upvalues.
        let list = unsafe { &mut *self.openupval.get() };
        match list.binary_search_by_key(&slot, |u| u.slot().unwrap_or(u32::MAX)) {
            Ok(i) => list[i],
            Err(i) => {
                let uv = alloc();
                list.insert(i, uv);
                uv
            }
        }
    }

    /// Close all open upvalues at or above `level`.
    pub fn close_upvals(&self, level: u32) {
        // SAFETY: short-lived exclusive access; `close` only reads this thread's stack.
        let list = unsafe { &mut *self.openupval.get() };
        let i = list.partition_point(|u| u.slot().is_some_and(|s| s < level));
        for uv in list.drain(i..) {
            uv.close();
        }
    }

    /// Number of open upvalues.
    pub fn open_upval_count(&self) -> usize {
        // SAFETY: short-lived shared access.
        unsafe { (*self.openupval.get()).len() }
    }
}

impl Trace for Thread {
    /// `gc_traverse_thread`: mark the stack below top, clear the unused part (the collector is
    /// always in its atomic phase), and mark frames, open upvalues and the environment.
    fn trace(&self, m: &mut Marker) {
        let top = self.top.get();
        // SAFETY: tracing runs with no other borrow of the stack alive (GC safe point).
        let st = unsafe { &mut *self.stack.get() };
        let top = top.min(st.len());
        for &v in &st[..top] {
            m.mark_value(v);
        }
        for v in &mut st[top..] {
            *v = Value::Nil;
        }
        // SAFETY: as above.
        let frames = unsafe { &*self.frames.get() };
        for f in frames {
            m.mark_value(f.func);
        }
        // SAFETY: as above.
        let uvs = unsafe { &*self.openupval.get() };
        for &u in uvs {
            m.mark(u);
        }
        m.mark(self.env.get());
    }

    fn size(&self) -> usize {
        // SAFETY: short-lived shared access.
        let (s, f, u) =
            unsafe { ((*self.stack.get()).capacity(), (*self.frames.get()).capacity(), (*self.openupval.get()).capacity()) };
        std::mem::size_of::<GcBox<Thread>>()
            + s * std::mem::size_of::<Value>()
            + f * std::mem::size_of::<Frame>()
            + u * std::mem::size_of::<Gc<UpVal>>()
    }
}

impl GcObject for Thread {
    const KIND: GcKind = GcKind::Thread;
}
