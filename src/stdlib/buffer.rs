//! String buffer library (`require "string.buffer"`). Port of lib_buffer.c and of the
//! extended string buffer (`SBufExt`) parts of lj_buf.c/lj_buf.h. Serialization lives in
//! [`super::serialize`].
//!
//! A buffer is a `UDTYPE_BUFFER` userdata whose Rust payload is an [`SBufExt`]. Its memory is
//! a raw allocation with stable addresses, because `buf:ref()`/`buf:reserve()` hand pointers
//! to the FFI; growth follows upstream exactly (`buf_grow`, `lj_buf_more2` compaction), so the
//! sizes reported by `reserve` match.

use std::alloc::{self, Layout};
use std::cell::Cell;
use std::ptr;

use crate::cdata::CTID_P_UINT8;
use crate::error::LuaResult;
use crate::func::NativeFn;
use crate::gc::{Gc, Marker, Trace};
use crate::state::Vm;
use crate::table::Table;
use crate::udata::{UDTYPE_BUFFER, Userdata};
use crate::value::Value;
use crate::vm::ErrMsg;

use super::libinit;
use super::serialize;
use super::string::err_mem;

/// `LJ_MAX_BUF`.
pub const LJ_MAX_BUF: u32 = 0x7fff_ff00;
/// `LJ_MIN_SBUF`.
const LJ_MIN_SBUF: u32 = 32;
/// Alignment of buffer memory (malloc alignment).
const SBUF_ALIGN: usize = 16;

/// Memory allocation failure or size overflow (upstream `lj_err_mem`).
#[derive(Debug)]
pub struct MemError;

/// `SBufExt`: extended string buffer. All state is in `Cell`s (the payload is shared).
pub struct SBufExt {
    /// Base pointer.
    b: Cell<*mut u8>,
    /// End pointer.
    e: Cell<*mut u8>,
    /// Write pointer.
    w: Cell<*mut u8>,
    /// Read pointer.
    r: Cell<*mut u8>,
    /// `SBUF_FLAG_COW`: the memory belongs to `cowref`.
    cow: Cell<bool>,
    /// Copy-on-write object reference (string or cdata).
    cowref: Cell<Value>,
    /// Serialization string dictionary.
    pub dict_str: Cell<Option<Gc<Table>>>,
    /// Serialization metatable dictionary.
    pub dict_mt: Cell<Option<Gc<Table>>>,
    /// Remaining serialization recursion depth.
    pub depth: Cell<i32>,
}

impl Trace for SBufExt {
    fn trace(&self, m: &mut Marker) {
        m.mark_value(self.cowref.get());
        if let Some(t) = self.dict_str.get() {
            m.mark(t);
        }
        if let Some(t) = self.dict_mt.get() {
            m.mark(t);
        }
    }
    fn size(&self) -> usize {
        std::mem::size_of::<SBufExt>() + if self.cow.get() { 0 } else { self.sz() as usize }
    }
}

impl Drop for SBufExt {
    fn drop(&mut self) {
        self.release();
    }
}

impl Default for SBufExt {
    fn default() -> Self {
        Self::new()
    }
}

impl SBufExt {
    /// `lj_bufx_init`.
    pub fn new() -> SBufExt {
        SBufExt {
            b: Cell::new(ptr::null_mut()),
            e: Cell::new(ptr::null_mut()),
            w: Cell::new(ptr::null_mut()),
            r: Cell::new(ptr::null_mut()),
            cow: Cell::new(false),
            cowref: Cell::new(Value::Nil),
            dict_str: Cell::new(None),
            dict_mt: Cell::new(None),
            depth: Cell::new(0),
        }
    }

    /// `lj_bufx_set_cow` (without the reference): read-only view of `len` bytes at `p`.
    pub fn new_cow(p: *const u8, len: usize) -> SBufExt {
        let s = SBufExt::new();
        s.set_cow(p, len);
        s
    }

    fn set_cow(&self, p: *const u8, len: usize) {
        let p = p as *mut u8;
        self.cow.set(true);
        self.b.set(p);
        self.r.set(p);
        // SAFETY: `p..p+len` is the referenced object's memory.
        let e = unsafe { p.add(len) };
        self.w.set(e);
        self.e.set(e);
    }

    #[inline]
    fn diff(a: *mut u8, b: *mut u8) -> u32 {
        (a as usize).wrapping_sub(b as usize) as u32
    }
    /// `sbufsz`.
    #[inline]
    pub fn sz(&self) -> u32 {
        Self::diff(self.e.get(), self.b.get())
    }
    /// `sbuflen`.
    #[inline]
    fn blen(&self) -> u32 {
        Self::diff(self.w.get(), self.b.get())
    }
    /// `sbufleft`.
    #[inline]
    pub fn left(&self) -> u32 {
        Self::diff(self.e.get(), self.w.get())
    }
    /// `sbufxlen`: number of unread bytes.
    #[inline]
    pub fn len(&self) -> u32 {
        Self::diff(self.w.get(), self.r.get())
    }
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// `sbufxslack`.
    #[inline]
    fn slack(&self) -> u32 {
        Self::diff(self.r.get(), self.b.get())
    }
    #[inline]
    pub fn is_cow(&self) -> bool {
        self.cow.get()
    }
    /// Read pointer.
    #[inline]
    pub fn rptr(&self) -> *mut u8 {
        self.r.get()
    }
    /// Write pointer.
    #[inline]
    pub fn wptr(&self) -> *mut u8 {
        self.w.get()
    }
    #[inline]
    pub fn set_r(&self, p: *mut u8) {
        self.r.set(p);
    }
    #[inline]
    pub fn set_w(&self, p: *mut u8) {
        self.w.set(p);
    }

    /// The unread contents.
    pub fn contents(&self) -> &[u8] {
        let n = self.len() as usize;
        if n == 0 {
            return &[];
        }
        // SAFETY: `r..w` is initialized buffer (or referenced object) memory.
        unsafe { std::slice::from_raw_parts(self.r.get(), n) }
    }

    /// Free owned memory (not the referenced memory of a COW buffer).
    fn release(&self) {
        let b = self.b.get();
        if !self.cow.get() && !b.is_null() {
            // SAFETY: `b` was allocated by `buf_grow` with this layout.
            unsafe { alloc::dealloc(b, Layout::from_size_align_unchecked(self.sz() as usize, SBUF_ALIGN)) };
        }
    }

    /// `buf_grow`.
    fn grow(&self, sz: u32) -> Result<(), MemError> {
        let osz = self.sz();
        let len = self.blen();
        let mut nsz = osz.max(LJ_MIN_SBUF);
        while nsz < sz {
            nsz = nsz.checked_add(nsz).ok_or(MemError)?;
        }
        let ob = self.b.get();
        let layout = Layout::from_size_align(nsz as usize, SBUF_ALIGN).map_err(|_| MemError)?;
        let b = if self.cow.get() {
            // Copy-on-write semantics.
            // SAFETY: fresh allocation of a non-zero size; the source holds `osz` bytes.
            let b = unsafe { alloc::alloc(layout) };
            if b.is_null() {
                return Err(MemError);
            }
            if osz > 0 {
                unsafe { ptr::copy_nonoverlapping(ob, b, osz as usize) };
            }
            self.cow.set(false);
            self.cowref.set(Value::Nil);
            b
        } else if ob.is_null() {
            // SAFETY: non-zero size.
            unsafe { alloc::alloc(layout) }
        } else {
            // SAFETY: `ob` was allocated with size `osz` and this alignment.
            unsafe { alloc::realloc(ob, Layout::from_size_align_unchecked(osz as usize, SBUF_ALIGN), nsz as usize) }
        };
        if b.is_null() {
            return Err(MemError);
        }
        let roff = Self::diff(self.r.get(), ob) as usize;
        // SAFETY: offsets are within the new allocation.
        unsafe {
            self.r.set(b.add(roff));
            self.w.set(b.add(len as usize));
            self.e.set(b.add(nsz as usize));
        }
        self.b.set(b);
        Ok(())
    }

    /// `lj_buf_need2`: total size at least `sz`.
    pub fn need2(&self, sz: u32) -> Result<(), MemError> {
        if sz > LJ_MAX_BUF {
            return Err(MemError);
        }
        self.grow(sz)
    }

    /// `lj_buf_more2` (extended buffer variant).
    fn more2(&self, sz: u32) -> Result<(), MemError> {
        let len = self.len();
        if sz > LJ_MAX_BUF || len as u64 + sz as u64 > LJ_MAX_BUF as u64 {
            return Err(MemError);
        }
        if len + sz > self.sz() {
            // Must grow.
            self.grow(len + sz)?;
        } else if self.cow.get() || self.slack() < (self.sz() >> 3) {
            // Also grow to avoid excessive compactions, if slack < size/8.
            return self.grow(self.blen() + sz); // Not sbufxlen!
        }
        let b = self.b.get();
        if self.r.get() != b {
            // Compact by moving down.
            // SAFETY: `r..w` lies within the allocation starting at `b`.
            unsafe {
                ptr::copy(self.r.get(), b, len as usize);
                self.r.set(b);
                self.w.set(b.add(len as usize));
            }
        }
        Ok(())
    }

    /// `lj_buf_more`: at least `sz` free bytes after the write pointer.
    pub fn more(&self, sz: u32) -> Result<(), MemError> {
        if sz > self.left() { self.more2(sz) } else { Ok(()) }
    }

    /// Append bytes after a successful [`SBufExt::more`] (no capacity check).
    pub fn wmem(&self, s: &[u8]) {
        debug_assert!(s.len() as u32 <= self.left());
        if !s.is_empty() {
            // SAFETY: the caller reserved `s.len()` bytes after `w`.
            unsafe {
                ptr::copy_nonoverlapping(s.as_ptr(), self.w.get(), s.len());
                self.w.set(self.w.get().add(s.len()));
            }
        }
    }

    /// `lj_buf_putmem`.
    pub fn putmem(&self, s: &[u8]) -> Result<(), MemError> {
        if s.len() as u64 > LJ_MAX_BUF as u64 {
            return Err(MemError);
        }
        self.more(s.len() as u32)?;
        self.wmem(s);
        Ok(())
    }

    /// `lj_bufx_reset`.
    pub fn reset(&self) {
        if self.cow.get() {
            self.cow.set(false);
            self.cowref.set(Value::Nil);
            self.b.set(ptr::null_mut());
            self.e.set(ptr::null_mut());
        }
        self.r.set(self.b.get());
        self.w.set(self.b.get());
    }

    /// `lj_bufx_free`.
    pub fn free(&self) {
        self.release();
        self.cow.set(false);
        self.cowref.set(Value::Nil);
        for p in [&self.r, &self.w, &self.b, &self.e] {
            p.set(ptr::null_mut());
        }
    }

    /// `lj_bufx_set` (cow reference to `o`'s memory).
    fn set_ref(&self, p: *const u8, len: usize, o: Value) {
        self.free();
        self.set_cow(p, len);
        self.cowref.set(o);
    }
}

/// `tvisbuf`: the buffer userdata, if `v` is one.
pub fn tobuf(v: Value) -> Option<Gc<Userdata>> {
    match v {
        Value::Udata(u) if u.udtype() == UDTYPE_BUFFER => Some(u),
        _ => None,
    }
}

/// `bufV`: the payload of a buffer userdata.
pub fn sbx(u: &Userdata) -> &SBufExt {
    u.ext::<SBufExt>().expect("string buffer payload")
}

/// Read pointer of a buffer userdata (FFI conversion of a buffer to a pointer).
fn bufptr(u: &Userdata) -> *const u8 {
    sbx(u).rptr()
}

/// `lj_lib_init_buffer_method` from the reference build's lj_libdef.h.
#[rustfmt::skip]
static LIB_INIT_BUFFER_METHOD: &[u8] = &[
    196,57,18,4,102,114,101,101,5,114,101,115,101,116,4,115,107,105,112,3,115,101,116,3,112,117,116,
    4,112,117,116,102,3,103,101,116,8,112,117,116,99,100,97,116,97,7,114,101,115,101,114,118,101,6,
    99,111,109,109,105,116,3,114,101,102,6,101,110,99,111,100,101,6,100,101,99,111,100,101,4,95,95,
    103,99,10,95,95,116,111,115,116,114,105,110,103,5,95,95,108,101,110,198,98,117,102,102,101,114,
    203,95,95,109,101,116,97,116,97,98,108,101,250,252,1,199,95,95,105,110,100,101,120,250,255,
];

/// `lj_lib_init_buffer` from the reference build's lj_libdef.h.
#[rustfmt::skip]
static LIB_INIT_BUFFER: &[u8] = &[
    212,57,4,252,2,192,250,3,110,101,119,6,101,110,99,111,100,101,6,100,101,99,111,100,101,255,
];

/// `luaopen_string_buffer` (the `package.preload["string.buffer"]` loader).
pub fn luaopen_string_buffer(vm: &mut Vm) -> LuaResult<usize> {
    vm.rt.ffi.bufptr = Some(bufptr);
    let methods: &[NativeFn] = &[
        buffer_method_free,
        buffer_method_reset,
        buffer_method_skip,
        buffer_method_set,
        buffer_method_put,
        buffer_method_putf,
        buffer_method_get,
        buffer_method_putcdata,
        buffer_method_reserve,
        buffer_method_commit,
        buffer_method_ref,
        buffer_method_encode,
        buffer_method_decode,
        buffer_method_mm_gc,
        buffer_method_mm_tostring,
        buffer_method_mm_len,
    ];
    let mut stack = Vec::new();
    let mt = libinit::lib_register(vm, None, LIB_INIT_BUFFER_METHOD, methods, &mut stack)?;
    let k = vm.new_str(b"__tostring");
    let ts = mt.get_str(k);
    let k = vm.new_str(b"tostring");
    mt.set_str(k, ts).map_err(|e| vm.tab_error(e))?;
    let funcs: &[NativeFn] = &[buffer_new, buffer_encode, buffer_decode];
    let lib = libinit::lib_register(vm, None, LIB_INIT_BUFFER, funcs, &mut stack)?;
    vm.ret1(Value::Table(lib))
}

// -- Helpers ----------------------------------------------------------------------------------

/// `buffer_tobuf`: the first argument must be a buffer.
fn buffer_tobuf(vm: &mut Vm) -> LuaResult<Gc<Userdata>> {
    match tobuf(vm.arg(1)) {
        Some(u) if vm.nargs() >= 1 => Ok(u),
        _ => Err(vm.type_error(1, "buffer")),
    }
}

/// `lj_lib_checkintrange`.
pub fn check_intrange(vm: &mut Vm, narg: usize, a: i32, b: i32) -> LuaResult<i32> {
    if narg <= vm.nargs() {
        match vm.arg(narg) {
            Value::Num(n) => {
                let i = crate::vm::num2int(n);
                if i >= a && i <= b {
                    return Ok(i);
                }
            }
            Value::CData(cd) if cd.ctypeid() == crate::cdata::CTID_INT64 => {
                // SAFETY: int64 cdata own 8 aligned bytes.
                let i = unsafe { (cd.ptr() as *const i64).read() };
                if i >= a as i64 && i <= b as i64 {
                    return Ok(i as i32);
                }
            }
            Value::CData(cd) if cd.ctypeid() == crate::cdata::CTID_UINT64 => {
                // SAFETY: uint64 cdata own 8 aligned bytes.
                let i = unsafe { (cd.ptr() as *const u64).read() };
                if (a < 0 || i >= a as u64) && i <= b as u64 {
                    return Ok(i as i32);
                }
            }
            _ => return Err(vm.argt_error(narg, "number")),
        }
        return Err(vm.arg_error_em(narg as i32, ErrMsg::NUMRNG));
    }
    Err(vm.argt_error(narg, "number"))
}

/// `lj_lib_checkstrx`: a string (no number coercion).
pub fn check_strx(vm: &mut Vm, narg: usize) -> LuaResult<Gc<crate::lstr::LuaStr>> {
    match vm.arg(narg) {
        Value::Str(s) if narg <= vm.nargs() => Ok(s),
        _ => Err(vm.argt_error(narg, "string")),
    }
}

/// Map a buffer memory error.
fn mem(vm: &mut Vm, r: Result<(), MemError>) -> LuaResult<()> {
    r.map_err(|_| err_mem(vm))
}

/// Return the buffer object (`L->top = L->base+1`: chain buffer object).
fn chain(vm: &mut Vm) -> LuaResult<usize> {
    vm.set_top(1);
    Ok(1)
}

/// A new `uint8_t *` cdata holding `p` (after `ctype_loadffi`).
fn new_u8ptr(vm: &mut Vm, p: *mut u8) -> LuaResult<Value> {
    crate::parse::ffi_literal_hook(vm)?;
    let cd = vm.new_cdata(CTID_P_UINT8, 8, 8);
    // SAFETY: a pointer cdata owns 8 aligned bytes.
    unsafe { (cd.ptr() as *mut *mut u8).write(p) };
    Ok(Value::CData(cd))
}

/// `lj_cconv_ct_tv` of argument `narg` to `const void *` (FFI).
fn toptr(vm: &mut Vm, narg: usize) -> LuaResult<*const u8> {
    let o = vm.arg(narg);
    match vm.rt.ffi.toptr {
        Some(f) => f(vm, o, narg as u32),
        None => Err(vm.argt_error(narg, "cdata")),
    }
}

// -- Buffer methods ---------------------------------------------------------------------------

fn buffer_method_free(vm: &mut Vm) -> LuaResult<usize> {
    let ud = buffer_tobuf(vm)?;
    sbx(&ud).free();
    chain(vm)
}

fn buffer_method_reset(vm: &mut Vm) -> LuaResult<usize> {
    let ud = buffer_tobuf(vm)?;
    sbx(&ud).reset();
    chain(vm)
}

fn buffer_method_skip(vm: &mut Vm) -> LuaResult<usize> {
    let ud = buffer_tobuf(vm)?;
    let n = check_intrange(vm, 2, 0, LJ_MAX_BUF as i32)? as u32;
    let s = sbx(&ud);
    let len = s.len();
    if n < len {
        // SAFETY: `r + n` stays within `r..w`.
        s.set_r(unsafe { s.rptr().add(n as usize) });
    } else if s.is_cow() {
        s.set_r(s.wptr());
    } else {
        s.set_r(s.b.get());
        s.set_w(s.b.get());
    }
    chain(vm)
}

fn buffer_method_set(vm: &mut Vm) -> LuaResult<usize> {
    let ud = buffer_tobuf(vm)?;
    let (p, len) = if let Value::CData(_) = vm.arg(2) {
        let p = toptr(vm, 2)?;
        let len = check_intrange(vm, 3, 0, LJ_MAX_BUF as i32)?;
        (p, len as usize)
    } else {
        let s = check_strx(vm, 2)?;
        (s.as_bytes().as_ptr(), s.len())
    };
    let o = vm.arg(2);
    sbx(&ud).set_ref(p, len, o);
    chain(vm)
}

fn buffer_method_put(vm: &mut Vm) -> LuaResult<usize> {
    let ud = buffer_tobuf(vm)?;
    let narg = vm.nargs();
    for arg in 2..=narg {
        let mut o = vm.arg(arg);
        let mut mo_tried = false;
        loop {
            let s = sbx(&ud);
            match o {
                Value::Str(st) => {
                    let r = s.putmem(st.as_bytes());
                    mem(vm, r)?;
                }
                Value::Num(n) => {
                    let b = crate::text::strfmt::fmt_num(n);
                    let r = s.putmem(&b);
                    mem(vm, r)?;
                }
                _ if tobuf(o).is_some() => {
                    let ud2 = tobuf(o).expect("buffer");
                    if ud2 == ud {
                        return Err(vm.arg_error_em(arg as i32, ErrMsg::BUFFER_SELF));
                    }
                    let r = s.putmem(sbx(&ud2).contents());
                    mem(vm, r)?;
                }
                _ => {
                    let mo = if mo_tried { Value::Nil } else { vm.meta_lookup(o, crate::bc::MM::Tostring) };
                    if mo.is_nil() {
                        return Err(vm.type_error(arg as i32, "string/number/__tostring"));
                    }
                    // Call __tostring metamethod inline.
                    mo_tried = true;
                    vm.call_value(mo, &[o], 1)?;
                    o = vm.pop();
                    vm.set_arg(arg, o);
                    continue; // Retry with the result.
                }
            }
            break;
        }
    }
    vm.set_top(1);
    vm.gc_safe();
    Ok(1)
}

fn buffer_method_putf(vm: &mut Vm) -> LuaResult<usize> {
    let ud = buffer_tobuf(vm)?;
    let mut out = Vec::new();
    let r = super::string::strfmt_putarg(vm, &mut out, 2, Some(ud));
    // Output produced before an error stays in the buffer, as upstream writes in place.
    let w = sbx(&ud).putmem(&out);
    r?;
    mem(vm, w)?;
    vm.set_top(1);
    vm.gc_safe();
    Ok(1)
}

fn buffer_method_get(vm: &mut Vm) -> LuaResult<usize> {
    let ud = buffer_tobuf(vm)?;
    let mut narg = vm.nargs();
    if narg == 1 {
        narg += 1;
        vm.push_nil(); // get() is the same as get(nil).
    }
    for arg in 2..=narg {
        let n = if vm.arg(arg).is_nil() { LJ_MAX_BUF } else { check_intrange(vm, arg, 0, LJ_MAX_BUF as i32)? as u32 };
        let s = sbx(&ud);
        let n = n.min(s.len());
        // SAFETY: `r..r+n` is unread buffer memory.
        let bytes = unsafe { std::slice::from_raw_parts(s.rptr(), n as usize) };
        let st = if n == 0 { vm.new_str(b"") } else { vm.new_str(bytes) };
        vm.set_arg(arg, Value::Str(st));
        // SAFETY: as above.
        s.set_r(unsafe { s.rptr().add(n as usize) });
    }
    let s = sbx(&ud);
    if s.rptr() == s.wptr() && !s.is_cow() {
        s.set_r(s.b.get());
        s.set_w(s.b.get());
    }
    vm.gc_safe();
    Ok(narg - 1)
}

fn buffer_method_putcdata(vm: &mut Vm) -> LuaResult<usize> {
    let ud = buffer_tobuf(vm)?;
    let p = if let Value::CData(_) = vm.arg(2) {
        toptr(vm, 2)?
    } else {
        return Err(vm.type_error(2, "cdata"));
    };
    let len = check_intrange(vm, 3, 0, LJ_MAX_BUF as i32)? as usize;
    let bytes: &[u8] = if len == 0 {
        &[]
    } else {
        // SAFETY: the caller vouches for `len` readable bytes at `p` (as in upstream).
        unsafe { std::slice::from_raw_parts(p, len) }
    };
    let r = sbx(&ud).putmem(bytes);
    mem(vm, r)?;
    chain(vm)
}

fn buffer_method_reserve(vm: &mut Vm) -> LuaResult<usize> {
    let ud = buffer_tobuf(vm)?;
    let sz = check_intrange(vm, 2, 0, LJ_MAX_BUF as i32)? as u32;
    let r = sbx(&ud).more(sz);
    mem(vm, r)?;
    let p = new_u8ptr(vm, sbx(&ud).wptr())?;
    vm.push(p);
    vm.push(Value::Num(sbx(&ud).left() as f64));
    Ok(2)
}

fn buffer_method_commit(vm: &mut Vm) -> LuaResult<usize> {
    let ud = buffer_tobuf(vm)?;
    let len = check_intrange(vm, 2, 0, LJ_MAX_BUF as i32)? as u32;
    let s = sbx(&ud);
    if len > s.left() {
        return Err(vm.arg_error_em(2, ErrMsg::NUMRNG));
    }
    // SAFETY: `w + len <= e`.
    s.set_w(unsafe { s.wptr().add(len as usize) });
    chain(vm)
}

fn buffer_method_ref(vm: &mut Vm) -> LuaResult<usize> {
    let ud = buffer_tobuf(vm)?;
    let p = new_u8ptr(vm, sbx(&ud).rptr())?;
    vm.push(p);
    vm.push(Value::Num(sbx(&ud).len() as f64));
    Ok(2)
}

fn buffer_method_encode(vm: &mut Vm) -> LuaResult<usize> {
    let ud = buffer_tobuf(vm)?;
    let o = vm.check_any(2)?;
    serialize::put(vm, sbx(&ud), o)?;
    vm.gc_safe();
    chain(vm)
}

fn buffer_method_decode(vm: &mut Vm) -> LuaResult<usize> {
    let ud = buffer_tobuf(vm)?;
    let v = serialize::get(vm, sbx(&ud))?;
    vm.push(v);
    vm.gc_safe();
    Ok(1)
}

fn buffer_method_mm_gc(vm: &mut Vm) -> LuaResult<usize> {
    let ud = buffer_tobuf(vm)?;
    sbx(&ud).free();
    Ok(0)
}

fn buffer_method_mm_tostring(vm: &mut Vm) -> LuaResult<usize> {
    let ud = buffer_tobuf(vm)?;
    let s = vm.new_str(sbx(&ud).contents());
    vm.push(Value::Str(s));
    vm.gc_safe();
    Ok(1)
}

fn buffer_method_mm_len(vm: &mut Vm) -> LuaResult<usize> {
    let ud = buffer_tobuf(vm)?;
    let n = sbx(&ud).len();
    vm.ret1(Value::Num(n as i32 as f64))
}

// -- Buffer library functions -----------------------------------------------------------------

fn buffer_new(vm: &mut Vm) -> LuaResult<usize> {
    let mut sz = 0u32;
    let mut targ = 1;
    let mut dict_str = None;
    let mut dict_mt = None;
    if vm.nargs() >= 1 && !matches!(vm.arg(1), Value::Table(_)) {
        targ = 2;
        if !vm.arg(1).is_nil() {
            sz = check_intrange(vm, 1, 0, LJ_MAX_BUF as i32)? as u32;
        }
    }
    if vm.nargs() >= targ {
        let options = vm.check_tab(targ)?;
        let k = vm.new_str(b"dict");
        if let Value::Table(d) = options.get_str(k) {
            serialize::dict_prep(vm, d, false)?;
            dict_str = Some(d);
        }
        let k = vm.new_str(b"metatable");
        if let Value::Table(d) = options.get_str(k) {
            serialize::dict_prep(vm, d, true)?;
            dict_mt = Some(d);
        }
    }
    let env = vm.current_env();
    let s = SBufExt::new();
    s.dict_str.set(dict_str);
    s.dict_mt.set(dict_mt);
    let ud = vm.new_udata_ext(Box::new(s), UDTYPE_BUFFER, env);
    ud.set_metatable(Some(env));
    vm.push(Value::Udata(ud));
    if sz > 0 {
        let r = sbx(&ud).need2(sz);
        mem(vm, r)?;
    }
    vm.gc_safe();
    Ok(1)
}

fn buffer_encode(vm: &mut Vm) -> LuaResult<usize> {
    let o = vm.check_any(1)?;
    let s = serialize::encode(vm, o)?;
    vm.push(Value::Str(s));
    vm.gc_safe();
    Ok(1)
}

fn buffer_decode(vm: &mut Vm) -> LuaResult<usize> {
    let s = check_strx(vm, 1)?;
    let v = serialize::decode(vm, s)?;
    vm.push(v);
    vm.gc_safe();
    Ok(1)
}
