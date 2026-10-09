//! The `luajit` command-line frontend. Port of luajit.c: option parsing, `LUA_INIT`, the
//! `arg` table, `-e`/`-l`/`-j`/`-O`/`-b`, script execution, error reporting with traceback,
//! SIGINT interruption and the read-eval-print loop.

use std::cell::RefCell;
use std::sync::atomic::{AtomicPtr, Ordering};

use crate::error::{ErrorKind, LuaResult};
use crate::func::FF_C;
use crate::state::Vm;
use crate::value::Value;
use crate::vm::{HookEvent, LUA_MASKCALL, LUA_MASKCOUNT, LUA_MASKRET};

use crate::stdlib::jit::LUAJIT_VERSION;
const LUAJIT_COPYRIGHT: &str = "Copyright (C) 2005-2026 Mike Pall";
const LUAJIT_URL: &str = "https://luajit.org/";
const LUA_PROGNAME: &[u8] = b"luajit";
const LUA_PROMPT: &[u8] = b"> ";
const LUA_PROMPT2: &[u8] = b">> ";
const LUA_MAXINPUT: usize = 512;

const FLAGS_INTERACTIVE: u32 = 1;
const FLAGS_VERSION: u32 = 2;
const FLAGS_EXEC: u32 = 4;
const FLAGS_OPTION: u32 = 8;
const FLAGS_NOENV: u32 = 16;

/// Status codes as in lua.h (`LUA_OK` = 0, nonzero = failure, -1 = `-b` done).
type Status = i32;

struct Smain {
    argv: Vec<Vec<u8>>,
    status: Status,
    progname: Option<Vec<u8>>,
}

thread_local! {
    static SMAIN: RefCell<Smain> = const { RefCell::new(Smain { argv: Vec::new(), status: 0, progname: None }) };
}

/// `globalL`: the state the SIGINT handler installs its hook into.
static GLOBAL_L: AtomicPtr<Vm> = AtomicPtr::new(std::ptr::null_mut());

unsafe extern "C" {
    #[link_name = "stdin"]
    static mut C_STDIN: *mut libc::FILE;
}

fn flush_stdout() {
    // SAFETY: `stdout` is a valid stream.
    unsafe { libc::fflush(crate::stdlib::c_stdout()) };
}

fn write_stderr(s: &[u8]) {
    use std::io::Write;
    let _ = std::io::stderr().write_all(s);
}

fn signal_set(sig: libc::c_int, h: libc::sighandler_t) {
    // SAFETY: plain sigaction with a zeroed struct, as upstream.
    unsafe {
        let mut sa: libc::sigaction = std::mem::zeroed();
        sa.sa_sigaction = h;
        libc::sigemptyset(&mut sa.sa_mask);
        libc::sigaction(sig, &sa, std::ptr::null_mut());
    }
}

fn lstop(vm: &mut Vm, _ev: HookEvent, _line: i32) -> LuaResult<()> {
    vm.set_hook(None, 0, 0);
    // Avoid luaL_error -- a C hook doesn't add an extra frame.
    let mut m = vm.where_(0);
    m.extend_from_slice(b"interrupted!");
    let s = vm.new_str(&m);
    Err(vm.error(Value::Str(s)))
}

extern "C" fn laction(i: libc::c_int) {
    // Terminate process if another SIGINT happens (double CTRL-C).
    signal_set(i, libc::SIG_DFL);
    let p = GLOBAL_L.load(Ordering::SeqCst);
    if !p.is_null() {
        // SAFETY: like upstream, the handler interrupts the VM thread (SIGINT is blocked in all
        // other threads) and only installs a hook.
        unsafe { (*p).set_hook(Some(lstop), LUA_MASKCALL | LUA_MASKRET | LUA_MASKCOUNT, 1) };
    }
}

fn print_usage() {
    let prog = SMAIN.with(|s| s.borrow().progname.clone()).unwrap_or_default();
    let mut m = b"usage: ".to_vec();
    m.extend_from_slice(&prog);
    m.extend_from_slice(
        b" [options]... [script [args]...].\n\
Available options are:\n\
\x20 -e chunk  Execute string 'chunk'.\n\
\x20 -l name   Require library 'name'.\n\
\x20 -b ...    Save or list bytecode.\n\
\x20 -j cmd    Perform LuaJIT control command.\n\
\x20 -O[opt]   Control LuaJIT optimizations.\n\
\x20 -i        Enter interactive mode after executing 'script'.\n\
\x20 -v        Show version information.\n\
\x20 -E        Ignore environment variables.\n\
\x20 --        Stop handling options.\n\
\x20 -         Execute stdin and stop handling options.\n",
    );
    write_stderr(&m);
}

fn l_message(msg: &[u8]) {
    let mut m = Vec::new();
    if let Some(p) = SMAIN.with(|s| s.borrow().progname.clone()) {
        m.extend_from_slice(&p);
        m.extend_from_slice(b": ");
    }
    m.extend_from_slice(msg);
    m.push(b'\n');
    write_stderr(&m);
}

/// `report`: print a non-nil error object.
fn report(vm: &mut Vm, r: Result<(), Value>) -> Status {
    match r {
        Ok(()) => 0,
        Err(v) => {
            if !v.is_nil() {
                let msg = vm.to_str(v).map(|s| s.as_bytes().to_vec());
                l_message(msg.as_deref().unwrap_or(b"(error object is not a string)"));
            }
            1
        }
    }
}

fn is_string(v: Value) -> bool {
    matches!(v, Value::Str(_) | Value::Num(_))
}

/// `traceback`: error handler adding a stack traceback.
fn traceback(vm: &mut Vm) -> LuaResult<usize> {
    let mut v = vm.arg(1);
    if !is_string(v) {
        // Non-string error object? Try metamethod.
        if v.is_nil() {
            return Ok(1);
        }
        let mo = vm.meta_lookup(v, crate::bc::MM::Tostring);
        if mo.is_nil() {
            return Ok(1);
        }
        vm.call_value(mo, &[v], 1)?;
        let r = vm.pop();
        if !is_string(r) {
            vm.push(r);
            return Ok(1);
        }
        v = r;
    }
    let msg = vm.to_str(v).map(|s| s.as_bytes().to_vec());
    let th = vm.cur_thread;
    let tb = vm.traceback(th, msg.as_deref(), 1);
    vm.push_str(&tb);
    Ok(1)
}

/// `docall`: call the function below the `narg` topmost values with the traceback handler.
/// On success the results (none if `clear`) are left on the stack.
fn docall(vm: &mut Vm, narg: usize, clear: bool) -> Result<(), Value> {
    let th = vm.cur_thread;
    let base = th.top() - narg - 1;
    let h = vm.native(traceback, FF_C, vec![]);
    vm.push(Value::Nil);
    for i in (base..th.top() - 1).rev() {
        th.set(i + 1, th.get(i));
    }
    th.set(base, Value::Func(h));
    signal_set(libc::SIGINT, laction as *const () as libc::sighandler_t);
    let r = vm.pcall(narg, if clear { 0 } else { -1 }, Some(base));
    signal_set(libc::SIGINT, libc::SIG_DFL);
    // Remove the traceback function.
    let top = th.top();
    for i in base + 1..top {
        th.set(i - 1, th.get(i));
    }
    th.set_top(top - 1);
    match r {
        Ok(_) => Ok(()),
        Err(e) => {
            // Force a complete garbage collection in case of errors.
            let v = e.value;
            vm.push(v);
            vm.gc_collect();
            vm.pop();
            Err(v)
        }
    }
}

fn print_version() {
    crate::stdlib::write_stdout(format!("{LUAJIT_VERSION} -- {LUAJIT_COPYRIGHT}. {LUAJIT_URL}\n").as_bytes());
}

fn getfield(vm: &mut Vm, o: Value, k: &[u8]) -> LuaResult<Value> {
    let k = vm.new_str(k);
    vm.index(o, Value::Str(k))
}

fn loaded(vm: &mut Vm) -> Value {
    Value::Table(crate::stdlib::loaded_table(vm))
}

fn print_jit_status(vm: &mut Vm) -> LuaResult<()> {
    let l = loaded(vm);
    let jit = getfield(vm, l, b"jit")?;
    let f = getfield(vm, jit, b"status")?;
    let n = vm.get_top();
    vm.push(f);
    let nres = vm.call(0, -1)?;
    let mut out = if nres > 0 && vm.get((n + 1) as i32).is_truthy() { b"JIT: ON".to_vec() } else { b"JIT: OFF".to_vec() };
    for i in n + 2..=n + nres {
        let v = vm.get(i as i32);
        match vm.to_str(v) {
            Some(s) => {
                out.push(b' ');
                out.extend_from_slice(s.as_bytes());
            }
            None => break,
        }
    }
    out.push(b'\n');
    crate::stdlib::write_stdout(&out);
    vm.set_top(0);
    Ok(())
}

fn createargtable(vm: &mut Vm, argn: usize) -> LuaResult<()> {
    let argv = SMAIN.with(|s| s.borrow().argv.clone());
    let argc = argv.len();
    let t = vm.new_table_ah((argc - argn) as u32, argn as u32)?;
    vm.push(Value::Table(t));
    for (i, a) in argv.iter().enumerate() {
        let s = vm.new_str(a);
        vm.rawset(t, Value::Num(i as f64 - argn as f64), Value::Str(s))?;
    }
    let k = vm.new_str(b"arg");
    let g = Value::Table(vm.globals);
    vm.setindex(g, Value::Str(k), Value::Table(t))?;
    vm.pop();
    Ok(())
}

fn load_err(e: crate::error::LuaError) -> Value {
    e.value
}

fn dofile(vm: &mut Vm, name: Option<&[u8]>) -> Status {
    let r = match vm.load_file(name, None) {
        Ok(f) => {
            vm.push(f);
            docall(vm, 0, true)
        }
        Err(e) => Err(load_err(e)),
    };
    report(vm, r)
}

fn dostring(vm: &mut Vm, s: &[u8], name: &[u8]) -> Status {
    let r = match vm.load(s, name, None) {
        Ok(f) => {
            vm.push(f);
            docall(vm, 0, true)
        }
        Err(e) => Err(load_err(e)),
    };
    report(vm, r)
}

fn dolibrary(vm: &mut Vm, name: &[u8]) -> LuaResult<Status> {
    let g = Value::Table(vm.globals);
    let req = getfield(vm, g, b"require")?;
    vm.push(req);
    vm.push_str(name);
    let r = docall(vm, 1, true);
    Ok(report(vm, r))
}

fn write_prompt(vm: &mut Vm, firstline: bool) -> LuaResult<()> {
    let g = Value::Table(vm.globals);
    let v = getfield(vm, g, if firstline { b"_PROMPT" } else { b"_PROMPT2" })?;
    let p = vm.to_str(v).map(|s| s.as_bytes().to_vec());
    crate::stdlib::write_stdout(p.as_deref().unwrap_or(if firstline { LUA_PROMPT } else { LUA_PROMPT2 }));
    flush_stdout();
    Ok(())
}

/// `incomplete`: a syntax error ending in `'<eof>'` asks for more input.
fn incomplete(e: &crate::error::LuaError) -> bool {
    if e.kind == ErrorKind::Syntax
        && let Value::Str(s) = e.value
    {
        let msg = s.as_bytes();
        const EOF: &[u8] = b"'<eof>'";
        let first = msg.windows(EOF.len()).position(|w| w == EOF);
        return msg.len() >= EOF.len() && first == Some(msg.len() - EOF.len());
    }
    false
}

/// `pushline` without the push: read one line (at most `LUA_MAXINPUT - 1` bytes, `fgets`).
fn readline(vm: &mut Vm, firstline: bool) -> LuaResult<Option<Vec<u8>>> {
    write_prompt(vm, firstline)?;
    let mut buf = [0 as libc::c_char; LUA_MAXINPUT];
    // SAFETY: `buf` is valid for LUA_MAXINPUT bytes; `stdin` is a valid stream.
    let p = unsafe { libc::fgets(buf.as_mut_ptr(), LUA_MAXINPUT as libc::c_int, C_STDIN) };
    if p.is_null() {
        return Ok(None);
    }
    // SAFETY: fgets NUL-terminated the buffer.
    let mut line = unsafe { std::ffi::CStr::from_ptr(buf.as_ptr()) }.to_bytes().to_vec();
    if line.last() == Some(&b'\n') {
        line.pop();
    }
    if firstline && line.first() == Some(&b'=') {
        let mut r = b"return ".to_vec();
        r.extend_from_slice(&line[1..]);
        line = r;
    }
    Ok(Some(line))
}

/// `loadline`: `None` at end of input; else the load result (function pushed on success).
fn loadline(vm: &mut Vm) -> LuaResult<Option<Result<(), Value>>> {
    vm.set_top(0);
    let Some(mut buf) = readline(vm, true)? else { return Ok(None) };
    loop {
        match vm.load(&buf, b"=stdin", None) {
            Ok(f) => {
                vm.push(f);
                return Ok(Some(Ok(())));
            }
            Err(e) if incomplete(&e) => {
                let Some(more) = readline(vm, false)? else { return Ok(None) };
                buf.push(b'\n');
                buf.extend_from_slice(&more);
            }
            Err(e) => return Ok(Some(Err(e.value))),
        }
    }
}

fn dotty(vm: &mut Vm) -> LuaResult<()> {
    let old = SMAIN.with(|s| s.borrow_mut().progname.take());
    while let Some(mut r) = loadline(vm)? {
        if r.is_ok() {
            r = docall(vm, 0, false);
        }
        let ok = r.is_ok();
        report(vm, r);
        if ok && vm.get_top() > 0 {
            // Any result to print?
            let g = Value::Table(vm.globals);
            let p = getfield(vm, g, b"print")?;
            let n = vm.get_top();
            vm.push(p);
            let th = vm.cur_thread;
            let base = vm.frame_base();
            for i in (base..base + n).rev() {
                th.set(i + 1, th.get(i));
            }
            th.set(base, p);
            if let Err(e) = vm.pcall(n, 0, None) {
                let s = vm.to_str(e.value).map(|s| s.as_bytes().to_vec());
                let mut m = b"error calling 'print' (".to_vec();
                m.extend_from_slice(s.as_deref().unwrap_or(b"(null)"));
                m.push(b')');
                l_message(&m);
            }
        }
    }
    vm.set_top(0);
    crate::stdlib::write_stdout(b"\n");
    flush_stdout();
    SMAIN.with(|s| s.borrow_mut().progname = old);
    Ok(())
}

fn handle_script(vm: &mut Vm, argn: usize) -> LuaResult<Status> {
    let (fname, prev) = SMAIN.with(|s| {
        let s = s.borrow();
        (s.argv[argn].clone(), s.argv[argn - 1].clone())
    });
    let stdin = fname == b"-" && prev != b"--";
    let r = match vm.load_file(if stdin { None } else { Some(&fname) }, None) {
        Ok(f) => {
            vm.push(f);
            // Fetch args from arg table. LUA_INIT or -e might have changed them.
            let g = Value::Table(vm.globals);
            let a = getfield(vm, g, b"arg")?;
            let mut narg = 0;
            if let Value::Table(t) = a {
                loop {
                    let v = t.get_int(narg as i32 + 1);
                    if v.is_nil() {
                        break;
                    }
                    vm.push(v);
                    narg += 1;
                }
            }
            docall(vm, narg, false)
        }
        Err(e) => Err(load_err(e)),
    };
    Ok(report(vm, r))
}

/// `loadjitmodule`: `require("jit." .. name).start` pushed; nonzero on failure.
fn loadjitmodule(vm: &mut Vm, name: &[u8]) -> LuaResult<Status> {
    let g = Value::Table(vm.globals);
    let req = getfield(vm, g, b"require")?;
    vm.push(req);
    let mut m = b"jit.".to_vec();
    m.extend_from_slice(name);
    vm.push_str(&m);
    if let Err(e) = vm.pcall(1, 1, None) {
        let msg = vm.to_str(e.value).map(|s| s.as_bytes().to_vec());
        if msg.as_deref().is_some_and(|m| m.starts_with(b"module ")) {
            l_message(b"unknown luaJIT command or jit.* modules not installed");
            return Ok(1);
        }
        return Ok(report(vm, Err(e.value)));
    }
    let md = vm.get(-1);
    let start = getfield(vm, md, b"start")?;
    if start.is_nil() {
        l_message(b"unknown luaJIT command or jit.* modules not installed");
        return Ok(1);
    }
    vm.replace(-1, start);
    Ok(0)
}

/// `runcmdopt`: call the function on top with the comma-separated options.
fn runcmdopt(vm: &mut Vm, opt: Option<&[u8]>) -> Status {
    let mut narg = 0;
    if let Some(opt) = opt.filter(|o| !o.is_empty()) {
        for part in opt.split(|&c| c == b',') {
            narg += 1;
            if part.is_empty() {
                vm.push_nil();
            } else {
                vm.push_str(part);
            }
        }
    }
    let r = vm.pcall(narg, 0, None).map(|_| ()).map_err(|e| e.value);
    report(vm, r)
}

fn dojitcmd(vm: &mut Vm, cmd: &[u8]) -> LuaResult<Status> {
    let eq = cmd.iter().position(|&c| c == b'=');
    let name = &cmd[..eq.unwrap_or(cmd.len())];
    let l = loaded(vm);
    let jit = getfield(vm, l, b"jit")?;
    let f = getfield(vm, jit, name)?;
    if matches!(f, Value::Func(_)) {
        vm.push(f);
    } else if loadjitmodule(vm, name)? != 0 {
        return Ok(1);
    }
    Ok(runcmdopt(vm, eq.map(|p| &cmd[p + 1..])))
}

fn dojitopt(vm: &mut Vm, opt: &[u8]) -> LuaResult<Status> {
    let l = loaded(vm);
    let o = getfield(vm, l, b"jit.opt")?;
    let start = getfield(vm, o, b"start")?;
    vm.push(start);
    Ok(runcmdopt(vm, Some(opt)))
}

fn dobytecode(vm: &mut Vm, i: usize) -> LuaResult<Status> {
    if loadjitmodule(vm, b"bcsave")? != 0 {
        return Ok(1);
    }
    let argv = SMAIN.with(|s| s.borrow().argv.clone());
    let mut narg = 0;
    if argv[i].len() > 2 {
        narg += 1;
        let mut a = b"-".to_vec();
        a.extend_from_slice(&argv[i][2..]);
        vm.push_str(&a);
    }
    for a in &argv[i + 1..] {
        vm.push_str(a);
        narg += 1;
    }
    let r = vm.pcall(narg, 0, None).map(|_| ()).map_err(|e| e.value);
    report(vm, r);
    Ok(-1)
}

/// `collectargs`: index of the script (or `argc`), or `None` for invalid options.
fn collectargs(argv: &[Vec<u8>], flags: &mut u32) -> Option<usize> {
    let mut i = 1;
    while i < argv.len() {
        let a = &argv[i];
        if a.first() != Some(&b'-') {
            return Some(i);
        }
        let notail = a.len() <= 2;
        match a.get(1).copied() {
            Some(b'-') => return if notail { Some(i + 1) } else { None },
            None => return Some(i),
            Some(b'i') => {
                if !notail {
                    return None;
                }
                *flags |= FLAGS_INTERACTIVE | FLAGS_VERSION;
            }
            Some(b'v') => {
                if !notail {
                    return None;
                }
                *flags |= FLAGS_VERSION;
            }
            Some(c @ (b'e' | b'j' | b'l')) => {
                if c == b'e' {
                    *flags |= FLAGS_EXEC;
                }
                *flags |= FLAGS_OPTION;
                if a.len() == 2 {
                    i += 1;
                    if i >= argv.len() {
                        return None;
                    }
                }
            }
            Some(b'O') => {}
            Some(b'b') => {
                if *flags != 0 {
                    return None;
                }
                *flags |= FLAGS_EXEC;
                return Some(i + 1);
            }
            Some(b'E') => *flags |= FLAGS_NOENV,
            _ => return None,
        }
        i += 1;
    }
    Some(i)
}

fn runargs(vm: &mut Vm, argn: usize) -> LuaResult<Status> {
    let argv = SMAIN.with(|s| s.borrow().argv.clone());
    let mut i = 1;
    while i < argn {
        let a = &argv[i];
        let optarg = |i: &mut usize| -> Vec<u8> {
            if a.len() > 2 {
                a[2..].to_vec()
            } else {
                *i += 1;
                argv[*i].clone()
            }
        };
        match a.get(1).copied() {
            Some(b'e') => {
                let chunk = optarg(&mut i);
                if dostring(vm, &chunk, b"=(command line)") != 0 {
                    return Ok(1);
                }
            }
            Some(b'l') => {
                let name = optarg(&mut i);
                if dolibrary(vm, &name)? != 0 {
                    return Ok(1);
                }
            }
            Some(b'j') => {
                let cmd = optarg(&mut i);
                if dojitcmd(vm, &cmd)? != 0 {
                    return Ok(1);
                }
            }
            Some(b'O') => {
                if dojitopt(vm, &a[2..])? != 0 {
                    return Ok(1);
                }
            }
            Some(b'b') => return dobytecode(vm, i),
            _ => {}
        }
        i += 1;
    }
    Ok(0)
}

fn handle_luainit(vm: &mut Vm) -> Status {
    match std::env::var_os("LUA_INIT") {
        None => 0,
        Some(init) => {
            use std::os::unix::ffi::OsStrExt;
            let init = init.as_bytes();
            if init.first() == Some(&b'@') { dofile(vm, Some(&init[1..])) } else { dostring(vm, init, b"=LUA_INIT") }
        }
    }
}

fn set_status(st: Status) {
    SMAIN.with(|s| s.borrow_mut().status = st);
}

fn pmain(vm: &mut Vm) -> LuaResult<usize> {
    GLOBAL_L.store(vm as *mut Vm, Ordering::SeqCst);
    let argv = SMAIN.with(|s| s.borrow().argv.clone());
    let mut flags = 0;
    let Some(argn) = collectargs(&argv, &mut flags) else {
        // Invalid args?
        print_usage();
        set_status(1);
        return Ok(0);
    };
    if flags & FLAGS_NOENV != 0 {
        let k = vm.new_str(b"LUA_NOENV");
        let _ = vm.registry.set_str(k, Value::Bool(true));
    }
    crate::stdlib::open_libs(vm)?;
    createargtable(vm, argn)?;
    if flags & FLAGS_NOENV == 0 {
        let st = handle_luainit(vm);
        set_status(st);
        if st != 0 {
            return Ok(0);
        }
    }
    if flags & FLAGS_VERSION != 0 {
        print_version();
    }
    let st = runargs(vm, argn)?;
    set_status(st);
    if st != 0 {
        return Ok(0);
    }
    if argv.len() > argn {
        let st = handle_script(vm, argn)?;
        set_status(st);
        if st != 0 {
            return Ok(0);
        }
    }
    if flags & FLAGS_INTERACTIVE != 0 {
        print_jit_status(vm)?;
        dotty(vm)?;
    } else if argv.len() == argn && flags & (FLAGS_EXEC | FLAGS_VERSION) == 0 {
        // SAFETY: isatty on fd 0.
        if unsafe { libc::isatty(0) } != 0 {
            print_version();
            print_jit_status(vm)?;
            dotty(vm)?;
        } else {
            dofile(vm, None); // Executes stdin as a file.
        }
    }
    Ok(0)
}

fn run(argv: Vec<Vec<u8>>) -> i32 {
    let progname = match argv.first() {
        Some(a) if !a.is_empty() => Some(a.clone()),
        _ => Some(LUA_PROGNAME.to_vec()),
    };
    let argv = if argv.is_empty() { vec![Vec::new()] } else { argv };
    SMAIN.with(|s| *s.borrow_mut() = Smain { argv, status: 0, progname });
    let mut vm = Vm::new_state();
    let pm = vm.native(pmain, FF_C, vec![]);
    vm.push(Value::Func(pm));
    let r = vm.pcall(0, 0, None).map(|_| ()).map_err(|e| e.value);
    let failed = r.is_err();
    report(&mut vm, r);
    flush_stdout();
    GLOBAL_L.store(std::ptr::null_mut(), Ordering::SeqCst);
    vm.close();
    flush_stdout();
    let status = SMAIN.with(|s| s.borrow().status);
    if failed || status > 0 { 1 } else { 0 }
}

/// Entry point: run the frontend on a thread with a large stack; returns the exit status.
pub fn main() -> i32 {
    use std::os::unix::ffi::OsStringExt;
    let argv: Vec<Vec<u8>> = std::env::args_os().map(OsStringExt::into_vec).collect();
    let h = std::thread::Builder::new().stack_size(256 << 20).spawn(move || run(argv)).expect("spawn");
    // Deliver SIGINT to the VM thread only (its signal handler installs the stop hook there).
    // SAFETY: blocking a signal in the calling thread.
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        libc::sigaddset(&mut set, libc::SIGINT);
        libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut());
    }
    h.join().unwrap_or(1)
}
