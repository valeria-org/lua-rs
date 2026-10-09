//! The native API: everything a Rust native function (`NativeFn = fn(&mut Vm) ->
//! LuaResult<usize>`) uses. Mirrors lj_lib.c (argument checks with upstream coercions and
//! messages), lj_api.c (stack manipulation, calls, metamethod-aware operations) and the
//! error builders of lj_err.c.
//!
//! # Calling convention
//! A native runs with its own frame on top of the current thread. Its arguments are the stack
//! slots `[base, top)`: `arg(1)` is the first, `nargs()` their count. It pushes its results
//! and returns how many there are (the last `n` values below `top`). The VM guarantees
//! `LUA_MINSTACK` (20) free slots on entry; `push` grows the stack when needed.
//!
//! # Rooting
//! A `Value` held only in a Rust local stays valid until the native calls back into Lua (any
//! `call*`/`pcall*`, metamethod-aware helper, `tostring`, `gc_*`) — keep anything that must
//! survive such a call on the stack (`push`).
//!
//! # Errors
//! Return `Err` from the native. Build errors with [`Vm::arg_error`], [`Vm::type_error`],
//! [`Vm::rt_error`], [`Vm::err_caller`], [`Vm::err_msg`] or [`Vm::error`] so messages and
//! error-handler semantics match upstream. Never construct `LuaError` by hand for runtime
//! errors that originate in natives.
//!
//! # Builtins
//! Natives with an upstream builtin id (`ffid > FF_C`, see [`super::ffid`]) print as
//! `function: builtin#N`. `pcall`/`xpcall` are intercepted by the VM (protected frames);
//! their registered Rust functions only run when called through `Vm::call`.

use crate::bc::MM;
use crate::error::{ErrorKind, LuaError, LuaResult};
use crate::func::{Function, NativeImpl};
use crate::gc::Gc;
use crate::lstr::LuaStr;
use crate::state::Vm;
use crate::table::Table;
use crate::thread::{ErrFunc, FrameKind, Thread};
use crate::value::Value;

use super::err::ErrMsg;
use super::meta::{Meta, num2bit, num2int, str2num};

/// Return value of a native that tail-calls: see [`Vm::tailcall`] (`FFH_TAILCALL`).
pub const TAILCALL: usize = usize::MAX;

impl Vm {
    // -- Frame and arguments --------------------------------------------------------------------

    /// First argument slot of the running native (0 outside any frame).
    #[inline]
    pub fn frame_base(&self) -> usize {
        self.cur_thread.current_frame().map_or(0, |f| f.base as usize)
    }

    /// Number of values in the current frame (`lua_gettop`): arguments plus pushed values.
    #[inline]
    pub fn nargs(&self) -> usize {
        self.cur_thread.top().saturating_sub(self.frame_base())
    }

    /// Argument `n` (1-based), nil if absent.
    #[inline]
    pub fn arg(&self, n: usize) -> Value {
        if n == 0 || n > self.nargs() {
            return Value::Nil;
        }
        self.cur_thread.get(self.frame_base() + n - 1)
    }

    /// Overwrite argument/slot `n` (1-based, must be below top).
    #[inline]
    pub fn set_arg(&self, n: usize, v: Value) {
        self.cur_thread.set(self.frame_base() + n - 1, v);
    }

    /// `lua_gettop`.
    #[inline]
    pub fn get_top(&self) -> usize {
        self.nargs()
    }

    /// `lua_settop(L, n)` for `n >= 0`: truncate or fill with nil.
    pub fn set_top(&mut self, n: usize) {
        let th = self.cur_thread;
        let base = self.frame_base();
        let top = th.top();
        let nt = base + n;
        if nt > top {
            self.reserve(nt - top);
            for i in top..nt {
                th.set(i, Value::Nil);
            }
        }
        th.set_top(nt);
    }

    /// Absolute stack slot for a C-API-style index (positive: from base, negative: from top).
    pub fn abs_index(&self, idx: i32) -> usize {
        if idx > 0 { self.frame_base() + idx as usize - 1 } else { (self.cur_thread.top() as i64 + idx as i64) as usize }
    }

    /// Value at a C-API-style index (nil if out of range).
    pub fn get(&self, idx: i32) -> Value {
        let s = self.abs_index(idx);
        if s < self.cur_thread.top() { self.cur_thread.get(s) } else { Value::Nil }
    }

    /// Store into a C-API-style index (`lua_replace`).
    pub fn replace(&self, idx: i32, v: Value) {
        let s = self.abs_index(idx);
        self.cur_thread.set(s, v);
    }

    fn reserve(&mut self, n: usize) {
        let th = self.cur_thread;
        if th.top() + n > th.max_stack() {
            // Overflow still enlarges the stack (error reserve), so the push stays in bounds.
            let _ = th.grow_stack(n);
        }
    }

    /// Push a value (`lua_pushvalue`, `setxV(L->top++)`).
    #[inline]
    pub fn push(&mut self, v: impl Into<Value>) {
        let v = v.into();
        let th = self.cur_thread;
        if th.top() + 1 > th.stack_size() - 1 {
            self.reserve(1);
        }
        th.push(v);
    }

    /// Push nil.
    pub fn push_nil(&mut self) {
        self.push(Value::Nil);
    }

    /// Push an interned string.
    pub fn push_str(&mut self, s: &[u8]) {
        let s = self.new_str(s);
        self.push(Value::Str(s));
    }

    /// Pop the top value.
    pub fn pop(&mut self) -> Value {
        let th = self.cur_thread;
        let t = th.top() - 1;
        th.set_top(t);
        th.get(t)
    }

    /// Push `v` and return 1 (convenience for natives returning one value).
    pub fn ret1(&mut self, v: impl Into<Value>) -> LuaResult<usize> {
        self.push(v);
        Ok(1)
    }

    /// `luaL_checkstack`: ensure `n` free slots or raise `stack overflow (msg)`.
    pub fn check_stack(&mut self, n: usize, msg: Option<&str>) -> LuaResult<()> {
        let th = self.cur_thread;
        if n > crate::thread::LJ_STACK_MAX || th.top() + n > crate::thread::LJ_STACK_MAX {
            return Err(match msg {
                Some(m) => self.rt_error(&super::err::fmt_msg(ErrMsg::STKOVM, &[m.as_bytes()])),
                None => self.err_msg(ErrMsg::STKOV),
            });
        }
        self.check_stack_th(th, n)
    }

    /// `lua_checkstack`: try to ensure `n` free slots.
    pub fn try_check_stack(&mut self, n: usize) -> bool {
        let th = self.cur_thread;
        if n > 8000 || th.top() + n > crate::thread::LJ_STACK_MAX {
            return false;
        }
        self.check_stack_th(th, n).is_ok()
    }

    // -- Function, upvalues, environment ------------------------------------------------------

    /// The running function (the native itself inside a native).
    pub fn current_function(&self) -> Option<Gc<Function>> {
        self.cur_thread.current_frame().map(|f| f.func)
    }

    /// Upvalue `i` (1-based, `lua_upvalueindex(i)`) of the running function.
    pub fn upvalue(&self, i: usize) -> Value {
        self.current_function().and_then(|f| f.upvalue(i - 1)).unwrap_or(Value::Nil)
    }

    /// Set upvalue `i` (1-based) of the running function.
    pub fn set_upvalue(&self, i: usize, v: Value) {
        if let Some(f) = self.current_function() {
            f.set_upvalue(i - 1, v);
        }
    }

    /// `getcurrenv`: environment of the running function, else of the current thread. New
    /// natives created by libraries should use it as their environment.
    pub fn current_env(&self) -> Gc<Table> {
        self.current_function().map_or_else(|| self.cur_thread.env(), |f| f.env())
    }

    /// Create a native closure with the current environment (`lua_pushcclosure` / `lj_lib_pushcc`).
    pub fn native(&mut self, f: crate::func::NativeFn, ffid: u8, upvals: Vec<Value>) -> Gc<Function> {
        let env = self.current_env();
        self.new_native(NativeImpl::Rust(f), ffid, upvals, env)
    }

    // -- Argument checks (lj_lib.c) -------------------------------------------------------------

    /// `lj_lib_checkany`: argument `n` must exist.
    pub fn check_any(&mut self, n: usize) -> LuaResult<Value> {
        if n > self.nargs() {
            return Err(self.arg_error_em(n as i32, ErrMsg::NOVAL));
        }
        Ok(self.arg(n))
    }

    /// `lj_err_argt`: `bad argument #n to 'f' (<type> expected, got <actual>)`.
    pub fn argt_error(&mut self, n: usize, expected: &str) -> LuaError {
        self.type_error(n as i32, expected)
    }

    /// `lj_lib_checkstr`: a string; numbers are converted in place.
    pub fn check_str(&mut self, n: usize) -> LuaResult<Gc<LuaStr>> {
        if n <= self.nargs() {
            match self.arg(n) {
                Value::Str(s) => return Ok(s),
                Value::Num(x) => {
                    let b = crate::text::strfmt::fmt_num(x);
                    let s = self.new_str(&b);
                    self.set_arg(n, Value::Str(s));
                    return Ok(s);
                }
                _ => {}
            }
        }
        Err(self.argt_error(n, "string"))
    }

    /// `lj_lib_optstr`: nil/absent gives `None`.
    pub fn opt_str(&mut self, n: usize) -> LuaResult<Option<Gc<LuaStr>>> {
        if n <= self.nargs() && !self.arg(n).is_nil() { self.check_str(n).map(Some) } else { Ok(None) }
    }

    /// `lj_lib_checknum`: a number; numeric strings are converted in place.
    pub fn check_num(&mut self, n: usize) -> LuaResult<f64> {
        if n <= self.nargs() {
            match self.arg(n) {
                Value::Num(x) => return Ok(x),
                Value::Str(s) => {
                    if let Some(x) = crate::text::strscan::scan_num(s.as_bytes()) {
                        self.set_arg(n, Value::Num(x));
                        return Ok(x);
                    }
                }
                _ => {}
            }
        }
        Err(self.argt_error(n, "number"))
    }

    /// `lj_lib_optnum`.
    pub fn opt_num(&mut self, n: usize, def: f64) -> LuaResult<f64> {
        if n <= self.nargs() && !self.arg(n).is_nil() { self.check_num(n) } else { Ok(def) }
    }

    /// `lj_lib_checkint`: number truncated like `(int32_t)` on x86-64 (`lj_num2int`).
    pub fn check_int(&mut self, n: usize) -> LuaResult<i32> {
        Ok(num2int(self.check_num(n)?))
    }

    /// `lj_lib_optint`.
    pub fn opt_int(&mut self, n: usize, def: i32) -> LuaResult<i32> {
        if n <= self.nargs() && !self.arg(n).is_nil() { self.check_int(n) } else { Ok(def) }
    }

    /// Number converted to a 64-bit integer (`lj_num2i64`, used by e.g. `os` and `string`).
    pub fn check_int64(&mut self, n: usize) -> LuaResult<i64> {
        Ok(crate::text::num2i64(self.check_num(n)?))
    }

    /// `lj_lib_checkbit`: number converted with `lj_num2bit` (bit library).
    pub fn check_bit(&mut self, n: usize) -> LuaResult<i32> {
        Ok(num2bit(self.check_num(n)?))
    }

    /// `lj_lib_checktab`.
    pub fn check_tab(&mut self, n: usize) -> LuaResult<Gc<Table>> {
        match self.arg(n) {
            Value::Table(t) if n <= self.nargs() => Ok(t),
            _ => Err(self.argt_error(n, "table")),
        }
    }

    /// `lj_lib_checktabornil`.
    pub fn check_tab_or_nil(&mut self, n: usize) -> LuaResult<Option<Gc<Table>>> {
        if n <= self.nargs() {
            match self.arg(n) {
                Value::Table(t) => return Ok(Some(t)),
                Value::Nil => return Ok(None),
                _ => {}
            }
        }
        Err(self.arg_error_em(n as i32, ErrMsg::NOTABN))
    }

    /// `lj_lib_checkfunc`.
    pub fn check_func(&mut self, n: usize) -> LuaResult<Gc<Function>> {
        match self.arg(n) {
            Value::Func(f) if n <= self.nargs() => Ok(f),
            _ => Err(self.argt_error(n, "function")),
        }
    }

    /// A thread argument (`coroutine expected` style checks are done by callers).
    pub fn check_thread(&mut self, n: usize) -> LuaResult<Gc<Thread>> {
        match self.arg(n) {
            Value::Thread(t) if n <= self.nargs() => Ok(t),
            _ => Err(self.argt_error(n, "thread")),
        }
    }

    /// `lj_lib_checkopt`: index of the string argument in `list`; `def` (>= 0) if the argument
    /// is absent/nil, a missing argument is an error if `def < 0`. Unknown strings raise
    /// `bad argument #n to 'f' (invalid option 'x')`.
    pub fn check_opt(&mut self, n: usize, def: i32, list: &[&str]) -> LuaResult<i32> {
        let s = if def >= 0 { self.opt_str(n)? } else { Some(self.check_str(n)?) };
        if let Some(s) = s {
            for (i, o) in list.iter().enumerate() {
                if o.as_bytes() == s.as_bytes() {
                    return Ok(i as i32);
                }
            }
            let m = super::err::fmt_msg(ErrMsg::INVOPTM, &[s.as_bytes()]);
            return Err(self.arg_error(n as i32, &m));
        }
        Ok(def)
    }

    // -- Errors ---------------------------------------------------------------------------------

    /// `luaL_error`: message with the position of the calling Lua code (`chunk:line: msg`).
    pub fn rt_error(&mut self, msg: &[u8]) -> LuaError {
        self.err_callermsg(msg)
    }

    /// `lua_error`: raise `v` as is.
    pub fn error(&mut self, v: Value) -> LuaError {
        LuaError::new(ErrorKind::Run, v)
    }

    // -- Calls ----------------------------------------------------------------------------------

    /// `lua_call`: call the function at `top - nargs - 1` with the `nargs` values above it.
    /// Results (adjusted to `nres`, -1 = all) replace function and arguments; returns their
    /// count. This is a C-call boundary.
    pub fn call(&mut self, nargs: usize, nres: i32) -> LuaResult<usize> {
        let th = self.cur_thread;
        let fslot = self.make_call_slot(nargs);
        self.call_at(th, fslot, nargs, FrameKind::C, nres)
    }

    /// Push `f` and `args` and `call` them.
    pub fn call_value(&mut self, f: Value, args: &[Value], nres: i32) -> LuaResult<usize> {
        self.check_stack(args.len() + 2, None)?;
        self.push(f);
        for &a in args {
            self.push(a);
        }
        self.call(args.len(), nres)
    }

    /// `lua_pcall`: protected [`Vm::call`]. On error the stack is restored to below the
    /// function and the error is returned; `errfunc` is the absolute stack slot of a handler
    /// (`None` = no handler).
    pub fn pcall(&mut self, nargs: usize, nres: i32, errfunc: Option<usize>) -> LuaResult<usize> {
        let th = self.cur_thread;
        let fslot = self.make_call_slot(nargs);
        let ef = errfunc.map_or(ErrFunc::None, |s| ErrFunc::Slot(s as u32));
        let r = self.call_at(th, fslot, nargs, FrameKind::Cp(ef), nres);
        if r.is_err() {
            self.rt_clear_thrown();
            th.set_top(fslot);
        }
        r
    }

    /// Move the arguments up by one to make room for the frame-link slot (`api_call_base`).
    fn make_call_slot(&mut self, nargs: usize) -> usize {
        let th = self.cur_thread;
        self.reserve(1);
        let top = th.top();
        let fslot = top - nargs - 1;
        for i in (fslot + 1..top).rev() {
            th.set(i + 1, th.get(i));
        }
        th.set(fslot + 1, Value::Nil);
        th.set_top(top + 1);
        fslot
    }

    /// Tail call `f` with the native's current arguments `[base, top)` (`FFH_TAILCALL`):
    /// `return vm.tailcall(f)`. No C-call boundary is created.
    pub fn tailcall(&mut self, f: Value) -> LuaResult<usize> {
        let base = self.frame_base();
        self.cur_thread.set(base - 2, f);
        Ok(TAILCALL)
    }

    // -- Metamethod-aware operations ------------------------------------------------------------

    /// Call a metamethod from Rust and return its first result.
    fn mm_call1(&mut self, mo: Value, args: &[Value]) -> LuaResult<Value> {
        let n = self.call_value(mo, args, 1)?;
        debug_assert_eq!(n, 1);
        Ok(self.pop())
    }

    /// `lua_gettable`: `o[k]` with `__index`.
    pub fn index(&mut self, o: Value, k: Value) -> LuaResult<Value> {
        match self.meta_tget(o, k, None)? {
            Meta::Done(v) => Ok(v),
            Meta::Call { mo, a, b, .. } => self.mm_call1(mo, &[a, b]),
        }
    }

    /// `lua_settable`: `o[k] = v` with `__newindex`.
    pub fn setindex(&mut self, o: Value, k: Value, v: Value) -> LuaResult<()> {
        match self.meta_tset(o, k, v, None)? {
            Meta::Done(()) => Ok(()),
            Meta::Call { mo, a, b, .. } => {
                self.call_value(mo, &[a, b, v], 0)?;
                Ok(())
            }
        }
    }

    /// Raw `t[k] = v` with upstream errors for nil/NaN keys (`lua_rawset`).
    pub fn rawset(&mut self, t: Gc<Table>, k: Value, v: Value) -> LuaResult<()> {
        self.raw_set(t, k, v)
    }

    /// Arithmetic `a <op> b` with coercion and metamethods (`mm` in `Add..=Unm`).
    pub fn arith(&mut self, mm: MM, a: Value, b: Value) -> LuaResult<Value> {
        match self.meta_arith(mm, a, b, None, None)? {
            Meta::Done(n) => Ok(Value::Num(n)),
            Meta::Call { mo, a, b, .. } => self.mm_call1(mo, &[a, b]),
        }
    }

    /// `lua_equal`: equality with `__eq` for tables/userdata.
    pub fn equal(&mut self, a: Value, b: Value) -> LuaResult<bool> {
        if let (Value::Num(x), Value::Num(y)) = (a, b) {
            return Ok(x == y);
        }
        if a.vtype() != b.vtype() && !(matches!(a, Value::Bool(_)) && matches!(b, Value::Bool(_))) {
            return Ok(false);
        }
        if a.raw_equal(b) {
            return Ok(true);
        }
        if !matches!(a, Value::Table(_) | Value::Udata(_)) {
            return Ok(false);
        }
        match self.meta_equal(a, b, false) {
            None => Ok(false),
            Some(Meta::Call { mo, a, b, .. }) => Ok(self.mm_call1(mo, &[a, b])?.is_truthy()),
            Some(Meta::Done(x)) => Ok(x),
        }
    }

    fn compare(&mut self, a: Value, b: Value, op: u8) -> LuaResult<bool> {
        if let (Value::Num(x), Value::Num(y)) = (a, b) {
            return Ok(if op == 2 { x <= y } else { x < y });
        }
        match self.meta_comp(a, b, op)? {
            Meta::Done(c) => Ok(c),
            Meta::Call { cont, mo, a, b } => {
                let r = self.mm_call1(mo, &[a, b])?.is_truthy();
                Ok(if cont == crate::thread::Cont::CondF { !r } else { r })
            }
        }
    }

    /// `lua_lessthan`: `a < b` with `__lt`.
    pub fn less_than(&mut self, a: Value, b: Value) -> LuaResult<bool> {
        self.compare(a, b, 0)
    }

    /// `a <= b` with `__le` (falling back to `not (b < a)`).
    pub fn less_equal(&mut self, a: Value, b: Value) -> LuaResult<bool> {
        self.compare(a, b, 2)
    }

    /// `lua_concat`: concatenate the top `n` values (with `__concat`), leaving the result.
    pub fn concat(&mut self, n: usize) -> LuaResult<()> {
        let th = self.cur_thread;
        if n == 0 {
            self.push_str(b"");
            return Ok(());
        }
        if n == 1 {
            return Ok(());
        }
        let first = th.top() - n;
        let mut top = th.top() - 1;
        let mut left = n - 1;
        loop {
            match self.meta_cat(top, left)? {
                Meta::Done(()) => break,
                Meta::Call { cont: crate::thread::Cont::Cat { top: t }, mo, a, b } => {
                    let t = t as usize;
                    let r = self.mm_call1(mo, &[a, b])?;
                    th.set(t - 1, r);
                    top = t - 1;
                    left = top - first;
                    if left == 0 {
                        break;
                    }
                }
                Meta::Call { .. } => unreachable!(),
            }
        }
        th.set_top(first + 1);
        self.gc_safe();
        Ok(())
    }

    /// `#o` with Lua 5.1 rules: strings and tables are raw, others use `__len`.
    pub fn len(&mut self, o: Value) -> LuaResult<Value> {
        match o {
            Value::Str(s) => Ok(Value::Num(s.len() as f64)),
            Value::Table(t) => Ok(Value::Num(t.len() as f64)),
            _ => match self.meta_len(o, None)? {
                Meta::Done(v) => Ok(v),
                Meta::Call { mo, a, b, .. } => self.mm_call1(mo, &[a, b]),
            },
        }
    }

    /// `lua_objlen`: raw length (strings, tables, userdata payload size).
    pub fn objlen(&self, o: Value) -> usize {
        match o {
            Value::Str(s) => s.len(),
            Value::Table(t) => t.len(),
            Value::Udata(u) => u.len(),
            _ => 0,
        }
    }

    /// `tostring(v)`: `__tostring` (called with `v`, first result returned) or the default
    /// conversion (`lj_strfmt_obj`).
    pub fn tostring(&mut self, v: Value) -> LuaResult<Value> {
        let mo = self.meta_lookup(v, MM::Tostring);
        if !mo.is_nil() {
            return self.mm_call1(mo, &[v]);
        }
        Ok(Value::Str(self.tostring_basic(v)))
    }

    /// `lua_tolstring` without the metamethod: bytes of a string or number.
    pub fn tostring_opt(&mut self, v: Value) -> Option<Vec<u8>> {
        match v {
            Value::Str(s) => Some(s.as_bytes().to_vec()),
            Value::Num(n) => Some(crate::text::strfmt::fmt_num(n)),
            _ => None,
        }
    }

    /// `lua_tolstring` returning an interned string (numbers converted).
    pub fn to_str(&mut self, v: Value) -> Option<Gc<LuaStr>> {
        match v {
            Value::Str(s) => Some(s),
            Value::Num(n) => {
                let b = crate::text::strfmt::fmt_num(n);
                Some(self.new_str(&b))
            }
            _ => None,
        }
    }

    /// `lua_tonumber`: number or numeric string (`lj_strscan_number`).
    pub fn tonumber(&self, v: Value) -> Option<f64> {
        str2num(v)
    }

    /// Forget the "handler already applied" mark of a caught error (after a native caught an
    /// error and turned it into a value), deleting the foreign exception it came from, if any.
    pub fn rt_clear_thrown(&mut self) {
        self.rt.thrown = None;
        self.rt.capi.foreign = None;
    }

    // -- GC -------------------------------------------------------------------------------------

    /// `collectgarbage("collect")` / `lua_gc(LUA_GCCOLLECT)`: full cycle, then finalizers.
    pub fn gc_collect(&mut self) {
        self.gc_full();
        self.run_finalizers();
    }

    /// `lua_gc(LUA_GCSTEP, kb)`: returns true if a cycle finished; runs finalizers.
    pub fn gc_step_kb(&mut self, kb: i32) -> bool {
        let r = self.gc_step(kb);
        self.run_finalizers();
        r
    }
}
