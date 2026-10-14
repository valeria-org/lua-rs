//! Runs Lua programs on the VM with the base and coroutine libraries and compares their
//! output with the reference binary's (`tests/fixtures/vm/*.expected`, generated with
//! `luajit file.lua 2>/dev/null`, bottom `[C]: at 0x...` traceback lines removed).
//!
//! The harness provides a capturing `print` and a minimal test-only `debug` table
//! (`traceback`, `getinfo`, `getlocal`, `sethook`) built on the VM's debug API.

use std::cell::RefCell;
use std::path::Path;

use luajit::error::LuaResult;
use luajit::state::Vm;
use luajit::value::Value;
use luajit::vm::{HookEvent, InfoTarget};

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
    let (th, a) = match vm.arg(1) {
        Value::Thread(t) => (t, 2),
        _ => (vm.cur_thread, 1),
    };
    let msg = vm.arg(a);
    if !msg.is_nil() && vm.tostring_opt(msg).is_none() {
        return vm.ret1(msg);
    }
    let m = vm.tostring_opt(msg);
    let lvl = if th == vm.cur_thread { 1 } else { 0 };
    let tb = vm.traceback(th, m.as_deref(), lvl);
    vm.push_str(&tb);
    Ok(1)
}

fn hookf(vm: &mut Vm, ev: HookEvent, line: i32) -> LuaResult<()> {
    let k = vm.new_str(b"__hookfn");
    let f = vm.registry.get_str(k);
    if let Value::Func(_) = f {
        let name = vm.new_str(ev.name().as_bytes());
        let l = if line >= 0 {
            Value::Num(line as f64)
        } else {
            Value::Nil
        };
        vm.call_value(f, &[Value::Str(name), l], 0)?;
    }
    Ok(())
}

fn sethook(vm: &mut Vm) -> LuaResult<usize> {
    let k = vm.new_str(b"__hookfn");
    if vm.nargs() == 0 || vm.arg(1).is_nil() {
        vm.set_hook(None, 0, 0);
        return Ok(0);
    }
    let f = vm.arg(1);
    vm.registry.set_str(k, f).unwrap();
    let m = vm.check_str(2)?;
    let mut mask = 0;
    for &c in m.as_bytes() {
        match c {
            b'c' => mask |= luajit::vm::LUA_MASKCALL,
            b'r' => mask |= luajit::vm::LUA_MASKRET,
            b'l' => mask |= luajit::vm::LUA_MASKLINE,
            _ => {}
        }
    }
    let count = vm.opt_int(3, 0)?;
    if count > 0 {
        mask |= luajit::vm::LUA_MASKCOUNT;
    }
    vm.set_hook(Some(hookf), mask, count);
    Ok(0)
}

fn getinfo(vm: &mut Vm) -> LuaResult<usize> {
    let what = vm
        .opt_str(2)?
        .map(|s| s.as_bytes().to_vec())
        .unwrap_or(b"flnSu".to_vec());
    let th = vm.cur_thread;
    let target = match vm.arg(1) {
        Value::Func(f) => InfoTarget::Func(f),
        _ => {
            let l = vm.check_int(1)?;
            match vm.get_stack(th, l) {
                Some(fr) => InfoTarget::Frame(fr),
                None => return Ok(0),
            }
        }
    };
    let Some(ar) = vm.get_info(target, &what) else {
        return Err(vm.arg_error(2, b"invalid option"));
    };
    let t = vm.empty_table();
    let set = |vm: &mut Vm, k: &[u8], v: Value| {
        let k = vm.new_str(k);
        t.set_str(k, v).unwrap();
    };
    if what.contains(&b'S') {
        let v = vm.new_str(&ar.source);
        set(vm, b"source", Value::Str(v));
        let v = vm.new_str(&ar.short_src);
        set(vm, b"short_src", Value::Str(v));
        set(vm, b"linedefined", Value::Num(ar.linedefined as f64));
        set(
            vm,
            b"lastlinedefined",
            Value::Num(ar.lastlinedefined as f64),
        );
        let v = vm.new_str(ar.what.as_bytes());
        set(vm, b"what", Value::Str(v));
    }
    if what.contains(&b'l') {
        set(vm, b"currentline", Value::Num(ar.currentline as f64));
    }
    if what.contains(&b'u') {
        set(vm, b"nups", Value::Num(ar.nups as f64));
        set(vm, b"nparams", Value::Num(ar.nparams as f64));
        set(vm, b"isvararg", Value::Bool(ar.isvararg));
    }
    if what.contains(&b'n') {
        if let Some(n) = &ar.name {
            let v = vm.new_str(n);
            set(vm, b"name", Value::Str(v));
        }
        let v = vm.new_str(ar.namewhat.as_bytes());
        set(vm, b"namewhat", Value::Str(v));
    }
    if let Some(f) = ar.func {
        set(vm, b"func", Value::Func(f));
    }
    if let Some(a) = ar.activelines {
        set(vm, b"activelines", a);
    }
    vm.ret1(Value::Table(t))
}

fn getlocal(vm: &mut Vm) -> LuaResult<usize> {
    let l = vm.check_int(1)?;
    let n = vm.check_int(2)?;
    let th = vm.cur_thread;
    let Some(fr) = vm.get_stack(th, l) else {
        return Err(vm.arg_error(1, b"level out of range"));
    };
    match vm.get_local(fr, n) {
        Some((name, v)) => {
            vm.push_str(&name);
            vm.push(v);
            Ok(2)
        }
        None => vm.ret1(Value::Nil),
    }
}

fn handler(vm: &mut Vm) -> LuaResult<usize> {
    let v = vm.arg(1);
    let m = vm
        .tostring_opt(v)
        .unwrap_or_else(|| b"(error object is not a string)".to_vec());
    let th = vm.cur_thread;
    let tb = vm.traceback(th, Some(&m), 1);
    vm.push_str(&tb);
    Ok(1)
}

fn pmain(vm: &mut Vm) -> LuaResult<usize> {
    let (Value::Str(src), Value::Str(name)) = (vm.upvalue(1), vm.upvalue(2)) else {
        unreachable!()
    };
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
            let dbg = vm.empty_table();
            let fns: [(&[u8], luajit::func::NativeFn); 4] = [
                (b"traceback", traceback),
                (b"getinfo", getinfo),
                (b"getlocal", getlocal),
                (b"sethook", sethook),
            ];
            for (n, f) in fns {
                let f = vm.native(f, luajit::func::FF_C, vec![]);
                let k = vm.new_str(n);
                dbg.set_str(k, Value::Func(f)).unwrap();
            }
            let k = vm.new_str(b"debug");
            vm.globals.set_str(k, Value::Table(dbg)).unwrap();
            // Like luajit.c: the chunk runs inside a C function (`pmain`) called by
            // `lua_cpcall`, and `docall` uses a traceback handler.
            let sk = vm.new_str(&src);
            let ck = vm.new_str(&chunkname);
            let pm = vm.native(
                pmain,
                luajit::func::FF_C,
                vec![Value::Str(sk), Value::Str(ck)],
            );
            vm.push(Value::Func(pm));
            if let Err(e) = vm.pcall(0, 0, None) {
                let m = vm.tostring_opt(e.value).unwrap_or_default();
                out(b"ERROR: ");
                out(&m);
                out(b"\n");
            }
            vm.close();
            OUT.with(|o| {
                let s = String::from_utf8_lossy(&o.borrow()).into_owned();
                // The bottom C frame's address differs between runs; the expectations omit it.
                s.lines()
                    .filter(|l| !l.starts_with("\t[C]: at 0x"))
                    .map(|l| format!("{l}\n"))
                    .collect()
            })
        })
        .unwrap()
        .join()
        .unwrap()
}

fn run_str(src: &str) -> String {
    run_lua(src.as_bytes().to_vec(), b"=test".to_vec())
}

fn fixture(name: &str) {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/vm");
    let src = std::fs::read(dir.join(format!("{name}.lua"))).unwrap();
    let expected = std::fs::read_to_string(dir.join(format!("{name}.expected"))).unwrap();
    // The fixtures are run from their directory by name, like `luajit name.lua`.
    let got = run_lua(src, format!("@{name}.lua").into_bytes());
    if got != expected {
        let (g, e): (Vec<_>, Vec<_>) = (got.lines().collect(), expected.lines().collect());
        for i in 0..g.len().max(e.len()) {
            if g.get(i) != e.get(i) {
                panic!(
                    "{name}: first difference at line {}:\n  got:      {:?}\n  expected: {:?}",
                    i + 1,
                    g.get(i),
                    e.get(i)
                );
            }
        }
        panic!("{name}: outputs differ");
    }
}

#[test]
fn all_opcodes() {
    fixture("ops");
}
#[test]
fn syntax_extensions() {
    fixture("syn");
}
#[test]
fn error_messages_with_variable_names() {
    fixture("errs");
}
#[test]
fn metamethods() {
    fixture("meta");
}
#[test]
fn coroutines_and_yield_across_metamethods() {
    fixture("coro");
}
#[test]
fn pcall_error_levels_and_coroutine_errors() {
    fixture("pcall_coro");
}
#[test]
fn varargs_tailcalls_upvalues_fenv() {
    fixture("misc");
}
#[test]
fn debug_info_and_tracebacks() {
    fixture("dbg");
}
#[test]
fn finalizers_and_weak_tables() {
    fixture("gc");
}
#[test]
fn stack_overflow() {
    fixture("stackov");
}
#[test]
fn hooks_names() {
    fixture("hook_names");
}
#[test]
fn hooks_lines() {
    fixture("hook_lines");
}
#[test]
fn hooks_calls() {
    fixture("hook_calls");
}
#[test]
fn base_library() {
    fixture("base");
}

#[test]
fn deep_tail_recursion_keeps_stack_small() {
    let out = run_str(
        "local function t(n) if n == 0 then return collectgarbage('count') end return t(n - 1) end \
         local function d(n) if n == 0 then return 0 end return 1 + d(n - 1) end \
         print(t(1000000) < 1000, d(10000))",
    );
    assert_eq!(out, "true\t10000\n");
}

#[test]
fn close_runs_pending_finalizers() {
    let out = run_str(
        "local u = newproxy(true) getmetatable(u).__gc = function() print('fin') end KEEP = u print('end')",
    );
    assert_eq!(out, "end\nfin\n");
}

#[test]
fn yield_across_c_boundary() {
    let out = run_str(
        "print(pcall(coroutine.yield)) \
         print(coroutine.resume(coroutine.create(function() return load(function() coroutine.yield() end) end)))",
    );
    assert_eq!(
        out,
        "false\tattempt to yield across C-call boundary\n\
         true\tnil\ttest:1: attempt to yield across C-call boundary\n"
    );
}

#[test]
fn c_stack_overflow_is_an_error() {
    let out = run_str(
        "local function f() return coroutine.wrap(f)() end \
         local ok, e = pcall(f) print(ok, e)",
    );
    assert!(out.starts_with("false\t"), "{out}");
    assert!(out.contains("C stack overflow"), "{out}");
}

/// The language tests of the upstream test suite that only need the base and coroutine
/// libraries.
#[test]
#[ignore = "requires the upstream test suite; set LUAJIT_TEST_DIR to its test directory"]
fn upstream_lang_tests() {
    let root = std::env::var_os("LUAJIT_TEST_DIR")
        .expect("set LUAJIT_TEST_DIR to the upstream test directory");
    let dir = Path::new(&root).join("lang");
    assert!(
        dir.is_dir(),
        "upstream language tests not found at {}",
        dir.display()
    );
    let files = [
        "andor.lua",
        "assignment.lua",
        "compare_nan.lua",
        "coroutine.lua",
        "for.lua",
        "gc.lua",
        "length.lua",
        "self.lua",
        "table.lua",
        "tail_recursion.lua",
        "vararg_jit.lua",
        "constant/number.lua",
        "constant/table.lua",
        "meta/arith.lua",
        "meta/arith_jit.lua",
        "meta/cat.lua",
        "meta/comp.lua",
        "meta/eq.lua",
        "meta/framegap.lua",
        "meta/newindex.lua",
        "meta/nomm.lua",
        "upvalue/closure.lua",
    ];
    for f in files {
        let src = std::fs::read(dir.join(f)).unwrap();
        let out = run_lua(src, format!("@{f}").into_bytes());
        assert!(!out.contains("ERROR: "), "{f}: {out}");
    }
}
