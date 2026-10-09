//! FFI C callback handling (x86-64).
//!
//! Port of lj_ccallback.c and of `vm_ffi_callback` from vm_x64.dasc. A page of
//! machine code holds numbered slots (`mov al, slot; jmp group`), each group
//! ends with `push rbp; mov ah, slot>>8; mov rbp, registry; jmp [handler]`.
//! The assembly handler saves the argument registers and the stack argument
//! pointer into a [`CallbackFrame`] and dispatches to the Rust handler
//! registered for the slot, which converts the arguments, runs the callback
//! and stores the raw result back into the frame.

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::mem::offset_of;
use std::ptr;
use std::rc::Rc;

use super::ccall::{CCALL_NARG_FPR, CCALL_NARG_GPR};
use super::ctype::*;
use super::{FfiError, errmsg};

/// Size of the callback machine code area (one 4K page, `LJ_NUM_CBPAGE`).
pub const CALLBACK_MCODE_SIZE: usize = 4096;
const CALLBACK_MCODE_SLOTSZ: u32 = 4;
const CALLBACK_MCODE_NSLOT: u32 = 128 / CALLBACK_MCODE_SLOTSZ;
const CALLBACK_MCODE_HEAD: u32 = 8;
const CALLBACK_MCODE_GROUP: u32 = 1 + 2 + 10 + 6 - 2;
/// Max. number of callback slots.
pub const CALLBACK_MAX_SLOT: u32 = ((CALLBACK_MCODE_SIZE as u32 - CALLBACK_MCODE_HEAD)
    / (CALLBACK_MCODE_GROUP + 128))
    * CALLBACK_MCODE_NSLOT;
/// `LUA_MINSTACK`: limits the number of callback arguments.
const LUA_MINSTACK: u32 = 20;

#[inline]
const fn callback_slot2ofs(slot: u32) -> u32 {
    CALLBACK_MCODE_HEAD
        + CALLBACK_MCODE_GROUP * (slot / CALLBACK_MCODE_NSLOT)
        + CALLBACK_MCODE_SLOTSZ * slot
}

#[inline]
const fn callback_ofs2slot(ofs: u32) -> u32 {
    let ofs = ofs.wrapping_sub(CALLBACK_MCODE_HEAD);
    let group = ofs / (128 + CALLBACK_MCODE_GROUP);
    (ofs % (128 + CALLBACK_MCODE_GROUP)) / CALLBACK_MCODE_SLOTSZ + group * CALLBACK_MCODE_NSLOT
}

/// Argument/result image of a running callback (upstream `CCallback`
/// register part). Layout is shared with the assembly handler.
#[repr(C)]
pub struct CallbackFrame {
    /// xmm0-xmm7 (low 64 bits). `fpr[0]` holds the FP result.
    pub fpr: [u64; 8],
    /// rdi, rsi, rdx, rcx, r8, r9 (+2 unused). `gpr[0]` holds the result (rax).
    pub gpr: [u64; 8],
    /// Pointer to the arguments on the stack.
    pub stack: *const u64,
    /// Callback slot number.
    pub slot: u32,
    _pad: u32,
}

const CBFRAME_SIZE: usize = std::mem::size_of::<CallbackFrame>();
const _: () = {
    assert!(offset_of!(CallbackFrame, gpr) == 64);
    assert!(offset_of!(CallbackFrame, stack) == 128);
    assert!(offset_of!(CallbackFrame, slot) == 136);
    assert!(CBFRAME_SIZE == 144);
};

/// One callback argument: its raw C type and the location of its value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CallbackArg {
    pub ctid: CTypeID,
    pub ptr: *const u8,
}

impl CallbackFrame {
    /// Locate the arguments of a callback of function type `fid`
    /// (register assignment part of `callback_conv_args`). Each argument is
    /// to be converted with `lj_cconv_tv_ct(cts, arg.ctid, 0, o, arg.ptr)`.
    pub fn args(&self, cts: &CTState, fid: CTypeID) -> Vec<CallbackArg> {
        let mut out = Vec::new();
        let (mut ngpr, mut nfpr, mut nsp) = (0usize, 0usize, 0usize);
        let maxgpr = CCALL_NARG_GPR;
        let mut f = cts.get(fid).sib as CTypeID;
        while f != 0 {
            let ctf = *cts.get(f);
            if !ctype_isattrib(ctf.info) {
                debug_assert!(ctype_isfield(ctf.info), "field expected");
                let cta = cts.rawchild(f);
                let a = cts.get(cta);
                let isfp = ctype_isfp(a.info);
                let sz = (a.size + CTSIZE_PTR - 1) & !(CTSIZE_PTR - 1);
                let n = (sz / CTSIZE_PTR) as usize; // Number of GPRs or stack slots needed.
                let sp: *const u8 = if isfp && nfpr + n <= CCALL_NARG_FPR {
                    let p = ptr::from_ref(&self.fpr[nfpr]).cast();
                    nfpr += n;
                    p
                } else if !isfp && ngpr + n <= maxgpr {
                    let p = ptr::from_ref(&self.gpr[ngpr]).cast();
                    ngpr += n;
                    p
                } else {
                    // Otherwise pass argument on stack.
                    let p = self.stack.wrapping_add(nsp).cast();
                    nsp += n;
                    p
                };
                out.push(CallbackArg { ctid: cta, ptr: sp });
            }
            f = ctf.sib as CTypeID;
        }
        out
    }

    /// Store the callback result for return type `rid` (`callback_conv_result`).
    /// `value` holds the converted result (`lj_cconv_ct_tv` into the raw
    /// return type, i.e. at least its size).
    pub fn set_result(&mut self, cts: &CTState, rid: CTypeID, value: &[u8]) {
        let ctr = *cts.get(cts.raw(rid));
        if ctype_isvoid(ctr.info) {
            return;
        }
        let slot = if ctype_isfp(ctr.info) {
            &mut self.fpr[0]
        } else {
            &mut self.gpr[0]
        };
        let mut dp = slot.to_ne_bytes();
        let size = (ctr.size as usize).min(8);
        dp[..size].copy_from_slice(&value[..size]);
        if ctype_isinteger_or_bool(ctr.info) && ctr.size < 4 {
            // Extend returned integers to (at least) 32 bits.
            let v = match (ctr.size, ctr.info & CTF_UNSIGNED != 0) {
                (1, true) => dp[0] as u32,
                (1, false) => dp[0] as i8 as i32 as u32,
                (_, true) => u16::from_ne_bytes([dp[0], dp[1]]) as u32,
                (_, false) => i16::from_ne_bytes([dp[0], dp[1]]) as i32 as u32,
            };
            dp[..4].copy_from_slice(&v.to_ne_bytes());
        }
        if ctr.size <= 4 && ctype_isinteger_or_bool(ctr.info) {
            // Always zero-extend results to 64 bits.
            dp[4..].fill(0);
        }
        *slot = u64::from_ne_bytes(dp);
    }
}

/// Rust handler invoked for a callback slot.
pub type CallbackHandler = Rc<dyn Fn(&mut CallbackFrame)>;

/// Dispatch state referenced by the machine code (its address is stored in
/// the code page, so it lives in its own heap allocation).
struct Registry {
    handlers: RefCell<Vec<Option<CallbackHandler>>>,
    fallback: RefCell<Option<CallbackHandler>>,
    /// Current callback slot (`cts->cb.slot`).
    slot: Cell<u32>,
}

/// Callback state (upstream `CCallback` minus the register image, which
/// lives in the [`CallbackFrame`] on the native stack).
pub struct CallbackState {
    /// Machine code for callback function pointers (null until first use).
    mcode: *mut u8,
    /// Callback type table: slot -> function ctype id (0 = free).
    cbid: Vec<CTypeID1>,
    /// Lowest possibly unused callback type table slot.
    topid: u32,
    registry: Box<Registry>,
}

impl Default for CallbackState {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for CallbackState {
    fn drop(&mut self) {
        // lj_ccallback_mcode_free
        if !self.mcode.is_null() {
            // SAFETY: mcode was mapped by mcode_new with this size.
            unsafe { libc::munmap(self.mcode.cast(), CALLBACK_MCODE_SIZE) };
        }
    }
}

// Handler for callback functions (`vm_ffi_callback`). Entered by a jump from
// the slot code with the C caller's return address at [rsp+8], the caller's
// rbp saved at [rsp], rbp = registry pointer and ax = slot number.
core::arch::global_asm!(
    ".pushsection .text.luajit_rs_vm_ffi_callback,\"ax\",@progbits",
    ".p2align 4",
    ".globl luajit_rs_vm_ffi_callback",
    ".hidden luajit_rs_vm_ffi_callback",
    ".type luajit_rs_vm_ffi_callback,@function",
    "luajit_rs_vm_ffi_callback:",
    ".cfi_startproc",
    ".cfi_def_cfa_offset 16",
    ".cfi_offset rbp, -16",
    "movzx eax, ax",
    "sub rsp, {size}",
    ".cfi_def_cfa_offset {size} + 16",
    "mov qword ptr [rsp + {gpr}], rdi",
    "mov qword ptr [rsp + {gpr} + 8], rsi",
    "mov qword ptr [rsp + {gpr} + 16], rdx",
    "mov qword ptr [rsp + {gpr} + 24], rcx",
    "mov qword ptr [rsp + {gpr} + 32], r8",
    "mov qword ptr [rsp + {gpr} + 40], r9",
    "movsd qword ptr [rsp], xmm0",
    "movsd qword ptr [rsp + 8], xmm1",
    "movsd qword ptr [rsp + 16], xmm2",
    "movsd qword ptr [rsp + 24], xmm3",
    "movsd qword ptr [rsp + 32], xmm4",
    "movsd qword ptr [rsp + 40], xmm5",
    "movsd qword ptr [rsp + 48], xmm6",
    "movsd qword ptr [rsp + 56], xmm7",
    "lea rcx, [rsp + {size} + 16]",
    "mov qword ptr [rsp + {stack}], rcx",
    "mov dword ptr [rsp + {slot}], eax",
    "mov rdi, rsp",
    "mov rsi, rbp",
    "call {dispatch}",
    "mov rax, qword ptr [rsp + {gpr}]",
    "movsd xmm0, qword ptr [rsp]",
    "add rsp, {size}",
    ".cfi_def_cfa_offset 16",
    "pop rbp",
    ".cfi_def_cfa_offset 8",
    ".cfi_restore rbp",
    "ret",
    ".cfi_endproc",
    ".size luajit_rs_vm_ffi_callback, .-luajit_rs_vm_ffi_callback",
    ".popsection",
    size = const CBFRAME_SIZE,
    gpr = const offset_of!(CallbackFrame, gpr),
    stack = const offset_of!(CallbackFrame, stack),
    slot = const offset_of!(CallbackFrame, slot),
    dispatch = sym callback_dispatch,
);

unsafe extern "C" {
    fn luajit_rs_vm_ffi_callback();
}

/// Called from the assembly handler with the saved register image.
unsafe extern "C-unwind" fn callback_dispatch(frame: *mut CallbackFrame, reg: *const Registry) {
    // SAFETY: reg is the registry address baked into the code page, which
    // outlives the page; frame points to the handler's stack frame.
    let (frame, reg) = unsafe { (&mut *frame, &*reg) };
    reg.slot.set(frame.slot);
    let handler = reg
        .handlers
        .borrow()
        .get(frame.slot as usize)
        .cloned()
        .flatten()
        .or_else(|| reg.fallback.borrow().clone());
    match handler {
        Some(h) => h(frame),
        None => {
            // No way to raise an error into the C caller without a handler.
            eprintln!("{}", errmsg::FFI_BADCBACK);
            std::process::abort();
        }
    }
}

impl CallbackState {
    pub fn new() -> CallbackState {
        CallbackState {
            mcode: ptr::null_mut(),
            cbid: Vec::new(),
            topid: 0,
            registry: Box::new(Registry {
                handlers: RefCell::new(Vec::new()),
                fallback: RefCell::new(None),
                slot: Cell::new(0),
            }),
        }
    }

    /// Convert callback slot number to callback function pointer.
    pub fn slot2ptr(&self, slot: u32) -> *const c_void {
        self.mcode
            .wrapping_add(callback_slot2ofs(slot) as usize)
            .cast()
    }

    /// Convert callback function pointer to slot number (`lj_ccallback_ptr2slot`).
    /// Returns `!0` if not a known callback function pointer.
    pub fn ptr2slot(&self, p: *const c_void) -> u32 {
        let ofs = (p as usize).wrapping_sub(self.mcode as usize);
        if !self.mcode.is_null() && ofs < CALLBACK_MCODE_SIZE {
            let slot = callback_ofs2slot(ofs as u32);
            if callback_slot2ofs(slot) == ofs as u32 {
                return slot;
            }
        }
        !0
    }

    /// Function type id registered for a slot, or 0 if the slot is unused.
    pub fn cbid(&self, slot: u32) -> CTypeID {
        self.cbid.get(slot as usize).map_or(0, |&id| id as CTypeID)
    }

    /// Size of the callback type table (`cts->cb.sizeid`).
    pub fn sizeid(&self) -> u32 {
        self.cbid.len() as u32
    }

    /// Current callback slot (`cts->cb.slot`): set on every callback entry.
    pub fn slot(&self) -> u32 {
        self.registry.slot.get()
    }

    pub fn set_slot(&self, slot: u32) {
        self.registry.slot.set(slot);
    }

    /// Set (or clear) the Rust handler of a slot.
    pub fn set_handler(&self, slot: u32, handler: Option<CallbackHandler>) {
        let mut h = self.registry.handlers.borrow_mut();
        let i = slot as usize;
        if h.len() <= i {
            h.resize(i + 1, None);
        }
        h[i] = handler;
    }

    /// Handler used for slots without their own handler (e.g. freed slots,
    /// where upstream raises `bad callback`).
    pub fn set_fallback(&self, handler: Option<CallbackHandler>) {
        *self.registry.fallback.borrow_mut() = handler;
    }

    /// Free a callback slot (`cb:free()`): the slot may be reused.
    pub fn free(&mut self, slot: u32) {
        if let Some(id) = self.cbid.get_mut(slot as usize) {
            *id = 0;
            self.topid = self.topid.min(slot);
        }
        self.set_handler(slot, None);
    }

    /// Allocate and initialize area for callback function pointers.
    fn mcode_new(&mut self) -> Result<(), FfiError> {
        // SAFETY: anonymous private mapping, no preconditions.
        let p = unsafe {
            libc::mmap(
                ptr::null_mut(),
                CALLBACK_MCODE_SIZE,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        if p == libc::MAP_FAILED {
            return Err(FfiError::caller(errmsg::FFI_CBACKOV));
        }
        let page = p.cast::<u8>();
        // SAFETY: page is a fresh writable mapping of CALLBACK_MCODE_SIZE bytes.
        let code = unsafe { std::slice::from_raw_parts_mut(page, CALLBACK_MCODE_SIZE) };
        let used = mcode_init(code, page as usize, ptr::from_ref(&*self.registry) as u64);
        debug_assert!(
            used <= CALLBACK_MCODE_SIZE,
            "miscalculated CALLBACK_MAX_SLOT"
        );
        // SAFETY: page is the mapping created above.
        unsafe { libc::mprotect(p, CALLBACK_MCODE_SIZE, libc::PROT_READ | libc::PROT_EXEC) };
        self.mcode = page;
        Ok(())
    }

    /// Get an unused slot in the callback slot table.
    fn slot_new(&mut self, id: CTypeID) -> Result<u32, FfiError> {
        let mut top = self.topid;
        while (top as usize) < self.cbid.len() {
            if self.cbid[top as usize] == 0 {
                self.cbid[top as usize] = id as CTypeID1;
                self.topid = top + 1;
                return Ok(top);
            }
            top += 1;
        }
        if top >= CALLBACK_MAX_SLOT {
            return Err(FfiError::caller(errmsg::FFI_CBACKOV));
        }
        if self.mcode.is_null() {
            self.mcode_new()?;
        }
        let newsize = (self.cbid.len() * 2).clamp(8, CALLBACK_MAX_SLOT as usize);
        self.cbid.resize(newsize, 0);
        self.cbid[top as usize] = id as CTypeID1;
        self.topid = top + 1;
        Ok(top)
    }
}

/// Emit the slot code (`callback_mcode_init`). Returns the number of bytes used.
fn mcode_init(p: &mut [u8], page: usize, registry: u64) -> usize {
    let target = (luajit_rs_vm_ffi_callback as unsafe extern "C" fn()) as usize as u64;
    p[0..8].copy_from_slice(&target.to_ne_bytes());
    let mut i = 8;
    for slot in 0..CALLBACK_MAX_SLOT {
        // mov al, slot; jmp group
        p[i] = 0xb0;
        p[i + 1] = slot as u8;
        i += 2;
        if (slot & (CALLBACK_MCODE_NSLOT - 1)) == CALLBACK_MCODE_NSLOT - 1
            || slot == CALLBACK_MAX_SLOT - 1
        {
            // push rbp; mov ah, slot>>8; mov rbp, &registry.
            p[i] = 0x55;
            p[i + 1] = 0xb4;
            p[i + 2] = (slot >> 8) as u8;
            p[i + 3] = 0x48;
            p[i + 4] = 0xbd;
            p[i + 5..i + 13].copy_from_slice(&registry.to_ne_bytes());
            i += 13;
            // jmp [rip-pageofs] where the handler address is stored.
            p[i] = 0xff;
            p[i + 1] = 0x25;
            let rel = page as i64 - (page + i + 6) as i64;
            p[i + 2..i + 6].copy_from_slice(&(rel as i32).to_ne_bytes());
            i += 6;
        } else {
            p[i] = 0xeb;
            p[i + 1] = (CALLBACK_MCODE_SLOTSZ
                * (CALLBACK_MCODE_NSLOT - 1 - (slot & (CALLBACK_MCODE_NSLOT - 1)))
                - 2) as u8;
            i += 2;
        }
    }
    i
}

/// Check for function pointer and supported argument/result types
/// (`callback_checkfunc`). Returns the function type id.
pub fn callback_checkfunc(cts: &CTState, ptr_id: CTypeID) -> Option<CTypeID> {
    let ct = cts.get(ptr_id);
    if !ctype_isptr(ct.info) || ct.size != CTSIZE_PTR {
        return None;
    }
    let fid = cts.rawchild(ptr_id);
    let f = cts.get(fid);
    if !ctype_isfunc(f.info) {
        return None;
    }
    let ctr = cts.get(cts.rawchild(fid));
    if !(ctype_isvoid(ctr.info)
        || ctype_isenum(ctr.info)
        || ctype_isptr(ctr.info)
        || (ctype_isnum(ctr.info) && ctr.size <= 8))
    {
        return None;
    }
    if f.info & CTF_VARARG != 0 {
        return None;
    }
    let mut narg = 0;
    let mut a = f.sib as CTypeID;
    while a != 0 {
        let ctf = cts.get(a);
        if !ctype_isattrib(ctf.info) {
            debug_assert!(ctype_isfield(ctf.info), "field expected");
            let cta = cts.get(cts.rawchild(a));
            narg += 1;
            if !(ctype_isenum(cta.info)
                || ctype_isptr(cta.info)
                || (ctype_isnum(cta.info) && cta.size <= 8))
                || narg >= LUA_MINSTACK - 3
            {
                return None;
            }
        }
        a = ctf.sib as CTypeID;
    }
    Some(fid)
}

/// Create a new callback and return the callback function pointer
/// (`lj_ccallback_new`). `ptr_id` is the raw destination pointer type.
/// Returns `Ok(None)` for an unsupported type (a bad conversion). The slot of
/// the result is `cts.cb.ptr2slot(p)`; `handler` (if any) is registered for it.
pub fn callback_new(
    cts: &mut CTState,
    ptr_id: CTypeID,
    handler: Option<CallbackHandler>,
) -> Result<Option<*const c_void>, FfiError> {
    let Some(fid) = callback_checkfunc(cts, ptr_id) else {
        return Ok(None);
    };
    let slot = cts.cb.slot_new(fid)?;
    cts.cb.set_handler(slot, handler);
    Ok(Some(cts.cb.slot2ptr(slot)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slot_layout() {
        assert_eq!(CALLBACK_MAX_SLOT, 896);
        assert_eq!(callback_slot2ofs(0), 8);
        assert_eq!(callback_slot2ofs(31), 8 + 31 * 4);
        assert_eq!(callback_slot2ofs(32), 8 + 17 + 32 * 4);
        for slot in 0..CALLBACK_MAX_SLOT {
            assert_eq!(callback_ofs2slot(callback_slot2ofs(slot)), slot);
        }
        let mut page = vec![0u8; CALLBACK_MCODE_SIZE];
        let used = mcode_init(&mut page, 0x1000, 0);
        assert!(used <= CALLBACK_MCODE_SIZE);
        assert_eq!(used as u32, callback_slot2ofs(CALLBACK_MAX_SLOT - 1) + 21);
    }
}
