//! String library. Port of lib_string.c, including the semantics of the assembler fast paths
//! of vm_x64.dasc (`string.byte`, `string.char`, `string.sub`, `string.reverse/lower/upper`)
//! and `lj_strfmt_putarg` (lj_strfmt.c) for `string.format` / `buffer:putf`.

use crate::bc::MM;
use crate::cdata::{CTID_INT64, CTID_UINT64};
use crate::error::{ErrorKind, LuaError, LuaResult};
use crate::func::NativeFn;
use crate::gc::Gc;
use crate::lstr::LuaStr;
use crate::state::Vm;
use crate::text::pattern::{self, Capture, MatchState, PatError};
use crate::text::strfmt::{self, FmtItem, FormatParser, FormatType, SFormat};
use crate::udata::Userdata;
use crate::value::{VType, Value};
use crate::vm::ffid::FF_STRING_GMATCH_AUX;
use crate::vm::{ErrMsg, fmt_msg};

use super::libinit;

/// `lj_lib_init_string` from the reference build's lj_libdef.h.
#[rustfmt::skip]
static LIB_INIT_STRING: &[u8] = &[
    76,51,14,249,3,108,101,110,0,1,2,0,0,0,3,16,0,5,0,21,1,0,0,76,1,2,0,68,98,121,116,101,68,99,104,
    97,114,67,115,117,98,3,114,101,112,71,114,101,118,101,114,115,101,133,108,111,119,101,114,133,
    117,112,112,101,114,4,100,117,109,112,4,102,105,110,100,5,109,97,116,99,104,254,6,103,109,97,
    116,99,104,4,103,115,117,98,6,102,111,114,109,97,116,255,
];

/// `LUAI_MAXCSTACK`.
const LUAI_MAXCSTACK: u32 = 8000;
/// `LJ_MAX_STR`.
pub const LJ_MAX_STR: u64 = 0x7fff_ff00;

/// `luaopen_string`.
pub fn open_string(vm: &mut Vm) -> LuaResult<()> {
    let funcs: &[NativeFn] = &[
        string_byte,
        string_char,
        string_sub,
        string_rep,
        string_reverse,
        string_lower,
        string_upper,
        string_dump,
        string_find,
        string_match,
        string_gmatch,
        string_gsub,
        string_format,
    ];
    let mut stack = Vec::new();
    let lib = libinit::lib_register(vm, Some("string"), LIB_INIT_STRING, funcs, &mut stack)?;
    let mt = vm.new_table(0, crate::table::hsize2hbits(1))?;
    vm.set_basemt(VType::Str, Some(mt));
    let idx = vm.mm_name(MM::Index);
    mt.set_str(idx, Value::Table(lib)).map_err(|e| vm.tab_error(e))?;
    mt.set_nomm(!(1u8 << MM::Index as u8));
    libinit::lib_prereg(vm, "string.buffer", super::buffer::luaopen_string_buffer, lib)?;
    Ok(())
}

/// `lj_err_mem`: "not enough memory" (`LUA_ERRMEM`, no position).
pub fn err_mem(vm: &mut Vm) -> LuaError {
    let v = vm.err_str(ErrMsg::ERRMEM);
    LuaError::new(ErrorKind::Mem, v)
}

/// Pattern matcher errors are raised with `lj_err_caller`.
fn pat_error(vm: &mut Vm, e: PatError) -> LuaError {
    vm.err_callermsg(e.message().as_bytes())
}

// -- Simple functions -------------------------------------------------------------------------

fn string_byte(vm: &mut Vm) -> LuaResult<usize> {
    let s = vm.check_str(1)?;
    let len = s.len() as i32;
    let mut start = vm.opt_int(2, 1)?;
    let mut stop = vm.opt_int(3, start)?;
    if stop < 0 {
        stop = stop.wrapping_add(len + 1);
    }
    if start < 0 {
        start = start.wrapping_add(len + 1);
    }
    if start <= 0 {
        start = 1;
    }
    if stop > len {
        stop = len;
    }
    if start > stop {
        return Ok(0); // Empty interval: return no results.
    }
    start -= 1;
    let n = stop.wrapping_sub(start);
    if n as u32 > LUAI_MAXCSTACK {
        return Err(vm.err_caller(ErrMsg::STRSLC));
    }
    vm.check_stack(n as usize, None)?;
    for &c in &s.as_bytes()[start as usize..stop as usize] {
        vm.push(Value::Num(c as f64));
    }
    Ok(n as usize)
}

fn string_char(vm: &mut Vm) -> LuaResult<usize> {
    let nargs = vm.nargs();
    let mut buf = Vec::with_capacity(nargs);
    for i in 1..=nargs {
        let k = vm.check_int(i)?;
        if k as u32 > 255 {
            return Err(vm.arg_error_em(i as i32, ErrMsg::BADVAL));
        }
        buf.push(k as u8);
    }
    vm.push_str(&buf);
    Ok(1)
}

fn string_sub(vm: &mut Vm) -> LuaResult<usize> {
    let s = vm.check_str(1)?;
    let mut start = vm.check_int(2)?;
    let mut end = vm.opt_int(3, -1)?;
    let len = s.len() as i32;
    // Fast path semantics (vm_x64.dasc `string_sub`).
    if (len as u32) < end as u32 {
        end = if end < 0 { end.wrapping_add(len + 1) } else { len };
    }
    if start <= 0 {
        start = if start == 0 { 1 } else { start.wrapping_add(len + 1) };
        if start <= 0 {
            start = 1;
        }
    }
    let r = if end < start { &[][..] } else { &s.as_bytes()[(start - 1) as usize..end as usize] };
    vm.push_str(r);
    Ok(1)
}

fn string_rep(vm: &mut Vm) -> LuaResult<usize> {
    let mut s = vm.check_str(1)?;
    let mut rep = vm.check_int(2)?;
    let sep = vm.opt_str(3)?;
    let mut out = Vec::new();
    let joined;
    let mut sbytes = s.as_bytes();
    if let Some(sep) = sep
        && rep > 1
    {
        let mut s2 = Vec::with_capacity(sep.len() + s.len());
        s2.extend_from_slice(sep.as_bytes());
        s2.extend_from_slice(s.as_bytes());
        out.extend_from_slice(s.as_bytes());
        joined = s2;
        sbytes = &joined;
        rep -= 1;
    }
    let len = sbytes.len() as u64;
    if rep > 0 && len > 0 {
        let tlen = rep as u64 * len;
        if tlen > LJ_MAX_STR {
            return Err(err_mem(vm));
        }
        out.reserve_exact(tlen as usize);
        for _ in 0..rep {
            out.extend_from_slice(sbytes);
        }
    }
    s = new_str_checked(vm, &out)?;
    vm.push(Value::Str(s));
    vm.gc_safe();
    Ok(1)
}

/// `lj_str_new` with its length check (`string length overflow`).
pub fn new_str_checked(vm: &mut Vm, b: &[u8]) -> LuaResult<Gc<LuaStr>> {
    if b.len() as u64 > LJ_MAX_STR {
        return Err(vm.err_msg(ErrMsg::STROV));
    }
    Ok(vm.new_str(b))
}

fn string_reverse(vm: &mut Vm) -> LuaResult<usize> {
    let s = vm.check_str(1)?;
    let r: Vec<u8> = s.as_bytes().iter().rev().copied().collect();
    vm.push_str(&r);
    Ok(1)
}

fn string_lower(vm: &mut Vm) -> LuaResult<usize> {
    let s = vm.check_str(1)?;
    let r: Vec<u8> = s.as_bytes().iter().map(|&c| if c.is_ascii_uppercase() { c + 0x20 } else { c }).collect();
    vm.push_str(&r);
    Ok(1)
}

fn string_upper(vm: &mut Vm) -> LuaResult<usize> {
    let s = vm.check_str(1)?;
    let r: Vec<u8> = s.as_bytes().iter().map(|&c| if c.is_ascii_lowercase() { c - 0x20 } else { c }).collect();
    vm.push_str(&r);
    Ok(1)
}

fn string_dump(vm: &mut Vm) -> LuaResult<usize> {
    // lj_lib_checkLproto(L, 1, 1).
    let pt = match vm.arg(1) {
        Value::Func(f) if vm.nargs() >= 1 => f.proto(),
        _ => return Err(vm.argt_error(1, "function")),
    };
    let mut flags = 0;
    if vm.nargs() >= 2 {
        match vm.arg(2) {
            Value::Str(mode) => {
                for &c in mode.as_bytes().iter().take_while(|&&c| c != 0) {
                    if c == b's' {
                        flags |= crate::bcdump::BCDUMP_F_STRIP;
                    }
                    if c == b'd' {
                        flags |= crate::bcdump::BCDUMP_F_DETERMINISTIC;
                    }
                }
            }
            v if v.is_truthy() => flags |= crate::bcdump::BCDUMP_F_STRIP,
            _ => {}
        }
    }
    let Some(pt) = pt else {
        return Err(vm.err_caller(ErrMsg::STRDUMP));
    };
    vm.set_top(1);
    let b = crate::bcdump::write_flags(vm, pt, flags);
    let s = vm.new_str(&b);
    vm.set_arg(1, Value::Str(s));
    vm.gc_safe();
    Ok(1)
}

// -- Pattern matching -------------------------------------------------------------------------

/// `push_onecapture`.
fn push_onecapture(vm: &mut Vm, ms: &MatchState<'_>, i: usize, s: usize, e: usize) -> LuaResult<()> {
    match ms.capture(i, s, e) {
        Ok(Capture::Span { start, end }) => {
            vm.push_str(&ms.src()[start..end]);
            Ok(())
        }
        Ok(Capture::Position(p)) => {
            vm.push(Value::Num(p as f64));
            Ok(())
        }
        Err(pe) => Err(pat_error(vm, pe)),
    }
}

/// `push_captures`: `whole` is the match span (`None` = upstream's `s == NULL`).
fn push_captures(vm: &mut Vm, ms: &MatchState<'_>, whole: Option<(usize, usize)>) -> LuaResult<usize> {
    let n = ms.num_captures(whole.is_some());
    vm.check_stack(n, Some("too many captures"))?;
    let (s, e) = whole.unwrap_or((0, 0));
    for i in 0..n {
        push_onecapture(vm, ms, i, s, e)?;
    }
    Ok(n)
}

fn str_find_aux(vm: &mut Vm, find: bool) -> LuaResult<usize> {
    let s = vm.check_str(1)?;
    let p = vm.check_str(2)?;
    let mut start = vm.opt_int(3, 1)?;
    let slen = s.len();
    if start < 0 {
        start = start.wrapping_add(slen as i32);
    } else {
        start = start.wrapping_sub(1);
    }
    if start < 0 {
        start = 0;
    }
    let st = (start as usize).min(slen);
    let src = s.as_bytes();
    let pat = p.as_bytes();
    if find && ((vm.nargs() >= 4 && vm.arg(4).is_truthy()) || !pattern::has_pattern(pat)) {
        // Search for fixed string.
        if let Some(q) = pattern::find_plain(&src[st..], pat) {
            let q = (st + q) as i32;
            vm.push(Value::Num((q + 1) as f64));
            vm.push(Value::Num((q + p.len() as i32) as f64));
            return Ok(2);
        }
    } else {
        match pattern::find(src, pat, st) {
            Err(pe) => return Err(pat_error(vm, pe)),
            Ok(Some(f)) => {
                if find {
                    vm.push(Value::Num((f.start + 1) as f64));
                    vm.push(Value::Num(f.end as f64));
                    return Ok(push_captures(vm, &f.state, None)? + 2);
                } else {
                    return push_captures(vm, &f.state, Some((f.start, f.end)));
                }
            }
            Ok(None) => {}
        }
    }
    vm.push_nil(); // Not found.
    Ok(1)
}

fn string_find(vm: &mut Vm) -> LuaResult<usize> {
    str_find_aux(vm, true)
}

fn string_match(vm: &mut Vm) -> LuaResult<usize> {
    str_find_aux(vm, false)
}

fn string_gmatch_aux(vm: &mut Vm) -> LuaResult<usize> {
    let (Value::Str(str), Value::Str(p), Value::Num(pos)) = (vm.upvalue(1), vm.upvalue(2), vm.upvalue(3)) else {
        unreachable!("gmatch_aux upvalues")
    };
    let mut ms = MatchState::new(str.as_bytes());
    match pattern::gmatch_next(&mut ms, p.as_bytes(), pos as usize) {
        Err(pe) => Err(pat_error(vm, pe)),
        Ok(Some((s, e, next))) => {
            vm.set_upvalue(3, Value::Num(next as u32 as f64));
            push_captures(vm, &ms, Some((s, e)))
        }
        Ok(None) => Ok(0), // Not found.
    }
}

fn string_gmatch(vm: &mut Vm) -> LuaResult<usize> {
    let s = vm.check_str(1)?;
    let p = vm.check_str(2)?;
    let f = vm.native(string_gmatch_aux, FF_STRING_GMATCH_AUX, vec![Value::Str(s), Value::Str(p), Value::Num(0.0)]);
    vm.push(Value::Func(f));
    Ok(1)
}

/// `luaL_optint` (`(int)lua_tointeger`, which truncates to 64 bits first).
fn aux_optint(vm: &mut Vm, n: usize, def: i32) -> LuaResult<i32> {
    if n > vm.nargs() || vm.arg(n).is_nil() {
        return Ok(def);
    }
    match vm.tonumber(vm.arg(n)) {
        Some(x) => Ok(crate::text::num2i64(x) as i32),
        None => Err(vm.argt_error(n, "number")),
    }
}

/// `add_value` of gsub: append the replacement for the match `src[s..e]`.
fn gsub_add_value(
    vm: &mut Vm,
    ms: &MatchState<'_>,
    out: &mut Vec<u8>,
    s: usize,
    e: usize,
) -> LuaResult<()> {
    let repl = vm.arg(3);
    match repl {
        Value::Num(_) | Value::Str(_) => {
            // add_s: lua_tolstring converts a number in place.
            let r = match repl {
                Value::Str(r) => r,
                _ => {
                    let r = vm.to_str(repl).expect("number");
                    vm.set_arg(3, Value::Str(r));
                    r
                }
            };
            return pattern::expand_replacement(ms, r.as_bytes(), s, e, out).map_err(|pe| pat_error(vm, pe));
        }
        Value::Func(_) => {
            vm.push(repl);
            let n = push_captures(vm, ms, Some((s, e)))?;
            vm.call(n, 1)?;
        }
        Value::Table(_) => {
            push_onecapture(vm, ms, 0, s, e)?;
            let k = vm.pop();
            let v = vm.index(repl, k)?;
            vm.push(v);
        }
        _ => unreachable!("gsub replacement type checked"),
    }
    let v = vm.pop();
    if v.is_falsy() {
        out.extend_from_slice(&ms.src()[s..e]); // Keep original text.
    } else {
        match vm.tostring_opt(v) {
            Some(b) => out.extend_from_slice(&b),
            None => {
                let m = fmt_msg(ErrMsg::STRGSRV, &[v.type_name().as_bytes()]);
                return Err(vm.err_callermsg(&m));
            }
        }
    }
    Ok(())
}

fn string_gsub(vm: &mut Vm) -> LuaResult<usize> {
    let s = vm.check_str(1)?;
    let p = vm.check_str(2)?;
    let srcl = s.len();
    let tr = vm.arg(3);
    let max_s = aux_optint(vm, 4, (srcl + 1) as i32)?;
    let mut pat = p.as_bytes();
    let anchor = pat.first() == Some(&b'^');
    if anchor {
        pat = &pat[1..];
    }
    if !matches!(tr, Value::Num(_) | Value::Str(_) | Value::Func(_) | Value::Table(_)) || vm.nargs() < 3 {
        return Err(vm.arg_error_em(3, ErrMsg::NOSFT));
    }
    let src_all = s.as_bytes();
    let mut ms = MatchState::new(src_all);
    let mut out = Vec::with_capacity(srcl);
    let mut src = 0;
    let mut n = 0;
    while n < max_s {
        let e = ms.match_at(src, pat).map_err(|pe| pat_error(vm, pe))?;
        if let Some(e) = e {
            n += 1;
            gsub_add_value(vm, &ms, &mut out, src, e)?;
        }
        match e {
            Some(e) if e > src => src = e, // Non-empty match: skip it.
            _ if src < srcl => {
                out.push(src_all[src]);
                src += 1;
            }
            _ => break,
        }
        if anchor {
            break;
        }
    }
    out.extend_from_slice(&src_all[src..]);
    let r = new_str_checked(vm, &out)?;
    vm.push(Value::Str(r));
    vm.push(Value::Num(n as f64));
    Ok(2)
}

// -- Formatting -------------------------------------------------------------------------------

/// The raw 64 bits of an int64/uint64 cdata (`%d`/`%x` etc. print them directly).
fn cdata_i64_bits(v: Value) -> Option<u64> {
    match v {
        Value::CData(cd) if cd.ctypeid() == CTID_INT64 || cd.ctypeid() == CTID_UINT64 => {
            // SAFETY: int64/uint64 cdata own 8 bytes of 8-byte aligned storage.
            Some(unsafe { (cd.ptr() as *const u64).read() })
        }
        _ => None,
    }
}

/// Output of [`strfmt_putarg`]: receives each produced piece together with the size upstream
/// reserves (`lj_buf_more`) before writing it, which is at least the piece's length. String
/// buffers honour the reservation because their remaining capacity is observable.
pub type FmtSink<'a> = dyn FnMut(&mut Vm, &[u8], u32) -> LuaResult<()> + 'a;

/// `lj_strfmt_putarg`: format arguments `arg..` with the format string at `arg` and append
/// each piece to the output through `put` as soon as it is produced (upstream writes into the
/// buffer in place, so output before an error or a `__tostring` call is already there).
/// `selfbuf` is the buffer userdata being written to by `buffer:putf` (putting it into
/// itself is an error). `__tostring` metamethods for `%s` are called once and their result
/// replaces the argument in place (keeping it rooted).
pub fn strfmt_putarg(vm: &mut Vm, arg: usize, selfbuf: Option<Gc<Userdata>>, put: &mut FmtSink<'_>) -> LuaResult<()> {
    let narg = vm.nargs();
    let fmt = vm.check_str(arg)?;
    let mut arg = arg;
    let mut tmp = Vec::new();
    for item in FormatParser::new(fmt.as_bytes()) {
        let sf = match item {
            FmtItem::Lit(s) => {
                put(vm, s, s.len() as u32)?;
                continue;
            }
            FmtItem::Err(s) => {
                let m = fmt_msg(ErrMsg::STRFMT, &[s]);
                return Err(vm.err_callermsg(&m));
            }
            FmtItem::Spec(sf) => sf,
        };
        arg += 1;
        if arg > narg {
            return Err(vm.arg_error_em(arg as i32, ErrMsg::NOVAL));
        }
        let o = vm.arg(arg);
        tmp.clear();
        let out = &mut tmp;
        // Reservation, if it differs from the output length.
        let mut reserve = None;
        match sf.ty() {
            FormatType::Int => match cdata_i64_bits(o) {
                Some(k) => strfmt::put_fxint(out, sf, k),
                None => {
                    let n = vm.check_num(arg)?;
                    let k = crate::text::num2i64(n);
                    if k == k as i32 as i64 && sf == SFormat::D {
                        reserve = Some(strfmt::MAXBUF_INT as u32); // lj_strfmt_putint.
                    }
                    strfmt::put_fnum_int(out, sf, n);
                }
            },
            FormatType::Uint => match cdata_i64_bits(o) {
                Some(k) => strfmt::put_fxint(out, sf, k),
                None => {
                    let n = vm.check_num(arg)?;
                    strfmt::put_fnum_uint(out, sf, n);
                }
            },
            FormatType::Num => {
                let n = vm.check_num(arg)?;
                reserve = Some(crate::text::numfmt::put_fnum_reserve(out, sf, n));
            }
            FormatType::Str => {
                let mut o = o;
                if !matches!(o, Value::Str(_)) && super::buffer::tobuf(o).is_none() {
                    let mo = vm.meta_lookup(o, MM::Tostring);
                    if !mo.is_nil() {
                        // Call __tostring metamethod once.
                        vm.call_value(mo, &[o], 1)?;
                        o = vm.pop();
                        vm.set_arg(arg, o);
                    }
                }
                let s: Vec<u8> = match o {
                    Value::Str(st) => st.as_bytes().to_vec(),
                    _ => match super::buffer::tobuf(o) {
                        Some(ud) => {
                            if Some(ud) == selfbuf {
                                return Err(vm.arg_error_em(arg as i32 + 1, ErrMsg::BUFFER_SELF));
                            }
                            super::buffer::sbx(&ud).contents().to_vec()
                        }
                        None => vm.tostring_basic(o).as_bytes().to_vec(),
                    },
                };
                if sf.is_quoted() {
                    // No formatting; upstream reserves 1 byte per quote and 4 per character.
                    put(vm, b"\"", 1)?;
                    for idx in 0..s.len() {
                        tmp.clear();
                        strfmt::put_quoted_char(&mut tmp, &s, idx);
                        put(vm, &tmp, 4)?;
                    }
                    put(vm, b"\"", 1)?;
                    continue;
                }
                strfmt::put_fstr(out, sf, &s);
            }
            FormatType::Char => {
                let c = vm.check_int(arg)?;
                strfmt::put_fchar(out, sf, c);
            }
            FormatType::Ptr => {
                // No formatting.
                strfmt::put_ptr(out, crate::vm::obj_ptr(o));
                reserve = Some(strfmt::MAXBUF_PTR as u32);
            }
            FormatType::Eof | FormatType::Err | FormatType::Lit => unreachable!("bad string format type"),
        }
        let r = reserve.unwrap_or(tmp.len() as u32);
        put(vm, &tmp, r)?;
    }
    Ok(())
}

fn string_format(vm: &mut Vm) -> LuaResult<usize> {
    let mut out = Vec::new();
    strfmt_putarg(vm, 1, None, &mut |_, s, _| {
        out.extend_from_slice(s);
        Ok(())
    })?;
    let s = new_str_checked(vm, &out)?;
    vm.push(Value::Str(s));
    vm.gc_safe();
    Ok(1)
}
