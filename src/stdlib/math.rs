//! Math library. Port of lib_math.c, including the semantics of the assembler fast paths of
//! vm_x64.dasc (SSE `minsd`/`maxsd` for `min`/`max`, x87 `fscale` for `ldexp`, libm calls for
//! the transcendental functions). `deg`/`rad` are upstream's precompiled bytecode.

use crate::error::LuaResult;
use crate::func::NativeFn;
use crate::state::Vm;
use crate::text::prng::{self, PrngState};
use crate::value::Value;
use crate::vm::ErrMsg;

use super::libinit;

/// `lj_lib_init_math` from the reference build's lj_libdef.h.
#[rustfmt::skip]
static LIB_INIT_MATH: &[u8] = &[
    38,16,30,67,97,98,115,133,102,108,111,111,114,132,99,101,105,108,68,115,113,114,116,133,108,111,
    103,49,48,131,101,120,112,131,115,105,110,131,99,111,115,131,116,97,110,132,97,115,105,110,132,
    97,99,111,115,132,97,116,97,110,132,115,105,110,104,132,99,111,115,104,132,116,97,110,104,133,
    102,114,101,120,112,132,109,111,100,102,67,108,111,103,249,3,100,101,103,0,1,2,0,0,1,2,24,1,0,0,
    76,1,2,0,241,135,158,166,3,220,203,178,130,4,249,3,114,97,100,0,1,2,0,0,1,2,24,1,0,0,76,1,2,0,
    243,244,148,165,20,198,190,199,252,3,69,97,116,97,110,50,131,112,111,119,132,102,109,111,100,69,
    108,100,101,120,112,67,109,105,110,131,109,97,120,251,24,45,68,84,251,33,9,64,194,112,105,250,
    251,0,0,0,0,0,0,240,127,196,104,117,103,101,250,252,2,6,114,97,110,100,111,109,252,2,10,114,97,
    110,100,111,109,115,101,101,100,255,
];

unsafe extern "C" {
    fn frexp(x: f64, e: *mut libc::c_int) -> f64;
    fn modf(x: f64, ip: *mut f64) -> f64;
}

/// `luaopen_math`.
pub fn open_math(vm: &mut Vm) -> LuaResult<()> {
    // Upvalue of random/randomseed: a userdata holding the PRNG state.
    let env = vm.cur_thread.env();
    let rs = vm.new_udata(std::mem::size_of::<PrngState>(), env);
    store_prng(rs.data_ptr(), PrngState::seed_fixed());
    let funcs: &[NativeFn] = &[
        math_abs,
        math_floor,
        math_ceil,
        math_sqrt,
        math_log10,
        math_exp,
        math_sin,
        math_cos,
        math_tan,
        math_asin,
        math_acos,
        math_atan,
        math_sinh,
        math_cosh,
        math_tanh,
        math_frexp,
        math_modf,
        math_log,
        math_atan2,
        math_pow,
        math_fmod,
        math_ldexp,
        math_min,
        math_max,
        math_random,
        math_randomseed,
    ];
    let mut stack = vec![Value::Udata(rs)];
    libinit::lib_register(vm, Some("math"), LIB_INIT_MATH, funcs, &mut stack)?;
    Ok(())
}

fn load_prng(p: *mut u8) -> PrngState {
    // SAFETY: the userdata payload is 16-byte aligned and holds a `PrngState`.
    unsafe { (p as *const PrngState).read() }
}

fn store_prng(p: *mut u8, rs: PrngState) {
    // SAFETY: as in `load_prng`.
    unsafe { (p as *mut PrngState).write(rs) }
}

/// One-argument function on a number (`.ffunc_n`).
fn unary(vm: &mut Vm, f: fn(f64) -> f64) -> LuaResult<usize> {
    let x = vm.check_num(1)?;
    vm.ret1(Value::Num(f(x)))
}

/// Two-argument function on numbers (`.ffunc_nn`).
fn binary(vm: &mut Vm, f: fn(f64, f64) -> f64) -> LuaResult<usize> {
    let x = vm.check_num(1)?;
    let y = vm.check_num(2)?;
    vm.ret1(Value::Num(f(x, y)))
}

fn math_abs(vm: &mut Vm) -> LuaResult<usize> {
    unary(vm, f64::abs)
}

fn math_floor(vm: &mut Vm) -> LuaResult<usize> {
    unary(vm, f64::floor)
}

fn math_ceil(vm: &mut Vm) -> LuaResult<usize> {
    unary(vm, f64::ceil)
}

fn math_sqrt(vm: &mut Vm) -> LuaResult<usize> {
    unary(vm, f64::sqrt)
}

fn math_log10(vm: &mut Vm) -> LuaResult<usize> {
    unary(vm, f64::log10)
}

fn math_exp(vm: &mut Vm) -> LuaResult<usize> {
    unary(vm, f64::exp)
}

fn math_sin(vm: &mut Vm) -> LuaResult<usize> {
    unary(vm, f64::sin)
}

fn math_cos(vm: &mut Vm) -> LuaResult<usize> {
    unary(vm, f64::cos)
}

fn math_tan(vm: &mut Vm) -> LuaResult<usize> {
    unary(vm, f64::tan)
}

fn math_asin(vm: &mut Vm) -> LuaResult<usize> {
    unary(vm, f64::asin)
}

fn math_acos(vm: &mut Vm) -> LuaResult<usize> {
    unary(vm, f64::acos)
}

fn math_atan(vm: &mut Vm) -> LuaResult<usize> {
    unary(vm, f64::atan)
}

fn math_sinh(vm: &mut Vm) -> LuaResult<usize> {
    unary(vm, f64::sinh)
}

fn math_cosh(vm: &mut Vm) -> LuaResult<usize> {
    unary(vm, f64::cosh)
}

fn math_tanh(vm: &mut Vm) -> LuaResult<usize> {
    unary(vm, f64::tanh)
}

fn math_frexp(vm: &mut Vm) -> LuaResult<usize> {
    let x = vm.check_num(1)?;
    let mut e: libc::c_int = 0;
    // SAFETY: plain libm call with a valid out pointer.
    let m = unsafe { frexp(x, &mut e) };
    vm.push(Value::Num(m));
    vm.push(Value::Num(e as f64));
    Ok(2)
}

fn math_modf(vm: &mut Vm) -> LuaResult<usize> {
    let x = vm.check_num(1)?;
    let mut ip = 0.0;
    // SAFETY: plain libm call with a valid out pointer.
    let fp = unsafe { modf(x, &mut ip) };
    vm.push(Value::Num(ip));
    vm.push(Value::Num(fp));
    Ok(2)
}

fn math_log(vm: &mut Vm) -> LuaResult<usize> {
    let x = vm.check_num(1)?;
    if vm.nargs() >= 2 {
        let y = vm.check_num(2)?;
        let x = x.log2();
        let y = 1.0 / y.log2();
        return vm.ret1(Value::Num(x * y)); // Do NOT join the expression to x / y.
    }
    vm.ret1(Value::Num(x.ln()))
}

fn math_atan2(vm: &mut Vm) -> LuaResult<usize> {
    binary(vm, f64::atan2)
}

fn math_pow(vm: &mut Vm) -> LuaResult<usize> {
    binary(vm, f64::powf)
}

fn math_fmod(vm: &mut Vm) -> LuaResult<usize> {
    binary(vm, |x, y| x % y)
}

/// x87 `fscale`: `x * 2^trunc(y)`.
fn fscale(x: f64, y: f64) -> f64 {
    if x.is_nan() || y.is_nan() {
        return x + y;
    }
    if y.is_infinite() {
        return if y > 0.0 {
            if x == 0.0 {
                f64::NAN
            } else {
                x * f64::INFINITY
            }
        } else if x.is_infinite() {
            f64::NAN
        } else {
            x * 0.0
        };
    }
    let e = y.trunc().clamp(-1e6, 1e6) as i32;
    crate::text::ldexp(x, e)
}

fn math_ldexp(vm: &mut Vm) -> LuaResult<usize> {
    let x = vm.check_num(1)?;
    let y = vm.check_num(2)?;
    vm.ret1(Value::Num(fscale(x, y)))
}

/// `math.min`/`math.max`: check every argument, then fold with SSE `minsd`/`maxsd` semantics
/// (the second operand is returned unless the comparison holds, so NaNs propagate from the
/// right).
fn minmax(vm: &mut Vm, max: bool) -> LuaResult<usize> {
    let mut x = vm.check_num(1)?;
    let n = vm.nargs();
    for i in 2..=n {
        let y = vm.check_num(i)?;
        x = if max {
            if x > y { x } else { y }
        } else if x < y {
            x
        } else {
            y
        };
    }
    vm.ret1(Value::Num(x))
}

fn math_min(vm: &mut Vm) -> LuaResult<usize> {
    minmax(vm, false)
}

fn math_max(vm: &mut Vm) -> LuaResult<usize> {
    minmax(vm, true)
}

fn math_random(vm: &mut Vm) -> LuaResult<usize> {
    let n = vm.nargs();
    let Value::Udata(ud) = vm.upvalue(1) else {
        unreachable!("math.random upvalue")
    };
    let mut rs = load_prng(ud.data_ptr());
    let mut d = rs.next_unit();
    store_prng(ud.data_ptr(), rs);
    if n > 0 {
        let r1 = vm.check_num(1)?;
        if n == 1 {
            d = prng::random_upto(d, r1); // d is an int in range [1, r1]
        } else {
            let r2 = vm.check_num(2)?;
            d = prng::random_range(d, r1, r2); // d is an int in range [r1, r2]
        }
    } // else: d is a double in range [0, 1]
    vm.ret1(Value::Num(d))
}

fn math_randomseed(vm: &mut Vm) -> LuaResult<usize> {
    let Value::Udata(ud) = vm.upvalue(1) else {
        unreachable!("math.randomseed upvalue")
    };
    let mut rs = load_prng(ud.data_ptr());
    if vm.nargs() > 0 {
        let d = vm.check_num(1)?;
        rs.seed(d);
    } else if !rs.seed_secure() {
        store_prng(ud.data_ptr(), rs);
        return Err(vm.err_caller(ErrMsg::PRNGSD));
    }
    store_prng(ud.data_ptr(), rs);
    Ok(0)
}
