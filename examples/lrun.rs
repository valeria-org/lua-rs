// Throwaway runner (not committed): lrun file.lua | lrun -e code
use luajit::state::Vm;
use luajit::value::Value;
use luajit::error::LuaResult;

fn traceback(vm: &mut Vm) -> LuaResult<usize> {
    let msg = vm.arg(1);
    let m = vm.tostring_opt(msg);
    let th = vm.cur_thread;
    let tb = vm.traceback(th, m.as_deref(), 1);
    vm.push_str(&tb);
    Ok(1)
}
fn dbg_traceback(vm: &mut Vm) -> LuaResult<usize> {
    let (th, a) = match vm.arg(1) { Value::Thread(t) => (t, 2), _ => (vm.cur_thread, 1) };
    let msg = vm.arg(a);
    let m = vm.tostring_opt(msg);
    let lvl = if th == vm.cur_thread { 1 } else { 0 };
    let tb = vm.traceback(th, m.as_deref(), lvl);
    vm.push_str(&tb);
    Ok(1)
}
fn hookf(vm: &mut Vm, ev: luajit::vm::HookEvent, line: i32) -> LuaResult<()> {
    let k = vm.new_str(b"__hookfn");
    let f = vm.registry.get_str(k);
    if let Value::Func(_) = f {
        let name = vm.new_str(ev.name().as_bytes());
        let l = if line >= 0 { Value::Num(line as f64) } else { Value::Nil };
        vm.call_value(f, &[Value::Str(name), l], 0)?;
    }
    Ok(())
}
fn sethook(vm: &mut Vm) -> LuaResult<usize> {
    let k = vm.new_str(b"__hookfn");
    if vm.nargs() == 0 || vm.arg(1).is_nil() { vm.set_hook(None, 0, 0); return Ok(0); }
    let f = vm.arg(1);
    vm.registry.set_str(k, f).unwrap();
    let m = vm.check_str(2)?;
    let mut mask = 0;
    for &c in m.as_bytes() { match c { b'c' => mask |= 1, b'r' => mask |= 2, b'l' => mask |= 4, _ => {} } }
    let count = vm.opt_int(3, 0)?;
    if count > 0 { mask |= 8; }
    vm.set_hook(Some(hookf), mask, count);
    Ok(0)
}
fn getinfo(vm: &mut Vm) -> LuaResult<usize> {
    let what = vm.opt_str(2)?.map(|s| s.as_bytes().to_vec()).unwrap_or(b"flnSu".to_vec());
    let th = vm.cur_thread;
    let target = match vm.arg(1) {
        Value::Func(f) => luajit::vm::InfoTarget::Func(f),
        _ => { let l = vm.check_int(1)?; match vm.get_stack(th, l) { Some(fr) => luajit::vm::InfoTarget::Frame(fr), None => return Ok(0) } }
    };
    let Some(ar) = vm.get_info(target, &what) else { return Err(vm.arg_error(2, b"invalid option")) };
    let t = vm.empty_table();
    macro_rules! s { ($k:expr, $v:expr) => {{ let k = vm.new_str($k); t.set_str(k, $v).unwrap(); }} }
    if what.contains(&b'S') { let v = vm.new_str(&ar.source); s!(b"source", Value::Str(v)); let v = vm.new_str(&ar.short_src); s!(b"short_src", Value::Str(v)); s!(b"linedefined", Value::Num(ar.linedefined as f64)); s!(b"lastlinedefined", Value::Num(ar.lastlinedefined as f64)); let v = vm.new_str(ar.what.as_bytes()); s!(b"what", Value::Str(v)); }
    if what.contains(&b'l') { s!(b"currentline", Value::Num(ar.currentline as f64)); }
    if what.contains(&b'u') { s!(b"nups", Value::Num(ar.nups as f64)); }
    if what.contains(&b'n') { if let Some(n) = &ar.name { let v = vm.new_str(n); s!(b"name", Value::Str(v)); } let v = vm.new_str(ar.namewhat.as_bytes()); s!(b"namewhat", Value::Str(v)); }
    if let Some(f) = ar.func { s!(b"func", Value::Func(f)); }
    if let Some(a) = ar.activelines { s!(b"activelines", a); }
    vm.ret1(Value::Table(t))
}
fn getlocal(vm: &mut Vm) -> LuaResult<usize> {
    let l = vm.check_int(1)?; let n = vm.check_int(2)?;
    let th = vm.cur_thread;
    let Some(fr) = vm.get_stack(th, l) else { return Err(vm.arg_error(1, b"level out of range")) };
    match vm.get_local(fr, n) { Some((name, v)) => { vm.push_str(&name); vm.push(v); Ok(2) } None => vm.ret1(Value::Nil) }
}
fn main() {
    let child = std::thread::Builder::new().stack_size(256 << 20).spawn(|| {
        let args: Vec<String> = std::env::args().collect();
        let mut vm = Vm::new_state();
        luajit::stdlib::open_libs(&mut vm).unwrap();
        let tb = vm.native(dbg_traceback, 1, vec![]);
        let dbg = vm.empty_table();
        let k = vm.new_str(b"traceback");
        dbg.set_str(k, Value::Func(tb)).unwrap();
        for (n, f) in [(&b"sethook"[..], sethook as fn(&mut Vm) -> LuaResult<usize>), (b"getinfo", getinfo), (b"getlocal", getlocal)] {
            let f = vm.native(f, 1, vec![]); let k = vm.new_str(n); dbg.set_str(k, Value::Func(f)).unwrap();
        }
        let k = vm.new_str(b"debug");
        vm.globals.set_str(k, Value::Table(dbg)).unwrap();
        let (src, name) = if args[1] == "-e" { (args[2].clone().into_bytes(), b"=(command line)".to_vec()) } else { (std::fs::read(&args[1]).unwrap(), format!("@{}", args[1]).into_bytes()) };
        let h = vm.native(traceback, 1, vec![]);
        vm.push(Value::Func(h));
        let hslot = vm.cur_thread.top() - 1;
        let code = match vm.load(&src, &name, None) {
            Ok(f) => {
                vm.push(Value::Func(f));
                match vm.pcall(0, 0, Some(hslot)) {
                    Ok(_) => 0,
                    Err(e) => { let m = vm.tostring_opt(e.value).unwrap_or(b"(error object is not a string)".to_vec()); eprintln!("luajit: {}", String::from_utf8_lossy(&m)); 1 }
                }
            }
            Err(e) => { let m = vm.tostring_opt(e.value).unwrap_or_default(); eprintln!("luajit: {}", String::from_utf8_lossy(&m)); 1 }
        };
        unsafe { libc::fflush(luajit::stdlib::c_stdout()); }
        vm.close();
        unsafe { libc::fflush(luajit::stdlib::c_stdout()); }
        code
    }).unwrap();
    std::process::exit(child.join().unwrap());
}
