//! Interim minimal runner (`luajit file.lua` / `luajit -e code`); replaced by the luajit.c port.

use luajit::func::FF_C;
use luajit::state::Vm;
use luajit::value::Value;
use luajit::error::LuaResult;

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
    let h = vm.native(handler, FF_C, vec![]);
    vm.push(Value::Func(h));
    let hslot = vm.cur_thread.top() - 1;
    let f = vm.load(src.as_bytes(), name.as_bytes(), None)?;
    vm.push(Value::Func(f));
    vm.pcall(0, 0, Some(hslot))?;
    Ok(0)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (src, name) = match args.first().map(String::as_str) {
        Some("-e") => (args.get(1).cloned().unwrap_or_default().into_bytes(), b"=(command line)".to_vec()),
        Some(path) => match std::fs::read(path) {
            Ok(s) => (s, format!("@{path}").into_bytes()),
            Err(e) => {
                eprintln!("luajit: cannot open {path}: {e}");
                std::process::exit(1);
            }
        },
        None => {
            eprintln!("usage: luajit file.lua | -e code");
            std::process::exit(1);
        }
    };
    let code = std::thread::Builder::new()
        .stack_size(256 << 20)
        .spawn(move || {
            let mut vm = Vm::new_state();
            luajit::stdlib::open_libs(&mut vm).expect("open_libs");
            let sk = vm.new_str(&src);
            let ck = vm.new_str(&name);
            let pm = vm.native(pmain, FF_C, vec![Value::Str(sk), Value::Str(ck)]);
            vm.push(Value::Func(pm));
            let code = match vm.pcall(0, 0, None) {
                Ok(_) => 0,
                Err(e) => {
                    let m = vm.tostring_opt(e.value).unwrap_or_default();
                    eprintln!("luajit: {}", String::from_utf8_lossy(&m));
                    1
                }
            };
            vm.close();
            code
        })
        .expect("spawn")
        .join()
        .unwrap_or(1);
    std::process::exit(code);
}
