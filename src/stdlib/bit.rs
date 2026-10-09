//! Bit operations library. Port of lib_bit.c (FFI-enabled variant: int64/uint64 cdata
//! operands give 64-bit results) with `lj_carith_check64`/`lj_carith_shift64` (lj_carith.c)
//! and the number semantics of the assembler fast paths of vm_x64.dasc.

use crate::cdata::CTID_UINT64;
use crate::error::LuaResult;
use crate::func::NativeFn;
use crate::state::Vm;
use crate::text::strfmt::{self, F_UPPER, FormatType, SFormat, SH_PREC, T_HEX};
use crate::value::Value;
use crate::vm::ffid::{FF_BIT_BOR, FF_BIT_LSHIFT};
use crate::vm::num2bit;

use super::libinit;

/// `lj_lib_init_bit` from the reference build's lj_libdef.h.
#[rustfmt::skip]
static LIB_INIT_BIT: &[u8] = &[
    64,40,12,69,116,111,98,105,116,68,98,110,111,116,69,98,115,119,97,112,70,108,115,104,105,102,
    116,134,114,115,104,105,102,116,135,97,114,115,104,105,102,116,131,114,111,108,131,114,111,114,
    68,98,97,110,100,131,98,111,114,132,98,120,111,114,5,116,111,104,101,120,255,
];

/// `luaopen_bit`.
pub fn open_bit(vm: &mut Vm) -> LuaResult<()> {
    let funcs: &[NativeFn] = &[
        bit_tobit, bit_bnot, bit_bswap, bit_shift, bit_shift, bit_shift, bit_shift, bit_shift, bit_nary, bit_nary,
        bit_nary, bit_tohex,
    ];
    let mut stack = Vec::new();
    libinit::lib_register(vm, Some("bit"), LIB_INIT_BIT, funcs, &mut stack)?;
    Ok(())
}

/// `lj_carith_check64`: argument `narg` as 64 bits. Numbers (and numeric strings, converted
/// in place) give their `lj_num2bit` value zero-extended; cdata are converted to int64 or
/// uint64 (`*id` is set to the result type).
fn check64(vm: &mut Vm, narg: usize, id: &mut u32) -> LuaResult<u64> {
    if narg <= vm.nargs()
        && let Value::CData(cd) = vm.arg(narg)
    {
        return match vm.rt.ffi.check64 {
            Some(f) => f(vm, cd, narg as u32, id),
            None => Err(vm.argt_error(narg, "number")),
        };
    }
    if narg <= vm.nargs()
        && let Some(n) = vm.tonumber(vm.arg(narg))
    {
        vm.set_arg(narg, Value::Num(n));
        return Ok(num2bit(n) as u32 as u64);
    }
    Err(vm.argt_error(narg, "number"))
}

/// `bit_result64`: a new int64/uint64 cdata.
fn result64(vm: &mut Vm, id: u32, x: u64) -> LuaResult<usize> {
    let cd = vm.new_cdata(id, 8, 8);
    // SAFETY: the cdata owns 8 bytes of 8-byte aligned storage.
    unsafe { (cd.ptr() as *mut u64).write(x) };
    vm.ret1(Value::CData(cd))
}

/// Result of a 32-bit operation (`fff_resbit`).
fn result32(vm: &mut Vm, x: u32) -> LuaResult<usize> {
    vm.ret1(Value::Num(x as i32 as f64))
}

/// The argument as `lj_num2bit` (after `check64` validated/converted it).
fn arg_bit(vm: &Vm, narg: usize) -> u32 {
    match vm.arg(narg) {
        Value::Num(n) => num2bit(n) as u32,
        _ => unreachable!("bit argument checked"),
    }
}

fn bit_tobit(vm: &mut Vm) -> LuaResult<usize> {
    let mut id = 0;
    let x = check64(vm, 1, &mut id)?;
    result32(vm, x as u32)
}

fn bit_bnot(vm: &mut Vm) -> LuaResult<usize> {
    let mut id = 0;
    let x = check64(vm, 1, &mut id)?;
    if id != 0 {
        return result64(vm, id, !x);
    }
    result32(vm, !(x as u32))
}

fn bit_bswap(vm: &mut Vm) -> LuaResult<usize> {
    let mut id = 0;
    let x = check64(vm, 1, &mut id)?;
    if id != 0 {
        return result64(vm, id, x.swap_bytes());
    }
    result32(vm, (x as u32).swap_bytes())
}

/// `lj_carith_shift64`.
fn shift64(x: u64, sh: i32, op: u8) -> u64 {
    let sh = (sh & 63) as u32;
    match op {
        0 => x << sh,
        1 => x >> sh,
        2 => ((x as i64) >> sh) as u64,
        3 => x.rotate_left(sh),
        _ => x.rotate_right(sh),
    }
}

/// `bit.lshift`, `rshift`, `arshift`, `rol`, `ror` (distinguished by the builtin id).
fn bit_shift(vm: &mut Vm) -> LuaResult<usize> {
    let (mut id, mut id2) = (0, 0);
    let x = check64(vm, 1, &mut id)?;
    let sh = check64(vm, 2, &mut id2)? as i32;
    let op = vm.current_function().map_or(0, |f| f.ffid() - FF_BIT_LSHIFT);
    if id != 0 {
        return result64(vm, id, shift64(x, sh, op));
    }
    let x = arg_bit(vm, 1);
    let sh = (sh & 31) as u32;
    let r = match op {
        0 => x << sh,
        1 => x >> sh,
        2 => ((x as i32) >> sh) as u32,
        3 => x.rotate_left(sh),
        _ => x.rotate_right(sh),
    };
    result32(vm, r)
}

/// `bit.band`, `bor`, `bxor` (distinguished by the builtin id).
fn bit_nary(vm: &mut Vm) -> LuaResult<usize> {
    let mut id = 0;
    let n = vm.nargs().max(1);
    for i in 1..=n {
        check64(vm, i, &mut id)?;
    }
    let op = vm.current_function().map_or(0, |f| f.ffid() as i32 - FF_BIT_BOR as i32);
    if id != 0 {
        let mut y: u64 = if op >= 0 { 0 } else { !0 };
        for i in 1..=n {
            // lj_cconv_ct_tv to the result type: numbers convert like (u)int64 casts.
            let x = match vm.arg(i) {
                Value::Num(d) => {
                    if id == CTID_UINT64 {
                        crate::text::num2u64(d)
                    } else {
                        crate::text::num2i64(d) as u64
                    }
                }
                Value::CData(cd) => {
                    let mut id3 = id;
                    match vm.rt.ffi.check64 {
                        Some(f) => f(vm, cd, i as u32, &mut id3)?,
                        None => unreachable!("cdata operand checked"),
                    }
                }
                _ => unreachable!("bit argument checked"),
            };
            if op < 0 {
                y &= x;
            } else if op == 0 {
                y |= x;
            } else {
                y ^= x;
            }
        }
        return result64(vm, id, y);
    }
    let mut y = arg_bit(vm, 1);
    for i in 2..=n {
        let x = arg_bit(vm, i);
        y = if op < 0 {
            y & x
        } else if op == 0 {
            y | x
        } else {
            y ^ x
        };
    }
    result32(vm, y)
}

fn bit_tohex(vm: &mut Vm) -> LuaResult<usize> {
    let (mut id, mut id2) = (0, 0);
    let mut b = check64(vm, 1, &mut id)?;
    let mut n = if vm.nargs() < 2 { if id != 0 { 16 } else { 8 } } else { check64(vm, 2, &mut id2)? as i32 };
    let mut sf = FormatType::Uint as u32 | T_HEX;
    if n < 0 {
        n = (!(n as u32)).wrapping_add(1) as i32;
        sf |= F_UPPER;
    }
    if n as u32 > 254 {
        n = 254;
    }
    sf |= ((n as u32 + 1) & 255) << SH_PREC;
    if n < 16 {
        b &= (1u64 << (4 * n)) - 1;
    }
    let mut out = Vec::new();
    strfmt::put_fxint(&mut out, SFormat(sf), b);
    vm.push_str(&out);
    vm.gc_safe();
    Ok(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shifts64() {
        assert_eq!(shift64(1, 65, 0), 2);
        assert_eq!(shift64(0x8000_0000_0000_0000, 63, 2), u64::MAX);
        assert_eq!(shift64(1, -1, 3), 0x8000_0000_0000_0000);
        assert_eq!(crate::cdata::CTID_INT64 + 1, CTID_UINT64);
    }
}
