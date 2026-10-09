//! Runs the FFI fixture programs (`tests/fixtures/ffi/*.lua`) and compares their output with
//! the reference binary's (`*.expected`, generated with
//! `luajit file.lua 2>&1 | sed -E 's/0x[0-9a-f]+/0xADDR/g' | grep -v '^\t\[C\]: at 0xADDR'`).
//!
//! The harness provides a capturing `print`, `debug.traceback` and, if the package library is
//! not available, a minimal `require` that resolves `package.loaded`/`package.preload` entries
//! from the registry (`ffi` is preloaded by `open_libs`, as in upstream lib_init.c).

use std::cell::RefCell;
use std::path::Path;

use luajit::error::LuaResult;
use luajit::state::Vm;
use luajit::value::Value;

thread_local! {
    static OUT: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

fn out(b: &[u8]) {
    OUT.with(|o| o.borrow_mut().extend_from_slice(b));
}

fn cap_print(vm: &mut Vm) -> LuaResult<usize> {
    let n = vm.nargs();
    let k = vm.new_str(b"tostring");
    let tostring = vm.globals.get_str(k);
    for i in 1..=n {
        let v = vm.arg(i);
        vm.call_value(tostring, &[v], 1)?;
        let r = vm.pop();
        let s = vm.tostring_opt(r).expect("tostring result");
        if i > 1 {
            out(b"\t");
        }
        out(&s);
    }
    out(b"\n");
    Ok(0)
}

fn traceback(vm: &mut Vm) -> LuaResult<usize> {
    let msg = vm.arg(1);
    if !msg.is_nil() && vm.tostring_opt(msg).is_none() {
        return vm.ret1(msg);
    }
    let m = vm.tostring_opt(msg);
    let th = vm.cur_thread;
    let tb = vm.traceback(th, m.as_deref(), 1);
    vm.push_str(&tb);
    Ok(1)
}

fn require(vm: &mut Vm) -> LuaResult<usize> {
    let name = vm.check_str(1)?;
    for reg in [&b"_LOADED"[..], b"_PRELOAD"] {
        let k = vm.new_str(reg);
        if let Value::Table(t) = vm.registry.get_str(k) {
            let v = t.get_str(name);
            if v.is_nil() {
                continue;
            }
            if reg == b"_LOADED" {
                return vm.ret1(v);
            }
            vm.call_value(v, &[Value::Str(name)], 1)?;
            return Ok(1);
        }
    }
    Err(vm.rt_error(format!("module '{}' not found", name.to_string_lossy()).as_bytes()))
}

fn set_global(vm: &mut Vm, name: &[u8], v: Value) {
    let k = vm.new_str(name);
    vm.globals.set_str(k, v).unwrap();
}

fn pmain(vm: &mut Vm) -> LuaResult<usize> {
    let (Value::Str(src), Value::Str(name)) = (vm.upvalue(1), vm.upvalue(2)) else { unreachable!() };
    let f = vm.load(src.as_bytes(), name.as_bytes(), None)?;
    vm.push(Value::Func(f));
    vm.call(0, 0)?;
    Ok(0)
}

/// Replace every `0x<hex digits>` by `0xADDR`.
fn normalize(s: &str) -> String {
    let b = s.as_bytes();
    let mut o = String::with_capacity(s.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'0' && b.get(i + 1) == Some(&b'x') && b.get(i + 2).is_some_and(|c| c.is_ascii_hexdigit()) {
            let mut j = i + 2;
            while j < b.len() && (b[j].is_ascii_digit() || (b'a'..=b'f').contains(&b[j])) {
                j += 1;
            }
            o.push_str("0xADDR");
            i = j;
        } else {
            let c = s[i..].chars().next().unwrap();
            o.push(c);
            i += c.len_utf8();
        }
    }
    o
}

fn run_lua(src: Vec<u8>, chunkname: Vec<u8>) -> String {
    std::thread::Builder::new()
        .stack_size(256 << 20)
        .spawn(move || {
            OUT.with(|o| o.borrow_mut().clear());
            let mut vm = Vm::new_state();
            luajit::stdlib::open_libs(&mut vm).unwrap();
            let ts = vm.new_str(b"tostring");
            let p = vm.native(cap_print, luajit::vm::ffid::FF_PRINT, vec![Value::Str(ts)]);
            set_global(&mut vm, b"print", Value::Func(p));
            let dk = vm.new_str(b"debug");
            let dbg = match vm.globals.get_str(dk) {
                Value::Table(t) => t,
                _ => {
                    let t = vm.empty_table();
                    set_global(&mut vm, b"debug", Value::Table(t));
                    t
                }
            };
            let tb = vm.native(traceback, luajit::func::FF_C, vec![]);
            let k = vm.new_str(b"traceback");
            dbg.set_str(k, Value::Func(tb)).unwrap();
            let rk = vm.new_str(b"require");
            if vm.globals.get_str(rk).is_nil() {
                let r = vm.native(require, luajit::func::FF_C, vec![]);
                set_global(&mut vm, b"require", Value::Func(r));
            }
            let sk = vm.new_str(&src);
            let ck = vm.new_str(&chunkname);
            let pm = vm.native(pmain, luajit::func::FF_C, vec![Value::Str(sk), Value::Str(ck)]);
            vm.push(Value::Func(pm));
            if let Err(e) = vm.pcall(0, 0, None) {
                let m = vm.tostring_opt(e.value).unwrap_or_default();
                out(b"ERROR: ");
                out(&m);
                out(b"\n");
            }
            vm.close();
            OUT.with(|o| normalize(&String::from_utf8_lossy(&o.borrow())))
                .lines()
                .filter(|l| !l.starts_with("\t[C]: at 0xADDR"))
                .map(|l| format!("{l}\n"))
                .collect()
        })
        .unwrap()
        .join()
        .unwrap()
}

fn run_fixture(name: &str) {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/ffi");
    let src = std::fs::read(dir.join(format!("{name}.lua"))).unwrap();
    let expected = std::fs::read_to_string(dir.join(format!("{name}.expected"))).unwrap();
    let got = run_lua(src, format!("@{name}.lua").into_bytes());
    if got != expected {
        let (g, e): (Vec<_>, Vec<_>) = (got.lines().collect(), expected.lines().collect());
        for i in 0..g.len().max(e.len()) {
            let (a, b) = (g.get(i).copied().unwrap_or("<eof>"), e.get(i).copied().unwrap_or("<eof>"));
            if a != b {
                panic!("{name}: first difference at line {}:\n  got:      {a}\n  expected: {b}", i + 1);
            }
        }
    }
}

/// Lua ↔ C conversions: every numeric type, bool/enum/string rules, casts, arrays, strings,
/// structs/unions/bitfields/VLS initializers and their error messages.
#[test]
fn conversions() {
    run_fixture("conv");
}

/// 64 bit integer arithmetic and comparisons with promotion rules, pointer arithmetic,
/// differences and comparisons, enum operands, arithmetic error messages, tostring forms.
#[test]
fn arithmetic_and_tostring() {
    run_fixture("arith");
}

/// C calls (varargs, struct-by-value results, complex, errno), callbacks through qsort,
/// callback set/free, errors raised in callbacks, `bad callback`, clib errors.
#[test]
fn calls_and_callbacks() {
    run_fixture("calls");
}

/// `ffi.metatype` (all metamethods, `__new`, `__gc`), `ffi.gc` finalizers, type
/// parameters, istype, sizeof/alignof/offsetof, struct constants, copy/fill/string, load.
#[test]
fn metatype_gc_and_library() {
    run_fixture("meta");
}

/// Errors raised by FFI metamethods: positions and the removed metamethod frame.
#[test]
fn metamethod_error_tracebacks() {
    run_fixture("traceback");
}

/// Error messages with and without caller positions (`cannot convert`, `too many
/// initializers`, `has no member named`, `is not callable`, missing declarations, ...).
#[test]
fn error_messages() {
    run_fixture("errors");
}

/// Yield across a callback, errors with non-string values, nested callbacks, callback slot
/// exhaustion and reuse, GC stress with cdata.
#[test]
fn callback_edge_cases() {
    run_fixture("callbacks");
}

/// The upstream FFI tests (`lib/ffi/*.lua` of the LuaJIT test suite) that only need the
/// libraries present in this crate state.
#[test]
fn upstream_ffi_tests() {
    let dir = Path::new(env!("HOME")).join("projects/luajit-ref/tests/test/lib/ffi");
    if !dir.is_dir() {
        return;
    }
    let mut failed = Vec::new();
    for name in ["cdata_var", "copy_fill", "istype", "jit_complex", "jit_misc", "jit_struct", "redir", "type_punning"] {
        let path = dir.join(format!("{name}.lua"));
        let src = std::fs::read(&path).unwrap();
        let got = run_lua(src, format!("@{name}.lua").into_bytes());
        if !got.is_empty() {
            failed.push(format!("{name}: {got}"));
        }
    }
    assert!(failed.is_empty(), "failed: {failed:#?}");
}
