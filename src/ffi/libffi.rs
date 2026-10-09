//! The `ffi` library. Port of lib_ffi.c (library functions, the shared cdata metatable, C
//! library namespaces and callback methods) plus the Lua-facing parts of lj_ccall.c
//! (`lj_ccall_func`), lj_ccallback.c (callback entry/exit) and lj_clib.c (namespace objects
//! and their symbol cache).

use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::ptr;
use std::rc::Rc;

use crate::bc::MM;
use crate::cdata::CData;
use crate::error::{LuaError, LuaResult};
use crate::func::{FF_C, NativeFn, NativeImpl};
use crate::gc::Gc;
use crate::state::Vm;
use crate::table::Table;
use crate::udata::{UDTYPE_FFI_CLIB, Userdata};
use crate::value::{VType, Value};
use crate::vm::ffid::*;
use crate::vm::{ErrMsg, Meta};

use super::callback::CallbackFrame;
use super::carith::{self, ctype_meta};
use super::ccall::{self, CCallArgs, CCallResult, CCallState};
use super::cconv::{self, CCF_CAST, CCF_IGNQUAL, ccf_arg, cdata_new, ffi_err, msgf, rd, wr};
use super::cdata::{cdata_get, cdata_index, cdata_isv, cdata_newx, cdata_set, cdata_setfin};
use super::clib::{CLibrary, ClibError, ClibValue};
use super::cparse::{self, CParam};
use super::ctype::*;
use super::state::{FfiState, enter_vm, state};
use super::{FfiError, errmsg};

// -- C type checks -----------------------------------------------------------------------------

/// Build the type parameters of `ffi.cdef`/`ffi.typeof` from arguments `start..`.
fn params_of<'a>(cts: &CTState, vals: &'a [Value], start: usize) -> Vec<CParam<'a>> {
    vals.iter()
        .enumerate()
        .map(|(i, v)| match v {
            Value::Num(n) => CParam::Num(crate::vm::num2int(*n)),
            Value::Str(s) => CParam::Str(s.as_bytes()),
            Value::CData(cd) => {
                if cd.ctypeid() == CTID_CTYPEID {
                    // SAFETY: ctype cdata hold a CTypeID.
                    CParam::CType(unsafe { rd::<CTypeID>(cd.ptr()) })
                } else {
                    CParam::CType(cd.ctypeid())
                }
            }
            _ => {
                let _ = cts;
                CParam::Other((start + i) as i32)
            }
        })
        .collect()
}

/// Arguments `start..=nargs`.
fn args_from(vm: &Vm, start: usize) -> Vec<Value> {
    (start..=vm.nargs()).map(|i| vm.arg(i)).collect()
}

/// Check the first argument for a C type and return its id (`ffi_checkctype`). `param` is
/// the first argument used as type parameter (`None`: no parameters allowed).
fn checkctype(vm: &mut Vm, cts: &mut CTState, param: Option<usize>) -> LuaResult<CTypeID> {
    if vm.nargs() < 1 {
        return Err(vm.type_error(1, "C type"));
    }
    match vm.arg(1) {
        Value::Str(s) => {
            // Parse an abstract C type declaration.
            let vals = param.map(|p| args_from(vm, p));
            let params = vals.as_ref().map(|v| params_of(cts, v, param.unwrap_or(0)));
            let mode = cparse::CPARSE_MODE_ABSTRACT | cparse::CPARSE_MODE_NOIMPLICIT;
            cparse::cparse(cts, s.as_bytes(), mode, params.as_deref()).map_err(|e| ffi_err(vm, e))
        }
        Value::CData(cd) => {
            if let Some(p) = param
                && p <= vm.nargs()
            {
                return Err(vm.arg_error(1, errmsg::FFI_NUMPARAM.as_bytes()));
            }
            Ok(if cd.ctypeid() == CTID_CTYPEID {
                // SAFETY: ctype cdata hold a CTypeID.
                unsafe { rd::<CTypeID>(cd.ptr()) }
            } else {
                cd.ctypeid()
            })
        }
        _ => Err(vm.type_error(1, "C type")),
    }
}

/// Check argument for C data and return it (`ffi_checkcdata`).
fn checkcdata(vm: &mut Vm, narg: usize) -> LuaResult<Gc<CData>> {
    match vm.arg(narg) {
        Value::CData(cd) if narg <= vm.nargs() => Ok(cd),
        _ => Err(vm.type_error(narg as i32, "cdata")),
    }
}

/// Convert argument to C pointer (`ffi_checkptr`).
fn checkptr(vm: &mut Vm, cts: &mut CTState, narg: usize, id: CTypeID) -> LuaResult<*mut u8> {
    if narg > vm.nargs() {
        return Err(vm.arg_error_em(narg as i32, ErrMsg::NOVAL));
    }
    let o = vm.arg(narg);
    let mut p: *mut u8 = ptr::null_mut();
    cconv::ct_tv(vm, cts, id, ptr::from_mut(&mut p).cast(), o, ccf_arg(narg as u32))?;
    Ok(p)
}

/// Convert argument to int32_t (`ffi_checkint`).
fn checkint(vm: &mut Vm, cts: &mut CTState, narg: usize) -> LuaResult<i32> {
    if narg > vm.nargs() {
        return Err(vm.arg_error_em(narg as i32, ErrMsg::NOVAL));
    }
    let o = vm.arg(narg);
    let mut i: i32 = 0;
    cconv::ct_tv(vm, cts, CTID_INT32, ptr::from_mut(&mut i).cast(), o, ccf_arg(narg as u32))?;
    Ok(i)
}

/// New `ctype` cdata holding `id`.
fn new_ctypeid(vm: &mut Vm, id: CTypeID) -> Value {
    let cd = cdata_new(vm, CTID_CTYPEID, 4);
    // SAFETY: the cdata owns 4 bytes.
    unsafe { wr::<CTypeID>(cd.ptr(), id) };
    Value::CData(cd)
}

/// Tail-call `f` with `args` replacing the native's arguments.
fn tailcall_with(vm: &mut Vm, f: Value, args: &[Value]) -> LuaResult<usize> {
    vm.set_top(args.len());
    for (i, &a) in args.iter().enumerate() {
        vm.set_arg(i + 1, a);
    }
    vm.tailcall(f)
}

// -- C type metamethods ------------------------------------------------------------------------

/// Handle ctype `__index`/`__newindex` metamethods (`ffi_index_meta`).
fn index_meta(vm: &mut Vm, st: &FfiState, id: CTypeID, mm: MM) -> LuaResult<usize> {
    let tv = ctype_meta(vm, &st.cts.borrow(), id, mm);
    let key = vm.arg(2);
    let err_index = |vm: &mut Vm| -> LuaError {
        let cts = st.cts.borrow();
        let s = cts.repr(id, None);
        let m = match key {
            Value::Str(k) => msgf(errmsg::FFI_BADMEMBER, &[&s, k.as_bytes()]),
            Value::CData(cd) => msgf(errmsg::FFI_BADIDXW, &[&s, &cts.repr(cd.ctypeid(), None)]),
            _ => msgf(errmsg::FFI_BADIDXW, &[&s, key.type_name().as_bytes()]),
        };
        drop(cts);
        vm.err_callermsg(&m)
    };
    let Some(tv) = tv else { return Err(err_index(vm)) };
    if let Value::Func(_) = tv {
        return vm.tailcall(tv);
    }
    if mm == MM::Index {
        match vm.meta_tget(tv, key, None)? {
            Meta::Done(v) => {
                if v.is_nil() {
                    return Err(err_index(vm));
                }
                vm.ret1(v)
            }
            Meta::Call { mo, a, b, .. } => tailcall_with(vm, mo, &[a, b]),
        }
    } else {
        let v = vm.arg(3);
        match vm.meta_tset(tv, key, v, None)? {
            Meta::Done(()) => Ok(0),
            Meta::Call { mo, a, b, .. } => tailcall_with(vm, mo, &[a, b, v]),
        }
    }
}

fn ffi_meta_index(vm: &mut Vm) -> LuaResult<usize> {
    let o = vm.arg(1);
    let Value::CData(cd) = o else { return Err(vm.type_error(1, "cdata")) };
    if vm.nargs() < 2 {
        return Err(vm.type_error(1, "cdata"));
    }
    let st = state(vm);
    let key = vm.arg(2);
    let (ct, p, qual) = cdata_index(vm, &st.cts.borrow(), cd, key)?;
    if qual & 1 != 0 {
        return index_meta(vm, &st, ct, MM::Index);
    }
    let v = cdata_get(vm, &mut st.cts.borrow_mut(), ct, p)?;
    vm.ret1(v)
}

fn ffi_meta_newindex(vm: &mut Vm) -> LuaResult<usize> {
    let o = vm.arg(1);
    let Value::CData(cd) = o else { return Err(vm.type_error(1, "cdata")) };
    if vm.nargs() < 3 {
        return Err(vm.type_error(1, "cdata"));
    }
    let st = state(vm);
    let key = vm.arg(2);
    let (ct, p, qual) = cdata_index(vm, &st.cts.borrow(), cd, key)?;
    if qual & 1 != 0 {
        if qual & CTF_CONST != 0 {
            return Err(vm.err_callermsg(errmsg::FFI_WRCONST.as_bytes()));
        }
        return index_meta(vm, &st, ct, MM::Newindex);
    }
    let v = vm.arg(3);
    cdata_set(vm, &mut st.cts.borrow_mut(), ct, p, v, qual)?;
    Ok(0)
}

macro_rules! arith_mm {
    ($($name:ident => $mm:ident),* $(,)?) => {
        $(fn $name(vm: &mut Vm) -> LuaResult<usize> {
            carith::carith_op(vm, MM::$mm)
        })*
    };
}

arith_mm! {
    ffi_meta_eq => Eq, ffi_meta_len => Len, ffi_meta_lt => Lt, ffi_meta_le => Le,
    ffi_meta_concat => Concat, ffi_meta_add => Add, ffi_meta_sub => Sub, ffi_meta_mul => Mul,
    ffi_meta_div => Div, ffi_meta_mod => Mod, ffi_meta_pow => Pow, ffi_meta_unm => Unm,
}

fn ffi_meta_call(vm: &mut Vm) -> LuaResult<usize> {
    let cd = checkcdata(vm, 1)?;
    let st = state(vm);
    let mut id = cd.ctypeid();
    let mut mm = MM::Call;
    if id == CTID_CTYPEID {
        // SAFETY: ctype cdata hold a CTypeID.
        id = unsafe { rd::<CTypeID>(cd.ptr()) };
        mm = MM::New;
    } else if let Some(n) = ccall_func(vm, &st, cd)? {
        return Ok(n);
    }
    // Handle ctype __call/__new metamethod.
    let tv = {
        let cts = st.cts.borrow();
        let ct = cts.raw(id);
        if ctype_isptr(cts.get(ct).info) {
            id = ctype_cid(cts.get(ct).info);
        }
        ctype_meta(vm, &cts, id, mm)
    };
    if let Some(tv) = tv {
        return vm.tailcall(tv);
    }
    if mm == MM::Call {
        let r = st.cts.borrow().repr(id, None);
        return Err(vm.err_callermsg(&msgf(errmsg::FFI_BADCALL, &[&r])));
    }
    ffi_new(vm)
}

fn ffi_meta_tostring(vm: &mut Vm) -> LuaResult<usize> {
    let cd = checkcdata(vm, 1)?;
    let st = state(vm);
    let cts = st.cts.borrow();
    let mut id = cd.ctypeid();
    let mut p = cd.ptr() as usize;
    let mut out = Vec::with_capacity(64);
    if id == CTID_CTYPEID {
        // SAFETY: ctype cdata hold a CTypeID.
        id = unsafe { rd::<CTypeID>(cd.ptr()) };
        out.extend_from_slice(b"ctype<");
        out.extend_from_slice(&cts.repr(id, None));
        out.push(b'>');
    } else {
        let mut ct = cts.raw(id);
        let mut enum_val: Option<i32> = None;
        if ctype_isref(cts.get(ct).info) {
            // SAFETY: reference cdata hold a pointer.
            p = unsafe { rd::<usize>(p as *const u8) };
            ct = cts.rawchild(ct);
        }
        let info = cts.get(ct).info;
        let size = cts.get(ct).size;
        if ctype_iscomplex(info) || (size == 8 && ctype_isinteger(info)) {
            // Upstream reads the payload of the cdata itself here.
            let mut buf = [0u8; 16];
            let n = cd.size().min(16);
            // SAFETY: the cdata owns at least n bytes.
            unsafe { ptr::copy_nonoverlapping(cd.ptr(), buf.as_mut_ptr(), n) };
            let s = if ctype_iscomplex(info) {
                repr_complex(&buf, size)
            } else {
                repr_int64(u64::from_ne_bytes(buf[..8].try_into().expect("8 bytes")), info & CTF_UNSIGNED != 0)
            };
            drop(cts);
            vm.push_str(&s);
            return Ok(1);
        } else if ctype_isfunc(info) {
            // SAFETY: function cdata hold the function pointer.
            p = unsafe { rd::<usize>(p as *const u8) };
        } else if ctype_isenum(info) {
            // SAFETY: enum cdata hold 4 bytes.
            enum_val = Some(unsafe { rd::<u32>(p as *const u8) } as i32);
        } else {
            if ctype_isptr(info) {
                // SAFETY: pointer cdata hold a pointer of their size.
                p = unsafe { cconv::getptr(p as *const u8, size) } as usize;
                ct = cts.rawchild(ct);
            }
            let info = cts.get(ct).info;
            if ctype_isstruct(info) || ctype_isvector(info) {
                // Handle ctype __tostring metamethod.
                if let Some(tv) = ctype_meta(vm, &cts, ct, MM::Tostring) {
                    drop(cts);
                    return vm.tailcall(tv);
                }
            }
        }
        out.extend_from_slice(b"cdata<");
        out.extend_from_slice(&cts.repr(id, None));
        out.extend_from_slice(b">: ");
        match enum_val {
            Some(v) => out.extend_from_slice(v.to_string().as_bytes()),
            None => crate::text::strfmt::put_ptr(&mut out, p),
        }
    }
    drop(cts);
    vm.push_str(&out);
    Ok(1)
}

fn ffi_pairs(vm: &mut Vm, mm: MM) -> LuaResult<usize> {
    let cd = checkcdata(vm, 1)?;
    let st = state(vm);
    let cts = st.cts.borrow();
    let mut id = cd.ctypeid();
    let ct = cts.raw(id);
    if ctype_isptr(cts.get(ct).info) {
        id = ctype_cid(cts.get(ct).info);
    }
    match ctype_meta(vm, &cts, id, mm) {
        Some(tv) => {
            drop(cts);
            vm.tailcall(tv)
        }
        None => {
            let m = msgf(errmsg::FFI_BADMM, &[&cts.repr(id, None), mm.name().as_bytes()]);
            drop(cts);
            Err(vm.err_callermsg(&m))
        }
    }
}

fn ffi_meta_pairs(vm: &mut Vm) -> LuaResult<usize> {
    ffi_pairs(vm, MM::Pairs)
}

fn ffi_meta_ipairs(vm: &mut Vm) -> LuaResult<usize> {
    ffi_pairs(vm, MM::Ipairs)
}

// -- C calls -----------------------------------------------------------------------------------

/// Error of an argument conversion during call setup.
enum CallErr {
    Lua(LuaError),
    Ffi(FfiError),
}

impl From<FfiError> for CallErr {
    fn from(e: FfiError) -> CallErr {
        CallErr::Ffi(e)
    }
}

/// The Lua arguments of a C call: arguments 2.. of the `__call` metamethod.
struct LuaArgs<'v> {
    vm: &'v mut Vm,
}

impl CCallArgs for LuaArgs<'_> {
    type Error = CallErr;

    fn count(&self) -> usize {
        self.vm.nargs().saturating_sub(1)
    }

    fn vararg_ctid(&mut self, cts: &mut CTState, narg: usize) -> Result<CTypeID, CallErr> {
        Ok(match self.vm.arg(narg + 1) {
            Value::Num(_) => ccall::CTID_VARARG_NUMBER,
            Value::CData(cd) => ccall::ctid_vararg_cdata(cts, cd.ctypeid())?,
            Value::Str(_) => ccall::CTID_VARARG_STRING,
            Value::Bool(_) => ccall::CTID_VARARG_BOOL,
            _ => ccall::CTID_VARARG_OTHER,
        })
    }

    fn convert(&mut self, cts: &mut CTState, narg: usize, did: CTypeID, dst: &mut [u8]) -> Result<(), CallErr> {
        let o = self.vm.arg(narg + 1);
        cconv::ct_tv(self.vm, cts, did, dst.as_mut_ptr(), o, ccf_arg(narg as u32)).map_err(CallErr::Lua)
    }
}

/// A Lua error unwinding from a callback through C frames to the C call that entered C.
struct CallbackUnwind(LuaError);

// SAFETY: the payload never leaves the thread it was raised on (it is caught by the
// `catch_unwind` around the C call on the same thread).
unsafe impl Send for CallbackUnwind {}

/// Call a C function (`lj_ccall_func`): `Ok(None)` if the cdata is not callable.
fn ccall_func(vm: &mut Vm, st: &Rc<FfiState>, cd: Gc<CData>) -> LuaResult<Option<usize>> {
    let (fid, sz) = match ccall::func_type(&st.cts.borrow(), cd.ctypeid()) {
        Some(x) => x,
        None => return Ok(None),
    };
    // SAFETY: function/pointer cdata hold a pointer of size `sz`.
    let fnptr = unsafe { cconv::getptr(cd.ptr(), sz) } as *const std::ffi::c_void;
    let setup = {
        let mut cts = st.cts.borrow_mut();
        let mut args = LuaArgs { vm };
        let r = CCallState::new(&mut cts, fid, fnptr, &mut args);
        cts.cb.set_slot(!0);
        r
    };
    let mut cc = match setup {
        Ok(cc) => cc,
        Err(CallErr::Lua(e)) => return Err(e),
        Err(CallErr::Ffi(e)) => return Err(ffi_err(vm, e)),
    };
    let r = {
        let _guard = enter_vm(vm);
        // SAFETY: the function pointer comes from a declared C function or a cast pointer,
        // exactly as upstream calls it.
        catch_unwind(AssertUnwindSafe(|| unsafe { cc.call() }))
    };
    if let Err(payload) = r {
        return match payload.downcast::<CallbackUnwind>() {
            Ok(e) => Err(e.0),
            Err(payload) => resume_unwind(payload),
        };
    }
    let mut cts = st.cts.borrow_mut();
    let (res, ctid, buf) = match cc.result(&cts) {
        CCallResult::Void => return Ok(Some(0)),
        CCallResult::Aggregate { ctid, bytes } => {
            let cdr = cdata_new(vm, ctid, bytes.len() as CTSize);
            // SAFETY: the new cdata owns bytes.len() bytes.
            unsafe { ptr::copy_nonoverlapping(bytes.as_ptr(), cdr.ptr(), bytes.len()) };
            (Some(Value::CData(cdr)), 0, [0u8; 16])
        }
        CCallResult::Scalar { ctid, bytes } => {
            let mut buf = [0u8; 16];
            buf[..bytes.len()].copy_from_slice(bytes);
            (None, ctid, buf)
        }
    };
    let v = match res {
        Some(v) => v,
        None => cconv::tv_ct(vm, &mut cts, ctid, 0, buf.as_ptr())?,
    };
    drop(cts);
    vm.push(v);
    Ok(Some(1))
}

/// `LUA_MINSTACK`.
const LUA_MINSTACK: usize = 20;

// -- Callbacks ---------------------------------------------------------------------------------

/// Handler for all callback slots (`lj_ccallback_enter` + `lj_ccallback_leave`). Lua errors
/// unwind through the C frames to the C call that is running on this thread.
pub(crate) fn callback_handler(frame: &mut CallbackFrame) {
    let vmp = super::state::current_vm();
    if vmp.is_null() {
        // No VM is running a C call on this thread: nothing to run the callback on.
        eprintln!("{}", errmsg::FFI_BADCBACK);
        std::process::exit(libc::EXIT_FAILURE);
    }
    // SAFETY: the pointer was registered by `ccall_func` from its `&mut Vm` right before
    // entering C, and that reference is not used until the C call returns.
    let vm = unsafe { &mut *vmp };
    if let Err(e) = callback_run(vm, frame) {
        resume_unwind(Box::new(CallbackUnwind(e)));
    }
}

fn callback_run(vm: &mut Vm, frame: &mut CallbackFrame) -> LuaResult<()> {
    let st = state(vm);
    let slot = frame.slot;
    let id = st.cts.borrow().cb.cbid(slot);
    if id == 0 {
        return Err(vm.err_callermsg(errmsg::FFI_BADCBACK.as_bytes()));
    }
    let f = st.miscmap.get_int(slot as i32);
    vm.check_stack(LUA_MINSTACK + 2, None)?;
    vm.push(f);
    let mut nargs = 0;
    {
        let mut cts = st.cts.borrow_mut();
        let args = frame.args(&cts, id);
        for a in args {
            let v = cconv::tv_ct(vm, &mut cts, a.ctid, 0, a.ptr)?;
            vm.push(v);
            nargs += 1;
        }
    }
    vm.call(nargs, 1)?;
    let o = vm.pop();
    let mut cts = st.cts.borrow_mut();
    let rid = ctype_cid(cts.get(id).info);
    let ctr = cts.raw(rid);
    if !ctype_isvoid(cts.get(ctr).info) {
        let mut buf = [0u8; 16];
        if let Err(e) = cconv::ct_tv(vm, &mut cts, ctr, buf.as_mut_ptr(), o, cconv::CCF_NOLOC) {
            // The PC of the return is lost: point to the last line of the callback function.
            drop(cts);
            return Err(match (f, e.value) {
                (Value::Func(func), Value::Str(m)) if func.proto().is_some() => {
                    let pt = func.proto().expect("Lua function");
                    let mut out = crate::vm::debug::shortname(pt.chunkname.as_bytes(), pt.firstline);
                    out.extend_from_slice(format!(":{}: ", pt.firstline + pt.numline).as_bytes());
                    out.extend_from_slice(m.as_bytes());
                    vm.error_msg_bytes(&out)
                }
                _ => e,
            });
        }
        frame.set_result(&cts, rid, &buf);
    }
    cts.cb.set_slot(0); // Blacklist C function that called the callback.
    Ok(())
}

/// `ffi_callback_set`: change (`fn` = Some) or free the Lua function of a callback.
fn callback_set(vm: &mut Vm, f: Option<Value>) -> LuaResult<usize> {
    let cd = checkcdata(vm, 1)?;
    let st = state(vm);
    let mut cts = st.cts.borrow_mut();
    let ct = cts.raw(cd.ctypeid());
    if ctype_isptr(cts.get(ct).info) && cts.get(ct).size == 8 {
        // SAFETY: 64 bit pointer cdata hold a pointer.
        let p = unsafe { rd::<*const std::ffi::c_void>(cd.ptr()) };
        let slot = cts.cb.ptr2slot(p);
        if slot < cts.cb.sizeid() && cts.cb.cbid(slot) != 0 {
            match f {
                Some(f) => st.miscmap.set_int(slot as i32, f).map_err(|e| vm.tab_error(e))?,
                None => {
                    st.miscmap.set_int(slot as i32, Value::Nil).map_err(|e| vm.tab_error(e))?;
                    cts.cb.free(slot);
                }
            }
            return Ok(0);
        }
    }
    drop(cts);
    Err(vm.err_callermsg(errmsg::FFI_BADCBACK.as_bytes()))
}

fn ffi_callback_free(vm: &mut Vm) -> LuaResult<usize> {
    callback_set(vm, None)
}

fn ffi_callback_set(vm: &mut Vm) -> LuaResult<usize> {
    let f = vm.check_func(2)?;
    callback_set(vm, Some(Value::Func(f)))
}

// -- C library namespaces ----------------------------------------------------------------------

/// Payload of a C library namespace userdata (`CLibrary`); the symbol cache is the userdata's
/// environment table.
struct ClibUd {
    lib: std::cell::RefCell<CLibrary>,
}

impl crate::gc::Trace for ClibUd {
    fn trace(&self, _m: &mut crate::gc::Marker) {}
    fn size(&self) -> usize {
        std::mem::size_of::<ClibUd>()
    }
}

/// Create a C library namespace object (`clib_new`).
fn clib_new(vm: &mut Vm, lib: CLibrary, mt: Gc<Table>) -> Gc<Userdata> {
    let cache = vm.empty_table();
    let ud = vm.new_udata_ext(Box::new(ClibUd { lib: std::cell::RefCell::new(lib) }), UDTYPE_FFI_CLIB, cache);
    ud.set_metatable(Some(mt));
    ud
}

/// Index a C library by name (`ffi_clib_index` + `lj_clib_index`).
fn clib_index(vm: &mut Vm) -> LuaResult<Value> {
    let ud = match vm.arg(1) {
        Value::Udata(u) if vm.nargs() >= 1 && u.udtype() == UDTYPE_FFI_CLIB => u,
        _ => return Err(vm.type_error(1, "userdata")),
    };
    let name = match vm.arg(2) {
        Value::Str(s) if vm.nargs() >= 2 => s,
        _ => return Err(vm.type_error(2, "string")),
    };
    let cache = ud.env();
    let tv = cache.get_str(name);
    if !tv.is_nil() {
        return Ok(tv);
    }
    let st = state(vm);
    let r = {
        let cts = st.cts.borrow();
        let cl = ud.ext::<ClibUd>().expect("clib userdata").lib.borrow();
        cl.index(&cts, name.as_bytes())
    };
    let v = match r {
        Ok(ClibValue::Int(i)) => Value::Num(i as f64),
        Ok(ClibValue::Num(n)) => Value::Num(n),
        Ok(ClibValue::Symbol { ctid, ptr: p }) => {
            let cd = cdata_new(vm, ctid, CTSIZE_PTR);
            // SAFETY: the cdata owns 8 bytes.
            unsafe { wr::<*mut std::ffi::c_void>(cd.ptr(), p) };
            Value::CData(cd)
        }
        Err(ClibError::NoDecl) => {
            return Err(vm.err_callermsg(&msgf(errmsg::FFI_NODECL, &[name.as_bytes()])));
        }
        Err(ClibError::Dl(e)) => return Err(ffi_err(vm, e)),
    };
    cache.set_str(name, v).map_err(|e| vm.tab_error(e))?;
    Ok(v)
}

fn ffi_clib_index(vm: &mut Vm) -> LuaResult<usize> {
    let tv = clib_index(vm)?;
    if let Value::CData(cd) = tv {
        let st = state(vm);
        let mut cts = st.cts.borrow_mut();
        let s = *cts.get(cd.ctypeid());
        if ctype_isextern(s.info) {
            let sid = ctype_cid(s.info);
            // SAFETY: extern cdata hold the symbol address.
            let sp = unsafe { rd::<*const u8>(cd.ptr()) };
            let ct = cts.raw(sid);
            let v = cconv::tv_ct(vm, &mut cts, ct, sid, sp)?;
            drop(cts);
            return vm.ret1(v);
        }
    }
    vm.ret1(tv)
}

fn ffi_clib_newindex(vm: &mut Vm) -> LuaResult<usize> {
    let tv = clib_index(vm)?;
    if vm.nargs() >= 3
        && let Value::CData(cd) = tv
    {
        let st = state(vm);
        let mut cts = st.cts.borrow_mut();
        let mut d = cd.ctypeid();
        if ctype_isextern(cts.get(d).info) {
            let mut qual = 0;
            loop {
                // Skip attributes and collect qualifiers.
                d = cts.child(d);
                let ct = cts.get(d);
                if !ctype_isattrib(ct.info) {
                    break;
                }
                if ctype_attrib(ct.info) == CTA_QUAL {
                    qual |= ct.size;
                }
            }
            if (cts.get(d).info | qual) & CTF_CONST == 0 {
                // SAFETY: extern cdata hold the symbol address.
                let dp = unsafe { rd::<*mut u8>(cd.ptr()) };
                let o = vm.arg(3);
                cconv::ct_tv(vm, &mut cts, d, dp, o, 0)?;
                return Ok(0);
            }
        }
    }
    Err(vm.err_callermsg(errmsg::FFI_WRCONST.as_bytes()))
}

fn ffi_clib_gc(vm: &mut Vm) -> LuaResult<usize> {
    if let Value::Udata(u) = vm.arg(1)
        && vm.nargs() >= 1
        && u.udtype() == UDTYPE_FFI_CLIB
        && let Some(c) = u.ext::<ClibUd>()
    {
        c.lib.borrow_mut().unload();
    }
    Ok(0)
}

// -- FFI library functions ---------------------------------------------------------------------

fn ffi_cdef(vm: &mut Vm) -> LuaResult<usize> {
    let s = vm.check_str(1)?;
    let st = state(vm);
    let mut cts = st.cts.borrow_mut();
    let vals = args_from(vm, 2);
    let params = params_of(&cts, &vals, 2);
    let mode = cparse::CPARSE_MODE_MULTI | cparse::CPARSE_MODE_DIRECT;
    let r = cparse::cparse(&mut cts, s.as_bytes(), mode, Some(&params));
    drop(params);
    drop(cts);
    r.map_err(|e| ffi_err(vm, e))?;
    Ok(0)
}

fn ffi_new(vm: &mut Vm) -> LuaResult<usize> {
    let st = state(vm);
    let mut cts = st.cts.borrow_mut();
    let id = checkctype(vm, &mut cts, None)?;
    let ct = cts.raw(id);
    let (info, mut sz) = cts.info(id);
    let mut o = 2;
    if info & CTF_VLA != 0 {
        o += 1;
        let n = checkint(vm, &mut cts, 2)?;
        sz = cts.vlsize(ct, n as CTSize);
    }
    if sz == CTSIZE_INVALID {
        return Err(vm.arg_error(1, errmsg::FFI_INVSIZE.as_bytes()));
    }
    let init: Vec<Value> = (o..=vm.nargs()).map(|i| vm.arg(i)).collect();
    let cd = cdata_newx(vm, id, sz, info);
    vm.push(Value::CData(cd)); // Anchor the uninitialized cdata.
    cconv::ct_init(vm, &mut cts, ct, sz, cd.ptr(), &init)?;
    if ctype_isstruct(cts.get(ct).info) {
        // Handle ctype __gc metamethod. Use the fast lookup here.
        if let Value::Table(mt) = st.miscmap.get(Value::Num(-(id as i32) as f64))
            && let Some(fin) = vm.fast_mm(Some(mt), MM::Gc)
            && let Some(t) = vm.ffi_fin
            && t.metatable().is_some()
        {
            // Add to finalizer table, if still enabled.
            t.set(Value::CData(cd), fin).map_err(|e| vm.tab_error(e))?;
            cd.set_has_finalizer(true);
        }
    }
    Ok(1)
}

fn ffi_cast(vm: &mut Vm) -> LuaResult<usize> {
    let st = state(vm);
    let mut cts = st.cts.borrow_mut();
    let id = checkctype(vm, &mut cts, None)?;
    let d = cts.raw(id);
    let o = vm.check_any(2)?;
    let dinfo = cts.get(d).info;
    if !(ctype_isnum(dinfo) || ctype_isptr(dinfo) || ctype_isenum(dinfo)) {
        return Err(vm.arg_error(1, errmsg::FFI_INVTYPE.as_bytes()));
    }
    if let Value::CData(cd) = o
        && cd.ctypeid() == id
    {
        return vm.ret1(o);
    }
    let cd = cdata_new(vm, id, cts.get(d).size);
    cconv::ct_tv(vm, &mut cts, d, cd.ptr(), o, CCF_CAST)?;
    vm.ret1(Value::CData(cd))
}

fn ffi_typeof(vm: &mut Vm) -> LuaResult<usize> {
    let st = state(vm);
    let id = checkctype(vm, &mut st.cts.borrow_mut(), Some(2))?;
    let v = new_ctypeid(vm, id);
    vm.ret1(v)
}

/// Internal and unsupported API.
fn ffi_typeinfo(vm: &mut Vm) -> LuaResult<usize> {
    let st = state(vm);
    let mut cts = st.cts.borrow_mut();
    let id = checkint(vm, &mut cts, 1)? as CTypeID;
    if id > 0 && id < cts.top() {
        let ct = *cts.get(id);
        let t = vm.new_table_ah(0, 4)?;
        let set = |vm: &mut Vm, k: &[u8], v: Value| {
            let k = vm.new_str(k);
            t.set_str(k, v).expect("string key");
        };
        set(vm, b"info", Value::Num(ct.info as i32 as f64));
        if ct.size != CTSIZE_INVALID {
            set(vm, b"size", Value::Num(ct.size as i32 as f64));
        }
        if ct.sib != 0 {
            set(vm, b"sib", Value::Num(ct.sib as f64));
        }
        if ct.name.is_some() {
            let n = vm.new_str(cts.name_str(ct.name));
            set(vm, b"name", Value::Str(n));
        }
        return vm.ret1(Value::Table(t));
    }
    Ok(0)
}

fn ffi_istype(vm: &mut Vm) -> LuaResult<usize> {
    let st = state(vm);
    let cts = &mut *st.cts.borrow_mut();
    let id1 = checkctype(vm, cts, None)?;
    let o = vm.check_any(2)?;
    let mut b = false;
    if let Value::CData(cd) = o {
        let id2 = if cd.ctypeid() == CTID_CTYPEID {
            // SAFETY: ctype cdata hold a CTypeID.
            unsafe { rd::<CTypeID>(cd.ptr()) }
        } else {
            cd.ctypeid()
        };
        let ct1 = cts.rawref(id1);
        let ct2 = cts.rawref(id2);
        let (c1, c2) = (*cts.get(ct1), *cts.get(ct2));
        if ct1 == ct2 {
            b = true;
        } else if ctype_type(c1.info) == ctype_type(c2.info) && c1.size == c2.size {
            if ctype_ispointer(c1.info) {
                b = cconv::compatptr(cts, ct1, ct2, CCF_IGNQUAL);
            } else if ctype_isnum(c1.info) || ctype_isvoid(c1.info) {
                b = ((c1.info ^ c2.info) & !(CTF_QUAL | CTF_LONG)) == 0;
            }
        } else if ctype_isstruct(c1.info) && ctype_isptr(c2.info) && ct1 == cts.rawchild(ct2) {
            b = true;
        }
    }
    vm.ret1(Value::Bool(b))
}

fn ffi_sizeof(vm: &mut Vm) -> LuaResult<usize> {
    let st = state(vm);
    let mut cts = st.cts.borrow_mut();
    let id = checkctype(vm, &mut cts, None)?;
    let sz = match vm.arg(1) {
        Value::CData(cd) if cdata_isv(&cts, cd) => cd.size() as CTSize,
        _ => {
            let ct = cts.rawref(id);
            let info = cts.get(ct).info;
            let sz = if ctype_isvltype(info) {
                let n = checkint(vm, &mut cts, 2)?;
                cts.vlsize(ct, n as CTSize)
            } else if ctype_hassize(info) {
                cts.get(ct).size
            } else {
                CTSIZE_INVALID
            };
            if sz == CTSIZE_INVALID {
                return vm.ret1(Value::Nil);
            }
            sz
        }
    };
    vm.ret1(Value::Num(sz as i32 as f64))
}

fn ffi_alignof(vm: &mut Vm) -> LuaResult<usize> {
    let st = state(vm);
    let mut cts = st.cts.borrow_mut();
    let id = checkctype(vm, &mut cts, None)?;
    let (info, _) = cts.info_raw(id);
    vm.ret1(Value::Num((1u32 << ctype_align(info)) as f64))
}

fn ffi_offsetof(vm: &mut Vm) -> LuaResult<usize> {
    let st = state(vm);
    let mut cts = st.cts.borrow_mut();
    let id = checkctype(vm, &mut cts, None)?;
    let name = vm.check_str(2)?;
    let ct = cts.rawref(id);
    let c = *cts.get(ct);
    if ctype_isstruct(c.info)
        && c.size != CTSIZE_INVALID
        && let Some((fct, ofs)) = cts.getfield(ct, name.as_bytes())
    {
        let finfo = cts.get(fct).info;
        if ctype_isfield(finfo) {
            return vm.ret1(Value::Num(ofs as f64));
        } else if ctype_isbitfield(finfo) {
            vm.push(Value::Num(ofs as f64));
            vm.push(Value::Num(ctype_bitpos(finfo) as f64));
            vm.push(Value::Num(ctype_bitbsz(finfo) as f64));
            return Ok(3);
        }
    }
    Ok(0)
}

fn ffi_errno(vm: &mut Vm) -> LuaResult<usize> {
    let err = ccall::errno();
    if vm.nargs() > 0 {
        let st = state(vm);
        let n = checkint(vm, &mut st.cts.borrow_mut(), 1)?;
        ccall::set_errno(n);
    }
    vm.ret1(Value::Num(err as f64))
}

fn ffi_string(vm: &mut Vm) -> LuaResult<usize> {
    let st = state(vm);
    let mut cts = st.cts.borrow_mut();
    let o = vm.check_any(1)?;
    let mut p: *const u8 = ptr::null();
    let len = if vm.nargs() >= 2 && !vm.arg(2).is_nil() {
        let len = checkint(vm, &mut cts, 2)? as u32 as usize;
        cconv::ct_tv(vm, &mut cts, CTID_P_CVOID, ptr::from_mut(&mut p).cast(), o, ccf_arg(1))?;
        len
    } else {
        cconv::ct_tv(vm, &mut cts, CTID_P_CCHAR, ptr::from_mut(&mut p).cast(), o, ccf_arg(1))?;
        // SAFETY: as upstream, the pointer must point to a NUL-terminated string.
        unsafe { libc::strlen(p.cast()) }
    };
    drop(cts);
    // SAFETY: as upstream, the caller guarantees `len` readable bytes at `p`.
    let s = if len == 0 { &[][..] } else { unsafe { std::slice::from_raw_parts(p, len) } };
    let s = vm.new_str(s);
    vm.ret1(Value::Str(s))
}

fn ffi_copy(vm: &mut Vm) -> LuaResult<usize> {
    let st = state(vm);
    let mut cts = st.cts.borrow_mut();
    let dp = checkptr(vm, &mut cts, 1, CTID_P_VOID)?;
    let sp = checkptr(vm, &mut cts, 2, CTID_P_CVOID)?;
    let len = match vm.arg(2) {
        // Copy Lua string including trailing '\0'.
        Value::Str(s) if vm.nargs() < 3 => s.len() + 1,
        _ => checkint(vm, &mut cts, 3)? as u32 as usize,
    };
    // SAFETY: as upstream, the caller guarantees valid memory ranges.
    unsafe { ptr::copy(sp, dp, len) };
    Ok(0)
}

fn ffi_fill(vm: &mut Vm) -> LuaResult<usize> {
    let st = state(vm);
    let mut cts = st.cts.borrow_mut();
    let dp = checkptr(vm, &mut cts, 1, CTID_P_VOID)?;
    let len = checkint(vm, &mut cts, 2)? as u32 as usize;
    let mut fill = 0;
    if vm.nargs() >= 3 && !vm.arg(3).is_nil() {
        fill = checkint(vm, &mut cts, 3)?;
    }
    // SAFETY: as upstream, the caller guarantees a valid memory range.
    unsafe { ptr::write_bytes(dp, fill as u8, len) };
    Ok(0)
}

/// Test ABI string.
fn ffi_abi(vm: &mut Vm) -> LuaResult<usize> {
    let s = vm.check_str(1)?;
    let b = cparse::cparse_case(s.as_bytes(), &["64bit", "fpu", "hardfp", "le", "gc64"]).is_some();
    vm.ret1(Value::Bool(b))
}

fn ffi_metatype(vm: &mut Vm) -> LuaResult<usize> {
    let st = state(vm);
    let mut cts = st.cts.borrow_mut();
    let id = checkctype(vm, &mut cts, None)?;
    let mt = vm.check_tab(2)?;
    let ct = cts.raw(id);
    let info = cts.get(ct).info;
    if !(ctype_isstruct(info) || ctype_iscomplex(info) || ctype_isvector(info)) {
        return Err(vm.arg_error(1, errmsg::FFI_INVTYPE.as_bytes()));
    }
    drop(cts);
    let key = Value::Num(-(ct as i32) as f64);
    if !st.miscmap.get(key).is_nil() {
        return Err(vm.err_caller(ErrMsg::PROTMT));
    }
    st.miscmap.set(key, Value::Table(mt)).map_err(|e| vm.tab_error(e))?;
    let v = new_ctypeid(vm, id);
    vm.ret1(v)
}

fn ffi_gc(vm: &mut Vm) -> LuaResult<usize> {
    let cd = checkcdata(vm, 1)?;
    let fin = vm.check_any(2)?;
    let st = state(vm);
    let info = {
        let cts = st.cts.borrow();
        cts.get(cts.raw(cd.ctypeid())).info
    };
    if !(ctype_isptr(info) || ctype_isstruct(info) || ctype_isrefarray(info)) {
        return Err(vm.arg_error(1, errmsg::FFI_INVTYPE.as_bytes()));
    }
    cdata_setfin(vm, cd, fin)?;
    vm.set_top(1); // Pass through the cdata object.
    Ok(1)
}

fn ffi_load(vm: &mut Vm) -> LuaResult<usize> {
    let name = vm.check_str(1)?;
    let global = vm.nargs() >= 2 && vm.arg(2).is_truthy();
    let lib = CLibrary::load(name.as_bytes(), global).map_err(|e| ffi_err(vm, e))?;
    let mt = state(vm).clib_mt.get().expect("clib metatable");
    let ud = clib_new(vm, lib, mt);
    vm.ret1(Value::Udata(ud))
}

// -- VM hooks ----------------------------------------------------------------------------------

/// `tonumber(cdata)` (lib_base.c): numbers, enums and complex values convert to a number.
fn tonumber_hook(vm: &mut Vm, cd: Gc<CData>) -> LuaResult<Option<f64>> {
    let st = state(vm);
    let cts = st.cts.borrow();
    let mut ct = cts.rawref(cd.ctypeid());
    if ctype_isenum(cts.get(ct).info) {
        ct = cts.child(ct);
    }
    let info = cts.get(ct).info;
    if ctype_isnum(info) || ctype_iscomplex(info) {
        let mut n = 0f64;
        cconv::ct_ct(vm, &cts, CTID_DOUBLE, ct, ptr::from_mut(&mut n).cast(), cd.ptr(), 0)?;
        return Ok(Some(n));
    }
    Ok(None)
}

/// Convert a value to `const void *` with argument context (`FfiHooks::toptr`).
fn toptr_hook(vm: &mut Vm, o: Value, narg: u32) -> LuaResult<*const u8> {
    let st = state(vm);
    let mut p: *const u8 = ptr::null();
    cconv::ct_tv(vm, &mut st.cts.borrow_mut(), CTID_P_CVOID, ptr::from_mut(&mut p).cast(), o, ccf_arg(narg))?;
    Ok(p)
}

/// Info and size of the raw C type of `id` (`FfiHooks::ctype_raw`).
fn ctype_raw_hook(vm: &Vm, id: u32) -> (u32, u32) {
    match super::state::try_state(vm) {
        Some(st) => {
            let cts = st.cts.borrow();
            let ct = cts.get(cts.raw(id));
            (ct.info, ct.size)
        }
        None => (0, CTSIZE_INVALID),
    }
}

// -- Library registration ----------------------------------------------------------------------

/// Create a library table (`lj_lib_register` with a NULL name): functions are created with
/// the environment `env`, numbered from their builtin ids.
fn new_lib_table(vm: &mut Vm, nhash: u32) -> LuaResult<Gc<Table>> {
    let t = vm.new_table_ah(0, nhash)?;
    t.set_nomm(0);
    Ok(t)
}

fn set_fn(vm: &mut Vm, t: Gc<Table>, name: &str, f: NativeFn, ffid: u8, env: Gc<Table>) -> LuaResult<()> {
    let func = vm.new_native(NativeImpl::Rust(f), ffid, Vec::new(), env);
    let k = vm.new_str(name.as_bytes());
    t.set_str(k, Value::Func(func)).map_err(|e| vm.tab_error(e))
}

fn set_val(vm: &mut Vm, t: Gc<Table>, name: &str, v: Value) -> LuaResult<()> {
    let k = vm.new_str(name.as_bytes());
    t.set_str(k, v).map_err(|e| vm.tab_error(e))
}

/// `luaopen_ffi`: create the FFI state and the library table, install the cdata metatable,
/// register the module as loaded (no global `ffi` is created). Returns the library table.
/// Loading again returns the registered module.
pub fn open_ffi(vm: &mut Vm) -> LuaResult<Gc<Table>> {
    let loaded = crate::stdlib::loaded_table(vm);
    let lk = vm.new_str(b"ffi");
    if super::state::try_state(vm).is_some()
        && let Value::Table(t) = loaded.get_str(lk)
    {
        return Ok(t);
    }
    let st = state(vm);
    let env = vm.cur_thread.env();

    // The shared cdata metatable.
    let meta: &[(&str, NativeFn, u8)] = &[
        ("__index", ffi_meta_index, FF_FFI_META___INDEX),
        ("__newindex", ffi_meta_newindex, FF_FFI_META___NEWINDEX),
        ("__eq", ffi_meta_eq, FF_FFI_META___EQ),
        ("__len", ffi_meta_len, FF_FFI_META___LEN),
        ("__lt", ffi_meta_lt, FF_FFI_META___LT),
        ("__le", ffi_meta_le, FF_FFI_META___LE),
        ("__concat", ffi_meta_concat, FF_FFI_META___CONCAT),
        ("__call", ffi_meta_call, FF_FFI_META___CALL),
        ("__add", ffi_meta_add, FF_FFI_META___ADD),
        ("__sub", ffi_meta_sub, FF_FFI_META___SUB),
        ("__mul", ffi_meta_mul, FF_FFI_META___MUL),
        ("__div", ffi_meta_div, FF_FFI_META___DIV),
        ("__mod", ffi_meta_mod, FF_FFI_META___MOD),
        ("__pow", ffi_meta_pow, FF_FFI_META___POW),
        ("__unm", ffi_meta_unm, FF_FFI_META___UNM),
        ("__tostring", ffi_meta_tostring, FF_FFI_META___TOSTRING),
        ("__pairs", ffi_meta_pairs, FF_FFI_META___PAIRS),
        ("__ipairs", ffi_meta_ipairs, FF_FFI_META___IPAIRS),
    ];
    let mt = new_lib_table(vm, 19)?;
    for &(n, f, id) in meta {
        set_fn(vm, mt, n, f, id, env)?;
    }
    let ffis = vm.new_str(b"ffi");
    set_val(vm, mt, "__metatable", Value::Str(ffis))?;
    vm.set_basemt(VType::CData, Some(mt));

    // C library namespace metatable.
    let clib = new_lib_table(vm, 3)?;
    set_fn(vm, clib, "__index", ffi_clib_index, FF_FFI_CLIB___INDEX, env)?;
    set_fn(vm, clib, "__newindex", ffi_clib_newindex, FF_FFI_CLIB___NEWINDEX, env)?;
    set_fn(vm, clib, "__gc", ffi_clib_gc, FF_FFI_CLIB___GC, env)?;
    st.clib_mt.set(Some(clib));

    // Callback function pointer methods.
    let cb = new_lib_table(vm, 3)?;
    set_fn(vm, cb, "free", ffi_callback_free, FF_FFI_CALLBACK_FREE, env)?;
    set_fn(vm, cb, "set", ffi_callback_set, FF_FFI_CALLBACK_SET, env)?;
    set_val(vm, cb, "__index", Value::Table(cb))?;
    let e = vm.empty_str;
    st.miscmap.set_str(e, Value::Table(cb)).map_err(|e| vm.tab_error(e))?;

    // ffi.C default namespace.
    let cdef = clib_new(vm, CLibrary::default_namespace(), clib);

    let t = new_lib_table(vm, 22)?;
    let funcs: &[(&str, NativeFn, u8)] = &[
        ("cdef", ffi_cdef, FF_FFI_CDEF),
        ("new", ffi_new, FF_FFI_NEW),
        ("cast", ffi_cast, FF_FFI_CAST),
        ("typeof", ffi_typeof, FF_FFI_TYPEOF),
        ("typeinfo", ffi_typeinfo, FF_FFI_TYPEINFO),
        ("istype", ffi_istype, FF_FFI_ISTYPE),
        ("sizeof", ffi_sizeof, FF_FFI_SIZEOF),
        ("alignof", ffi_alignof, FF_FFI_ALIGNOF),
        ("offsetof", ffi_offsetof, FF_FFI_OFFSETOF),
        ("errno", ffi_errno, FF_FFI_ERRNO),
        ("string", ffi_string, FF_FFI_STRING),
        ("copy", ffi_copy, FF_FFI_COPY),
        ("fill", ffi_fill, FF_FFI_FILL),
        ("abi", ffi_abi, FF_FFI_ABI),
    ];
    for &(n, f, id) in funcs {
        set_fn(vm, t, n, f, id, env)?;
    }
    // metatype and gc have the miscmap table as environment, load the clib metatable.
    set_fn(vm, t, "metatype", ffi_metatype, FF_FFI_METATYPE, st.miscmap)?;
    set_fn(vm, t, "gc", ffi_gc, FF_FFI_GC, st.miscmap)?;
    set_fn(vm, t, "load", ffi_load, FF_FFI_LOAD, clib)?;
    set_val(vm, t, "C", Value::Udata(cdef))?;
    let os = vm.new_str(b"Linux");
    set_val(vm, t, "os", Value::Str(os))?;
    let arch = vm.new_str(b"x64");
    set_val(vm, t, "arch", Value::Str(arch))?;

    // Register FFI module as loaded.
    loaded.set_str(lk, Value::Table(t)).map_err(|e| vm.tab_error(e))?;

    let h = &mut vm.rt.ffi;
    h.checkbit64 = Some(carith::checkbit64);
    h.check64 = Some(carith::check64);
    h.tonumber = Some(tonumber_hook);
    h.toptr = Some(toptr_hook);
    h.ctype_raw = Some(ctype_raw_hook);
    Ok(t)
}

/// `luaopen_ffi` as a library loader function (stored in `package.preload.ffi`).
pub fn luaopen_ffi(vm: &mut Vm) -> LuaResult<usize> {
    let t = open_ffi(vm)?;
    vm.ret1(Value::Table(t))
}

/// `ctype_loadffi`: the hook the lexer and the bytecode reader use for FFI constants.
pub(crate) fn load_ffi(vm: &mut Vm) -> LuaResult<()> {
    open_ffi(vm).map(|_| ())
}

/// Register the FFI with the standard library setup (`lj_lib_preload` of lib_init.c): the
/// `ffi` module goes to `package.preload`. (The lexer/bytecode loader hook is installed by
/// `Vm::new_state`.)
pub fn preload_ffi(vm: &mut Vm) -> LuaResult<()> {
    let pk = vm.new_str(b"_PRELOAD");
    let preload = match vm.registry.get_str(pk) {
        Value::Table(t) => t,
        _ => {
            let t = vm.new_table_ah(0, 1)?;
            vm.registry.set_str(pk, Value::Table(t)).map_err(|e| vm.tab_error(e))?;
            t
        }
    };
    let f = vm.new_native(NativeImpl::Rust(luaopen_ffi), FF_C, Vec::new(), vm.globals);
    set_val(vm, preload, "ffi", Value::Func(f))
}
