//! Coroutine library. Port of the coroutine part of lib_base.c (with the semantics of the
//! `coroutine_resume`/`coroutine_wrap_aux`/`coroutine_yield` fast functions of vm_x64.dasc).

use crate::error::LuaResult;
use crate::state::Vm;
use crate::thread::ThreadStatus;
use crate::value::Value;
use crate::vm::ErrMsg;
use crate::vm::ffid::*;

use super::{LibFn, register_lib};

/// `LJ_LIB_REG(L, "coroutine", coroutine)`.
pub fn open_coroutine(vm: &mut Vm) -> LuaResult<()> {
    let funcs: &[LibFn] = &[
        ("status", lj_cf_coroutine_status, FF_COROUTINE_STATUS),
        ("running", lj_cf_coroutine_running, FF_COROUTINE_RUNNING),
        (
            "isyieldable",
            lj_cf_coroutine_isyieldable,
            FF_COROUTINE_ISYIELDABLE,
        ),
        ("create", lj_cf_coroutine_create, FF_COROUTINE_CREATE),
        ("yield", lj_cf_coroutine_yield, FF_COROUTINE_YIELD),
        ("resume", lj_cf_coroutine_resume, FF_COROUTINE_RESUME),
        ("wrap", lj_cf_coroutine_wrap, FF_COROUTINE_WRAP),
    ];
    register_lib(vm, "coroutine", funcs)?;
    Ok(())
}

fn lj_cf_coroutine_status(vm: &mut Vm) -> LuaResult<usize> {
    let Value::Thread(co) = vm.arg(1) else {
        return Err(vm.arg_error_em(1, ErrMsg::NOCORO));
    };
    if vm.nargs() < 1 {
        return Err(vm.arg_error_em(1, ErrMsg::NOCORO));
    }
    let s: &[u8] = if co == vm.cur_thread {
        b"running"
    } else {
        match co.status() {
            // A fresh thread without a function (`lua_newthread`) is dead, as upstream's
            // `L->top == L->base` check.
            ThreadStatus::Fresh if vm.resume_check(co).is_some() => b"dead",
            ThreadStatus::Suspended | ThreadStatus::Fresh => b"suspended",
            ThreadStatus::Normal | ThreadStatus::Running => b"normal",
            ThreadStatus::Dead => b"dead",
        }
    };
    vm.push_str(s);
    Ok(1)
}

fn lj_cf_coroutine_running(vm: &mut Vm) -> LuaResult<usize> {
    if vm.cur_thread == vm.main_thread {
        return vm.ret1(Value::Nil);
    }
    let t = vm.cur_thread;
    vm.ret1(Value::Thread(t))
}

fn lj_cf_coroutine_isyieldable(vm: &mut Vm) -> LuaResult<usize> {
    let y = vm.can_yield();
    vm.ret1(Value::Bool(y))
}

fn lj_cf_coroutine_create(vm: &mut Vm) -> LuaResult<usize> {
    let Value::Func(f) = vm.arg(1) else {
        return Err(vm.argt_error(1, "function"));
    };
    if vm.nargs() < 1 {
        return Err(vm.argt_error(1, "function"));
    }
    let co = vm.new_coroutine(f);
    vm.ret1(Value::Thread(co))
}

fn lj_cf_coroutine_yield(vm: &mut Vm) -> LuaResult<usize> {
    let n = vm.nargs();
    vm.yield_(n)
}

fn lj_cf_coroutine_resume(vm: &mut Vm) -> LuaResult<usize> {
    let Value::Thread(co) = vm.arg(1) else {
        return Err(vm.arg_error_em(1, ErrMsg::NOCORO));
    };
    if vm.nargs() < 1 {
        return Err(vm.arg_error_em(1, ErrMsg::NOCORO));
    }
    if let Some(em) = vm.resume_check(co) {
        vm.push(Value::Bool(false));
        let s = vm.err_str(em);
        vm.push(s);
        return Ok(2);
    }
    let nargs = vm.nargs() - 1;
    match vm.resume(co, nargs)? {
        Ok(n) => {
            // Prepend true to the results.
            let th = vm.cur_thread;
            vm.push(Value::Nil);
            let top = th.top();
            for i in (top - n..top).rev() {
                th.set(i, th.get(i - 1));
            }
            th.set(top - n - 1, Value::Bool(true));
            Ok(n + 1)
        }
        Err(v) => {
            vm.push(Value::Bool(false));
            vm.push(v);
            Ok(2)
        }
    }
}

fn lj_cf_coroutine_wrap_aux(vm: &mut Vm) -> LuaResult<usize> {
    let Value::Thread(co) = vm.upvalue(1) else {
        unreachable!("wrap upvalue")
    };
    if let Some(em) = vm.resume_check(co) {
        return Err(vm.err_caller(em));
    }
    let nargs = vm.nargs();
    match vm.resume(co, nargs)? {
        Ok(n) => Ok(n),
        Err(v) => match v {
            Value::Str(s) => {
                let m = s.as_bytes().to_vec();
                Err(vm.err_callermsg(&m))
            }
            v => Err(vm.error(v)),
        },
    }
}

fn lj_cf_coroutine_wrap(vm: &mut Vm) -> LuaResult<usize> {
    lj_cf_coroutine_create(vm)?;
    let co = vm.pop();
    let f = vm.native(lj_cf_coroutine_wrap_aux, FF_COROUTINE_WRAP_AUX, vec![co]);
    vm.ret1(Value::Func(f))
}
