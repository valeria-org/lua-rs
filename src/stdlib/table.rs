//! Table library. Port of lib_table.c (the `LJLIB_LUA` functions `foreachi`, `foreach`,
//! `getn`, `remove` and `move` are upstream's precompiled bytecode) plus `lj_buf_puttab`
//! (lj_buf.c) for `table.concat`.

use crate::error::LuaResult;
use crate::func::NativeFn;
use crate::gc::Gc;
use crate::state::Vm;
use crate::table::Table;
use crate::value::Value;
use crate::vm::ffid::{FF_TABLE_CLEAR, FF_TABLE_NEW};
use crate::vm::{ErrMsg, fmt_msg};

use super::libinit;

/// `lj_lib_init_table` from the reference build's lj_libdef.h.
#[rustfmt::skip]
static LIB_INIT_TABLE: &[u8] = &[
    90,57,9,249,8,102,111,114,101,97,99,104,105,0,2,10,0,0,0,15,16,0,12,0,16,1,9,0,41,2,1,0,21,3,0,
    0,41,4,1,0,77,2,8,128,18,6,1,0,18,8,5,0,59,9,5,0,66,6,3,2,10,6,0,0,88,7,1,128,76,6,2,0,79,2,248,
    127,75,0,1,0,249,7,102,111,114,101,97,99,104,0,2,11,0,0,1,16,16,0,12,0,16,1,9,0,43,2,0,0,18,3,0,
    0,42,4,0,0,88,5,7,128,18,7,1,0,18,9,5,0,18,10,6,0,66,7,3,2,10,7,0,0,88,8,1,128,76,7,2,0,70,5,3,
    3,82,5,247,127,75,0,1,0,1,255,255,249,255,15,249,4,103,101,116,110,0,1,2,0,0,0,3,16,0,12,0,21,1,
    0,0,76,1,2,0,4,109,97,120,110,6,105,110,115,101,114,116,249,6,114,101,109,111,118,101,0,2,10,0,
    0,2,30,16,0,12,0,21,2,0,0,11,1,0,0,88,3,7,128,8,2,0,0,88,3,23,128,59,3,2,0,43,4,0,0,64,4,2,0,76,
    3,2,0,88,3,18,128,17,1,15,0,41,3,1,0,3,3,1,0,88,3,14,128,3,1,2,0,88,3,12,128,59,3,1,0,22,4,1,1,
    18,5,2,0,41,6,1,0,77,4,4,128,23,8,1,7,59,9,7,0,64,9,8,0,79,4,252,127,43,4,0,0,64,4,2,0,76,3,2,0,
    75,0,1,0,0,2,249,4,109,111,118,101,0,5,12,0,0,0,35,16,0,12,0,17,1,15,0,17,2,15,0,17,3,15,0,11,4,
    0,0,88,5,1,128,18,4,0,0,16,4,12,0,3,1,2,0,88,5,24,128,33,5,1,3,0,2,3,0,88,6,4,128,2,3,1,0,88,6,
    2,128,4,4,0,0,88,6,9,128,18,6,1,0,18,7,2,0,41,8,1,0,77,6,4,128,32,10,5,9,59,11,9,0,64,11,10,4,
    79,6,252,127,88,6,8,128,18,6,2,0,18,7,1,0,41,8,255,255,77,6,4,128,32,10,5,9,59,11,9,0,64,11,10,
    4,79,6,252,127,76,4,2,0,6,99,111,110,99,97,116,4,115,111,114,116,254,254,255,
];

/// `luaopen_table`.
pub fn open_table(vm: &mut Vm) -> LuaResult<()> {
    let funcs: &[NativeFn] = &[table_maxn, table_insert, table_concat, table_sort];
    let mut stack = Vec::new();
    let lib = libinit::lib_register(vm, Some("table"), LIB_INIT_TABLE, funcs, &mut stack)?;
    libinit::lib_prereg(vm, "table.new", luaopen_table_new, lib)?;
    libinit::lib_prereg(vm, "table.clear", luaopen_table_clear, lib)?;
    Ok(())
}

fn table_maxn(vm: &mut Vm) -> LuaResult<usize> {
    let t = vm.check_tab(1)?;
    let mut m = 0.0;
    for i in (0..t.asize()).rev() {
        if !t.array_get(i).is_nil() {
            m = i as i32 as f64;
            break;
        }
    }
    for i in (0..t.node_count()).rev() {
        let (k, v) = t.node_get(i);
        if !v.is_nil()
            && let Value::Num(n) = k
            && n > m
        {
            m = n;
        }
    }
    vm.ret1(Value::Num(m))
}

fn table_insert(vm: &mut Vm) -> LuaResult<usize> {
    let t = vm.check_tab(1)?;
    let mut i = (t.len() as i32).wrapping_add(1);
    let nargs = vm.nargs();
    if nargs != 2 {
        if nargs != 3 {
            return Err(vm.err_caller(ErrMsg::TABINS));
        }
        let n = vm.check_int(2)?;
        while i > n {
            let src = t.get_int(i - 1);
            t.set_int(i, src).map_err(|e| vm.tab_error(e))?;
            i -= 1;
        }
        i = n;
    }
    let v = vm.arg(nargs);
    t.set_int(i, v).map_err(|e| vm.tab_error(e))?;
    Ok(0)
}

/// `lj_buf_puttab`: `Err(index)` for an element that is not a string or number.
fn buf_puttab(out: &mut Vec<u8>, t: Gc<Table>, sep: &[u8], mut i: i32, e: i32) -> Result<(), i32> {
    if i <= e {
        loop {
            match t.get_int(i) {
                Value::Str(s) => out.extend_from_slice(s.as_bytes()),
                Value::Num(n) => crate::text::strfmt::put_num(out, n),
                _ => return Err(i),
            }
            if i == e {
                break;
            }
            i += 1;
            out.extend_from_slice(sep);
        }
    }
    Ok(())
}

fn table_concat(vm: &mut Vm) -> LuaResult<usize> {
    let t = vm.check_tab(1)?;
    let sep = vm.opt_str(2)?;
    let i = vm.opt_int(3, 1)?;
    let e = if vm.nargs() >= 4 && !vm.arg(4).is_nil() {
        vm.check_int(4)?
    } else {
        t.len() as i32
    };
    let mut out = Vec::new();
    let sepb: &[u8] = sep.as_ref().map_or(&[], |s| s.as_bytes());
    if let Err(idx) = buf_puttab(&mut out, t, sepb, i, e) {
        let tn = t.get_int(idx).type_name();
        let m = fmt_msg(ErrMsg::TABCAT, &[tn.as_bytes(), idx.to_string().as_bytes()]);
        return Err(vm.err_callermsg(&m));
    }
    let s = super::string::new_str_checked(vm, &out)?;
    vm.push(Value::Str(s));
    vm.gc_safe();
    Ok(1)
}

// -- table.sort -------------------------------------------------------------------------------
// A faithful port of upstream's quicksort over the Lua stack (lua_rawgeti/lua_rawseti on
// argument 1), so the compared values stay rooted while the comparator runs.

/// `lua_rawgeti(L, 1, i)`.
fn rawgeti(vm: &mut Vm, t: Gc<Table>, i: i32) {
    vm.push(t.get_int(i));
}

/// `lua_rawseti(L, 1, i)`: `t[i] = pop()`.
fn rawseti(vm: &mut Vm, t: Gc<Table>, i: i32) -> LuaResult<()> {
    let v = vm.pop();
    t.set_int(i, v).map_err(|e| vm.tab_error(e))
}

fn set2(vm: &mut Vm, t: Gc<Table>, i: i32, j: i32) -> LuaResult<()> {
    rawseti(vm, t, i)?;
    rawseti(vm, t, j)
}

fn pop_n(vm: &mut Vm, n: usize) {
    let top = vm.nargs();
    vm.set_top(top - n);
}

/// `sort_comp`: `a < b` for the values at negative stack indices `a` and `b`.
fn sort_comp(vm: &mut Vm, a: i32, b: i32) -> LuaResult<bool> {
    let va = vm.get(a);
    let vb = vm.get(b);
    let f = vm.arg(2);
    if !f.is_nil() {
        vm.call_value(f, &[va, vb], 1)?;
        Ok(vm.pop().is_truthy())
    } else {
        vm.less_than(va, vb)
    }
}

fn auxsort(vm: &mut Vm, t: Gc<Table>, mut l: i32, mut u: i32) -> LuaResult<()> {
    while l < u {
        // Sort elements a[l], a[(l+u)/2] and a[u].
        rawgeti(vm, t, l);
        rawgeti(vm, t, u);
        if sort_comp(vm, -1, -2)? {
            set2(vm, t, l, u)?; // Swap a[l] - a[u].
        } else {
            pop_n(vm, 2);
        }
        if u - l == 1 {
            break; // Only 2 elements.
        }
        let mut i = ((l as i64 + u as i64) / 2) as i32;
        rawgeti(vm, t, i);
        rawgeti(vm, t, l);
        if sort_comp(vm, -2, -1)? {
            set2(vm, t, i, l)?; // a[i] < a[l]?
        } else {
            pop_n(vm, 1); // Remove a[l].
            rawgeti(vm, t, u);
            if sort_comp(vm, -1, -2)? {
                set2(vm, t, i, u)?; // a[u] < a[i]?
            } else {
                pop_n(vm, 2);
            }
        }
        if u - l == 2 {
            break; // Only 3 elements.
        }
        rawgeti(vm, t, i); // Pivot.
        let p = vm.get(-1);
        vm.push(p);
        rawgeti(vm, t, u - 1);
        set2(vm, t, i, u - 1)?;
        // a[l] <= P == a[u-1] <= a[u], only need to sort from l+1 to u-2.
        i = l;
        let mut j = u - 1;
        loop {
            // Invariant: a[l..i] <= P <= a[j..u].
            // Repeat ++i until a[i] >= P.
            loop {
                i += 1;
                rawgeti(vm, t, i);
                if !sort_comp(vm, -1, -2)? {
                    break;
                }
                if i >= u {
                    return Err(vm.err_caller(ErrMsg::TABSORT));
                }
                pop_n(vm, 1); // Remove a[i].
            }
            // Repeat --j until a[j] <= P.
            loop {
                j -= 1;
                rawgeti(vm, t, j);
                if !sort_comp(vm, -3, -1)? {
                    break;
                }
                if j <= l {
                    return Err(vm.err_caller(ErrMsg::TABSORT));
                }
                pop_n(vm, 1); // Remove a[j].
            }
            if j < i {
                pop_n(vm, 3); // Pop pivot, a[i], a[j].
                break;
            }
            set2(vm, t, i, j)?;
        }
        rawgeti(vm, t, u - 1);
        rawgeti(vm, t, i);
        set2(vm, t, u - 1, i)?; // Swap pivot (a[u-1]) with a[i].
        // a[l..i-1] <= a[i] == P <= a[i+1..u].
        // Adjust so that smaller half is in [j..i] and larger one in [l..u].
        if i - l < u - i {
            j = l;
            i -= 1;
            l = i + 2;
        } else {
            j = i + 1;
            i = u;
            u = j - 2;
        }
        auxsort(vm, t, j, i)?; // Call recursively the smaller one.
    } // Repeat the routine for the larger one.
    Ok(())
}

fn table_sort(vm: &mut Vm) -> LuaResult<usize> {
    let t = vm.check_tab(1)?;
    let n = t.len() as i32;
    vm.set_top(2);
    if !vm.arg(2).is_nil() {
        vm.check_func(2)?;
    }
    auxsort(vm, t, 1, n)?;
    Ok(0)
}

// -- table.new / table.clear ------------------------------------------------------------------

fn table_new(vm: &mut Vm) -> LuaResult<usize> {
    let a = vm.check_int(1)?;
    let h = vm.check_int(2)?;
    let t = vm.new_table_ah(a.max(0) as u32, h.max(0) as u32)?;
    vm.ret1(Value::Table(t))
}

fn table_clear(vm: &mut Vm) -> LuaResult<usize> {
    let t = vm.check_tab(1)?;
    t.clear();
    Ok(0)
}

fn luaopen_table_new(vm: &mut Vm) -> LuaResult<usize> {
    libinit::lib_postreg(vm, table_new, FF_TABLE_NEW, "new")
}

fn luaopen_table_clear(vm: &mut Vm) -> LuaResult<usize> {
    libinit::lib_postreg(vm, table_clear, FF_TABLE_CLEAR, "clear")
}
