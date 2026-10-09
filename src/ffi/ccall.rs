//! FFI C call handling (x86-64 SysV).
//!
//! Port of the POSIX/x64 parts of lj_ccall.c/lj_ccall.h and of `vm_ffi_call`
//! from vm_x64.dasc. The conversion of Lua values to C arguments (and of the
//! results back) is delegated to the caller through [`CCallArgs`] and
//! [`CCallResult`], so this module only deals with C types and raw bytes.

use std::alloc::Layout;
use std::ffi::c_void;
use std::mem::offset_of;
use std::ptr::NonNull;

use super::ctype::*;
use super::{FfiError, errmsg};

/// Number of integer argument registers (rdi, rsi, rdx, rcx, r8, r9).
pub const CCALL_NARG_GPR: usize = 6;
/// Number of SSE argument registers (xmm0-xmm7).
pub const CCALL_NARG_FPR: usize = 8;
/// Integer result registers (rax, rdx).
pub const CCALL_NRET_GPR: usize = 2;
/// SSE result registers (xmm0, xmm1).
pub const CCALL_NRET_FPR: usize = 2;
pub const CCALL_NUM_GPR: usize = 6;
pub const CCALL_NUM_FPR: usize = 8;
pub const CCALL_NUM_STACK: usize = 31;
pub const CCALL_SIZE_STACK: u32 = CCALL_NUM_STACK as u32 * CTSIZE_PTR;
const CCALL_SPS_FREE: u32 = 1;
const CCALL_SPS_EXTRA: u32 = 0;

/// Register classes for x64 struct classification.
const CCALL_RCL_INT: u32 = 1;
const CCALL_RCL_SSE: u32 = 2;
const CCALL_RCL_MEM: u32 = 4;

/// Register and stack image consumed by the call trampoline
/// (upstream `CCallState`, field offsets used by the assembly below).
#[repr(C, align(16))]
pub struct CCallRegs {
    /// Pointer to called function.
    pub func: *const c_void,
    /// Stack pointer adjustment.
    pub spadj: u32,
    /// Number of bytes on stack.
    pub nsp: u8,
    /// Return value by reference.
    pub retref: u8,
    /// Number of arguments in GPRs.
    pub ngpr: u8,
    /// Number of arguments in FPRs (also the `al` value for varargs).
    pub nfpr: u8,
    /// Arguments/results in FPRs: 8 x 16 bytes (xmm0-xmm7).
    pub fpr: [u8; 16 * CCALL_NUM_FPR],
    /// Arguments/results in GPRs: 6 x 8 bytes.
    pub gpr: [u8; 8 * CCALL_NUM_GPR],
    /// Stack slots.
    pub stack: [u8; CCALL_SIZE_STACK as usize],
}

const _: () = {
    assert!(offset_of!(CCallRegs, spadj) == 8);
    assert!(offset_of!(CCallRegs, nsp) == 12);
    assert!(offset_of!(CCallRegs, nfpr) == 15);
    assert!(offset_of!(CCallRegs, fpr) == 16);
    assert!(offset_of!(CCallRegs, gpr) == 144);
    assert!(offset_of!(CCallRegs, stack) == 192);
};

// Call C function via FFI (`lj_vm_ffi_call`). Copies the stack slots, loads
// the argument registers (and `al` = number of vector registers), calls the
// function and stores rax/rdx/xmm0/xmm1 back into the register image.
core::arch::global_asm!(
    ".pushsection .text.luajit_rs_vm_ffi_call,\"ax\",@progbits",
    ".p2align 4",
    ".globl luajit_rs_vm_ffi_call",
    ".hidden luajit_rs_vm_ffi_call",
    ".type luajit_rs_vm_ffi_call,@function",
    "luajit_rs_vm_ffi_call:",
    ".cfi_startproc",
    "push rbp",
    ".cfi_def_cfa_offset 16",
    ".cfi_offset rbp, -16",
    "mov rbp, rsp",
    ".cfi_def_cfa_register rbp",
    "push rbx",
    ".cfi_offset rbx, -24",
    "mov rbx, rdi",
    // Readjust stack.
    "mov eax, dword ptr [rbx + {spadj}]",
    "sub rsp, rax",
    // Copy stack slots.
    "movzx ecx, byte ptr [rbx + {nsp}]",
    "sub ecx, 8",
    "js 3f",
    "2:",
    "mov rax, qword ptr [rbx + rcx + {stack}]",
    "mov qword ptr [rsp + rcx], rax",
    "sub ecx, 8",
    "jns 2b",
    "3:",
    "movzx eax, byte ptr [rbx + {nfpr}]",
    "mov rdi, qword ptr [rbx + {gpr}]",
    "mov rsi, qword ptr [rbx + {gpr} + 8]",
    "mov rdx, qword ptr [rbx + {gpr} + 16]",
    "mov rcx, qword ptr [rbx + {gpr} + 24]",
    "mov r8, qword ptr [rbx + {gpr} + 32]",
    "mov r9, qword ptr [rbx + {gpr} + 40]",
    "test eax, eax",
    "jz 5f",
    "movaps xmm0, xmmword ptr [rbx + {fpr}]",
    "movaps xmm1, xmmword ptr [rbx + {fpr} + 16]",
    "movaps xmm2, xmmword ptr [rbx + {fpr} + 32]",
    "movaps xmm3, xmmword ptr [rbx + {fpr} + 48]",
    "cmp eax, 4",
    "jbe 5f",
    "movaps xmm4, xmmword ptr [rbx + {fpr} + 64]",
    "movaps xmm5, xmmword ptr [rbx + {fpr} + 80]",
    "movaps xmm6, xmmword ptr [rbx + {fpr} + 96]",
    "movaps xmm7, xmmword ptr [rbx + {fpr} + 112]",
    "5:",
    "call qword ptr [rbx]",
    "mov qword ptr [rbx + {gpr}], rax",
    "movaps xmmword ptr [rbx + {fpr}], xmm0",
    "mov qword ptr [rbx + {gpr} + 8], rdx",
    "movaps xmmword ptr [rbx + {fpr} + 16], xmm1",
    "mov rbx, qword ptr [rbp - 8]",
    "leave",
    ".cfi_def_cfa rsp, 8",
    "ret",
    ".cfi_endproc",
    ".size luajit_rs_vm_ffi_call, .-luajit_rs_vm_ffi_call",
    ".popsection",
    spadj = const offset_of!(CCallRegs, spadj),
    nsp = const offset_of!(CCallRegs, nsp),
    nfpr = const offset_of!(CCallRegs, nfpr),
    fpr = const offset_of!(CCallRegs, fpr),
    gpr = const offset_of!(CCallRegs, gpr),
    stack = const offset_of!(CCallRegs, stack),
);

unsafe extern "C-unwind" {
    fn luajit_rs_vm_ffi_call(cc: *mut CCallRegs);
}

/// Source of the arguments of a C call (the Lua values in upstream).
pub trait CCallArgs {
    /// Error type of the conversions; FFI core errors convert into it.
    type Error: From<FfiError>;

    /// Number of passed arguments.
    fn count(&self) -> usize;

    /// Infer the destination C type of vararg argument `narg` (1-based),
    /// i.e. `lj_ccall_ctid_vararg` (see [`ctid_vararg_cdata`] for cdata).
    fn vararg_ctid(&mut self, cts: &mut CTState, narg: usize) -> Result<CTypeID, Self::Error>;

    /// Convert argument `narg` (1-based) to the raw C type `did` and store it
    /// into `dst` (exactly the size of `did`), i.e.
    /// `lj_cconv_ct_tv(cts, d, dst, o, CCF_ARG(narg))`.
    fn convert(
        &mut self,
        cts: &mut CTState,
        narg: usize,
        did: CTypeID,
        dst: &mut [u8],
    ) -> Result<(), Self::Error>;
}

/// The cdata case of `lj_ccall_ctid_vararg`: vararg type for a cdata of type `id`.
pub fn ctid_vararg_cdata(cts: &mut CTState, id: CTypeID) -> Result<CTypeID, FfiError> {
    let s = *cts.get(id);
    if ctype_isrefarray(s.info) {
        cts.intern(ctinfo(CT_PTR, CTALIGN_PTR | ctype_cid(s.info)), CTSIZE_PTR)
    } else if ctype_isstruct(s.info) || ctype_isfunc(s.info) {
        // NYI: how to pass a struct by value in a vararg argument?
        cts.intern(ctinfo(CT_PTR, CTALIGN_PTR | id), CTSIZE_PTR)
    } else if ctype_isfp(s.info) && s.size == 4 {
        Ok(CTID_DOUBLE)
    } else {
        Ok(id)
    }
}

/// Vararg type of a Lua number (`lj_ccall_ctid_vararg`).
pub const CTID_VARARG_NUMBER: CTypeID = CTID_DOUBLE;
/// Vararg type of a Lua string.
pub const CTID_VARARG_STRING: CTypeID = CTID_P_CCHAR;
/// Vararg type of a Lua boolean.
pub const CTID_VARARG_BOOL: CTypeID = CTID_BOOL;
/// Vararg type of any other Lua value (nil, lightuserdata, ...).
pub const CTID_VARARG_OTHER: CTypeID = CTID_P_VOID;

/// Resolve the callee of a cdata call (first part of `lj_ccall_func`): for a
/// cdata of type `ctid`, returns the raw function type id and the size of the
/// stored function pointer (`CTSIZE_PTR` for a function cdata itself), or
/// `None` if the cdata is not callable.
pub fn func_type(cts: &CTState, ctid: CTypeID) -> Option<(CTypeID, CTSize)> {
    let mut ct = cts.raw(ctid);
    let mut sz = CTSIZE_PTR;
    if ctype_isptr(cts.get(ct).info) {
        sz = cts.get(ct).size;
        ct = cts.rawchild(ct);
    }
    ctype_isfunc(cts.get(ct).info).then_some((ct, sz))
}

fn errno_location() -> *mut i32 {
    // SAFETY: __errno_location has no preconditions and returns the
    // thread-local errno address.
    unsafe { libc::__errno_location() }
}

/// `ffi.errno()`: the current C `errno` of this thread.
pub fn errno() -> i32 {
    // SAFETY: errno_location returns a valid pointer to this thread's errno.
    unsafe { *errno_location() }
}

/// `ffi.errno(newerr)`: set the C `errno` and return the previous value.
pub fn set_errno(newerr: i32) -> i32 {
    // SAFETY: errno_location returns a valid pointer to this thread's errno.
    unsafe { std::ptr::replace(errno_location(), newerr) }
}

/// Aligned heap storage for an aggregate (struct/complex) return value.
struct RetBuf {
    ptr: NonNull<u8>,
    layout: Layout,
}

impl RetBuf {
    fn new(size: CTSize, align: u32) -> RetBuf {
        let layout = Layout::from_size_align((size as usize).max(16), (1usize << align).max(16))
            .expect("bad struct return layout");
        // SAFETY: the layout has a non-zero size.
        let p = unsafe { std::alloc::alloc_zeroed(layout) };
        let ptr = NonNull::new(p).unwrap_or_else(|| std::alloc::handle_alloc_error(layout));
        RetBuf { ptr, layout }
    }

    fn bytes(&self, len: usize) -> &[u8] {
        // SAFETY: len <= allocation size (callers pass the return type size)
        // and the memory is initialized (zeroed on allocation).
        unsafe { std::slice::from_raw_parts(self.ptr.as_ptr(), len) }
    }

    fn bytes_mut(&mut self, len: usize) -> &mut [u8] {
        // SAFETY: as above, with unique access through &mut self.
        unsafe { std::slice::from_raw_parts_mut(self.ptr.as_ptr(), len) }
    }
}

impl Drop for RetBuf {
    fn drop(&mut self) {
        // SAFETY: allocated in RetBuf::new with the same layout.
        unsafe { std::alloc::dealloc(self.ptr.as_ptr(), self.layout) };
    }
}

/// Where a register/stack argument goes.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Dest {
    Fpr(usize),
    Gpr(usize),
    Stack(usize),
}

/// Result of a C call, to be converted by the caller.
#[derive(Debug, PartialEq, Eq)]
pub enum CCallResult<'a> {
    /// `void` function: zero results.
    Void,
    /// Struct or complex result: a new cdata of type `ctid` (the declared
    /// return type) holding these bytes.
    Aggregate { ctid: CTypeID, bytes: &'a [u8] },
    /// Any other result: convert with `lj_cconv_tv_ct(cts, ctid, 0, bytes)`,
    /// `ctid` being the raw return type.
    Scalar { ctid: CTypeID, bytes: &'a [u8] },
}

/// A prepared C call (upstream `CCallState` plus the preallocated aggregate
/// result storage).
pub struct CCallState {
    regs: CCallRegs,
    /// Raw function type.
    fid: CTypeID,
    /// Raw return type.
    rid: CTypeID,
    ret: Option<RetBuf>,
}

/// Classify a C type.
fn classify_ct(cts: &CTState, id: CTypeID, rcl: &mut [u32; 2], ofs: CTSize) {
    let ct = cts.get(id);
    if ctype_isarray(ct.info) {
        let cct = cts.rawchild(id);
        let esz = cts.get(cct).size;
        let asz = ct.size;
        let mut eofs = 0;
        while eofs < asz {
            classify_ct(cts, cct, rcl, ofs + eofs);
            eofs += esz;
        }
    } else if ctype_isstruct(ct.info) {
        classify_struct(cts, id, rcl, ofs);
    } else {
        let mut cl = if ctype_isfp(ct.info) {
            CCALL_RCL_SSE
        } else {
            CCALL_RCL_INT
        };
        debug_assert!(
            ctype_hassize(ct.info),
            "classify ctype {:08x} without size",
            ct.info
        );
        if ofs & (ct.size.wrapping_sub(1)) != 0 {
            cl = CCALL_RCL_MEM; // Unaligned.
        }
        rcl[(ofs >= 8) as usize] |= cl;
    }
}

/// Recursively classify a struct based on its fields. Returns non-zero for
/// the memory class.
fn classify_struct(cts: &CTState, mut id: CTypeID, rcl: &mut [u32; 2], ofs: CTSize) -> u32 {
    if cts.get(id).size > 16 {
        return CCALL_RCL_MEM; // Too big, gets memory class.
    }
    while cts.get(id).sib != 0 {
        id = cts.get(id).sib as CTypeID;
        let ct = cts.get(id);
        let fofs = ofs + ct.size;
        if ctype_isfield(ct.info) {
            classify_ct(cts, cts.rawchild(id), rcl, fofs);
        } else if ctype_isbitfield(ct.info) && ctype_bitbsz(ct.info) != 0 {
            rcl[(fofs >= 8) as usize] |= CCALL_RCL_INT; // NYI: unaligned bitfields?
        } else if ctype_isxattrib(ct.info, CTA_SUBTYPE) {
            classify_struct(cts, cts.rawchild(id), rcl, fofs);
        }
    }
    (rcl[0] | rcl[1]) & CCALL_RCL_MEM // Memory class?
}

fn nyi() -> FfiError {
    FfiError::caller(errmsg::FFI_NYICALL)
}

impl CCallState {
    /// Set up the arguments for a call of a function of raw type `fid`
    /// through the function pointer `func` (`ccall_set_args`).
    pub fn new<A: CCallArgs>(
        cts: &mut CTState,
        fid: CTypeID,
        func: *const c_void,
        args: &mut A,
    ) -> Result<CCallState, A::Error> {
        let info = cts.get(fid).info;
        debug_assert!(ctype_isfunc(info), "function type expected");
        let rid = cts.rawchild(fid);
        let mut cc = CCallState {
            regs: CCallRegs {
                func,
                spadj: 0,
                nsp: 0,
                retref: 0,
                ngpr: 0,
                nfpr: 0,
                // Clear unused regs to get some determinism in case of misdeclaration.
                fpr: [0; 16 * CCALL_NUM_FPR],
                gpr: [0; 8 * CCALL_NUM_GPR],
                stack: [0; CCALL_SIZE_STACK as usize],
            },
            fid,
            rid,
            ret: None,
        };
        let maxgpr = CCALL_NARG_GPR;
        let mut ngpr = 0usize;
        let mut nfpr = 0usize;
        let mut nsp: u32 = 0;

        // Perform required setup for some result types.
        let ctr = *cts.get(rid);
        if ctype_isvector(ctr.info) {
            if !(ctr.size == 8 || ctr.size == 16) {
                return Err(nyi().into());
            }
        } else if ctype_iscomplex(ctr.info) || ctype_isstruct(ctr.info) {
            // Preallocate the result storage.
            let buf = RetBuf::new(ctr.size, ctype_align(ctr.info));
            let dp = buf.ptr.as_ptr();
            cc.ret = Some(buf);
            if ctype_isstruct(ctr.info) {
                let mut rcl = [0u32; 2];
                if classify_struct(cts, rid, &mut rcl, 0) != 0 {
                    cc.regs.retref = 1; // Return struct by reference.
                    cc.regs.gpr[0..8].copy_from_slice(&(dp as u64).to_ne_bytes());
                    ngpr += 1;
                } else {
                    cc.regs.retref = 0; // Return small structs in registers.
                }
            } else {
                cc.regs.retref = 0; // Complex values are returned in one or two FPRs.
            }
        }

        // Skip initial attributes.
        let mut fidx = cts.get(fid).sib as CTypeID;
        while fidx != 0 {
            let ctf = cts.get(fidx);
            if !ctype_isattrib(ctf.info) {
                break;
            }
            fidx = ctf.sib as CTypeID;
        }

        // Walk through all passed arguments.
        for narg in 1..=args.count() {
            let did = if fidx != 0 {
                // Get argument type from field.
                let ctf = *cts.get(fidx);
                fidx = ctf.sib as CTypeID;
                debug_assert!(ctype_isfield(ctf.info), "field expected");
                ctype_cid(ctf.info)
            } else {
                if info & CTF_VARARG == 0 {
                    // Too many arguments.
                    return Err(FfiError::caller(errmsg::FFI_NUMARG).into());
                }
                args.vararg_ctid(cts, narg)? // Infer vararg type.
            };
            let d = cts.raw(did);
            let dct = *cts.get(d);
            let mut sz = dct.size;
            let mut isfp = 0;
            let mut onstack = false;

            // Find out how (by value/ref) and where (GPR/FPR) to pass an argument.
            if ctype_isnum(dct.info) {
                if sz > 8 {
                    return Err(nyi().into());
                }
                if dct.info & CTF_FP != 0 {
                    isfp = 1;
                }
            } else if ctype_isvector(dct.info) {
                if sz == 8 || sz == 16 {
                    isfp = 1;
                } else {
                    return Err(nyi().into());
                }
            } else if ctype_isstruct(dct.info) {
                let mut rcl = [0u32; 2];
                if classify_struct(cts, d, &mut rcl, 0) == 0 {
                    cc.regs.nsp = nsp as u8;
                    cc.regs.ngpr = ngpr as u8;
                    cc.regs.nfpr = nfpr as u8;
                    cc.struct_arg(cts, d, &rcl, args, narg)?;
                    nsp = cc.regs.nsp as u32;
                    ngpr = cc.regs.ngpr as usize;
                    nfpr = cc.regs.nfpr as usize;
                    continue;
                } else {
                    onstack = true; // Pass all other structs by value on stack.
                }
            } else if ctype_iscomplex(dct.info) {
                isfp = 2; // Pass complex in FPRs or on stack. Needs postprocessing.
            } else {
                sz = CTSIZE_PTR;
            }
            let n = sz.div_ceil(CTSIZE_PTR) as usize; // Number of GPRs or stack slots needed.

            // Handle register arguments.
            let mut dest = None;
            if isfp != 0 {
                // Try to pass argument in FPRs.
                let n2 = if ctype_isvector(dct.info) { 1 } else { n };
                if nfpr + n2 <= CCALL_NARG_FPR {
                    dest = Some(Dest::Fpr(nfpr));
                    nfpr += n2;
                }
            } else if !onstack && n <= 2 && ngpr + n <= maxgpr {
                // Try to pass argument in GPRs.
                // Note that reordering is explicitly allowed in the x64 ABI.
                dest = Some(Dest::Gpr(ngpr));
                ngpr += n;
            }
            let dest = match dest {
                Some(dest) => dest,
                None => {
                    // Otherwise pass argument on stack. Align argument on stack.
                    let align = (1u32 << ctype_align(dct.info)) - 1;
                    nsp = (nsp + align) & !align;
                    let dp = Dest::Stack(nsp as usize);
                    nsp += n as u32 * CTSIZE_PTR;
                    if nsp > CCALL_SIZE_STACK {
                        // Too many arguments.
                        return Err(nyi().into());
                    }
                    dp
                }
            };

            let size = dct.size as usize;
            args.convert(cts, narg, d, cc.dest_slice(dest, size))?;
            // Extend passed integers to 32 bits at least.
            if ctype_isinteger_or_bool(dct.info) && dct.size < 4 {
                let p = cc.dest_slice(dest, 4);
                let v = match (dct.size, dct.info & CTF_UNSIGNED != 0) {
                    (1, true) => p[0] as u32,
                    (1, false) => p[0] as i8 as i32 as u32,
                    (_, true) => u16::from_ne_bytes([p[0], p[1]]) as u32,
                    (_, false) => i16::from_ne_bytes([p[0], p[1]]) as i32 as u32,
                };
                p.copy_from_slice(&v.to_ne_bytes());
            }
            if isfp == 2 && n == 2 && nfpr >= 2 && dest == Dest::Fpr(nfpr - 2) {
                // Split complex double.
                let base = (nfpr - 2) * 16;
                let (lo, hi) = cc.regs.fpr.split_at_mut(base + 16);
                hi[0..8].copy_from_slice(&lo[base + 8..base + 16]);
                lo[base + 8..base + 16].fill(0);
            }
        }
        if fidx != 0 {
            // Too few arguments.
            return Err(FfiError::caller(errmsg::FFI_NUMARG).into());
        }

        cc.regs.ngpr = ngpr as u8;
        cc.regs.nfpr = nfpr as u8; // Required for vararg functions.
        let nsp = (nsp + CTSIZE_PTR - 1) & !(CTSIZE_PTR - 1);
        cc.regs.nsp = nsp as u8;
        cc.regs.spadj = (CCALL_SPS_FREE + CCALL_SPS_EXTRA) * CTSIZE_PTR;
        if nsp > CCALL_SPS_FREE * CTSIZE_PTR {
            cc.regs.spadj += ((nsp - CCALL_SPS_FREE * CTSIZE_PTR) + 15) & !15;
        }
        Ok(cc)
    }

    fn dest_slice(&mut self, dest: Dest, len: usize) -> &mut [u8] {
        match dest {
            Dest::Fpr(i) => &mut self.regs.fpr[i * 16..i * 16 + len],
            Dest::Gpr(i) => &mut self.regs.gpr[i * 8..i * 8 + len],
            Dest::Stack(ofs) => &mut self.regs.stack[ofs..ofs + len],
        }
    }

    /// Try to split up a small struct into registers. Returns true on
    /// register overflow.
    fn struct_reg(&mut self, dp: &[u8; 16], rcl: &[u32; 2]) -> bool {
        let mut ngpr = self.regs.ngpr as usize;
        let mut nfpr = self.regs.nfpr as usize;
        for i in 0..2 {
            debug_assert!(rcl[i] & CCALL_RCL_MEM == 0, "pass mem struct in reg");
            let word = &dp[i * 8..i * 8 + 8];
            if rcl[i] & CCALL_RCL_INT != 0 {
                // Integer class takes precedence.
                if ngpr >= CCALL_NARG_GPR {
                    return true; // Register overflow.
                }
                self.regs.gpr[ngpr * 8..ngpr * 8 + 8].copy_from_slice(word);
                ngpr += 1;
            } else if rcl[i] & CCALL_RCL_SSE != 0 {
                if nfpr >= CCALL_NARG_FPR {
                    return true; // Register overflow.
                }
                self.regs.fpr[nfpr * 16..nfpr * 16 + 8].copy_from_slice(word);
                nfpr += 1;
            }
        }
        self.regs.ngpr = ngpr as u8;
        self.regs.nfpr = nfpr as u8;
        false
    }

    /// Pass a small struct argument.
    fn struct_arg<A: CCallArgs>(
        &mut self,
        cts: &mut CTState,
        d: CTypeID,
        rcl: &[u32; 2],
        args: &mut A,
        narg: usize,
    ) -> Result<(), A::Error> {
        let mut dp = [0u8; 16];
        let dct = *cts.get(d);
        // Convert to temp. struct.
        args.convert(cts, narg, d, &mut dp[..dct.size as usize])?;
        if self.struct_reg(&dp, rcl) {
            // Register overflow? Pass on stack.
            let mut nsp = self.regs.nsp as u32;
            let sz = if rcl[1] != 0 {
                2 * CTSIZE_PTR
            } else {
                CTSIZE_PTR
            };
            let align = (1u32 << ctype_align(dct.info)) - 1;
            if align > CTSIZE_PTR - 1 {
                nsp = (nsp + align) & !align; // Align argument on stack.
            }
            // Checked after aligning: upstream checks before, which lets a
            // 16-byte aligned struct overrun the stack area by 8 bytes.
            if nsp + sz > CCALL_SIZE_STACK {
                return Err(nyi().into()); // Too many arguments.
            }
            self.regs.nsp = (nsp + sz) as u8;
            let ofs = nsp as usize;
            self.regs.stack[ofs..ofs + sz as usize].copy_from_slice(&dp[..sz as usize]);
        }
        Ok(())
    }

    /// The register/stack image (for inspection).
    pub fn regs(&self) -> &CCallRegs {
        &self.regs
    }

    /// Perform the call (`lj_vm_ffi_call`).
    ///
    /// # Safety
    /// The function pointer must point to a function with the declared C
    /// type, and all pointer arguments must satisfy its requirements.
    pub unsafe fn call(&mut self) {
        // SAFETY: the register image is fully set up and the caller guarantees
        // the callee matches its declaration.
        unsafe { luajit_rs_vm_ffi_call(&mut self.regs) };
    }

    /// Get the results of the call (`ccall_get_results`).
    pub fn result(&mut self, cts: &CTState) -> CCallResult<'_> {
        let ctr = *cts.get(self.rid);
        if ctype_isvoid(ctr.info) {
            return CCallResult::Void;
        }
        let ctid = ctype_cid(cts.get(self.fid).info);
        let size = ctr.size as usize;
        if ctype_isstruct(ctr.info) {
            let ret = self.ret.as_mut().expect("struct result storage");
            if self.regs.retref == 0 {
                // Combine returned small struct.
                let mut rcl = [0u32; 2];
                classify_struct(cts, self.rid, &mut rcl, 0);
                let mut sp = [0u8; 16];
                let (mut ngpr, mut nfpr) = (0, 0);
                for i in 0..2 {
                    if rcl[i] & CCALL_RCL_INT != 0 {
                        // Integer class takes precedence.
                        sp[i * 8..i * 8 + 8]
                            .copy_from_slice(&self.regs.gpr[ngpr * 8..ngpr * 8 + 8]);
                        ngpr += 1;
                    } else if rcl[i] & CCALL_RCL_SSE != 0 {
                        sp[i * 8..i * 8 + 8]
                            .copy_from_slice(&self.regs.fpr[nfpr * 16..nfpr * 16 + 8]);
                        nfpr += 1;
                    }
                }
                ret.bytes_mut(size).copy_from_slice(&sp[..size]);
            }
            return CCallResult::Aggregate {
                ctid,
                bytes: ret.bytes(size),
            };
        }
        if ctype_iscomplex(ctr.info) {
            let ret = self.ret.as_mut().expect("complex result storage");
            let dp = ret.bytes_mut(size);
            if size == 8 {
                // Copy complex float from FPR.
                dp.copy_from_slice(&self.regs.fpr[0..8]);
            } else {
                // Copy non-contiguous complex double from FPRs.
                dp[0..8].copy_from_slice(&self.regs.fpr[0..8]);
                dp[8..16].copy_from_slice(&self.regs.fpr[16..24]);
            }
            return CCallResult::Aggregate {
                ctid,
                bytes: ret.bytes(size),
            };
        }
        debug_assert!(!ctype_isrefarray(ctr.info), "unexpected reference ctype");
        let bytes = if ctype_isfp(ctr.info) || ctype_isvector(ctr.info) {
            &self.regs.fpr[..size.min(16)]
        } else {
            &self.regs.gpr[..size.min(16)]
        };
        CCallResult::Scalar {
            ctid: self.rid,
            bytes,
        }
    }
}
