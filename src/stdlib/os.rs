//! Port of lib_os.c: the OS library.

use libc::{c_char, c_int, time_t};

use crate::error::LuaResult;
use crate::gc::Gc;
use crate::state::Vm;
use crate::table::Table;
use crate::value::Value;
use crate::vm::ffid::*;
use crate::vm::{ErrMsg, fmt_msg};

use super::io::{cstring, errno, fileresult, set_errno};
use super::{LibFn, register_lib};

unsafe extern "C" {
    fn difftime(t1: time_t, t0: time_t) -> f64;
    fn clock() -> libc::clock_t;
}

/// `CLOCKS_PER_SEC` (POSIX requires one million).
const CLOCKS_PER_SEC: f64 = 1_000_000.0;

/// `lj_num2int_type(n, time_t)`: C `(time_t)n` (x86-64 `cvttsd2si`).
fn num2time(n: f64) -> time_t {
    crate::text::num2i64(n) as time_t
}

fn os_execute(vm: &mut Vm) -> LuaResult<usize> {
    let cmd = vm.opt_str(1)?.map(|s| cstring(s.as_bytes()));
    let p = cmd
        .as_ref()
        .map_or(std::ptr::null(), |c| c.as_ptr() as *const c_char);
    // SAFETY: NULL or a NUL-terminated command.
    let stat = unsafe { libc::system(p) };
    vm.ret1(Value::Num(stat as f64))
}

fn os_remove(vm: &mut Vm) -> LuaResult<usize> {
    let name = vm.check_str(1)?;
    let c = cstring(name.as_bytes());
    // SAFETY: NUL-terminated path.
    let ok = unsafe { libc::remove(c.as_ptr() as *const c_char) } == 0;
    let en = errno();
    Ok(fileresult(vm, ok, Some(name.as_bytes()), en))
}

fn os_rename(vm: &mut Vm) -> LuaResult<usize> {
    let from = vm.check_str(1)?;
    let to = vm.check_str(2)?;
    let (cf, ct) = (cstring(from.as_bytes()), cstring(to.as_bytes()));
    // SAFETY: NUL-terminated paths.
    let ok =
        unsafe { libc::rename(cf.as_ptr() as *const c_char, ct.as_ptr() as *const c_char) } == 0;
    let en = errno();
    Ok(fileresult(vm, ok, Some(from.as_bytes()), en))
}

fn os_tmpname(vm: &mut Vm) -> LuaResult<usize> {
    let mut buf = *b"/tmp/lua_XXXXXX\0";
    // SAFETY: writable NUL-terminated template.
    let fd = unsafe { libc::mkstemp(buf.as_mut_ptr() as *mut c_char) };
    if fd == -1 {
        return Err(vm.err_caller(ErrMsg::OSUNIQF));
    }
    // SAFETY: `fd` was just opened.
    unsafe { libc::close(fd) };
    vm.push_str(&buf[..15]);
    Ok(1)
}

fn os_getenv(vm: &mut Vm) -> LuaResult<usize> {
    let name = vm.check_str(1)?;
    let c = cstring(name.as_bytes());
    // SAFETY: NUL-terminated name; the result is copied before any other env access.
    let p = unsafe { libc::getenv(c.as_ptr() as *const c_char) };
    if p.is_null() {
        vm.push_nil();
    } else {
        // SAFETY: getenv returns a NUL-terminated string.
        let v = unsafe { std::ffi::CStr::from_ptr(p) }.to_bytes().to_vec();
        vm.push_str(&v);
    }
    Ok(1)
}

fn os_exit(vm: &mut Vm) -> LuaResult<usize> {
    let status = match vm.arg(1) {
        Value::Bool(b) if vm.nargs() >= 1 => {
            if b {
                libc::EXIT_SUCCESS
            } else {
                libc::EXIT_FAILURE
            }
        }
        _ => vm.opt_int(1, libc::EXIT_SUCCESS)?,
    };
    if vm.nargs() >= 2 && vm.arg(2).is_truthy() {
        vm.close();
    }
    // SAFETY: terminates the process, flushing C stdio like upstream.
    unsafe { libc::exit(status) }
}

fn os_clock(vm: &mut Vm) -> LuaResult<usize> {
    // SAFETY: plain libc call.
    let c = unsafe { clock() };
    vm.ret1(Value::Num(c as f64 * (1.0 / CLOCKS_PER_SEC)))
}

fn setfield(vm: &mut Vm, t: Gc<Table>, key: &[u8], value: c_int) -> LuaResult<()> {
    let k = vm.new_str(key);
    t.set_str(k, Value::Num(value as f64))
        .map_err(|e| vm.tab_error(e))
}

fn setboolfield(vm: &mut Vm, t: Gc<Table>, key: &[u8], value: c_int) -> LuaResult<()> {
    if value < 0 {
        return Ok(());
    }
    let k = vm.new_str(key);
    t.set_str(k, Value::Bool(value != 0))
        .map_err(|e| vm.tab_error(e))
}

fn os_date(vm: &mut Vm) -> LuaResult<usize> {
    let fmt = vm.opt_str(1)?;
    let fmt = cstring(fmt.as_ref().map_or(b"%c".as_slice(), |s| s.as_bytes()));
    let t: time_t = if vm.arg(2).is_nil() {
        // SAFETY: plain libc call.
        unsafe { libc::time(std::ptr::null_mut()) }
    } else {
        num2time(vm.check_num(2)?)
    };
    let mut s = &fmt[..fmt.len() - 1];
    // SAFETY: an all-zero `tm` is a valid value.
    let mut rtm: libc::tm = unsafe { std::mem::zeroed() };
    let stm = if s.first() == Some(&b'!') {
        s = &s[1..];
        // SAFETY: valid pointers.
        unsafe { libc::gmtime_r(&t, &mut rtm) }
    } else {
        // SAFETY: valid pointers.
        unsafe { libc::localtime_r(&t, &mut rtm) }
    };
    if stm.is_null() {
        vm.push_nil();
    } else if s == b"*t" {
        let tab = vm.new_table_ah(0, 9)?;
        vm.push(Value::Table(tab));
        setfield(vm, tab, b"sec", rtm.tm_sec)?;
        setfield(vm, tab, b"min", rtm.tm_min)?;
        setfield(vm, tab, b"hour", rtm.tm_hour)?;
        setfield(vm, tab, b"day", rtm.tm_mday)?;
        setfield(vm, tab, b"month", rtm.tm_mon + 1)?;
        setfield(vm, tab, b"year", rtm.tm_year + 1900)?;
        setfield(vm, tab, b"wday", rtm.tm_wday + 1)?;
        setfield(vm, tab, b"yday", rtm.tm_yday + 1)?;
        setboolfield(vm, tab, b"isdst", rtm.tm_isdst)?;
    } else if !s.is_empty() {
        let mut sz: u32 = 0;
        for &c in s {
            sz = sz.wrapping_add(if c == b'%' { 30 } else { 1 });
        }
        // `s` is a suffix of the NUL-terminated `fmt`.
        let sp = s.as_ptr() as *const c_char;
        let mut buf: Vec<u8> = Vec::new();
        for _ in 0..4 {
            // Limit growth for invalid format or empty result.
            buf.resize(sz as usize, 0);
            // SAFETY: `buf` has `buf.len()` writable bytes; format is NUL-terminated.
            let len =
                unsafe { libc::strftime(buf.as_mut_ptr() as *mut c_char, buf.len(), sp, &rtm) };
            if len != 0 {
                vm.push_str(&buf[..len]);
                vm.gc_safe();
                break;
            }
            sz = sz.wrapping_add(sz | 1);
        }
        // Upstream returns the top slot unchanged if every attempt produced an empty result
        // (the last argument; there is one, since the default "%c" is never empty).
    } else {
        vm.push_str(b"");
    }
    Ok(1)
}

/// `getfield`: `lua_getfield` (with metamethods) and `(int)lua_tointeger`.
fn getfield(vm: &mut Vm, t: Value, key: &[u8], d: c_int) -> LuaResult<c_int> {
    let k = vm.new_str(key);
    let v = vm.index(t, Value::Str(k))?;
    match vm.tonumber(v) {
        Some(n) => Ok(crate::text::num2i64(n) as c_int),
        None => {
            if d < 0 {
                let m = fmt_msg(ErrMsg::OSDATEF, &[key]);
                return Err(vm.rt_error(&m));
            }
            Ok(d)
        }
    }
}

fn getboolfield(vm: &mut Vm, t: Value, key: &[u8]) -> LuaResult<c_int> {
    let k = vm.new_str(key);
    let v = vm.index(t, Value::Str(k))?;
    Ok(if v.is_nil() {
        -1
    } else {
        v.is_truthy() as c_int
    })
}

fn os_time(vm: &mut Vm) -> LuaResult<usize> {
    set_errno(0);
    let t: time_t = if vm.arg(1).is_nil() {
        // SAFETY: plain libc call.
        unsafe { libc::time(std::ptr::null_mut()) }
    } else {
        vm.check_tab(1)?;
        vm.set_top(1);
        let tab = vm.arg(1);
        // SAFETY: an all-zero `tm` is a valid value.
        let mut ts: libc::tm = unsafe { std::mem::zeroed() };
        ts.tm_sec = getfield(vm, tab, b"sec", 0)?;
        ts.tm_min = getfield(vm, tab, b"min", 0)?;
        ts.tm_hour = getfield(vm, tab, b"hour", 12)?;
        ts.tm_mday = getfield(vm, tab, b"day", -1)?;
        ts.tm_mon = (getfield(vm, tab, b"month", -1)? as u32).wrapping_sub(1) as c_int;
        ts.tm_year = (getfield(vm, tab, b"year", -1)? as u32).wrapping_sub(1900) as c_int;
        ts.tm_isdst = getboolfield(vm, tab, b"isdst")?;
        // Metamethods may have run Lua code that changed errno.
        set_errno(0);
        // SAFETY: valid `tm`.
        unsafe { libc::mktime(&mut ts) }
    };
    if t == -1 && errno() != 0 {
        vm.push_nil();
    } else {
        vm.push(Value::Num(t as f64));
    }
    Ok(1)
}

fn os_difftime(vm: &mut Vm) -> LuaResult<usize> {
    let a = num2time(vm.check_num(1)?);
    let b = num2time(vm.opt_num(2, 0.0)?);
    // SAFETY: plain libc call.
    vm.ret1(Value::Num(unsafe { difftime(a, b) }))
}

fn os_setlocale(vm: &mut Vm) -> LuaResult<usize> {
    let s = vm.opt_str(1)?.map(|s| cstring(s.as_bytes()));
    // lj_lib_checkopt(L, 2, 6, "\5ctype\7numeric\4time\7collate\10monetary\1\377\3all").
    const OPTS: [&[u8]; 7] = [
        b"ctype",
        b"numeric",
        b"time",
        b"collate",
        b"monetary",
        b"\xff",
        b"all",
    ];
    let opt = match vm.opt_str(2)? {
        None => 6,
        Some(o) => match OPTS.iter().position(|&x| x == o.as_bytes()) {
            Some(i) => i,
            None => {
                let m = fmt_msg(ErrMsg::INVOPTM, &[o.as_bytes()]);
                return Err(vm.arg_error(2, &m));
            }
        },
    };
    let cat = match opt {
        0 => libc::LC_CTYPE,
        1 => libc::LC_NUMERIC,
        2 => libc::LC_TIME,
        3 => libc::LC_COLLATE,
        4 => libc::LC_MONETARY,
        6 => libc::LC_ALL,
        n => n as c_int,
    };
    let p = s
        .as_ref()
        .map_or(std::ptr::null(), |c| c.as_ptr() as *const c_char);
    // SAFETY: NULL or NUL-terminated locale name.
    let r = unsafe { libc::setlocale(cat, p) };
    if r.is_null() {
        vm.push_nil();
    } else {
        // SAFETY: setlocale returns a NUL-terminated string.
        let v = unsafe { std::ffi::CStr::from_ptr(r) }.to_bytes().to_vec();
        vm.push_str(&v);
    }
    Ok(1)
}

/// `luaopen_os`.
pub fn open_os(vm: &mut Vm) -> LuaResult<()> {
    let funcs: &[LibFn] = &[
        ("execute", os_execute, FF_OS_EXECUTE),
        ("remove", os_remove, FF_OS_REMOVE),
        ("rename", os_rename, FF_OS_RENAME),
        ("tmpname", os_tmpname, FF_OS_TMPNAME),
        ("getenv", os_getenv, FF_OS_GETENV),
        ("exit", os_exit, FF_OS_EXIT),
        ("clock", os_clock, FF_OS_CLOCK),
        ("date", os_date, FF_OS_DATE),
        ("time", os_time, FF_OS_TIME),
        ("difftime", os_difftime, FF_OS_DIFFTIME),
        ("setlocale", os_setlocale, FF_OS_SETLOCALE),
    ];
    register_lib(vm, "os", funcs)?;
    Ok(())
}
