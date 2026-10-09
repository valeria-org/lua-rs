//! Garbage-collected heap: `Gc<T>` handles, object headers, marking and sweeping.
//!
//! Port of the object-management parts of lj_gc.c, adapted to a non-incremental (stop-the-world)
//! mark & sweep collector. The collection driver (roots, weak tables, finalizers, pacing) lives in
//! `crate::state`; this module provides the mechanics.
//!
//! Debugging aid: with `LUAJIT_RS_GCSTRESS=1` in the environment every GC safe point runs a full
//! collection (see `crate::state::GcState::stress`), and debug builds keep freed objects in a
//! quarantine ([`Heap::enable_quarantine`]) so that using a handle that was not rooted across a
//! safe point panics with "use of a freed GC object" instead of reading reused memory.

use std::alloc::{self, Layout};
use std::cell::Cell;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::ops::Deref;
use std::ptr::NonNull;

use crate::table::Table;
use crate::value::Value;

// -- Header flags -------------------------------------------------------------------------------

/// Object reached during the current mark phase.
pub const GC_MARKED: u8 = 0x01;
/// Object is never collected (metamethod names, lexer keywords, ...).
pub const GC_FIXED: u8 = 0x02;
/// Userdata/cdata whose finalizer has been queued (or which was found to need none).
/// Upstream `LJ_GC_FINALIZED`.
pub const GC_FINALIZED: u8 = 0x04;
/// Cdata registered in the FFI finalizer table. Upstream `LJ_GC_CDATA_FIN`.
pub const GC_CDATA_FIN: u8 = 0x08;
/// Freed object kept in the heap's quarantine (GC stress mode in debug builds): its value has
/// been dropped and only the header is still valid. Dereferencing it panics.
pub const GC_DEAD: u8 = 0x80;

/// Type tag stored in every object header, used by the collector to find userdata/cdata during
/// finalizer separation and to skip leaf objects on the gray stack.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum GcKind {
    Str,
    UpVal,
    Thread,
    Proto,
    Func,
    CData,
    Table,
    Udata,
    /// Objects of types defined outside the core object model.
    Other,
}

impl GcKind {
    /// Leaf objects contain no references and are never pushed onto the gray stack.
    #[inline]
    pub fn is_leaf(self) -> bool {
        matches!(self, GcKind::Str | GcKind::CData)
    }

    /// Objects whose traversal upstream's incremental marker charges as GC work
    /// (`propagatemark`: tables, functions, prototypes, threads). Strings, userdata, cdata and
    /// upvalues are marked directly, at no cost.
    #[inline]
    pub fn charged_traversal(self) -> bool {
        matches!(self, GcKind::Table | GcKind::Func | GcKind::Proto | GcKind::Thread)
    }
}

/// Per-object header.
pub struct GcHeader {
    flags: Cell<u8>,
    kind: GcKind,
}

impl GcHeader {
    #[inline]
    pub fn kind(&self) -> GcKind {
        self.kind
    }
    #[inline]
    pub fn flags(&self) -> u8 {
        self.flags.get()
    }
    #[inline]
    pub fn set_flag(&self, f: u8) {
        self.flags.set(self.flags.get() | f);
    }
    #[inline]
    pub fn clear_flag(&self, f: u8) {
        self.flags.set(self.flags.get() & !f);
    }
    #[inline]
    pub fn has(&self, f: u8) -> bool {
        self.flags.get() & f != 0
    }
    #[inline]
    pub fn is_marked(&self) -> bool {
        self.has(GC_MARKED)
    }
    #[inline]
    pub fn is_fixed(&self) -> bool {
        self.has(GC_FIXED)
    }
    #[inline]
    pub fn is_finalized(&self) -> bool {
        self.has(GC_FINALIZED)
    }
}

/// Heap cell: header followed by the object.
#[repr(C)]
pub struct GcBox<T: ?Sized> {
    header: GcHeader,
    value: T,
}

impl<T: ?Sized> GcBox<T> {
    #[inline]
    pub fn header(&self) -> &GcHeader {
        &self.header
    }
    #[inline]
    pub fn value(&self) -> &T {
        &self.value
    }
}

// -- Trace --------------------------------------------------------------------------------------

/// Implemented by everything stored in the GC heap (and by Rust-side userdata extensions).
pub trait Trace {
    /// Mark every GC reference held by the object.
    fn trace(&self, m: &mut Marker);
    /// Current size in bytes, including owned out-of-line storage. Used for accounting.
    fn size(&self) -> usize;
}

/// A type that can be allocated in the heap.
pub trait GcObject: Trace + 'static {
    const KIND: GcKind;
}

// -- Gc<T> --------------------------------------------------------------------------------------

/// Thin, copyable handle to a heap object. Equality and hashing are by identity.
///
/// Validity follows the rooting rule of the design contract: a handle is valid as long as the
/// object is reachable from a GC root at every collection since the handle was obtained.
pub struct Gc<T> {
    ptr: NonNull<GcBox<T>>,
}

impl<T> Clone for Gc<T> {
    #[inline]
    fn clone(&self) -> Self {
        *self
    }
}
impl<T> Copy for Gc<T> {}

impl<T> PartialEq for Gc<T> {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        self.ptr == other.ptr
    }
}
impl<T> Eq for Gc<T> {}

impl<T> Hash for Gc<T> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.ptr.hash(state)
    }
}

impl<T> Deref for Gc<T> {
    type Target = T;
    #[inline]
    #[cfg_attr(debug_assertions, track_caller)]
    fn deref(&self) -> &T {
        #[cfg(debug_assertions)]
        self.assert_live();
        // SAFETY: by the rooting rule a live handle points to an allocated, initialized GcBox.
        unsafe { &(*self.ptr.as_ptr()).value }
    }
}

impl<T> fmt::Debug for Gc<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Gc({:?}@{:#x})", self.header().kind(), self.addr())
    }
}

impl<T> Gc<T> {
    /// Panic if the object was freed (only detectable while it sits in the stress-mode
    /// quarantine, see [`Heap::enable_quarantine`]).
    #[cfg(debug_assertions)]
    #[inline]
    #[track_caller]
    fn assert_live(&self) {
        if self.header().has(GC_DEAD) {
            use_after_free(self.header().kind());
        }
    }

    /// Object header (mark/fixed/finalizer flags).
    #[inline]
    pub fn header(&self) -> &GcHeader {
        // SAFETY: see `Deref`.
        unsafe { &(*self.ptr.as_ptr()).header }
    }

    /// Raw pointer to the heap cell, for the C API and identity printing.
    #[inline]
    pub fn as_ptr(self) -> *mut GcBox<T> {
        self.ptr.as_ptr()
    }

    /// Rebuild a handle from `as_ptr`.
    ///
    /// # Safety
    /// `p` must come from `Gc::<T>::as_ptr` of an object that is still alive.
    #[inline]
    pub unsafe fn from_ptr(p: *mut GcBox<T>) -> Gc<T> {
        // SAFETY: caller guarantees `p` is a live, non-null GcBox<T>.
        Gc { ptr: unsafe { NonNull::new_unchecked(p) } }
    }

    /// Address of the object, as printed by `tostring` and used for hashing.
    #[inline]
    pub fn addr(self) -> usize {
        self.ptr.as_ptr() as *const u8 as usize
    }

    /// Exclude the object from collection forever.
    #[inline]
    pub fn fix(self) {
        self.header().set_flag(GC_FIXED);
    }

    #[inline]
    pub fn ptr_eq(a: Gc<T>, b: Gc<T>) -> bool {
        a.ptr == b.ptr
    }
}

#[cfg(debug_assertions)]
#[cold]
#[inline(never)]
#[track_caller]
fn use_after_free(kind: GcKind) -> ! {
    panic!("use of a freed GC object ({kind:?})");
}

// -- Accounting ---------------------------------------------------------------------------------

thread_local! {
    static DEBT: Cell<isize> = const { Cell::new(0) };
}

/// Report a change of `delta` bytes in the size of an already-allocated object
/// (table resize, stack growth, ...). Folded into the heap total at the next GC check.
///
/// The counter is thread-local because objects mutate through `&self` without access to the
/// heap. Several `Vm`s on one OS thread would share it; that only affects GC pacing, never
/// correctness, because every sweep recomputes the exact live size.
#[inline]
pub fn account(delta: isize) {
    DEBT.with(|d| d.set(d.get() + delta));
}

/// Take and reset the pending size delta.
#[inline]
pub fn take_debt() -> isize {
    DEBT.with(|d| d.replace(0))
}

// -- Marker -------------------------------------------------------------------------------------

/// Weak-table mode bits (upstream `LJ_GC_WEAKKEY` / `LJ_GC_WEAKVAL`).
pub const WEAK_KEY: u8 = 0x08;
pub const WEAK_VAL: u8 = 0x10;
pub const WEAK_KV: u8 = WEAK_KEY | WEAK_VAL;

/// Mark-phase state: gray stack and the list of weak tables found while traversing.
pub struct Marker {
    gray: Vec<NonNull<GcBox<dyn Trace>>>,
    current: Option<NonNull<GcBox<dyn Trace>>>,
    weak: Vec<(Gc<Table>, u8)>,
    mode_name: Option<Gc<crate::lstr::LuaStr>>,
    ffi_fin: Option<Gc<Table>>,
}

impl Marker {
    /// `mode_name` is the interned `"__mode"` string; `ffi_fin` is the FFI finalizer table,
    /// whose keys are weak but which is never cleared (upstream `GCROOT_FFI_FIN` special case).
    pub fn new(mode_name: Option<Gc<crate::lstr::LuaStr>>, ffi_fin: Option<Gc<Table>>) -> Marker {
        Marker { gray: Vec::new(), current: None, weak: Vec::new(), mode_name, ffi_fin }
    }

    /// Mark an object, queueing it for traversal unless it is a leaf.
    #[inline]
    pub fn mark<T: GcObject>(&mut self, g: Gc<T>) {
        let h = g.header();
        if h.is_marked() {
            return;
        }
        #[cfg(debug_assertions)]
        if h.has(GC_DEAD) {
            use_after_free(h.kind());
        }
        h.set_flag(GC_MARKED);
        if !T::KIND.is_leaf() {
            let p: NonNull<GcBox<dyn Trace>> = g.ptr;
            self.gray.push(p);
        }
    }

    /// Mark the object referenced by a value, if any.
    #[inline]
    pub fn mark_value(&mut self, v: Value) {
        match v {
            Value::Str(s) => self.mark(s),
            Value::Table(t) => self.mark(t),
            Value::Func(f) => self.mark(f),
            Value::Thread(t) => self.mark(t),
            Value::Udata(u) => self.mark(u),
            Value::CData(c) => self.mark(c),
            Value::Proto(p) => self.mark(p),
            Value::Nil | Value::Bool(_) | Value::Num(_) | Value::LightUd(_) => {}
        }
    }

    /// Traverse gray objects until none are left.
    pub fn propagate(&mut self) {
        while let Some(p) = self.gray.pop() {
            self.current = Some(p);
            // SAFETY: only live heap objects are pushed; marking never frees.
            unsafe { (*p.as_ptr()).value.trace(self) };
        }
        self.current = None;
    }

    /// The interned `"__mode"` string, if the collector provided one.
    #[inline]
    pub fn mode_name(&self) -> Option<Gc<crate::lstr::LuaStr>> {
        self.mode_name
    }

    /// Called by `Table::trace` for a table with a valid `__mode`. Returns the effective weak
    /// mode to apply while traversing (upstream gc_traverse_tab).
    pub fn register_weak_current(&mut self, mode: u8) -> u8 {
        let cur = self.current.expect("register_weak_current outside traversal");
        // SAFETY: Table::trace is the only caller, so the current object is a GcBox<Table>.
        let t: Gc<Table> = Gc { ptr: cur.cast::<GcBox<Table>>() };
        if self.ffi_fin == Some(t) {
            // Keys are weak, values strong, and the table is not cleared.
            return !WEAK_VAL;
        }
        self.weak.push((t, mode));
        mode
    }

    /// Weak tables found during marking, with their modes.
    pub fn take_weak(&mut self) -> Vec<(Gc<Table>, u8)> {
        std::mem::take(&mut self.weak)
    }
}

// -- Heap ---------------------------------------------------------------------------------------

/// Reference to an object in the heap's object list.
#[derive(Clone, Copy)]
pub struct ObjRef(NonNull<GcBox<dyn Trace>>);

impl ObjRef {
    #[inline]
    pub fn header(&self) -> &GcHeader {
        // SAFETY: ObjRefs are only handed out for objects in the heap list, which stay live until
        // the next sweep; the borrow is tied to this ObjRef, which callers drop before sweeping.
        unsafe { &(*self.0.as_ptr()).header }
    }
    #[inline]
    pub fn kind(self) -> GcKind {
        self.header().kind()
    }
    /// Typed handle if the object is a `T` (by kind tag; never succeeds for `GcKind::Other`).
    pub fn downcast<T: GcObject>(self) -> Option<Gc<T>> {
        if T::KIND != GcKind::Other && self.kind() == T::KIND {
            Some(Gc { ptr: self.0.cast::<GcBox<T>>() })
        } else {
            None
        }
    }
}

/// Cells of at most this many bytes are recycled through the heap's free lists.
const POOL_MAX: usize = 256;
/// Size-class granularity and alignment of pooled cells.
const POOL_GRAIN: usize = 16;
const POOL_CLASSES: usize = POOL_MAX / POOL_GRAIN;

/// Size class of a cell layout, if it is pooled.
#[inline]
fn pool_class(l: Layout) -> Option<usize> {
    (l.size() <= POOL_MAX && l.align() <= POOL_GRAIN).then(|| l.size().max(1).div_ceil(POOL_GRAIN) - 1)
}

#[inline]
fn class_layout(c: usize) -> Layout {
    // SAFETY: non-zero multiple of the power-of-two alignment, far below `isize::MAX`.
    unsafe { Layout::from_size_align_unchecked((c + 1) * POOL_GRAIN, POOL_GRAIN) }
}

/// Free cells of the pooled size classes, linked through their first word. Allocation-heavy
/// code frees and reallocates cells of the same few sizes at a high rate, which the system
/// allocator handles poorly in bulk (sweeps free thousands of cells at once).
struct Pool {
    head: [*mut u8; POOL_CLASSES],
    count: [usize; POOL_CLASSES],
}

impl Pool {
    #[inline]
    fn take(&mut self, c: usize) -> *mut u8 {
        let p = self.head[c];
        if p.is_null() {
            // SAFETY: the class layout has a non-zero size.
            let p = unsafe { alloc::alloc(class_layout(c)) };
            if p.is_null() {
                alloc::handle_alloc_error(class_layout(c));
            }
            return p;
        }
        // SAFETY: free cells store the next free cell of their class in their first word.
        self.head[c] = unsafe { *(p as *mut *mut u8) };
        self.count[c] -= 1;
        p
    }

    #[inline]
    fn give(&mut self, c: usize, p: *mut u8) {
        // SAFETY: `p` is a dead cell of class `c` (at least one aligned word).
        unsafe { *(p as *mut *mut u8) = self.head[c] };
        self.head[c] = p;
        self.count[c] += 1;
    }

    /// Return free cells to the system allocator until at most `keep` remain in total.
    fn trim(&mut self, keep: usize) {
        let mut total: usize = self.count.iter().sum();
        for c in (0..POOL_CLASSES).rev() {
            while total > keep && self.count[c] > 0 {
                let p = self.take(c);
                // SAFETY: allocated with the class layout by `take`.
                unsafe { alloc::dealloc(p, class_layout(c)) };
                total -= 1;
            }
        }
    }
}

/// List of all heap objects plus byte accounting.
pub struct Heap {
    objects: Vec<NonNull<GcBox<dyn Trace>>>,
    total: usize,
    quarantine: Option<Quarantine>,
    pool: Pool,
}

impl Default for Heap {
    fn default() -> Self {
        Heap::new()
    }
}

impl Heap {
    pub fn new() -> Heap {
        Heap {
            objects: Vec::new(),
            total: 0,
            pool: Pool { head: [std::ptr::null_mut(); POOL_CLASSES], count: [0; POOL_CLASSES] },
            quarantine: None,
        }
    }

    /// Debugging aid used by the GC stress mode: from now on, swept objects are dropped but
    /// their memory is kept (header flagged [`GC_DEAD`]) for a while, so that a dangling `Gc`
    /// is not silently backed by a reused cell. In debug builds, dereferencing or marking
    /// a dead object panics. Release builds have no checks, so this is a no-op there.
    pub fn enable_quarantine(&mut self) {
        if cfg!(debug_assertions) {
            self.quarantine.get_or_insert_with(Quarantine::default);
        }
    }

    /// Allocate and register an object. Never collects.
    pub fn alloc<T: GcObject>(&mut self, value: T) -> Gc<T> {
        let size = value.size();
        let layout = Layout::new::<GcBox<T>>();
        let raw = match pool_class(layout) {
            Some(c) => self.pool.take(c),
            None => {
                // SAFETY: `GcBox` contains a header, so the layout has a non-zero size.
                let p = unsafe { alloc::alloc(layout) };
                if p.is_null() {
                    alloc::handle_alloc_error(layout);
                }
                p
            }
        } as *mut GcBox<T>;
        // SAFETY: `raw` is a fresh, suitably sized and aligned allocation. Ownership moves to
        // the object list and is released exactly once by `sweep` or `Drop for Heap`.
        unsafe { raw.write(GcBox { header: GcHeader { flags: Cell::new(0), kind: T::KIND }, value }) };
        // SAFETY: allocation failure was handled above.
        let ptr = unsafe { NonNull::new_unchecked(raw) };
        let dynp: NonNull<GcBox<dyn Trace>> = ptr;
        self.objects.push(dynp);
        self.total += size;
        Gc { ptr }
    }

    /// Drop the object in cell `p` and release the cell (or, with the quarantine enabled, keep
    /// it as a dead cell for a while).
    ///
    /// # Safety
    /// `p` must come from `alloc` and not be used afterwards.
    unsafe fn free(pool: &mut Pool, quarantine: &mut Option<Quarantine>, p: NonNull<GcBox<dyn Trace>>) {
        // SAFETY: `p` is a live, initialized cell (caller contract).
        let layout = unsafe { Layout::for_value(p.as_ref()) };
        match quarantine {
            None => {
                // SAFETY: as above; the object is dropped exactly once.
                unsafe { std::ptr::drop_in_place(p.as_ptr()) };
                release_cell(pool, p.cast(), layout);
            }
            Some(q) => {
                // SAFETY: as above. Only the value is dropped; the header stays valid and flags
                // the cell dead, which `Gc::deref` and `Marker::mark` check before any access.
                unsafe {
                    (*p.as_ptr()).header.flags.set(GC_DEAD);
                    std::ptr::drop_in_place(&raw mut (*p.as_ptr()).value);
                }
                if let Some((old, l)) = q.hold(p.cast(), layout) {
                    release_cell(pool, old, l);
                }
            }
        }
    }

    /// Bytes in use, including pending size deltas reported through `account`.
    pub fn total(&mut self) -> usize {
        let d = take_debt();
        self.total = (self.total as isize + d).max(0) as usize;
        self.total
    }

    /// Number of live objects.
    pub fn object_count(&self) -> usize {
        self.objects.len()
    }

    /// Objects in allocation order (oldest first).
    pub fn objects(&self) -> impl DoubleEndedIterator<Item = ObjRef> + '_ {
        self.objects.iter().map(|&p| ObjRef(p))
    }

    /// Free every unmarked, unfixed object; clear marks of survivors and recompute the live size.
    pub fn sweep(&mut self) -> Swept {
        let mut swept = Swept { live: 0, traversal: 0 };
        let before = self.objects.len();
        let (pool, quarantine) = (&mut self.pool, &mut self.quarantine);
        self.objects.retain(|&p| {
            // SAFETY: every pointer in the list is a live cell created by `alloc`.
            let b = unsafe { &*p.as_ptr() };
            let flags = b.header.flags.get();
            if flags & (GC_MARKED | GC_FIXED) != 0 {
                b.header.flags.set(flags & !GC_MARKED);
                let size = b.value.size();
                swept.live += size;
                if b.header.kind.charged_traversal() {
                    swept.traversal += size;
                }
                true
            } else {
                // SAFETY: unreachable object, removed from the list.
                unsafe { Heap::free(pool, quarantine, p) };
                false
            }
        });
        // The program will likely allocate about as many cells until the next collection as
        // this one freed: keep that many (or as many as are live), release the rest.
        let freed = before - self.objects.len();
        self.pool.trim(freed.max(self.objects.len()));
        take_debt();
        self.total = swept.live;
        swept
    }
}

/// Return a dead cell of `layout` (allocated by `Heap::alloc`) to its pool or the allocator.
fn release_cell(pool: &mut Pool, p: NonNull<u8>, layout: Layout) {
    match pool_class(layout) {
        Some(c) => pool.give(c, p.as_ptr()),
        // SAFETY: allocated by `Heap::alloc` with this exact layout.
        None => unsafe { alloc::dealloc(p.as_ptr(), layout) },
    }
}

/// Outcome of [`Heap::sweep`].
#[derive(Clone, Copy, Debug)]
pub struct Swept {
    /// Bytes of all survivors (the new heap total).
    pub live: usize,
    /// Bytes of the surviving objects that upstream charges for traversal
    /// ([`GcKind::charged_traversal`]): the mark work of a cycle over the same heap.
    pub traversal: usize,
}

/// Dead cells whose release is deferred (see [`Heap::enable_quarantine`]).
#[derive(Default)]
struct Quarantine {
    cells: std::collections::VecDeque<(NonNull<u8>, Layout)>,
}

impl Quarantine {
    /// Cells kept before the oldest is released (bounds the memory held by dead cells).
    const MAX_CELLS: usize = 1 << 18;

    /// Keep a dead cell; returns the oldest one once the quarantine is full.
    fn hold(&mut self, p: NonNull<u8>, layout: Layout) -> Option<(NonNull<u8>, Layout)> {
        self.cells.push_back((p, layout));
        if self.cells.len() > Self::MAX_CELLS { self.cells.pop_front() } else { None }
    }
}

impl Drop for Heap {
    fn drop(&mut self) {
        let mut quarantine = self.quarantine.take();
        for &p in &self.objects {
            // SAFETY: each pointer was created by `alloc` and is freed once.
            unsafe { Heap::free(&mut self.pool, &mut None, p) };
        }
        self.objects.clear();
        if let Some(q) = &mut quarantine {
            for (p, l) in q.cells.drain(..) {
                release_cell(&mut self.pool, p, l);
            }
        }
        self.pool.trim(0);
        take_debt();
    }
}

// -- Aligned byte storage -----------------------------------------------------------------------

/// Owned, zero-initialized byte buffer with a guaranteed alignment (≥ 16).
/// Used for raw userdata payloads and cdata storage.
pub struct AlignedBytes {
    ptr: NonNull<u8>,
    len: usize,
    align: usize,
}

impl AlignedBytes {
    /// Allocate `len` zeroed bytes aligned to `max(align, 16)` (`align` must be a power of two).
    pub fn new(len: usize, align: usize) -> AlignedBytes {
        let align = align.max(16);
        let layout = Layout::from_size_align(len.max(1), align).expect("invalid layout");
        // SAFETY: layout has non-zero size.
        let p = unsafe { alloc::alloc_zeroed(layout) };
        let ptr = NonNull::new(p).unwrap_or_else(|| alloc::handle_alloc_error(layout));
        AlignedBytes { ptr, len, align }
    }
    #[inline]
    pub fn as_ptr(&self) -> *mut u8 {
        self.ptr.as_ptr()
    }
    #[inline]
    pub fn len(&self) -> usize {
        self.len
    }
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    #[inline]
    pub fn align(&self) -> usize {
        self.align
    }
}

impl Drop for AlignedBytes {
    fn drop(&mut self) {
        let layout = Layout::from_size_align(self.len.max(1), self.align).expect("invalid layout");
        // SAFETY: allocated in `new` with the same layout.
        unsafe { alloc::dealloc(self.ptr.as_ptr(), layout) };
    }
}

#[cfg(all(test, debug_assertions))]
mod tests {
    use super::*;
    use crate::lstr::StrTab;

    /// A swept string, interned and dropped again, with its heap and table.
    fn swept_string(heap: &mut Heap) -> Gc<crate::lstr::LuaStr> {
        let mut st = StrTab::new(0);
        let s = st.intern(heap, b"gone");
        st.purge();
        heap.sweep();
        assert_eq!(heap.object_count(), 0);
        s
    }

    #[test]
    #[should_panic(expected = "use of a freed GC object (Str)")]
    fn quarantine_turns_use_after_free_into_panic() {
        let mut heap = Heap::new();
        heap.enable_quarantine();
        let s = swept_string(&mut heap);
        let _ = s.len();
    }

    #[test]
    #[should_panic(expected = "use of a freed GC object (Str)")]
    fn quarantine_catches_marking_freed_objects() {
        let mut heap = Heap::new();
        heap.enable_quarantine();
        let s = swept_string(&mut heap);
        Marker::new(None, None).mark(s);
    }
}
