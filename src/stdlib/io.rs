//! Port of lib_io.c: the I/O library on top of C stdio.
//!
//! File handles are raw userdata (`UDTYPE_IO_FILE`) whose payload is upstream's `IOFileUD`
//! layout, with the `FILE*` metatable stored in the registry under `"FILE*"`, so C modules
//! using `luaL_checkudata(L, n, "FILE*")` interoperate.

use std::cell::RefCell;
use std::ffi::CStr;

use libc::{FILE, c_char, c_int};

use crate::error::{ErrorKind, LuaError, LuaResult};
use crate::gc::Gc;
use crate::state::Vm;
use crate::table::Table;
use crate::udata::{UDTYPE_IO_FILE, Userdata};
use crate::value::Value;
use crate::vm::ErrMsg;
use crate::vm::ffid::*;

use super::{LibFn, c_stdout, register_lib};

/// Userdata payload of an I/O file (`IOFileUD`).
#[repr(C)]
pub struct IOFileUD {
    /// File handle.
    pub fp: *mut FILE,
    /// File type (`IOFILE_TYPE_*`, `IOFILE_FLAG_*`).
    pub type_: u32,
}

pub const IOFILE_TYPE_FILE: u32 = 0;
pub const IOFILE_TYPE_PIPE: u32 = 1;
pub const IOFILE_TYPE_STDF: u32 = 2;
pub const IOFILE_TYPE_MASK: u32 = 3;
/// Close after the `io.lines()` iterator reaches the end.
pub const IOFILE_FLAG_CLOSE: u32 = 4;

/// Registry key of the file handle metatable (`LUA_FILEHANDLE`).
pub const LUA_FILEHANDLE: &[u8] = b"FILE*";

/// `LUAL_BUFFERSIZE` (glibc `BUFSIZ` = 8192).
const LUAL_BUFFERSIZE: usize = 8192;
/// `LJ_MAX_BUF`: larger temporary buffers raise a memory error.
const LJ_MAX_BUF: usize = 0x7fffff00;
/// `LJ_MAX_UPVAL`.
const LJ_MAX_UPVAL: usize = 60;

unsafe extern "C" {
    #[link_name = "stdin"]
    static mut C_STDIN: *mut FILE;
    #[link_name = "stderr"]
    static mut C_STDERR: *mut FILE;
}

thread_local! {
    /// Temporary read buffer (`G(L)->tmpbuf` / `lj_buf_tmp`).
    static TMPBUF: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

/// Current `errno`.
pub(crate) fn errno() -> c_int {
    // SAFETY: `__errno_location` returns the calling thread's errno slot.
    unsafe { *libc::__errno_location() }
}

pub(crate) fn set_errno(e: c_int) {
    // SAFETY: see `errno`.
    unsafe { *libc::__errno_location() = e };
}

/// `strerror(en)` as bytes.
pub(crate) fn strerror(en: c_int) -> Vec<u8> {
    // SAFETY: strerror returns a valid NUL-terminated string.
    unsafe { CStr::from_ptr(libc::strerror(en)) }.to_bytes().to_vec()
}

/// Bytes as C sees them through `strdata()`: up to the first NUL.
pub(crate) fn cpart(b: &[u8]) -> &[u8] {
    match b.iter().position(|&c| c == 0) {
        Some(i) => &b[..i],
        None => b,
    }
}

/// NUL-terminated copy of the C view of `b`.
pub(crate) fn cstring(b: &[u8]) -> Vec<u8> {
    let mut v = cpart(b).to_vec();
    v.push(0);
    v
}

/// `luaL_fileresult` with the `errno` captured right after the failing call.
pub(crate) fn fileresult(vm: &mut Vm, ok: bool, fname: Option<&[u8]>, en: c_int) -> usize {
    if ok {
        vm.push(Value::Bool(true));
        1
    } else {
        vm.push_nil();
        let mut m = Vec::new();
        if let Some(f) = fname {
            m.extend_from_slice(cpart(f));
            m.extend_from_slice(b": ");
        }
        m.extend_from_slice(&strerror(en));
        vm.push_str(&m);
        vm.push(Value::Num(en as f64));
        3
    }
}

/// The `IOFileUD` payload of a file userdata.
fn iof_of(ud: Gc<Userdata>) -> *mut IOFileUD {
    ud.data_ptr() as *mut IOFileUD
}

/// Read the payload fields.
fn iof_get(p: *mut IOFileUD) -> (*mut FILE, u32) {
    // SAFETY: `p` points to the 16-byte aligned raw payload of a live UDTYPE_IO_FILE userdata,
    // which is always allocated with `size_of::<IOFileUD>()` bytes.
    unsafe { ((*p).fp, (*p).type_) }
}

fn iof_set_fp(p: *mut IOFileUD, fp: *mut FILE) {
    // SAFETY: see `iof_get`.
    unsafe { (*p).fp = fp };
}

fn iof_set_type(p: *mut IOFileUD, t: u32) {
    // SAFETY: see `iof_get`.
    unsafe { (*p).type_ = t };
}

fn is_file_ud(v: Value) -> Option<Gc<Userdata>> {
    match v {
        Value::Udata(u) if u.udtype() == UDTYPE_IO_FILE => Some(u),
        _ => None,
    }
}

// -- Open/close helpers ---------------------------------------------------------------------

/// `io_tofilep`: argument 1 must be a file handle (open or closed).
fn tofilep(vm: &mut Vm) -> LuaResult<*mut IOFileUD> {
    match if vm.nargs() >= 1 { is_file_ud(vm.arg(1)) } else { None } {
        Some(u) => Ok(iof_of(u)),
        None => Err(vm.type_error(1, "FILE*")),
    }
}

/// `io_tofile`: argument 1 must be an open file handle.
fn tofile(vm: &mut Vm) -> LuaResult<*mut IOFileUD> {
    let p = tofilep(vm)?;
    if iof_get(p).0.is_null() {
        return Err(vm.err_caller(ErrMsg::IOCLFL));
    }
    Ok(p)
}

/// The default input or output handle.
fn std_ud(vm: &Vm, output: bool) -> Gc<Userdata> {
    match if output { vm.io_output } else { vm.io_input } {
        Value::Udata(u) => u,
        _ => unreachable!("io library not opened"),
    }
}

/// `io_stdfile`: the default input/output, which must be open.
fn stdfile(vm: &mut Vm, output: bool) -> LuaResult<*mut IOFileUD> {
    let p = iof_of(std_ud(vm, output));
    if iof_get(p).0.is_null() {
        return Err(vm.err_caller(ErrMsg::IOSTDCL));
    }
    Ok(p)
}

/// `io_file_new`: push a new closed file handle whose metatable is the running function's
/// environment (the `FILE*` metatable for the io functions).
fn file_new(vm: &mut Vm) -> *mut IOFileUD {
    let env = vm.current_env();
    let ud = vm.new_udata(std::mem::size_of::<IOFileUD>(), env);
    ud.set_udtype(UDTYPE_IO_FILE);
    ud.set_metatable(Some(env));
    vm.push(Value::Udata(ud));
    let p = iof_of(ud);
    iof_set_fp(p, std::ptr::null_mut());
    iof_set_type(p, IOFILE_TYPE_FILE);
    p
}

/// `io_file_open`: open the file named by argument 1 or raise an argument error.
fn file_open(vm: &mut Vm, mode: &[u8]) -> LuaResult<*mut IOFileUD> {
    let fname = vm.check_str(1)?;
    let p = file_new(vm);
    let cname = cstring(fname.as_bytes());
    let cmode = cstring(mode);
    // SAFETY: both strings are NUL-terminated.
    let fp = unsafe { libc::fopen(cname.as_ptr() as *const c_char, cmode.as_ptr() as *const c_char) };
    iof_set_fp(p, fp);
    if fp.is_null() {
        let en = errno();
        let mut m = cpart(fname.as_bytes()).to_vec();
        m.extend_from_slice(b": ");
        m.extend_from_slice(&strerror(en));
        return Err(vm.arg_error(1, &m));
    }
    Ok(p)
}

/// `io_file_close`.
fn file_close(vm: &mut Vm, p: *mut IOFileUD) -> usize {
    let (fp, ty) = iof_get(p);
    let ok = match ty & IOFILE_TYPE_MASK {
        IOFILE_TYPE_FILE => {
            // SAFETY: `fp` is an open stream owned by this handle.
            unsafe { libc::fclose(fp) == 0 }
        }
        IOFILE_TYPE_PIPE => {
            // SAFETY: `fp` was returned by popen.
            let stat = unsafe { libc::pclose(fp) };
            stat != -1
        }
        _ => {
            debug_assert_eq!(ty & IOFILE_TYPE_MASK, IOFILE_TYPE_STDF, "close of unknown FILE* type");
            vm.push_nil();
            vm.push_str(b"cannot close standard file");
            return 2;
        }
    };
    let en = errno();
    iof_set_fp(p, std::ptr::null_mut());
    fileresult(vm, ok, None, en)
}

// -- Read/write helpers ---------------------------------------------------------------------

/// `io_file_readnum`: `fscanf(fp, "%lf")`.
fn file_readnum(vm: &mut Vm, fp: *mut FILE) -> bool {
    let mut d: f64 = 0.0;
    // SAFETY: valid stream, format with one `double*` conversion.
    if unsafe { libc::fscanf(fp, c"%lf".as_ptr(), &mut d as *mut f64) } == 1 {
        vm.push(Value::Num(d));
        true
    } else {
        vm.push_nil();
        false
    }
}

/// Run `f` with the temporary buffer grown to at least `m` bytes.
fn with_tmp<R>(m: usize, f: impl FnOnce(&mut Vec<u8>) -> R) -> R {
    TMPBUF.with(|b| {
        let mut b = b.borrow_mut();
        if b.len() < m {
            b.resize(m, 0);
        }
        f(&mut b)
    })
}

/// `io_file_readline`: read a line with `fgets` (embedded NULs truncate like upstream);
/// `chop` drops the trailing newline.
fn file_readline(vm: &mut Vm, fp: *mut FILE, chop: bool) -> bool {
    let mut m = LUAL_BUFFERSIZE;
    let mut n = 0usize;
    let mut ok = false;
    let s = loop {
        let done = with_tmp(m, |buf| {
            // SAFETY: `buf` has at least `m` bytes and `n < m`.
            let r = unsafe { libc::fgets(buf.as_mut_ptr().add(n) as *mut c_char, (m - n) as c_int, fp) };
            if r.is_null() {
                return Some(vm.new_str(&buf[..n]));
            }
            // SAFETY: fgets NUL-terminated the data it stored at `buf + n`.
            n += unsafe { libc::strlen(buf.as_ptr().add(n) as *const c_char) };
            ok |= n != 0;
            if n != 0 && buf[n - 1] == b'\n' {
                if chop {
                    n -= 1;
                }
                return Some(vm.new_str(&buf[..n]));
            }
            None
        });
        if let Some(s) = done {
            break s;
        }
        if n >= m - 64 {
            m += m;
        }
    };
    vm.push(Value::Str(s));
    vm.gc_safe();
    ok
}

/// `io_file_readall`.
fn file_readall(vm: &mut Vm, fp: *mut FILE) {
    let mut m = LUAL_BUFFERSIZE;
    let mut n = 0usize;
    loop {
        let s = with_tmp(m, |buf| {
            // SAFETY: `buf` has at least `m` bytes and `n <= m`.
            n += unsafe { libc::fread(buf.as_mut_ptr().add(n) as *mut libc::c_void, 1, m - n, fp) };
            if n != m { Some(vm.new_str(&buf[..n])) } else { None }
        });
        if let Some(s) = s {
            vm.push(Value::Str(s));
            vm.gc_safe();
            return;
        }
        m += m;
    }
}

/// `io_file_readlen`.
fn file_readlen(vm: &mut Vm, fp: *mut FILE, m: u32) -> LuaResult<bool> {
    if m != 0 {
        let m = m as usize;
        if m > LJ_MAX_BUF {
            let v = vm.err_str(ErrMsg::ERRMEM);
            return Err(LuaError::new(ErrorKind::Mem, v));
        }
        let (s, n) = with_tmp(m, |buf| {
            // SAFETY: `buf` has at least `m` bytes.
            let n = unsafe { libc::fread(buf.as_mut_ptr() as *mut libc::c_void, 1, m, fp) };
            (vm.new_str(&buf[..n]), n)
        });
        vm.push(Value::Str(s));
        vm.gc_safe();
        Ok(n > 0)
    } else {
        // SAFETY: valid stream.
        let c = unsafe { libc::fgetc(fp) };
        // SAFETY: valid stream; ungetc(EOF) is a no-op.
        unsafe { libc::ungetc(c, fp) };
        vm.push_str(b"");
        Ok(c != libc::EOF)
    }
}

/// `io_file_read`: read with the formats in argument slots `start..`; pushes the results and
/// returns their count.
fn file_read(vm: &mut Vm, p: *mut IOFileUD, start: usize) -> LuaResult<usize> {
    let fp = iof_get(p).0;
    let nargs = vm.nargs() - start;
    // SAFETY: valid stream.
    unsafe { libc::clearerr(fp) };
    let mut n;
    let mut ok;
    if nargs == 0 {
        ok = file_readline(vm, fp, true);
        n = start + 1;
    } else {
        vm.check_stack(nargs + 20, Some("too many arguments"))?;
        ok = true;
        n = start;
        let mut left = nargs;
        while left > 0 && ok {
            left -= 1;
            match vm.arg(n + 1) {
                Value::Str(s) => {
                    let b = s.as_bytes();
                    let at = |i: usize| b.get(i).copied().unwrap_or(0);
                    let mut i = 0;
                    if at(0) == b'*' {
                        i = 1;
                    }
                    let c = at(i);
                    if c == b'n' {
                        ok = file_readnum(vm, fp);
                    } else if c & !0x20 == b'L' {
                        ok = file_readline(vm, fp, c == b'l');
                    } else if c == b'a' {
                        file_readall(vm, fp);
                    } else {
                        return Err(vm.arg_error_em((n + 1) as i32, ErrMsg::INVFMT));
                    }
                }
                Value::Num(_) => {
                    let len = vm.check_int(n + 1)? as u32;
                    ok = file_readlen(vm, fp, len)?;
                }
                _ => return Err(vm.arg_error_em((n + 1) as i32, ErrMsg::INVOPT)),
            }
            n += 1;
        }
    }
    // SAFETY: valid stream.
    if unsafe { libc::ferror(fp) } != 0 {
        let en = errno();
        return Ok(fileresult(vm, false, None, en));
    }
    if !ok {
        let th = vm.cur_thread;
        th.set(th.top() - 1, Value::Nil);
    }
    Ok(n - start)
}

/// `io_file_write`: write argument slots `start..` (strings or numbers).
fn file_write(vm: &mut Vm, p: *mut IOFileUD, start: usize) -> LuaResult<usize> {
    let fp = iof_get(p).0;
    let mut status = true;
    for i in start + 1..=vm.nargs() {
        let v = vm.arg(i);
        let num;
        let str_;
        let b: &[u8] = match v {
            Value::Str(s) => {
                str_ = s;
                str_.as_bytes()
            }
            Value::Num(x) => {
                num = crate::text::strfmt::fmt_num(x);
                &num
            }
            _ => return Err(vm.type_error(i as i32, "string")),
        };
        // SAFETY: valid buffer and stream.
        status = status && unsafe { libc::fwrite(b.as_ptr() as *const libc::c_void, 1, b.len(), fp) } == b.len();
    }
    let en = errno();
    Ok(fileresult(vm, status, None, en))
}

/// The iterator of `lines`: upvalue 1 is the file, the rest are read formats.
fn io_file_iter(vm: &mut Vm) -> LuaResult<usize> {
    let f = vm.current_function().expect("running native");
    let Some(ud) = f.upvalue(0).and_then(is_file_ud) else { unreachable!("lines iterator without file") };
    let p = iof_of(ud);
    let n = f.nupvalues() - 1;
    if iof_get(p).0.is_null() {
        return Err(vm.err_caller(ErrMsg::IOCLFL));
    }
    vm.set_top(0);
    if n > 0 {
        vm.check_stack(n, None)?;
        for i in 1..=n {
            let v = f.upvalue(i).unwrap_or(Value::Nil);
            vm.push(v);
        }
    }
    let n = file_read(vm, p, 0)?;
    let (fp, ty) = iof_get(p);
    // SAFETY: valid stream (the handle is still open here).
    if unsafe { libc::ferror(fp) } != 0 {
        let msg = vm.get(-2);
        let m = vm.tostring_opt(msg).unwrap_or_default();
        return Err(vm.rt_error(&m));
    }
    if vm.arg(1).is_nil() && ty & IOFILE_FLAG_CLOSE != 0 {
        file_close(vm, p);
        return Ok(0);
    }
    Ok(n)
}

/// `io_file_lines`: closure over all arguments (file plus formats).
fn file_lines(vm: &mut Vm) -> LuaResult<usize> {
    let n = vm.nargs();
    if n > LJ_MAX_UPVAL {
        return Err(vm.err_caller(ErrMsg::UNPACK));
    }
    let ups: Vec<Value> = (1..=n).map(|i| vm.arg(i)).collect();
    let f = vm.native(io_file_iter, FF_C, ups);
    vm.ret1(Value::Func(f))
}

// -- I/O file methods -----------------------------------------------------------------------

fn io_method_close(vm: &mut Vm) -> LuaResult<usize> {
    let p = if vm.nargs() > 0 {
        tofile(vm)?
    } else {
        let p = iof_of(std_ud(vm, true));
        if iof_get(p).0.is_null() {
            return Err(vm.err_caller(ErrMsg::IOCLFL));
        }
        p
    };
    Ok(file_close(vm, p))
}

fn io_method_read(vm: &mut Vm) -> LuaResult<usize> {
    let p = tofile(vm)?;
    file_read(vm, p, 1)
}

fn io_method_write(vm: &mut Vm) -> LuaResult<usize> {
    let p = tofile(vm)?;
    file_write(vm, p, 1)
}

fn io_method_flush(vm: &mut Vm) -> LuaResult<usize> {
    let fp = iof_get(tofile(vm)?).0;
    // SAFETY: valid stream.
    let ok = unsafe { libc::fflush(fp) } == 0;
    let en = errno();
    Ok(fileresult(vm, ok, None, en))
}

fn io_method_seek(vm: &mut Vm) -> LuaResult<usize> {
    let fp = iof_get(tofile(vm)?).0;
    let opt = match vm.check_opt(2, 1, &["set", "cur", "end"])? {
        0 => libc::SEEK_SET,
        1 => libc::SEEK_CUR,
        _ => libc::SEEK_END,
    };
    let mut ofs: i64 = 0;
    if vm.nargs() >= 3 {
        let mut o = vm.arg(3);
        if let Value::Str(s) = o
            && let Some(x) = crate::text::strscan::scan_num(s.as_bytes())
        {
            o = Value::Num(x);
            vm.set_arg(3, o);
        }
        match o {
            Value::Num(x) => ofs = crate::text::num2i64(x),
            Value::Nil => {}
            _ => return Err(vm.type_error(3, "number")),
        }
    }
    // SAFETY: valid stream.
    if unsafe { libc::fseeko(fp, ofs, opt) } != 0 {
        let en = errno();
        return Ok(fileresult(vm, false, None, en));
    }
    // SAFETY: valid stream.
    let pos = unsafe { libc::ftello(fp) };
    vm.ret1(Value::Num(pos as f64))
}

fn io_method_setvbuf(vm: &mut Vm) -> LuaResult<usize> {
    let fp = iof_get(tofile(vm)?).0;
    let opt = match vm.check_opt(2, -1, &["full", "line", "no"])? {
        0 => libc::_IOFBF,
        1 => libc::_IOLBF,
        _ => libc::_IONBF,
    };
    let sz = vm.opt_int(3, LUAL_BUFFERSIZE as i32)? as isize as usize;
    // SAFETY: valid stream; a NULL buffer lets stdio allocate.
    let ok = unsafe { libc::setvbuf(fp, std::ptr::null_mut(), opt, sz) } == 0;
    let en = errno();
    Ok(fileresult(vm, ok, None, en))
}

fn io_method_lines(vm: &mut Vm) -> LuaResult<usize> {
    tofile(vm)?;
    file_lines(vm)
}

fn io_method_gc(vm: &mut Vm) -> LuaResult<usize> {
    let p = tofilep(vm)?;
    let (fp, ty) = iof_get(p);
    if !fp.is_null() && ty & IOFILE_TYPE_MASK != IOFILE_TYPE_STDF {
        file_close(vm, p);
    }
    Ok(0)
}

fn io_method_tostring(vm: &mut Vm) -> LuaResult<usize> {
    let fp = iof_get(tofilep(vm)?).0;
    if fp.is_null() {
        vm.push_str(b"file (closed)");
    } else {
        let mut b = b"file (".to_vec();
        crate::text::strfmt::put_ptr(&mut b, fp as usize);
        b.push(b')');
        vm.push_str(&b);
    }
    Ok(1)
}

// -- I/O library functions ------------------------------------------------------------------

fn io_open(vm: &mut Vm) -> LuaResult<usize> {
    let fname = vm.check_str(1)?;
    let mode = vm.opt_str(2)?;
    let cmode = cstring(mode.as_ref().map_or(b"r".as_slice(), |s| s.as_bytes()));
    let p = file_new(vm);
    let cname = cstring(fname.as_bytes());
    // SAFETY: NUL-terminated strings.
    let fp = unsafe { libc::fopen(cname.as_ptr() as *const c_char, cmode.as_ptr() as *const c_char) };
    let en = errno();
    iof_set_fp(p, fp);
    if fp.is_null() { Ok(fileresult(vm, false, Some(fname.as_bytes()), en)) } else { Ok(1) }
}

fn io_popen(vm: &mut Vm) -> LuaResult<usize> {
    let fname = vm.check_str(1)?;
    let mode = vm.opt_str(2)?;
    let cmode = cstring(mode.as_ref().map_or(b"r".as_slice(), |s| s.as_bytes()));
    let p = file_new(vm);
    iof_set_type(p, IOFILE_TYPE_PIPE);
    let cname = cstring(fname.as_bytes());
    // SAFETY: flushing all streams; NUL-terminated strings for popen.
    let fp = unsafe {
        libc::fflush(std::ptr::null_mut());
        libc::popen(cname.as_ptr() as *const c_char, cmode.as_ptr() as *const c_char)
    };
    let en = errno();
    iof_set_fp(p, fp);
    if fp.is_null() { Ok(fileresult(vm, false, Some(fname.as_bytes()), en)) } else { Ok(1) }
}

fn io_tmpfile(vm: &mut Vm) -> LuaResult<usize> {
    let p = file_new(vm);
    // SAFETY: plain libc call.
    let fp = unsafe { libc::tmpfile() };
    let en = errno();
    iof_set_fp(p, fp);
    if fp.is_null() { Ok(fileresult(vm, false, None, en)) } else { Ok(1) }
}

fn io_close(vm: &mut Vm) -> LuaResult<usize> {
    io_method_close(vm)
}

fn io_read(vm: &mut Vm) -> LuaResult<usize> {
    let p = stdfile(vm, false)?;
    file_read(vm, p, 0)
}

fn io_write(vm: &mut Vm) -> LuaResult<usize> {
    let p = stdfile(vm, true)?;
    file_write(vm, p, 0)
}

fn io_flush(vm: &mut Vm) -> LuaResult<usize> {
    let fp = iof_get(stdfile(vm, true)?).0;
    // SAFETY: valid stream.
    let ok = unsafe { libc::fflush(fp) } == 0;
    let en = errno();
    Ok(fileresult(vm, ok, None, en))
}

/// `io_std_getset`.
fn std_getset(vm: &mut Vm, output: bool, mode: &[u8]) -> LuaResult<usize> {
    if vm.nargs() > 0 && !vm.arg(1).is_nil() {
        if matches!(vm.arg(1), Value::Udata(_)) {
            tofile(vm)?;
            vm.set_top(1);
        } else {
            file_open(vm, mode)?;
        }
        let v = vm.get(-1);
        if output {
            vm.io_output = v;
        } else {
            vm.io_input = v;
        }
    } else {
        let v = if output { vm.io_output } else { vm.io_input };
        vm.push(v);
    }
    Ok(1)
}

fn io_input(vm: &mut Vm) -> LuaResult<usize> {
    std_getset(vm, false, b"r")
}

fn io_output(vm: &mut Vm) -> LuaResult<usize> {
    std_getset(vm, true, b"w")
}

fn io_lines(vm: &mut Vm) -> LuaResult<usize> {
    if vm.nargs() == 0 {
        vm.push_nil();
    }
    if !vm.arg(1).is_nil() {
        let p = file_open(vm, b"r")?;
        iof_set_type(p, IOFILE_TYPE_FILE | IOFILE_FLAG_CLOSE);
        let ud = vm.pop();
        vm.set_arg(1, ud);
    } else {
        let v = vm.io_input;
        vm.set_arg(1, v);
    }
    file_lines(vm)
}

fn io_type(vm: &mut Vm) -> LuaResult<usize> {
    let o = vm.check_any(1)?;
    match is_file_ud(o) {
        None => vm.push_nil(),
        Some(u) if !iof_get(iof_of(u)).0.is_null() => vm.push_str(b"file"),
        Some(_) => vm.push_str(b"closed file"),
    }
    Ok(1)
}

// -- Registration ---------------------------------------------------------------------------

/// `io_std_new`: a standard file handle stored as `io.<name>`.
fn std_new(vm: &mut Vm, io: Gc<Table>, mt: Gc<Table>, fp: *mut FILE, name: &[u8]) -> LuaResult<Value> {
    let env = vm.current_env();
    let ud = vm.new_udata(std::mem::size_of::<IOFileUD>(), env);
    ud.set_udtype(UDTYPE_IO_FILE);
    ud.set_metatable(Some(mt));
    let p = iof_of(ud);
    iof_set_fp(p, fp);
    iof_set_type(p, IOFILE_TYPE_STDF);
    let k = vm.new_str(name);
    io.set_str(k, Value::Udata(ud)).map_err(|e| vm.tab_error(e))?;
    Ok(Value::Udata(ud))
}

/// `luaopen_io`.
pub fn open_io(vm: &mut Vm) -> LuaResult<()> {
    let methods: &[LibFn] = &[
        ("close", io_method_close, FF_IO_METHOD_CLOSE),
        ("read", io_method_read, FF_IO_METHOD_READ),
        ("write", io_method_write, FF_IO_METHOD_WRITE),
        ("flush", io_method_flush, FF_IO_METHOD_FLUSH),
        ("seek", io_method_seek, FF_IO_METHOD_SEEK),
        ("setvbuf", io_method_setvbuf, FF_IO_METHOD_SETVBUF),
        ("lines", io_method_lines, FF_IO_METHOD_LINES),
        ("__gc", io_method_gc, FF_IO_METHOD___GC),
        ("__tostring", io_method_tostring, FF_IO_METHOD___TOSTRING),
    ];
    let mt = vm.new_table_ah(0, 10)?;
    for &(name, f, ffid) in methods {
        let func = super::new_lib_fn(vm, f, ffid, Vec::new());
        let k = vm.new_str(name.as_bytes());
        mt.set_str(k, Value::Func(func)).map_err(|e| vm.tab_error(e))?;
    }
    let k = vm.new_str(b"__index");
    mt.set_str(k, Value::Table(mt)).map_err(|e| vm.tab_error(e))?;
    let k = vm.new_str(LUA_FILEHANDLE);
    vm.registry.set_str(k, Value::Table(mt)).map_err(|e| vm.tab_error(e))?;

    let funcs: &[LibFn] = &[
        ("open", io_open, FF_IO_OPEN),
        ("popen", io_popen, FF_IO_POPEN),
        ("tmpfile", io_tmpfile, FF_IO_TMPFILE),
        ("close", io_close, FF_IO_CLOSE),
        ("read", io_read, FF_IO_READ),
        ("write", io_write, FF_IO_WRITE),
        ("flush", io_flush, FF_IO_FLUSH),
        ("input", io_input, FF_IO_INPUT),
        ("output", io_output, FF_IO_OUTPUT),
        ("lines", io_lines, FF_IO_LINES),
        ("type", io_type, FF_IO_TYPE),
    ];
    let io = register_lib(vm, "io", funcs)?;
    // The io functions run with the FILE* metatable as environment (`LJLIB_SET(!)`); new
    // handles take their metatable from it.
    for &(name, _, _) in funcs {
        let k = vm.new_str(name.as_bytes());
        if let Value::Func(f) = io.get_str(k) {
            f.set_env(mt);
        }
    }
    // SAFETY: reading the glibc stream pointers, initialized before main.
    let (sin, serr) = unsafe { (C_STDIN, C_STDERR) };
    vm.io_input = std_new(vm, io, mt, sin, b"stdin")?;
    vm.io_output = std_new(vm, io, mt, c_stdout(), b"stdout")?;
    std_new(vm, io, mt, serr, b"stderr")?;
    Ok(())
}
