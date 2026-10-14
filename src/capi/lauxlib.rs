//! `luaL_*` functions. Port of lib_aux.c plus the auxiliary functions upstream keeps in
//! lj_api.c (`luaL_check*`/`luaL_opt*`, `luaL_newmetatable`, `luaL_*udata`, ...), lj_err.c
//! (`luaL_argerror`, `luaL_typerror`, `luaL_where`; `luaL_error` is in `vararg`), lj_load.c
//! (`luaL_load*`) and lj_debug.c (`luaL_traceback`).

use std::ffi::{c_char, c_int, c_void};

use crate::error::LuaError;
use crate::gc::Gc;
use crate::state::Vm;
use crate::thread::Thread;
use crate::value::Value;
use crate::vm::{ErrMsg, fmt_msg};

use super::lapi::*;
use super::*;

/// `LUA_REFNIL`.
const LUA_REFNIL: c_int = -1;
/// Free-list head of reference tables.
const FREELIST_REF: c_int = 0;

// -- Argument errors (lj_err.c) -----------------------------------------------------------------

/// `lj_err_argtype`: `bad argument #narg to 'f' (xname expected, got tname)`.
fn argtype(vm: &mut Vm, th: Gc<Thread>, narg: c_int, xname: &[u8]) -> LuaError {
    let tname: &[u8] = if narg <= LUA_REGISTRYINDEX {
        if narg >= LUA_GLOBALSINDEX {
            b"table"
        } else {
            match idx_get(vm, th, narg) {
                Some(v) => v.type_name().as_bytes(),
                None => b"no value",
            }
        }
    } else {
        match idx_get(vm, th, narg) {
            Some(v) => v.type_name().as_bytes(),
            None => b"no value",
        }
    };
    let m = fmt_msg(ErrMsg::BADTYPE, &[xname, tname]);
    vm.arg_error(narg, &m)
}

/// `lj_err_argt`: type error for one of the basic `LUA_T*` types.
fn argt(vm: &mut Vm, th: Gc<Thread>, narg: c_int, tt: c_int) -> LuaError {
    let name = TYPENAMES[(tt + 1) as usize].to_bytes();
    argtype(vm, th, narg, name)
}

/// `luaL_argerror`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn luaL_argerror(
    L: *mut lua_State,
    narg: c_int,
    msg: *const c_char,
) -> c_int {
    // SAFETY: `L` is a live state; `msg` is a C string.
    unsafe { api(L, |vm, _| Err(vm.arg_error(narg, cstr(msg)))) }
}

/// `luaL_typerror`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn luaL_typerror(
    L: *mut lua_State,
    narg: c_int,
    xname: *const c_char,
) -> c_int {
    // SAFETY: `L` is a live state; `xname` is a C string.
    unsafe { api(L, |vm, th| Err(argtype(vm, th, narg, cstr(xname)))) }
}

/// `luaL_where`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn luaL_where(L: *mut lua_State, level: c_int) {
    // SAFETY: `L` is a live state.
    unsafe {
        api(L, |vm, _| {
            let s = vm.where_(level);
            vm.push_str(&s);
            Ok(())
        })
    }
}

// -- Argument checks (lj_api.c) -----------------------------------------------------------------

/// `luaL_checktype`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn luaL_checktype(L: *mut lua_State, idx: c_int, tt: c_int) {
    // SAFETY: `L` is a live state.
    unsafe {
        api(L, |vm, th| {
            let t = idx_get(vm, th, idx).map_or(LUA_TNONE, type_of);
            if t != tt {
                return Err(argt(vm, th, idx, tt));
            }
            Ok(())
        })
    }
}

/// `luaL_checkany`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn luaL_checkany(L: *mut lua_State, idx: c_int) {
    // SAFETY: `L` is a live state.
    unsafe {
        api(L, |vm, th| match idx_get(vm, th, idx) {
            Some(_) => Ok(()),
            None => Err(vm.arg_error_em(idx, ErrMsg::NOVAL)),
        })
    }
}

/// `luaL_checknumber`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn luaL_checknumber(L: *mut lua_State, idx: c_int) -> lua_Number {
    // SAFETY: `L` is a live state.
    unsafe {
        api(L, |vm, th| match idx_val(vm, th, idx) {
            Value::Num(n) => Ok(n),
            Value::Str(s) if let Some(n) = crate::text::strscan::scan_num(s.as_bytes()) => Ok(n),
            _ => Err(argt(vm, th, idx, LUA_TNUMBER)),
        })
    }
}

/// `luaL_optnumber`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn luaL_optnumber(
    L: *mut lua_State,
    idx: c_int,
    def: lua_Number,
) -> lua_Number {
    // SAFETY: `L` is a live state.
    unsafe {
        api(L, |vm, th| match idx_val(vm, th, idx) {
            Value::Num(n) => Ok(n),
            Value::Nil => Ok(def),
            Value::Str(s) if let Some(n) = crate::text::strscan::scan_num(s.as_bytes()) => Ok(n),
            _ => Err(argt(vm, th, idx, LUA_TNUMBER)),
        })
    }
}

/// `luaL_checkinteger`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn luaL_checkinteger(L: *mut lua_State, idx: c_int) -> lua_Integer {
    // SAFETY: `L` is a live state.
    unsafe {
        api(L, |vm, th| match idx_val(vm, th, idx) {
            Value::Num(n) => Ok(crate::text::num2i64(n) as lua_Integer),
            Value::Str(s) if let Some(n) = crate::text::strscan::scan_num(s.as_bytes()) => {
                Ok(crate::text::num2i64(n) as lua_Integer)
            }
            _ => Err(argt(vm, th, idx, LUA_TNUMBER)),
        })
    }
}

/// `luaL_optinteger`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn luaL_optinteger(
    L: *mut lua_State,
    idx: c_int,
    def: lua_Integer,
) -> lua_Integer {
    // SAFETY: `L` is a live state.
    unsafe {
        api(L, |vm, th| match idx_val(vm, th, idx) {
            Value::Num(n) => Ok(crate::text::num2i64(n) as lua_Integer),
            Value::Nil => Ok(def),
            Value::Str(s) if let Some(n) = crate::text::strscan::scan_num(s.as_bytes()) => {
                Ok(crate::text::num2i64(n) as lua_Integer)
            }
            _ => Err(argt(vm, th, idx, LUA_TNUMBER)),
        })
    }
}

/// `luaL_checklstring`: numbers are converted in place.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn luaL_checklstring(
    L: *mut lua_State,
    idx: c_int,
    len: *mut usize,
) -> *const c_char {
    // SAFETY: `L` is a live state; `len` is NULL or valid.
    unsafe {
        api(L, |vm, th| {
            let s = match idx_val(vm, th, idx) {
                Value::Str(s) => s,
                Value::Num(n) => tostr_inplace(vm, th, idx, n),
                _ => return Err(argt(vm, th, idx, LUA_TSTRING)),
            };
            if !len.is_null() {
                *len = s.len();
            }
            Ok(strdata(s))
        })
    }
}

/// `luaL_optlstring`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn luaL_optlstring(
    L: *mut lua_State,
    idx: c_int,
    def: *const c_char,
    len: *mut usize,
) -> *const c_char {
    // SAFETY: `L` is a live state; `def` is NULL or a C string; `len` is NULL or valid.
    unsafe {
        api(L, |vm, th| {
            let s = match idx_val(vm, th, idx) {
                Value::Str(s) => s,
                Value::Nil => {
                    if !len.is_null() {
                        *len = if def.is_null() { 0 } else { cstr(def).len() };
                    }
                    return Ok(def);
                }
                Value::Num(n) => tostr_inplace(vm, th, idx, n),
                _ => return Err(argt(vm, th, idx, LUA_TSTRING)),
            };
            if !len.is_null() {
                *len = s.len();
            }
            Ok(strdata(s))
        })
    }
}

/// `luaL_checkoption`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn luaL_checkoption(
    L: *mut lua_State,
    idx: c_int,
    def: *const c_char,
    lst: *const *const c_char,
) -> c_int {
    // SAFETY: `L` is a live state; `def` is NULL or a C string; `lst` is a NULL-terminated array.
    unsafe {
        let s = lua_tolstring(L, idx, std::ptr::null_mut());
        api(L, |vm, th| {
            let s = if !s.is_null() {
                cstr(s)
            } else if !def.is_null() {
                cstr(def)
            } else {
                return Err(argt(vm, th, idx, LUA_TSTRING));
            };
            let mut i = 0;
            while !(*lst.add(i)).is_null() {
                if cstr(*lst.add(i)) == s {
                    return Ok(i as c_int);
                }
                i += 1;
            }
            let m = fmt_msg(ErrMsg::INVOPTM, &[s]);
            Err(vm.arg_error(idx, &m))
        })
    }
}

/// `luaL_checkstack`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn luaL_checkstack(
    L: *mut lua_State,
    size: c_int,
    msg: *const c_char,
) {
    // SAFETY: `L` is a live state; `msg` is a C string.
    unsafe {
        if lua_checkstack(L, size) == 0 {
            api(L, |vm, _| Err::<(), _>(stkovm(vm, cstr(msg))));
        }
    }
}

// -- Metatables and userdata --------------------------------------------------------------------

/// `luaL_newmetatable`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn luaL_newmetatable(
    L: *mut lua_State,
    tname: *const c_char,
) -> c_int {
    // SAFETY: `L` is a live state; `tname` is a C string.
    unsafe {
        api(L, |vm, _| {
            let k = vm.new_str(cstr(tname));
            let reg = vm.registry;
            match reg.get_str(k) {
                Value::Nil => {
                    let mt = vm.new_table(0, 1)?;
                    reg.set_str(k, Value::Table(mt))
                        .map_err(|e| vm.tab_error(e))?;
                    vm.push(Value::Table(mt));
                    Ok(1)
                }
                v => {
                    vm.push(v);
                    Ok(0)
                }
            }
        })
    }
}

/// `luaL_setmetatable`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn luaL_setmetatable(L: *mut lua_State, tname: *const c_char) {
    // SAFETY: forwarded contract.
    unsafe {
        lua_getfield(L, LUA_REGISTRYINDEX, tname);
        lua_setmetatable(L, -2);
    }
}

/// `luaL_testudata`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn luaL_testudata(
    L: *mut lua_State,
    idx: c_int,
    tname: *const c_char,
) -> *mut c_void {
    // SAFETY: `L` is a live state; `tname` is a C string.
    unsafe {
        api(L, |vm, th| {
            if let Value::Udata(u) = idx_val(vm, th, idx) {
                let k = vm.new_str(cstr(tname));
                if let Value::Table(mt) = vm.registry.get_str(k)
                    && u.metatable() == Some(mt)
                {
                    return Ok(u.data_ptr().cast());
                }
            }
            Ok(std::ptr::null_mut())
        })
    }
}

/// `luaL_checkudata`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn luaL_checkudata(
    L: *mut lua_State,
    idx: c_int,
    tname: *const c_char,
) -> *mut c_void {
    // SAFETY: forwarded contract.
    unsafe {
        let p = luaL_testudata(L, idx, tname);
        if p.is_null() {
            api(L, |vm, th| Err::<(), _>(argtype(vm, th, idx, cstr(tname))));
        }
        p
    }
}

/// `luaL_getmetafield`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn luaL_getmetafield(
    L: *mut lua_State,
    idx: c_int,
    field: *const c_char,
) -> c_int {
    // SAFETY: `L` is a live state; `field` is a C string.
    unsafe {
        api(L, |vm, th| {
            let v = idx_val(vm, th, idx);
            if let Some(mt) = vm.metatable_of(v) {
                let k = vm.new_str(cstr(field));
                let f = mt.get_str(k);
                if !f.is_nil() {
                    vm.push(f);
                    return Ok(1);
                }
            }
            Ok(0)
        })
    }
}

/// `luaL_callmeta`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn luaL_callmeta(
    L: *mut lua_State,
    idx: c_int,
    field: *const c_char,
) -> c_int {
    // SAFETY: `L` is a live state; `field` is a C string.
    unsafe {
        api(L, |vm, th| {
            let o = idx_val(vm, th, idx);
            if let Some(mt) = vm.metatable_of(o) {
                let k = vm.new_str(cstr(field));
                let f = mt.get_str(k);
                if !f.is_nil() {
                    vm.push(f);
                    vm.push(o);
                    vm.call(1, 1)?;
                    return Ok(1);
                }
            }
            Ok(0)
        })
    }
}

// -- Module registration (lib_aux.c) ------------------------------------------------------------

/// `luaL_findtable`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn luaL_findtable(
    L: *mut lua_State,
    idx: c_int,
    fname: *const c_char,
    szhint: c_int,
) -> *const c_char {
    // SAFETY: `L` is a live state; `fname` is a C string.
    unsafe {
        let mut fname = fname;
        lua_pushvalue(L, idx);
        loop {
            let s = cstr(fname);
            let n = s.iter().position(|&c| c == b'.').unwrap_or(s.len());
            let e = fname.add(n);
            lua_pushlstring(L, fname, n);
            lua_rawget(L, -2);
            if lua_type(L, -1) == LUA_TNIL {
                lua_settop(L, -2);
                lua_createtable(L, 0, if *e == b'.' as c_char { 1 } else { szhint });
                lua_pushlstring(L, fname, n);
                lua_pushvalue(L, -2);
                lua_settable(L, -4);
            } else if lua_type(L, -1) != LUA_TTABLE {
                lua_settop(L, -3);
                return fname;
            }
            lua_remove(L, -2);
            fname = e.add(1);
            if *e != b'.' as c_char {
                return std::ptr::null();
            }
        }
    }
}

/// Number of entries of a `luaL_Reg` array.
unsafe fn libsize(l: *const luaL_Reg) -> c_int {
    let mut n = 0;
    if !l.is_null() {
        // SAFETY: NULL-name-terminated array.
        unsafe {
            while !(*l.add(n)).name.is_null() {
                n += 1;
            }
        }
    }
    n as c_int
}

/// `luaL_pushmodule`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn luaL_pushmodule(
    L: *mut lua_State,
    modname: *const c_char,
    sizehint: c_int,
) {
    // SAFETY: `L` is a live state; `modname` is a C string.
    unsafe {
        luaL_findtable(L, LUA_REGISTRYINDEX, c"_LOADED".as_ptr(), 16);
        lua_getfield(L, -1, modname);
        if lua_type(L, -1) != LUA_TTABLE {
            lua_settop(L, -2);
            if !luaL_findtable(L, LUA_GLOBALSINDEX, modname, sizehint).is_null() {
                api(L, |vm, _| {
                    let m = fmt_msg(ErrMsg::BADMODN, &[cstr(modname)]);
                    Err::<(), _>(vm.err_callermsg(&m))
                });
            }
            lua_pushvalue(L, -1);
            lua_setfield(L, -3, modname);
        }
        lua_remove(L, -2);
    }
}

/// `luaL_openlib`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn luaL_openlib(
    L: *mut lua_State,
    libname: *const c_char,
    l: *const luaL_Reg,
    nup: c_int,
) {
    // SAFETY: `L` is a live state; `libname` is NULL or a C string; `l` is NULL or a
    // NULL-terminated array.
    unsafe {
        if !libname.is_null() {
            luaL_pushmodule(L, libname, libsize(l));
            lua_insert(L, -(nup + 1));
        }
        if !l.is_null() {
            luaL_setfuncs(L, l, nup);
        } else {
            lua_settop(L, -nup - 1);
        }
    }
}

/// `luaL_register`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn luaL_register(
    L: *mut lua_State,
    libname: *const c_char,
    l: *const luaL_Reg,
) {
    // SAFETY: forwarded contract.
    unsafe { luaL_openlib(L, libname, l, 0) }
}

/// `luaL_setfuncs`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn luaL_setfuncs(L: *mut lua_State, l: *const luaL_Reg, nup: c_int) {
    // SAFETY: `L` is a live state; `l` is a NULL-terminated array.
    unsafe {
        luaL_checkstack(L, nup, c"too many upvalues".as_ptr());
        let mut l = l;
        while !(*l).name.is_null() {
            for _ in 0..nup {
                lua_pushvalue(L, -nup);
            }
            match (*l).func {
                Some(f) => lua_pushcclosure(L, f, nup),
                None => {
                    lua_settop(L, -nup - 1);
                    lua_pushboolean(L, 0);
                }
            }
            lua_setfield(L, -(nup + 2), (*l).name);
            l = l.add(1);
        }
        lua_settop(L, -nup - 1);
    }
}

/// `luaL_gsub`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn luaL_gsub(
    L: *mut lua_State,
    s: *const c_char,
    p: *const c_char,
    r: *const c_char,
) -> *const c_char {
    // SAFETY: `L` is a live state; the strings are C strings.
    unsafe {
        let (s, p, r) = (cstr(s), cstr(p), cstr(r));
        let mut out = Vec::with_capacity(s.len());
        let mut rest = s;
        if !p.is_empty() {
            while let Some(i) = rest.windows(p.len()).position(|w| w == p) {
                out.extend_from_slice(&rest[..i]);
                out.extend_from_slice(r);
                rest = &rest[i + p.len()..];
            }
        } else {
            // An empty pattern never terminates upstream (strstr matches without advancing);
            // here the replacement is inserted once in front.
            out.extend_from_slice(r);
        }
        out.extend_from_slice(rest);
        lua_pushlstring(L, out.as_ptr().cast(), out.len());
        lua_tolstring(L, -1, std::ptr::null_mut())
    }
}

// -- Buffers ------------------------------------------------------------------------------------

#[inline]
unsafe fn bufflen(b: *const luaL_Buffer) -> usize {
    // SAFETY: `b` is a valid buffer whose `p` points into `buffer`.
    unsafe { (*b).p.offset_from((*b).buffer.as_ptr()) as usize }
}

#[inline]
unsafe fn bufffree(b: *const luaL_Buffer) -> usize {
    // SAFETY: as `bufflen`.
    LUAL_BUFFERSIZE - unsafe { bufflen(b) }
}

unsafe fn emptybuffer(b: *mut luaL_Buffer) -> bool {
    // SAFETY: `b` is a valid buffer of a live state.
    unsafe {
        let l = bufflen(b);
        if l == 0 {
            return false;
        }
        lua_pushlstring((*b).L, (*b).buffer.as_ptr(), l);
        (*b).p = (*b).buffer.as_mut_ptr();
        (*b).lvl += 1;
        true
    }
}

unsafe fn adjuststack(b: *mut luaL_Buffer) {
    // SAFETY: `b` is a valid buffer of a live state.
    unsafe {
        if (*b).lvl > 1 {
            let L = (*b).L;
            let mut toget = 1;
            let mut toplen = lua_objlen(L, -1);
            loop {
                let l = lua_objlen(L, -(toget + 1));
                if !((*b).lvl - toget + 1 >= 20 / 2 || toplen > l) {
                    break;
                }
                toplen += l;
                toget += 1;
                if toget >= (*b).lvl {
                    break;
                }
            }
            lua_concat(L, toget);
            (*b).lvl = (*b).lvl - toget + 1;
        }
    }
}

/// `luaL_buffinit`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn luaL_buffinit(L: *mut lua_State, b: *mut luaL_Buffer) {
    // SAFETY: `b` points to a luaL_Buffer.
    unsafe {
        (*b).L = L;
        (*b).p = (*b).buffer.as_mut_ptr();
        (*b).lvl = 0;
    }
}

/// `luaL_prepbuffer`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn luaL_prepbuffer(b: *mut luaL_Buffer) -> *mut c_char {
    // SAFETY: `b` is a valid buffer.
    unsafe {
        if emptybuffer(b) {
            adjuststack(b);
        }
        (*b).buffer.as_mut_ptr()
    }
}

/// `luaL_addlstring`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn luaL_addlstring(b: *mut luaL_Buffer, s: *const c_char, l: usize) {
    // SAFETY: `b` is a valid buffer; `s` points to `l` bytes.
    unsafe {
        if l <= bufffree(b) {
            std::ptr::copy_nonoverlapping(s, (*b).p, l);
            (*b).p = (*b).p.add(l);
        } else {
            emptybuffer(b);
            lua_pushlstring((*b).L, s, l);
            (*b).lvl += 1;
            adjuststack(b);
        }
    }
}

/// `luaL_addstring`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn luaL_addstring(b: *mut luaL_Buffer, s: *const c_char) {
    // SAFETY: forwarded contract; `s` is a C string.
    unsafe { luaL_addlstring(b, s, cstr(s).len()) }
}

/// `luaL_pushresult`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn luaL_pushresult(b: *mut luaL_Buffer) {
    // SAFETY: `b` is a valid buffer.
    unsafe {
        emptybuffer(b);
        lua_concat((*b).L, (*b).lvl);
        (*b).lvl = 1;
    }
}

/// `luaL_addvalue`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn luaL_addvalue(b: *mut luaL_Buffer) {
    // SAFETY: `b` is a valid buffer.
    unsafe {
        let L = (*b).L;
        let mut vl = 0usize;
        let s = lua_tolstring(L, -1, &mut vl);
        if vl <= bufffree(b) {
            std::ptr::copy_nonoverlapping(s, (*b).p, vl);
            (*b).p = (*b).p.add(vl);
            lua_settop(L, -2);
        } else {
            if emptybuffer(b) {
                lua_insert(L, -2);
            }
            (*b).lvl += 1;
            adjuststack(b);
        }
    }
}

// -- References ---------------------------------------------------------------------------------

/// Absolute form of an index (`abs_index` of lib_aux.c).
unsafe fn abs_index(L: *mut lua_State, i: c_int) -> c_int {
    // SAFETY: `L` is a live state.
    if i > 0 || i <= LUA_REGISTRYINDEX {
        i
    } else {
        (unsafe { lua_gettop(L) }) + i + 1
    }
}

/// `luaL_ref`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn luaL_ref(L: *mut lua_State, t: c_int) -> c_int {
    // SAFETY: `L` is a live state.
    unsafe {
        let t = abs_index(L, t);
        if lua_type(L, -1) == LUA_TNIL {
            lua_settop(L, -2);
            return LUA_REFNIL;
        }
        lua_rawgeti(L, t, FREELIST_REF);
        let mut r = lua_tointeger(L, -1) as c_int;
        lua_settop(L, -2);
        if r != 0 {
            lua_rawgeti(L, t, r);
            lua_rawseti(L, t, FREELIST_REF);
        } else {
            r = lua_objlen(L, t) as c_int;
            r += 1;
        }
        lua_rawseti(L, t, r);
        r
    }
}

/// `luaL_unref`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn luaL_unref(L: *mut lua_State, t: c_int, r: c_int) {
    // SAFETY: `L` is a live state.
    unsafe {
        if r >= 0 {
            let t = abs_index(L, t);
            lua_rawgeti(L, t, FREELIST_REF);
            lua_rawseti(L, t, r);
            lua_pushinteger(L, r as lua_Integer);
            lua_rawseti(L, t, FREELIST_REF);
        }
    }
}

// -- Loading (lj_load.c) ------------------------------------------------------------------------

/// `luaL_loadfilex`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn luaL_loadfilex(
    L: *mut lua_State,
    filename: *const c_char,
    mode: *const c_char,
) -> c_int {
    // SAFETY: `L` is a live state; the strings are NULL or C strings.
    unsafe {
        let name = (!filename.is_null()).then(|| cstr(filename));
        let mode = (!mode.is_null()).then(|| cstr(mode));
        api(L, |vm, _| {
            let r = vm.load_file(name, mode);
            Ok(load_status(vm, r))
        })
    }
}

/// `luaL_loadfile`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn luaL_loadfile(L: *mut lua_State, filename: *const c_char) -> c_int {
    // SAFETY: forwarded contract.
    unsafe { luaL_loadfilex(L, filename, std::ptr::null()) }
}

/// `luaL_loadbufferx`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn luaL_loadbufferx(
    L: *mut lua_State,
    buf: *const c_char,
    size: usize,
    name: *const c_char,
    mode: *const c_char,
) -> c_int {
    // SAFETY: `L` is a live state; `buf` points to `size` bytes; the strings are NULL or C
    // strings.
    unsafe {
        let src: &[u8] = if size == 0 {
            &[]
        } else {
            std::slice::from_raw_parts(buf.cast(), size)
        };
        let name: &[u8] = if name.is_null() { b"?" } else { cstr(name) };
        let mode = (!mode.is_null()).then(|| cstr(mode));
        api(L, |vm, _| {
            let r = vm.load(src, name, mode);
            Ok(load_status(vm, r))
        })
    }
}

/// `luaL_loadbuffer`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn luaL_loadbuffer(
    L: *mut lua_State,
    buf: *const c_char,
    size: usize,
    name: *const c_char,
) -> c_int {
    // SAFETY: forwarded contract.
    unsafe { luaL_loadbufferx(L, buf, size, name, std::ptr::null()) }
}

/// `luaL_loadstring`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn luaL_loadstring(L: *mut lua_State, s: *const c_char) -> c_int {
    // SAFETY: `s` is a C string.
    unsafe { luaL_loadbuffer(L, s, cstr(s).len(), s) }
}

// -- State creation -----------------------------------------------------------------------------

/// The default panic function of `luaL_newstate`.
unsafe extern "C-unwind" fn panic(L: *mut c_void) -> c_int {
    let L: *mut lua_State = L.cast();
    // SAFETY: called with a live state and the error object on top.
    unsafe {
        let s = lua_tolstring(L, -1, std::ptr::null_mut());
        let msg: &[u8] = if s.is_null() { b"?" } else { cstr(s) };
        let mut out = b"PANIC: unprotected error in call to Lua API (".to_vec();
        out.extend_from_slice(msg);
        out.extend_from_slice(b")\n");
        libc::fwrite(out.as_ptr().cast(), 1, out.len(), stderr());
        libc::fflush(stderr());
    }
    0
}

unsafe extern "C" {
    #[link_name = "stderr"]
    static mut C_STDERR: *mut libc::FILE;
}

/// The C library's `stderr`.
fn stderr() -> *mut libc::FILE {
    // SAFETY: reading the glibc `stderr` pointer, initialized before main.
    unsafe { C_STDERR }
}

/// `luaL_newstate`: a new state with the default allocator, panic function and finalizer
/// error reporting.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn luaL_newstate() -> *mut lua_State {
    let L = new_boxed_state(Vm::new_state(), l_alloc, std::ptr::null_mut());
    // SAFETY: freshly created state.
    let (vm, _) = unsafe { ctx(L) };
    vm.rt.capi.panic = Some(panic);
    L
}

// -- I/O results --------------------------------------------------------------------------------

/// `strerror(en)`.
fn strerror(en: c_int) -> Vec<u8> {
    // SAFETY: strerror returns a valid C string.
    unsafe { cstr(libc::strerror(en)).to_vec() }
}

/// `luaL_fileresult`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn luaL_fileresult(
    L: *mut lua_State,
    stat: c_int,
    fname: *const c_char,
) -> c_int {
    let en = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
    // SAFETY: `L` is a live state; `fname` is NULL or a C string.
    unsafe {
        api(L, |vm, _| {
            if stat != 0 {
                vm.push(Value::Bool(true));
                return Ok(1);
            }
            vm.push(Value::Nil);
            let mut m = Vec::new();
            if !fname.is_null() {
                m.extend_from_slice(cstr(fname));
                m.extend_from_slice(b": ");
            }
            m.extend_from_slice(&strerror(en));
            vm.push_str(&m);
            vm.push(Value::Num(en as f64));
            Ok(3)
        })
    }
}

/// `luaL_execresult`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn luaL_execresult(L: *mut lua_State, stat: c_int) -> c_int {
    if stat == -1 {
        // SAFETY: forwarded contract.
        return unsafe { luaL_fileresult(L, 0, std::ptr::null()) };
    }
    // SAFETY: `L` is a live state.
    unsafe {
        api(L, |vm, _| {
            let mut stat = stat;
            if libc::WIFSIGNALED(stat) {
                stat = libc::WTERMSIG(stat);
                vm.push(Value::Nil);
                vm.push_str(b"signal");
            } else {
                if libc::WIFEXITED(stat) {
                    stat = libc::WEXITSTATUS(stat);
                }
                vm.push(if stat == 0 {
                    Value::Bool(true)
                } else {
                    Value::Nil
                });
                vm.push_str(b"exit");
            }
            vm.push(Value::Num(stat as f64));
            Ok(3)
        })
    }
}

// -- Traceback ----------------------------------------------------------------------------------

/// `luaL_traceback`: push a traceback of `L1` (starting at `level`) onto `L`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn luaL_traceback(
    L: *mut lua_State,
    L1: *mut lua_State,
    msg: *const c_char,
    level: c_int,
) {
    // SAFETY: both are live states of the same VM; `msg` is NULL or a C string.
    unsafe {
        let (_, th1) = ctx(L1);
        let msg = (!msg.is_null()).then(|| cstr(msg));
        api(L, |vm, _| {
            let tb = vm.traceback(th1, msg, level);
            vm.push_str(&tb);
            Ok(())
        })
    }
}
