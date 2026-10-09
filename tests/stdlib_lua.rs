//! Runs Lua programs exercising the string, table, math, bit and string.buffer libraries and
//! compares their output with the reference binary's (`tests/fixtures/stdlib/*.expected`,
//! generated with `luajit file.lua 2>/dev/null` in that directory).
//!
//! The harness provides a capturing `print` and, while the package library is not part of
//! the build, a test-only `require` that resolves `package.loaded` / `package.preload`
//! entries (`registry._LOADED` / `registry._PRELOAD`), like the package library's first
//! searcher. A real `require`, if present, is left alone.

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

/// Test-only `require`: `_LOADED[name]`, else call `_PRELOAD[name](name)` and cache it.
fn preload_require(vm: &mut Vm) -> LuaResult<usize> {
    let name = vm.check_str(1)?;
    let loaded = luajit::stdlib::loaded_table(vm);
    let v = loaded.get_str(name);
    if !v.is_nil() {
        return vm.ret1(v);
    }
    let pre = luajit::stdlib::libinit::preload_table(vm)?;
    let f = pre.get_str(name);
    if f.is_nil() {
        let m = format!("module '{}' not found", name.to_string_lossy());
        return Err(vm.rt_error(m.as_bytes()));
    }
    vm.call_value(f, &[Value::Str(name)], 1)?;
    let mut r = vm.pop();
    if r.is_nil() {
        r = Value::Bool(true);
    }
    let cached = loaded.get_str(name);
    if cached.is_nil() {
        loaded.set_str(name, r).unwrap();
        vm.ret1(r)
    } else {
        vm.ret1(cached)
    }
}

fn handler(vm: &mut Vm) -> LuaResult<usize> {
    let v = vm.arg(1);
    let m = vm.tostring_opt(v).unwrap_or_else(|| b"(error object is not a string)".to_vec());
    let th = vm.cur_thread;
    let tb = vm.traceback(th, Some(&m), 1);
    vm.push_str(&tb);
    Ok(1)
}

fn pmain(vm: &mut Vm) -> LuaResult<usize> {
    let (Value::Str(src), Value::Str(name)) = (vm.upvalue(1), vm.upvalue(2)) else { unreachable!() };
    let h = vm.native(handler, luajit::func::FF_C, vec![]);
    vm.push(Value::Func(h));
    let hslot = vm.cur_thread.top() - 1;
    let f = vm.load(src.as_bytes(), name.as_bytes(), None)?;
    vm.push(f);
    vm.pcall(0, 0, Some(hslot))?;
    Ok(0)
}

/// Run a chunk; returns captured output plus `ERROR: ...` for an uncaught error.
fn run_lua(src: Vec<u8>, chunkname: Vec<u8>) -> String {
    std::thread::Builder::new()
        .stack_size(256 << 20)
        .spawn(move || {
            OUT.with(|o| o.borrow_mut().clear());
            let mut vm = Vm::new_state();
            luajit::stdlib::open_libs(&mut vm).unwrap();
            let ts = vm.new_str(b"tostring");
            let p = vm.native(cap_print, luajit::vm::ffid::FF_PRINT, vec![Value::Str(ts)]);
            let k = vm.new_str(b"print");
            vm.globals.set_str(k, Value::Func(p)).unwrap();
            let k = vm.new_str(b"require");
            if vm.globals.get_str(k).is_nil() {
                let rq = vm.native(preload_require, luajit::func::FF_C, vec![]);
                vm.globals.set_str(k, Value::Func(rq)).unwrap();
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
            OUT.with(|o| String::from_utf8_lossy(&o.borrow()).into_owned())
        })
        .unwrap()
        .join()
        .unwrap()
}

fn fixture(name: &str) {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/stdlib");
    let src = std::fs::read(dir.join(format!("{name}.lua"))).unwrap();
    let expected = String::from_utf8_lossy(&std::fs::read(dir.join(format!("{name}.expected"))).unwrap()).into_owned();
    let got = run_lua(src, format!("@{name}.lua").into_bytes());
    if got != expected {
        let (g, e): (Vec<_>, Vec<_>) = (got.lines().collect(), expected.lines().collect());
        for i in 0..g.len().max(e.len()) {
            if g.get(i) != e.get(i) {
                panic!("{name}: first difference at line {}:\n  got:      {:?}\n  expected: {:?}", i + 1, g.get(i), e.get(i));
            }
        }
        panic!("{name}: outputs differ");
    }
}

#[test]
fn string_library() {
    fixture("string");
}

#[test]
fn string_format() {
    fixture("format");
}

#[test]
fn table_library() {
    fixture("table");
}

#[test]
fn math_library() {
    fixture("math");
}

#[test]
fn bit_library() {
    fixture("bit");
}

#[test]
fn string_buffer() {
    fixture("buffer");
}

#[test]
fn string_buffer_growth() {
    fixture("buffer_growth");
}

#[test]
fn bit_library_cdata() {
    fixture("bit_ffi");
}

#[test]
fn string_buffer_cdata() {
    fixture("buffer_ffi");
}

#[test]
fn string_format_cdata() {
    fixture("format_ffi");
}

/// Library table contents, Lua-coded builtins and builtin ids follow upstream's registration.
#[test]
fn library_layout() {
    fixture("layout");
}
