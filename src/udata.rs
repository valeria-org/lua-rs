//! Userdata objects (`GCudata`, lj_udata.c).

use std::any::Any;
use std::cell::Cell;
use std::fmt;

use crate::gc::{AlignedBytes, Gc, GcBox, GcKind, GcObject, Marker, Trace};
use crate::table::Table;

/// Userdata types (`UDTYPE_*` in lj_obj.h).
pub const UDTYPE_USERDATA: u8 = 0;
/// I/O library FILE.
pub const UDTYPE_IO_FILE: u8 = 1;
/// FFI C library namespace.
pub const UDTYPE_FFI_CLIB: u8 = 2;
/// String buffer.
pub const UDTYPE_BUFFER: u8 = 3;
pub const UDTYPE_MAX: u8 = 4;

/// Rust-side payload of a userdata (string.buffer, ffi clib, ...). Implementations use interior
/// mutability; their `Drop` must not dereference GC handles (they may already be freed).
pub trait UdExt: Any + Trace {}

impl<T: Any + Trace> UdExt for T {}

/// Userdata payload.
pub enum UdPayload {
    /// Raw zero-initialized bytes, 16-byte aligned (C API `lua_newuserdata`, io FILE handles).
    Raw(AlignedBytes),
    /// Rust object.
    Ext(Box<dyn UdExt>),
}

/// A full userdata.
pub struct Userdata {
    meta: Cell<Option<Gc<Table>>>,
    env: Cell<Gc<Table>>,
    udtype: Cell<u8>,
    payload: UdPayload,
}

impl fmt::Debug for Userdata {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Userdata(type={}, len={})", self.udtype.get(), self.len())
    }
}

impl Userdata {
    /// `lj_udata_new`: `len` raw bytes, no metatable.
    pub fn new_raw(len: usize, env: Gc<Table>) -> Userdata {
        Userdata {
            meta: Cell::new(None),
            env: Cell::new(env),
            udtype: Cell::new(UDTYPE_USERDATA),
            payload: UdPayload::Raw(AlignedBytes::new(len, 16)),
        }
    }
    /// Userdata wrapping a Rust object.
    pub fn new_ext(ext: Box<dyn UdExt>, udtype: u8, env: Gc<Table>) -> Userdata {
        Userdata { meta: Cell::new(None), env: Cell::new(env), udtype: Cell::new(udtype), payload: UdPayload::Ext(ext) }
    }

    #[inline]
    pub fn metatable(&self) -> Option<Gc<Table>> {
        self.meta.get()
    }
    #[inline]
    pub fn set_metatable(&self, mt: Option<Gc<Table>>) {
        self.meta.set(mt);
    }
    #[inline]
    pub fn env(&self) -> Gc<Table> {
        self.env.get()
    }
    #[inline]
    pub fn set_env(&self, t: Gc<Table>) {
        self.env.set(t);
    }
    #[inline]
    pub fn udtype(&self) -> u8 {
        self.udtype.get()
    }
    #[inline]
    pub fn set_udtype(&self, t: u8) {
        self.udtype.set(t);
    }
    #[inline]
    pub fn payload(&self) -> &UdPayload {
        &self.payload
    }
    /// Pointer to the raw payload (`uddata`); null for Rust payloads.
    #[inline]
    pub fn data_ptr(&self) -> *mut u8 {
        match &self.payload {
            UdPayload::Raw(b) => b.as_ptr(),
            UdPayload::Ext(_) => std::ptr::null_mut(),
        }
    }
    /// Payload length in bytes (raw payload) or 0 (Rust payload).
    #[inline]
    pub fn len(&self) -> usize {
        match &self.payload {
            UdPayload::Raw(b) => b.len(),
            UdPayload::Ext(_) => 0,
        }
    }
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// The Rust payload downcast to `T`.
    pub fn ext<T: UdExt>(&self) -> Option<&T> {
        match &self.payload {
            UdPayload::Ext(b) => {
                let any: &dyn Any = &**b;
                any.downcast_ref::<T>()
            }
            UdPayload::Raw(_) => None,
        }
    }
}

impl Trace for Userdata {
    fn trace(&self, m: &mut Marker) {
        if let Some(mt) = self.meta.get() {
            m.mark(mt);
        }
        m.mark(self.env.get());
        if let UdPayload::Ext(e) = &self.payload {
            e.trace(m);
        }
    }
    fn size(&self) -> usize {
        std::mem::size_of::<GcBox<Userdata>>()
            + match &self.payload {
                UdPayload::Raw(b) => b.len(),
                UdPayload::Ext(e) => e.size(),
            }
    }
}

impl GcObject for Userdata {
    const KIND: GcKind = GcKind::Udata;
}
