//! Cdata storage objects (`GCcdata`) and the predefined ctype ids (`CTTYDEF` in lj_ctype.h).

use std::fmt;

use crate::gc::{AlignedBytes, GC_CDATA_FIN, Gc, GcBox, GcKind, GcObject, Marker, Trace};

/// Predefined ctype ids, in exact upstream `CTTYDEF` order (x64: no `LINT32`).
pub const CTID_NONE: u32 = 0;
pub const CTID_VOID: u32 = 1;
pub const CTID_CVOID: u32 = 2;
pub const CTID_BOOL: u32 = 3;
pub const CTID_CCHAR: u32 = 4;
pub const CTID_INT8: u32 = 5;
pub const CTID_UINT8: u32 = 6;
pub const CTID_INT16: u32 = 7;
pub const CTID_UINT16: u32 = 8;
pub const CTID_INT32: u32 = 9;
pub const CTID_UINT32: u32 = 10;
pub const CTID_INT64: u32 = 11;
pub const CTID_UINT64: u32 = 12;
pub const CTID_INT128: u32 = 13;
pub const CTID_UINT128: u32 = 14;
pub const CTID_FLOAT: u32 = 15;
pub const CTID_DOUBLE: u32 = 16;
pub const CTID_COMPLEX_FLOAT: u32 = 17;
pub const CTID_COMPLEX_DOUBLE: u32 = 18;
pub const CTID_P_VOID: u32 = 19;
pub const CTID_P_CVOID: u32 = 20;
pub const CTID_P_CCHAR: u32 = 21;
pub const CTID_P_UINT8: u32 = 22;
pub const CTID_A_CCHAR: u32 = 23;
pub const CTID_CTYPEID: u32 = 24;
/// First id after the predefined types (predefined typedefs and keywords follow).
pub const CTID_PREDEF_END: u32 = 25;
/// Maximum number of ctypes.
pub const CTID_MAX: u32 = 65536;
/// Pointer-sized integer types (x64).
pub const CTID_INT_PSZ: u32 = CTID_INT64;
pub const CTID_UINT_PSZ: u32 = CTID_UINT64;
/// `wchar_t` (non-Windows, non-PPC).
pub const CTID_WCHAR: u32 = CTID_INT32;

/// Backing memory of a cdata object.
pub enum CDataStorage {
    /// Owned zero-initialized bytes (alignment ≥ 16).
    Owned(AlignedBytes),
    /// Memory owned elsewhere.
    External { ptr: *mut u8, size: usize },
}

/// A cdata object: a ctype id plus its storage. The FFI interprets the bytes.
pub struct CData {
    ctypeid: u32,
    storage: CDataStorage,
}

impl fmt::Debug for CData {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "CData(ctype={}, size={})", self.ctypeid, self.size())
    }
}

impl CData {
    /// Cdata owning `size` zeroed bytes aligned to `max(align, 16)`.
    pub fn new(ctypeid: u32, size: usize, align: usize) -> CData {
        CData {
            ctypeid,
            storage: CDataStorage::Owned(AlignedBytes::new(size, align)),
        }
    }
    /// Cdata referring to external memory.
    pub fn new_external(ctypeid: u32, ptr: *mut u8, size: usize) -> CData {
        CData {
            ctypeid,
            storage: CDataStorage::External { ptr, size },
        }
    }
    #[inline]
    pub fn ctypeid(&self) -> u32 {
        self.ctypeid
    }
    /// Pointer to the payload (`cdataptr`).
    #[inline]
    pub fn ptr(&self) -> *mut u8 {
        match &self.storage {
            CDataStorage::Owned(b) => b.as_ptr(),
            CDataStorage::External { ptr, .. } => *ptr,
        }
    }
    #[inline]
    pub fn size(&self) -> usize {
        match &self.storage {
            CDataStorage::Owned(b) => b.len(),
            CDataStorage::External { size, .. } => *size,
        }
    }
    #[inline]
    pub fn storage(&self) -> &CDataStorage {
        &self.storage
    }
}

/// Finalizer flag helpers on cdata handles (upstream `LJ_GC_CDATA_FIN`).
impl Gc<CData> {
    /// True if registered in the FFI finalizer table.
    #[inline]
    pub fn has_finalizer(self) -> bool {
        self.header().has(GC_CDATA_FIN)
    }
    /// Set or clear the finalizer flag (the FFI also updates `Vm::ffi_fin`).
    #[inline]
    pub fn set_has_finalizer(self, on: bool) {
        if on {
            self.header().set_flag(GC_CDATA_FIN)
        } else {
            self.header().clear_flag(GC_CDATA_FIN)
        }
    }
}

impl Trace for CData {
    fn trace(&self, _m: &mut Marker) {}
    fn size(&self) -> usize {
        std::mem::size_of::<GcBox<CData>>()
            + match &self.storage {
                CDataStorage::Owned(b) => b.len(),
                CDataStorage::External { .. } => 0,
            }
    }
}

impl GcObject for CData {
    const KIND: GcKind = GcKind::CData;
}
