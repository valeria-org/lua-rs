//! Base library. Port of lib_base.c (base part) including the semantics of the assembler fast
//! paths of vm_x64.dasc (`assert`, `type`, `next`, `pairs`, `ipairs_aux`, `getmetatable`,
//! `rawget`, `tonumber`, `tostring`, `pcall`, `xpcall`).

use crate::bc::MM;
use crate::error::{ErrorKind, LuaResult};
use crate::state::Vm;
use crate::value::Value;
use crate::vm::ErrMsg;
use crate::vm::ffid::*;

use super::{LibFn, new_lib_fn, register_lib};

/// `luaopen_base`: the base functions in `_G` plus the coroutine library.
pub fn open_base(vm: &mut Vm) -> LuaResult<()> {
    let g = vm.globals;
    let k = vm.new_str(b"_G");
    g.set_str(k, Value::Table(g)).map_err(|e| vm.tab_error(e))?;
    let funcs: &[LibFn] = &[
        ("assert", lj_cf_assert, FF_ASSERT),
        ("type", lj_cf_type, FF_TYPE),
        ("getmetatable", lj_cf_getmetatable, FF_GETMETATABLE),
        ("setmetatable", lj_cf_setmetatable, FF_SETMETATABLE),
        ("getfenv", lj_cf_getfenv, FF_GETFENV),
        ("setfenv", lj_cf_setfenv, FF_SETFENV),
        ("rawget", lj_cf_rawget, FF_RAWGET),
        ("rawset", lj_cf_rawset, FF_RAWSET),
        ("rawequal", lj_cf_rawequal, FF_RAWEQUAL),
        ("unpack", lj_cf_unpack, FF_UNPACK),
        ("select", lj_cf_select, FF_SELECT),
        ("tonumber", lj_cf_tonumber, FF_TONUMBER),
        ("tostring", lj_cf_tostring, FF_TOSTRING),
        ("error", lj_cf_error, FF_ERROR),
        ("pcall", lj_cf_pcall, FF_PCALL),
        ("xpcall", lj_cf_xpcall, FF_XPCALL),
        ("loadfile", lj_cf_loadfile, FF_LOADFILE),
        ("load", lj_cf_load, FF_LOAD),
        ("loadstring", lj_cf_loadstring, FF_LOADSTRING),
        ("dofile", lj_cf_dofile, FF_DOFILE),
        ("gcinfo", lj_cf_gcinfo, FF_GCINFO),
        ("collectgarbage", lj_cf_collectgarbage, FF_COLLECTGARBAGE),
    ];
    register_lib(vm, "_G", funcs)?;
    // Functions with upvalues.
    let next = new_lib_fn(vm, lj_cf_next, FF_NEXT, vec![]);
    let ipairs_aux = new_lib_fn(vm, lj_cf_ipairs_aux, FF_IPAIRS_AUX, vec![]);
    let pairs = new_lib_fn(vm, lj_cf_pairs, FF_PAIRS, vec![Value::Func(next)]);
    let ipairs = new_lib_fn(vm, lj_cf_ipairs, FF_IPAIRS, vec![Value::Func(ipairs_aux)]);
    // newproxy's weak table: its own metatable, __mode = "kv".
    let weak = vm.new_table(0, 1)?;
    let mode = vm.new_str(b"kv");
    let mk = vm.mm_name(MM::Mode);
    weak.set_str(mk, Value::Str(mode)).map_err(|e| vm.tab_error(e))?;
    weak.set_metatable(Some(weak));
    weak.set_nomm(!(1u8 << MM::Mode as u8));
    let newproxy = new_lib_fn(vm, lj_cf_newproxy, FF_NEWPROXY, vec![Value::Table(weak)]);
    let ts = vm.new_str(b"tostring");
    let print = new_lib_fn(vm, lj_cf_print, FF_PRINT, vec![Value::Str(ts)]);
    for (name, f) in [("next", next), ("pairs", pairs), ("ipairs", ipairs), ("newproxy", newproxy), ("print", print)] {
        let k = vm.new_str(name.as_bytes());
        g.set_str(k, Value::Func(f)).map_err(|e| vm.tab_error(e))?;
    }
    let k = vm.new_str(b"_VERSION");
    let v = vm.new_str(b"Lua 5.1");
    g.set_str(k, Value::Str(v)).map_err(|e| vm.tab_error(e))?;
    super::coroutine::open_coroutine(vm)?;
    Ok(())
}

// -- Checks ---------------------------------------------------------------------------------

fn lj_cf_assert(vm: &mut Vm) -> LuaResult<usize> {
    let v = vm.check_any(1)?;
    let n = vm.nargs();
    if v.is_truthy() {
        return Ok(n);
    }
    if n == 1 {
        return Err(vm.err_caller(ErrMsg::ASSERT));
    }
    match vm.arg(2) {
        Value::Str(_) | Value::Num(_) => {
            let s = vm.check_str(2)?;
            let b = s.as_bytes().to_vec();
            Err(vm.err_callermsg(&b))
        }
        _ => {
            let top = vm.arg(n);
            Err(vm.error(top))
        }
    }
}

fn lj_cf_type(vm: &mut Vm) -> LuaResult<usize> {
    let v = vm.check_any(1)?;
    vm.push_str(v.type_name().as_bytes());
    Ok(1)
}

// -- Iterators ------------------------------------------------------------------------------

fn lj_cf_next(vm: &mut Vm) -> LuaResult<usize> {
    let t = vm.check_tab(1)?;
    let k = vm.arg(2);
    match t.next(k) {
        Ok(Some((k, v))) => {
            vm.push(k);
            vm.push(v);
            Ok(2)
        }
        Ok(None) => vm.ret1(Value::Nil),
        Err(_) => Err(vm.err_msg(ErrMsg::NEXTIDX)),
    }
}

/// `ffh_pairs`.
fn ffh_pairs(vm: &mut Vm, mm: MM) -> LuaResult<usize> {
    let o = vm.check_any(1)?;
    let mo = vm.meta_lookup(o, mm);
    if matches!(o, Value::CData(_)) && !mo.is_nil() {
        vm.set_top(1);
        return vm.tailcall(mo);
    }
    if !matches!(o, Value::Table(_)) {
        return Err(vm.argt_error(1, "table"));
    }
    let f = vm.upvalue(1);
    vm.push(f);
    vm.push(o);
    vm.push(if mm == MM::Pairs { Value::Nil } else { Value::Num(0.0) });
    Ok(3)
}

fn lj_cf_pairs(vm: &mut Vm) -> LuaResult<usize> {
    ffh_pairs(vm, MM::Pairs)
}

fn lj_cf_ipairs(vm: &mut Vm) -> LuaResult<usize> {
    ffh_pairs(vm, MM::Ipairs)
}

fn lj_cf_ipairs_aux(vm: &mut Vm) -> LuaResult<usize> {
    let t = vm.check_tab(1)?;
    let i = vm.check_num(2)?;
    let i = crate::vm::num2int(i).wrapping_add(1);
    let v = t.get_int(i);
    if v.is_nil() {
        return Ok(0);
    }
    vm.push(Value::Num(i as f64));
    vm.push(v);
    Ok(2)
}

// -- Getters and setters --------------------------------------------------------------------

fn lj_cf_getmetatable(vm: &mut Vm) -> LuaResult<usize> {
    let o = vm.check_any(1)?;
    let r = match vm.metatable_of(o) {
        None => Value::Nil,
        Some(mt) => {
            let v = mt.get_str(vm.mm_name(MM::Metatable));
            if v.is_nil() { Value::Table(mt) } else { v }
        }
    };
    vm.ret1(r)
}

fn lj_cf_setmetatable(vm: &mut Vm) -> LuaResult<usize> {
    let t = vm.check_tab(1)?;
    let mt = vm.check_tab_or_nil(2)?;
    if !vm.meta_lookup(Value::Table(t), MM::Metatable).is_nil() {
        return Err(vm.err_caller(ErrMsg::PROTMT));
    }
    t.set_metatable(mt);
    vm.ret1(Value::Table(t))
}

/// Frame function for `getfenv`/`setfenv` level arguments.
fn level_func(vm: &mut Vm, level: i32) -> LuaResult<crate::gc::Gc<crate::func::Function>> {
    if level < 0 {
        return Err(vm.arg_error_em(1, ErrMsg::INVLVL));
    }
    match vm.get_stack(vm.cur_thread, level) {
        Some(fr) => Ok(fr.thread.frame(fr.idx).expect("frame").func),
        None => Err(vm.arg_error_em(1, ErrMsg::INVLVL)),
    }
}

fn lj_cf_getfenv(vm: &mut Vm) -> LuaResult<usize> {
    let f = match vm.arg(1) {
        Value::Func(f) if vm.nargs() >= 1 => f,
        _ => {
            let level = vm.opt_int(1, 1)?;
            level_func(vm, level)?
        }
    };
    let env = if f.is_lua() { f.env() } else { vm.cur_thread.env() };
    vm.ret1(Value::Table(env))
}

fn lj_cf_setfenv(vm: &mut Vm) -> LuaResult<usize> {
    let t = vm.check_tab(2)?;
    let f = match vm.arg(1) {
        Value::Func(f) if vm.nargs() >= 1 => f,
        _ => {
            let level = vm.check_int(1)?;
            if level == 0 {
                vm.cur_thread.set_env(t);
                return Ok(0);
            }
            level_func(vm, level)?
        }
    };
    if !f.is_lua() {
        return Err(vm.err_caller(ErrMsg::SETFENV));
    }
    f.set_env(t);
    vm.ret1(Value::Func(f))
}

fn lj_cf_rawget(vm: &mut Vm) -> LuaResult<usize> {
    let t = vm.check_tab(1)?;
    let k = vm.check_any(2)?;
    vm.ret1(t.get(k))
}

fn lj_cf_rawset(vm: &mut Vm) -> LuaResult<usize> {
    let t = vm.check_tab(1)?;
    let k = vm.check_any(2)?;
    let v = vm.check_any(3)?;
    vm.rawset(t, k, v)?;
    vm.ret1(Value::Table(t))
}

fn lj_cf_rawequal(vm: &mut Vm) -> LuaResult<usize> {
    let a = vm.check_any(1)?;
    let b = vm.check_any(2)?;
    vm.ret1(Value::Bool(a.raw_equal(b)))
}

fn lj_cf_unpack(vm: &mut Vm) -> LuaResult<usize> {
    let t = vm.check_tab(1)?;
    let mut i = vm.opt_int(2, 1)?;
    let e = if vm.nargs() >= 3 && !vm.arg(3).is_nil() { vm.check_int(3)? } else { t.len() as i32 };
    if i > e {
        return Ok(0);
    }
    let nu = (e as u32).wrapping_sub(i as u32);
    let n = nu.wrapping_add(1) as usize;
    if nu >= 8000 || !vm.try_check_stack(n) {
        return Err(vm.err_caller(ErrMsg::UNPACK));
    }
    loop {
        let v = t.get_int(i);
        vm.push(v);
        if i >= e {
            break;
        }
        i += 1;
    }
    Ok(n)
}

fn lj_cf_select(vm: &mut Vm) -> LuaResult<usize> {
    let n = vm.nargs() as i32;
    if n >= 1
        && let Value::Str(s) = vm.arg(1)
        && s.as_bytes().first() == Some(&b'#')
    {
        return vm.ret1(Value::Num((n - 1) as f64));
    }
    let mut i = vm.check_int(1)?;
    if i < 0 {
        i = n.wrapping_add(i);
    } else if i > n {
        i = n;
    }
    if i < 1 {
        return Err(vm.arg_error_em(1, ErrMsg::IDXRNG));
    }
    Ok((n - i) as usize)
}

// -- Conversions ----------------------------------------------------------------------------

fn lj_cf_tonumber(vm: &mut Vm) -> LuaResult<usize> {
    let base = vm.opt_int(2, 10)?;
    if base == 10 {
        let o = vm.check_any(1)?;
        if let Some(n) = vm.tonumber(o) {
            return vm.ret1(Value::Num(n));
        }
        if let Value::CData(cd) = o
            && let Some(f) = vm.rt.ffi.tonumber
            && let Some(n) = f(vm, cd)?
        {
            return vm.ret1(Value::Num(n));
        }
    } else {
        let s = vm.check_str(1)?;
        if !(2..=36).contains(&base) {
            return Err(vm.arg_error_em(2, ErrMsg::BASERNG));
        }
        if let Some(n) = crate::text::strscan::parse_int_base(s.as_bytes(), base as u32) {
            return vm.ret1(Value::Num(n));
        }
    }
    vm.ret1(Value::Nil)
}

fn lj_cf_tostring(vm: &mut Vm) -> LuaResult<usize> {
    let o = vm.check_any(1)?;
    vm.set_top(1);
    let mo = vm.meta_lookup(o, MM::Tostring);
    if !mo.is_nil() && !matches!(o, Value::Str(_)) {
        return vm.tailcall(mo);
    }
    if let Value::Str(_) = o {
        return Ok(1);
    }
    vm.gc_safe();
    let s = vm.tostring_basic(o);
    vm.ret1(Value::Str(s))
}

// -- Errors ---------------------------------------------------------------------------------

fn lj_cf_error(vm: &mut Vm) -> LuaResult<usize> {
    let level = vm.opt_int(2, 1)?;
    vm.set_top(1);
    let v = vm.arg(1);
    if matches!(v, Value::Str(_) | Value::Num(_)) && level > 0 {
        let mut m = vm.where_(level);
        m.extend_from_slice(&vm.tostring_opt(v).unwrap_or_default());
        let s = vm.new_str(&m);
        return Err(vm.error(Value::Str(s)));
    }
    Err(vm.error(v))
}

/// `pcall` when called from Rust (`Vm::call`); calls from Lua are handled by the VM.
fn lj_cf_pcall(vm: &mut Vm) -> LuaResult<usize> {
    vm.check_any(1)?;
    let n = vm.nargs();
    let base = vm.frame_base();
    match vm.pcall(n - 1, -1, None) {
        Ok(k) => {
            prepend(vm, base, k, Value::Bool(true));
            Ok(k + 1)
        }
        Err(e) => {
            vm.push(Value::Bool(false));
            vm.push(e.value);
            Ok(2)
        }
    }
}

/// Insert `v` before the `k` values starting at slot `at`.
fn prepend(vm: &mut Vm, at: usize, k: usize, v: Value) {
    vm.push(Value::Nil);
    let th = vm.cur_thread;
    for i in (0..k).rev() {
        th.set(at + i + 1, th.get(at + i));
    }
    th.set(at, v);
}

/// `xpcall` when called from Rust (`Vm::call`).
fn lj_cf_xpcall(vm: &mut Vm) -> LuaResult<usize> {
    vm.check_any(1)?;
    vm.check_func(2)?;
    let n = vm.nargs();
    let base = vm.frame_base();
    let th = vm.cur_thread;
    let f = th.get(base);
    th.set(base, th.get(base + 1));
    th.set(base + 1, f);
    match vm.pcall(n - 2, -1, Some(base)) {
        Ok(k) => {
            th.set(base, Value::Bool(true));
            Ok(k + 1)
        }
        Err(e) => {
            vm.push(Value::Bool(false));
            vm.push(e.value);
            Ok(2)
        }
    }
}

// -- Loading ------------------------------------------------------------------------------

/// `load_aux`.
fn load_aux(vm: &mut Vm, r: LuaResult<crate::gc::Gc<crate::func::Function>>, envarg: usize) -> LuaResult<usize> {
    match r {
        Ok(f) => {
            if let Value::Table(t) = vm.arg(envarg) {
                f.set_env(t);
            }
            vm.ret1(Value::Func(f))
        }
        Err(e) if e.kind == ErrorKind::Yield => Err(e),
        Err(e) => {
            vm.rt_clear_thrown();
            vm.push(Value::Nil);
            vm.push(e.value);
            Ok(2)
        }
    }
}

fn lj_cf_loadfile(vm: &mut Vm) -> LuaResult<usize> {
    let fname = vm.opt_str(1)?.map(|s| s.as_bytes().to_vec());
    let mode = vm.opt_str(2)?.map(|s| s.as_bytes().to_vec());
    vm.set_top(3);
    let r = vm.load_file(fname.as_deref(), mode.as_deref());
    load_aux(vm, r, 3)
}

fn lj_cf_load(vm: &mut Vm) -> LuaResult<usize> {
    let name = vm.opt_str(2)?.map(|s| s.as_bytes().to_vec());
    let mode = vm.opt_str(3)?.map(|s| s.as_bytes().to_vec());
    if vm.nargs() >= 1 && matches!(vm.arg(1), Value::Str(_) | Value::Num(_)) {
        let s = vm.check_str(1)?;
        vm.set_top(4);
        let src = s.as_bytes().to_vec();
        let cn = name.unwrap_or_else(|| src.clone());
        let r = vm.load(&src, &cn, mode.as_deref());
        load_aux(vm, r, 4)
    } else {
        let f = vm.check_func(1)?;
        vm.set_top(5);
        let cn = name.unwrap_or_else(|| b"=(load)".to_vec());
        let r = vm.load_reader(
            |vm| {
                vm.check_stack(2, Some("too many nested functions"))?;
                vm.call_value(Value::Func(f), &[], 1)?;
                let v = vm.pop();
                match v {
                    Value::Nil => Ok(None),
                    Value::Str(_) | Value::Num(_) => {
                        vm.set_arg(5, v);
                        Ok(vm.tostring_opt(v))
                    }
                    _ => Err(vm.err_caller(ErrMsg::RDRSTR)),
                }
            },
            &cn,
            mode.as_deref(),
        );
        load_aux(vm, r, 4)
    }
}

fn lj_cf_loadstring(vm: &mut Vm) -> LuaResult<usize> {
    lj_cf_load(vm)
}

fn lj_cf_dofile(vm: &mut Vm) -> LuaResult<usize> {
    let fname = vm.opt_str(1)?.map(|s| s.as_bytes().to_vec());
    vm.set_top(1);
    match vm.load_file(fname.as_deref(), None) {
        Ok(f) => {
            vm.push(Value::Func(f));
            let n = vm.call(0, -1)?;
            Ok(n)
        }
        Err(e) => {
            let v = e.value;
            Err(vm.error(v))
        }
    }
}

// -- GC control -----------------------------------------------------------------------------

fn lj_cf_gcinfo(vm: &mut Vm) -> LuaResult<usize> {
    let kb = (vm.gc_count_bytes() >> 10) as i32;
    vm.ret1(Value::Num(kb as f64))
}

fn lj_cf_collectgarbage(vm: &mut Vm) -> LuaResult<usize> {
    const OPTS: &[&str] =
        &["stop", "restart", "collect", "count", "\u{ff}", "step", "setpause", "setstepmul", "\u{ff}", "isrunning"];
    let opt = vm.check_opt(1, 2, OPTS)?;
    let data = vm.opt_int(2, 0)?;
    let r = match opt {
        0 => {
            vm.gc_stop();
            Value::Num(0.0)
        }
        1 => {
            vm.gc_restart(data);
            Value::Num(0.0)
        }
        2 => {
            vm.gc_collect();
            Value::Num(0.0)
        }
        3 => Value::Num(vm.gc_count_bytes() as f64 / 1024.0),
        5 => Value::Bool(vm.gc_step_kb(data)),
        6 => Value::Num(vm.gc_set_pause(data as u32) as f64),
        7 => Value::Num(vm.gc_set_stepmul(data as u32) as f64),
        9 => Value::Bool(vm.gc_is_running()),
        _ => Value::Num(-1.0),
    };
    vm.ret1(r)
}

// -- Miscellaneous --------------------------------------------------------------------------

fn lj_cf_newproxy(vm: &mut Vm) -> LuaResult<usize> {
    vm.set_top(1);
    let arg = vm.arg(1);
    let env = vm.current_env();
    let u = vm.new_udata(0, env);
    vm.push(Value::Udata(u));
    let Value::Table(weak) = vm.upvalue(1) else { unreachable!("newproxy upvalue") };
    if arg.is_falsy() {
        return Ok(1);
    } else if let Value::Bool(_) = arg {
        let mt = vm.empty_table();
        weak.set(Value::Table(mt), Value::Bool(true)).map_err(|e| vm.tab_error(e))?;
        u.set_metatable(Some(mt));
    } else {
        let mt = vm.metatable_of(arg);
        let valid = mt.is_some_and(|m| weak.get(Value::Table(m)).is_truthy());
        if !valid {
            return Err(vm.arg_error_em(1, ErrMsg::NOPROXY));
        }
        u.set_metatable(mt);
    }
    Ok(1)
}

fn lj_cf_print(vm: &mut Vm) -> LuaResult<usize> {
    let nargs = vm.nargs();
    let Value::Str(key) = vm.upvalue(1) else { unreachable!("print upvalue") };
    let mut tv = vm.cur_thread.env().get_str(key);
    if tv.is_nil() {
        let g = Value::Table(vm.globals);
        tv = vm.index(g, Value::Str(key))?;
    }
    vm.push(tv);
    let shortcut = matches!(tv, Value::Func(f) if f.ffid() == FF_TOSTRING) && vm.basemt(crate::value::VType::Num).is_none();
    let base = vm.frame_base();
    for i in 0..nargs {
        let o = vm.cur_thread.get(base + i);
        let s = match o {
            Value::Str(s) if shortcut => s.as_bytes().to_vec(),
            Value::Num(n) if shortcut => crate::text::strfmt::fmt_num(n),
            _ => {
                let f = vm.cur_thread.get(base + nargs);
                vm.call_value(f, &[o], 1)?;
                let r = vm.pop();
                match vm.tostring_opt(r) {
                    Some(b) => b,
                    None => return Err(vm.err_caller(ErrMsg::PRTOSTR)),
                }
            }
        };
        if i > 0 {
            super::write_stdout(b"\t");
        }
        super::write_stdout(&s);
    }
    super::write_stdout(b"\n");
    Ok(0)
}
