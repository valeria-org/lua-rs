//! Debug library. Port of lib_debug.c on top of the VM's debug/hook API (`vm::debug`,
//! `vm::hook`).

use std::io::Write;

use crate::error::{ErrorKind, LuaResult};
use crate::gc::Gc;
use crate::parse::lex::cstr;
use crate::state::Vm;
use crate::table::Table;
use crate::thread::Thread;
use crate::value::{VType, Value};
use crate::vm::ffid::*;
use crate::vm::{
    ErrMsg, HookEvent, InfoTarget, LUA_MASKCALL, LUA_MASKCOUNT, LUA_MASKLINE, LUA_MASKRET, num2int,
};

use super::{LibFn, register_lib};

/// `luaopen_debug`.
pub fn open_debug(vm: &mut Vm) -> LuaResult<()> {
    let funcs: &[LibFn] = &[
        ("getregistry", lj_cf_debug_getregistry, FF_DEBUG_GETREGISTRY),
        (
            "getmetatable",
            lj_cf_debug_getmetatable,
            FF_DEBUG_GETMETATABLE,
        ),
        (
            "setmetatable",
            lj_cf_debug_setmetatable,
            FF_DEBUG_SETMETATABLE,
        ),
        ("getfenv", lj_cf_debug_getfenv, FF_DEBUG_GETFENV),
        ("setfenv", lj_cf_debug_setfenv, FF_DEBUG_SETFENV),
        ("getinfo", lj_cf_debug_getinfo, FF_DEBUG_GETINFO),
        ("getlocal", lj_cf_debug_getlocal, FF_DEBUG_GETLOCAL),
        ("setlocal", lj_cf_debug_setlocal, FF_DEBUG_SETLOCAL),
        ("getupvalue", lj_cf_debug_getupvalue, FF_DEBUG_GETUPVALUE),
        ("setupvalue", lj_cf_debug_setupvalue, FF_DEBUG_SETUPVALUE),
        ("upvalueid", lj_cf_debug_upvalueid, FF_DEBUG_UPVALUEID),
        ("upvaluejoin", lj_cf_debug_upvaluejoin, FF_DEBUG_UPVALUEJOIN),
        ("sethook", lj_cf_debug_sethook, FF_DEBUG_SETHOOK),
        ("gethook", lj_cf_debug_gethook, FF_DEBUG_GETHOOK),
        ("debug", lj_cf_debug_debug, FF_DEBUG_DEBUG),
        ("traceback", lj_cf_debug_traceback, FF_DEBUG_TRACEBACK),
    ];
    register_lib(vm, "debug", funcs)?;
    Ok(())
}

fn lj_cf_debug_getregistry(vm: &mut Vm) -> LuaResult<usize> {
    let r = vm.registry;
    vm.ret1(Value::Table(r))
}

fn lj_cf_debug_getmetatable(vm: &mut Vm) -> LuaResult<usize> {
    let o = vm.check_any(1)?;
    let mt = vm.metatable_of(o);
    vm.ret1(mt)
}

/// `lua_setmetatable(L, 1)` with the metatable `mt`.
fn set_metatable(vm: &mut Vm, o: Value, mt: Option<Gc<Table>>) {
    match o {
        Value::Table(t) => t.set_metatable(mt),
        Value::Udata(u) => u.set_metatable(mt),
        Value::Bool(_) => vm.set_basemt(VType::Bool, mt),
        o => vm.set_basemt(o.vtype(), mt),
    }
}

fn lj_cf_debug_setmetatable(vm: &mut Vm) -> LuaResult<usize> {
    let mt = vm.check_tab_or_nil(2)?;
    vm.set_top(2);
    let o = vm.arg(1);
    set_metatable(vm, o, mt);
    vm.set_top(1);
    vm.set_arg(1, Value::Bool(true));
    Ok(1)
}

fn lj_cf_debug_getfenv(vm: &mut Vm) -> LuaResult<usize> {
    let o = vm.check_any(1)?;
    let env = match o {
        Value::Func(f) => Value::Table(f.env()),
        Value::Udata(u) => Value::Table(u.env()),
        Value::Thread(t) => Value::Table(t.env()),
        _ => Value::Nil,
    };
    vm.ret1(env)
}

fn lj_cf_debug_setfenv(vm: &mut Vm) -> LuaResult<usize> {
    let t = vm.check_tab(2)?;
    vm.set_top(2);
    match vm.arg(1) {
        Value::Func(f) => f.set_env(t),
        Value::Udata(u) => u.set_env(t),
        Value::Thread(th) => th.set_env(t),
        _ => return Err(vm.err_caller(ErrMsg::SETFENV)),
    }
    vm.set_top(1);
    Ok(1)
}

// -- Info, locals, upvalues -------------------------------------------------------------------

/// `getthread`: optional leading thread argument; returns the thread and the argument offset.
fn getthread(vm: &Vm) -> (Gc<Thread>, usize) {
    match vm.arg(1) {
        Value::Thread(t) if vm.nargs() >= 1 => (t, 1),
        _ => (vm.cur_thread, 0),
    }
}

fn set_field(vm: &mut Vm, t: Gc<Table>, k: &str, v: Value) -> LuaResult<()> {
    let k = vm.new_str(k.as_bytes());
    vm.setindex(Value::Table(t), Value::Str(k), v)
}

fn str_value(vm: &mut Vm, s: &[u8]) -> Value {
    Value::Str(vm.new_str(cstr(s)))
}

fn lj_cf_debug_getinfo(vm: &mut Vm) -> LuaResult<usize> {
    let (th, arg) = getthread(vm);
    let options = match vm.opt_str(arg + 2)? {
        Some(s) => cstr(s.as_bytes()).to_vec(),
        None => b"flnSu".to_vec(),
    };
    let a1 = vm.arg(arg + 1);
    let target = if arg < vm.nargs() && vm.tonumber(a1).is_some() {
        let level = num2int(vm.tonumber(a1).unwrap_or(0.0));
        match vm.get_stack(th, level) {
            Some(fr) => InfoTarget::Frame(fr),
            None => return vm.ret1(Value::Nil),
        }
    } else if let Value::Func(f) = a1
        && arg < vm.nargs()
    {
        InfoTarget::Func(f)
    } else {
        return Err(vm.arg_error_em(arg as i32 + 1, ErrMsg::NOFUNCL));
    };
    let Some(ar) = vm.get_info(target, &options) else {
        return Err(vm.arg_error_em(arg as i32 + 2, ErrMsg::INVOPT));
    };
    let t = vm.new_table(0, crate::table::hsize2hbits(16))?;
    vm.push(Value::Table(t));
    if let Some(Value::Table(al)) = ar.activelines {
        // Keep the active-lines table reachable while the result table is filled.
        vm.push(Value::Table(al));
    }
    let (mut opt_f, mut opt_l) = (false, false);
    for &c in &options {
        match c {
            b'S' => {
                let v = str_value(vm, &ar.source);
                set_field(vm, t, "source", v)?;
                let v = str_value(vm, &ar.short_src);
                set_field(vm, t, "short_src", v)?;
                set_field(vm, t, "linedefined", Value::Num(ar.linedefined as f64))?;
                set_field(
                    vm,
                    t,
                    "lastlinedefined",
                    Value::Num(ar.lastlinedefined as f64),
                )?;
                let v = str_value(vm, ar.what.as_bytes());
                set_field(vm, t, "what", v)?;
            }
            b'l' => set_field(vm, t, "currentline", Value::Num(ar.currentline as f64))?,
            b'u' => {
                set_field(vm, t, "nups", Value::Num(ar.nups as f64))?;
                set_field(vm, t, "nparams", Value::Num(ar.nparams as f64))?;
                set_field(vm, t, "isvararg", Value::Bool(ar.isvararg))?;
            }
            b'n' => {
                let v = match &ar.name {
                    Some(n) => str_value(vm, n),
                    None => Value::Nil,
                };
                set_field(vm, t, "name", v)?;
                let v = str_value(vm, ar.namewhat.as_bytes());
                set_field(vm, t, "namewhat", v)?;
            }
            b'f' => opt_f = true,
            b'L' => opt_l = true,
            _ => {}
        }
    }
    if opt_l {
        set_field(vm, t, "activelines", ar.activelines.unwrap_or(Value::Nil))?;
    }
    if opt_f {
        set_field(vm, t, "func", ar.func.map_or(Value::Nil, Value::Func))?;
    }
    vm.ret1(Value::Table(t))
}

fn lj_cf_debug_getlocal(vm: &mut Vm) -> LuaResult<usize> {
    let (th, arg) = getthread(vm);
    let slot = vm.check_int(arg + 2)?;
    if let Value::Func(f) = vm.arg(arg + 1)
        && arg < vm.nargs()
    {
        vm.set_top(arg + 1);
        let name = vm.local_name_of(f, slot);
        let v = match name {
            Some(n) => str_value(vm, &n),
            None => Value::Nil,
        };
        return vm.ret1(v);
    }
    let level = vm.check_int(arg + 1)?;
    let Some(fr) = vm.get_stack(th, level) else {
        return Err(vm.arg_error_em(arg as i32 + 1, ErrMsg::LVLRNG));
    };
    match vm.get_local(fr, slot) {
        Some((name, v)) => {
            vm.push(v);
            let n = str_value(vm, &name);
            vm.push(n);
            vm.push(v);
            Ok(2)
        }
        None => vm.ret1(Value::Nil),
    }
}

fn lj_cf_debug_setlocal(vm: &mut Vm) -> LuaResult<usize> {
    let (th, arg) = getthread(vm);
    let level = vm.check_int(arg + 1)?;
    let Some(fr) = vm.get_stack(th, level) else {
        return Err(vm.arg_error_em(arg as i32 + 1, ErrMsg::LVLRNG));
    };
    let v = vm.check_any(arg + 3)?;
    let n = vm.check_int(arg + 2)?;
    let name = vm.set_local(fr, n, v);
    let r = match name {
        Some(n) => str_value(vm, &n),
        None => Value::Nil,
    };
    vm.ret1(r)
}

/// Shared part of `getupvalue`/`setupvalue`.
fn debug_getupvalue(vm: &mut Vm, get: bool) -> LuaResult<usize> {
    let n = vm.check_int(2)?;
    let f = vm.check_func(1)?;
    let idx = (n as u32).wrapping_sub(1) as usize;
    let fv = Value::Func(f);
    if get {
        match vm.upvalue_info(fv, idx) {
            Some((name, v)) => {
                let s = str_value(vm, &name);
                vm.push(s);
                vm.push(v);
                Ok(2)
            }
            None => Ok(0),
        }
    } else {
        let v = vm.arg(3);
        match vm.set_upvalue_info(fv, idx, v) {
            Some(name) => {
                let s = str_value(vm, &name);
                vm.ret1(s)
            }
            None => Ok(0),
        }
    }
}

fn lj_cf_debug_getupvalue(vm: &mut Vm) -> LuaResult<usize> {
    debug_getupvalue(vm, true)
}

fn lj_cf_debug_setupvalue(vm: &mut Vm) -> LuaResult<usize> {
    vm.check_any(3)?;
    debug_getupvalue(vm, false)
}

fn lj_cf_debug_upvalueid(vm: &mut Vm) -> LuaResult<usize> {
    let f = vm.check_func(1)?;
    let n = vm.check_int(2)?.wrapping_sub(1);
    if n as u32 as usize >= f.nupvalues() {
        return Err(vm.arg_error_em(2, ErrMsg::IDXRNG));
    }
    let n = n as usize;
    let id = match &*f {
        crate::func::Function::Lua { upvals, .. } => upvals[n].get().addr(),
        crate::func::Function::Native { upvals, .. } => upvals[n].as_ptr() as usize,
    };
    vm.ret1(Value::LightUd(id))
}

fn lj_cf_debug_upvaluejoin(vm: &mut Vm) -> LuaResult<usize> {
    let mut fs = Vec::with_capacity(2);
    for i in 0..2 {
        let f = vm.check_func(2 * i + 1)?;
        if !f.is_lua() {
            return Err(vm.arg_error_em(2 * i as i32 + 1, ErrMsg::NOLFUNC));
        }
        let n = vm.check_int(2 * i + 2)?.wrapping_sub(1);
        if n as u32 as usize >= f.nupvalues() {
            return Err(vm.arg_error_em(2 * i as i32 + 2, ErrMsg::IDXRNG));
        }
        fs.push((f, n as usize));
    }
    let uv = fs[1].0.upval(fs[1].1).expect("Lua upvalue");
    fs[0].0.set_upval(fs[0].1, uv);
    Ok(0)
}

// -- Hooks ------------------------------------------------------------------------------------

/// `KEY_HOOK`: registry key of the Lua hook function.
fn key_hook() -> Value {
    Value::Num(f64::from_bits(0x8100_0000_0000_0000 | b'h' as u64))
}

/// `hookf`: call the Lua hook function with the event name and line.
fn hookf(vm: &mut Vm, ev: HookEvent, line: i32) -> LuaResult<()> {
    let f = vm.registry.get(key_hook());
    if let Value::Func(_) = f {
        vm.push(f);
        vm.push_str(ev.name().as_bytes());
        vm.push(if line >= 0 {
            Value::Num(line as f64)
        } else {
            Value::Nil
        });
        vm.call(2, 0)?;
    }
    Ok(())
}

fn makemask(smask: &[u8], count: i32) -> u8 {
    let s = cstr(smask);
    let mut mask = 0;
    if s.contains(&b'c') {
        mask |= LUA_MASKCALL;
    }
    if s.contains(&b'r') {
        mask |= LUA_MASKRET;
    }
    if s.contains(&b'l') {
        mask |= LUA_MASKLINE;
    }
    if count > 0 {
        mask |= LUA_MASKCOUNT;
    }
    mask
}

fn unmakemask(mask: u8) -> Vec<u8> {
    let mut s = Vec::with_capacity(3);
    if mask & LUA_MASKCALL != 0 {
        s.push(b'c');
    }
    if mask & LUA_MASKRET != 0 {
        s.push(b'r');
    }
    if mask & LUA_MASKLINE != 0 {
        s.push(b'l');
    }
    s
}

fn lj_cf_debug_sethook(vm: &mut Vm) -> LuaResult<usize> {
    let (_, arg) = getthread(vm);
    let (func, mask, count): (Option<crate::vm::HookFn>, u8, i32);
    if vm.arg(arg + 1).is_nil() {
        vm.set_top(arg + 1);
        (func, mask, count) = (None, 0, 0);
    } else {
        let smask = vm.check_str(arg + 2)?;
        if !matches!(vm.arg(arg + 1), Value::Func(_)) {
            return Err(vm.type_error(arg as i32 + 1, "function"));
        }
        let c = vm.opt_int(arg + 3, 0)?;
        (func, mask, count) = (
            Some(hookf as crate::vm::HookFn),
            makemask(smask.as_bytes(), c),
            c,
        );
    }
    let hf = vm.arg(arg + 1);
    let reg = vm.registry;
    vm.rawset(reg, key_hook(), hf)?;
    vm.set_hook(func, mask, count);
    Ok(0)
}

fn lj_cf_debug_gethook(vm: &mut Vm) -> LuaResult<usize> {
    let (hook, mask, count) = vm.get_hook();
    match hook {
        Some(h) if !std::ptr::fn_addr_eq(h, hookf as crate::vm::HookFn) => {
            vm.push_str(b"external hook")
        }
        _ => {
            let f = vm.registry.get(key_hook());
            vm.push(f);
        }
    }
    vm.push_str(&unmakemask(mask));
    vm.push(Value::Num(count as f64));
    Ok(3)
}

// -- debug.debug ------------------------------------------------------------------------------

unsafe extern "C" {
    #[link_name = "stdin"]
    static mut C_STDIN: *mut libc::FILE;
}

fn lj_cf_debug_debug(vm: &mut Vm) -> LuaResult<usize> {
    loop {
        let mut buffer = [0u8; 250];
        {
            let mut e = std::io::stderr();
            let _ = e.write_all(b"lua_debug> ");
            let _ = e.flush();
        }
        // SAFETY: `buffer` has room for 250 bytes; `stdin` is the C library's stream.
        let r = unsafe {
            libc::fgets(
                buffer.as_mut_ptr() as *mut libc::c_char,
                buffer.len() as i32,
                C_STDIN,
            )
        };
        if r.is_null() {
            return Ok(0);
        }
        let line = cstr(&buffer).to_vec();
        if line == b"cont\n" {
            return Ok(0);
        }
        let r = match vm.load(&line, b"=(debug command)", None) {
            Ok(f) => {
                vm.push(f);
                vm.pcall(0, 0, None).map(|_| ())
            }
            Err(e) => Err(e),
        };
        if let Err(e) = r {
            if e.kind == ErrorKind::Yield {
                return Err(e);
            }
            vm.rt_clear_thrown();
            let msg = vm.tostring_opt(e.value);
            let mut out = std::io::stderr();
            let _ = out.write_all(match &msg {
                Some(s) => cstr(s),
                None => b"(error object is not a string)",
            });
            let _ = out.write_all(b"\n");
        }
        vm.set_top(0);
    }
}

// -- Traceback --------------------------------------------------------------------------------

fn lj_cf_debug_traceback(vm: &mut Vm) -> LuaResult<usize> {
    let (th, arg) = getthread(vm);
    let a = vm.arg(arg + 1);
    let msg = if arg < vm.nargs() {
        vm.tostring_opt(a)
    } else {
        None
    };
    if msg.is_none() && vm.nargs() > arg {
        vm.set_top(arg + 1);
        return Ok(1);
    }
    let level = vm.opt_int(arg + 2, i32::from(th == vm.cur_thread))?;
    let tb = vm.traceback(th, msg.as_deref().map(cstr), level);
    vm.push_str(&tb);
    Ok(1)
}
