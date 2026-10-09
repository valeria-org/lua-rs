//! Runtime error messages and error construction. Port of lj_errmsg.h and the message parts of
//! lj_err.c (`lj_err_msg`, `lj_err_optype`, `lj_err_comp`, `lj_err_callermsg`,
//! `lj_err_arg*`).

use crate::error::{ErrorKind, LuaError};
use crate::gc::Gc;
use crate::state::Vm;
use crate::thread::Thread;
use crate::value::Value;

use super::debug;

/// An upstream error message (`LJ_ERR_*`). Messages with `%s`/`%d` are formatted by the
/// callers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ErrMsg(pub &'static str);

#[rustfmt::skip]
impl ErrMsg {
    pub const ERRMEM: ErrMsg = ErrMsg("not enough memory");
    pub const ERRERR: ErrMsg = ErrMsg("error in error handling");
    pub const STROV: ErrMsg = ErrMsg("string length overflow");
    pub const UDATAOV: ErrMsg = ErrMsg("userdata length overflow");
    pub const STKOV: ErrMsg = ErrMsg("stack overflow");
    pub const STKOVM: ErrMsg = ErrMsg("stack overflow (%s)");
    pub const TABOV: ErrMsg = ErrMsg("table overflow");
    pub const NANIDX: ErrMsg = ErrMsg("table index is NaN");
    pub const NILIDX: ErrMsg = ErrMsg("table index is nil");
    pub const NEXTIDX: ErrMsg = ErrMsg("invalid key to 'next'");
    pub const BADCALL: ErrMsg = ErrMsg("attempt to call a %s value");
    pub const BADOPRT: ErrMsg = ErrMsg("attempt to %s %s '%s' (a %s value)");
    pub const BADOPRV: ErrMsg = ErrMsg("attempt to %s a %s value");
    pub const BADCMPT: ErrMsg = ErrMsg("attempt to compare %s with %s");
    pub const BADCMPV: ErrMsg = ErrMsg("attempt to compare two %s values");
    pub const GETLOOP: ErrMsg = ErrMsg("loop in gettable");
    pub const SETLOOP: ErrMsg = ErrMsg("loop in settable");
    pub const OPCALL: ErrMsg = ErrMsg("call");
    pub const OPINDEX: ErrMsg = ErrMsg("index");
    pub const OPARITH: ErrMsg = ErrMsg("perform arithmetic on");
    pub const OPCAT: ErrMsg = ErrMsg("concatenate");
    pub const OPLEN: ErrMsg = ErrMsg("get length of");
    pub const BADSELF: ErrMsg = ErrMsg("calling '%s' on bad self (%s)");
    pub const BADARG: ErrMsg = ErrMsg("bad argument #%d to '%s' (%s)");
    pub const BADTYPE: ErrMsg = ErrMsg("%s expected, got %s");
    pub const BADVAL: ErrMsg = ErrMsg("invalid value");
    pub const NOVAL: ErrMsg = ErrMsg("value expected");
    pub const NOCORO: ErrMsg = ErrMsg("coroutine expected");
    pub const NOTABN: ErrMsg = ErrMsg("nil or table expected");
    pub const NOLFUNC: ErrMsg = ErrMsg("Lua function expected");
    pub const NOFUNCL: ErrMsg = ErrMsg("function or level expected");
    pub const NOSFT: ErrMsg = ErrMsg("string/function/table expected");
    pub const NOPROXY: ErrMsg = ErrMsg("boolean or proxy expected");
    pub const FORINIT: ErrMsg = ErrMsg("'for' initial value must be a number");
    pub const FORLIM: ErrMsg = ErrMsg("'for' limit must be a number");
    pub const FORSTEP: ErrMsg = ErrMsg("'for' step must be a number");
    pub const NOENV: ErrMsg = ErrMsg("no calling environment");
    pub const CYIELD: ErrMsg = ErrMsg("attempt to yield across C-call boundary");
    pub const BADLU: ErrMsg = ErrMsg("bad light userdata pointer");
    pub const NOGCMM: ErrMsg = ErrMsg("bad action while in __gc metamethod");
    pub const ASSERT: ErrMsg = ErrMsg("assertion failed!");
    pub const PROTMT: ErrMsg = ErrMsg("cannot change a protected metatable");
    pub const UNPACK: ErrMsg = ErrMsg("too many results to unpack");
    pub const RDRSTR: ErrMsg = ErrMsg("reader function must return a string");
    pub const PRTOSTR: ErrMsg = ErrMsg("'tostring' must return a string to 'print'");
    pub const NUMRNG: ErrMsg = ErrMsg("number out of range");
    pub const IDXRNG: ErrMsg = ErrMsg("index out of range");
    pub const BASERNG: ErrMsg = ErrMsg("base out of range");
    pub const LVLRNG: ErrMsg = ErrMsg("level out of range");
    pub const INVLVL: ErrMsg = ErrMsg("invalid level");
    pub const INVOPT: ErrMsg = ErrMsg("invalid option");
    pub const INVOPTM: ErrMsg = ErrMsg("invalid option '%s'");
    pub const INVFMT: ErrMsg = ErrMsg("invalid format");
    pub const SETFENV: ErrMsg = ErrMsg("'setfenv' cannot change environment of given object");
    pub const CORUN: ErrMsg = ErrMsg("cannot resume running coroutine");
    pub const CODEAD: ErrMsg = ErrMsg("cannot resume dead coroutine");
    pub const COSUSP: ErrMsg = ErrMsg("cannot resume non-suspended coroutine");
    pub const PRNGSD: ErrMsg = ErrMsg("PRNG seeding failed");
    pub const TABINS: ErrMsg = ErrMsg("wrong number of arguments to 'insert'");
    pub const TABCAT: ErrMsg = ErrMsg("invalid value (%s) at index %d in table for 'concat'");
    pub const TABSORT: ErrMsg = ErrMsg("invalid order function for sorting");
    pub const IOCLFL: ErrMsg = ErrMsg("attempt to use a closed file");
    pub const IOSTDCL: ErrMsg = ErrMsg("standard file is closed");
    pub const OSUNIQF: ErrMsg = ErrMsg("unable to generate a unique filename");
    pub const OSDATEF: ErrMsg = ErrMsg("field '%s' missing in date table");
    pub const STRDUMP: ErrMsg = ErrMsg("unable to dump given function");
    pub const STRSLC: ErrMsg = ErrMsg("string slice too long");
    pub const STRPATB: ErrMsg = ErrMsg("missing '[' after '%f' in pattern");
    pub const STRPATC: ErrMsg = ErrMsg("invalid pattern capture");
    pub const STRPATE: ErrMsg = ErrMsg("malformed pattern (ends with '%')");
    pub const STRPATM: ErrMsg = ErrMsg("malformed pattern (missing ']')");
    pub const STRPATU: ErrMsg = ErrMsg("unbalanced pattern");
    pub const STRPATX: ErrMsg = ErrMsg("pattern too complex");
    pub const STRCAPI: ErrMsg = ErrMsg("invalid capture index");
    pub const STRCAPN: ErrMsg = ErrMsg("too many captures");
    pub const STRCAPU: ErrMsg = ErrMsg("unfinished capture");
    pub const STRFMT: ErrMsg = ErrMsg("invalid option '%s' to 'format'");
    pub const STRGSRV: ErrMsg = ErrMsg("invalid replacement value (a %s)");
    pub const BUFFER_SELF: ErrMsg = ErrMsg("cannot put buffer into itself");
    pub const BUFFER_BADOPT: ErrMsg = ErrMsg("bad options table");
    pub const BUFFER_BADENC: ErrMsg = ErrMsg("cannot serialize '%s'");
    pub const BUFFER_BADDEC: ErrMsg = ErrMsg("cannot deserialize tag 0x%02x");
    pub const BUFFER_BADDICTX: ErrMsg = ErrMsg("cannot deserialize dictionary index %d");
    pub const BUFFER_DEPTH: ErrMsg = ErrMsg("too deep to serialize");
    pub const BUFFER_DUPKEY: ErrMsg = ErrMsg("duplicate table key");
    pub const BUFFER_EOB: ErrMsg = ErrMsg("unexpected end of buffer");
    pub const BUFFER_LEFTOV: ErrMsg = ErrMsg("left-over data in buffer");
    pub const BADMODN: ErrMsg = ErrMsg("name conflict for module '%s'");
    pub const NOJIT: ErrMsg = ErrMsg("JIT compiler permanently disabled by build option");
    pub const JITOPT: ErrMsg = ErrMsg("unknown or malformed optimization flag '%s'");
    pub const XMODE: ErrMsg = ErrMsg("attempt to load chunk with wrong mode");
    pub const BCFMT: ErrMsg = ErrMsg("cannot load incompatible bytecode");
    pub const BCBAD: ErrMsg = ErrMsg("cannot load malformed bytecode");
    /// Not an upstream message: raised when [`super::LJ_MAX_CCALLS`] is exceeded.
    pub const CCALLS: ErrMsg = ErrMsg("C stack overflow");
}

impl ErrMsg {
    /// The message text.
    pub fn text(self) -> &'static str {
        self.0
    }
}

/// Substitute `%s`/`%d` placeholders in order (enough for the upstream messages).
pub fn fmt_msg(em: ErrMsg, args: &[&[u8]]) -> Vec<u8> {
    let mut out = Vec::with_capacity(em.0.len() + 16);
    let b = em.0.as_bytes();
    let mut i = 0;
    let mut a = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 1 < b.len() && (b[i + 1] == b's' || b[i + 1] == b'd') {
            if let Some(arg) = args.get(a) {
                out.extend_from_slice(crate::parse::lex::cstr(arg));
            }
            a += 1;
            i += 2;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    out
}

/// `lj_typename`: `type()` name of a value.
#[inline]
pub fn typename(v: Value) -> &'static str {
    v.type_name()
}

impl Vm {
    /// A runtime error with a string message, no position (`lj_err_throw` of a message).
    pub fn error_msg_bytes(&mut self, msg: &[u8]) -> LuaError {
        let s = self.new_str(msg);
        LuaError::new(ErrorKind::Run, Value::Str(s))
    }

    /// `lj_err_str`: the message as a Lua string.
    pub fn err_str(&mut self, em: ErrMsg) -> Value {
        Value::Str(self.new_str(em.0.as_bytes()))
    }

    /// Index of the top frame of the current thread.
    pub(crate) fn top_frame_idx(&self) -> Option<usize> {
        self.cur_thread.frame_count().checked_sub(1)
    }

    /// `err_msgv`: formatted message with the position of the current frame (if it is a Lua
    /// frame).
    pub(crate) fn err_msgv(&mut self, msg: &[u8]) -> LuaError {
        let th = self.cur_thread;
        let s = self.addloc(th, self.top_frame_idx(), msg);
        self.error_msg_bytes(&s)
    }

    /// `lj_err_msg`: upstream message with the position of the current Lua frame.
    pub fn err_msg(&mut self, em: ErrMsg) -> LuaError {
        self.err_msgv(em.0.as_bytes())
    }

    /// `lj_err_optype`: type error for operand `o` of the current instruction; `slot` is the
    /// operand's absolute stack slot (used for the variable name), `None` for constants.
    pub(crate) fn err_optype(&mut self, o: Value, slot: Option<usize>, opm: ErrMsg) -> LuaError {
        let tname = typename(o);
        let th = self.cur_thread;
        if let Some(idx) = self.top_frame_idx()
            && let Some(f) = th.frame(idx)
            && let Some(pt) = f.func.proto()
            && let Some(slot) = slot
            && slot >= f.base as usize
            && f.pc > 0
            && let Some((kind, name)) = debug::slotname(&pt, f.pc - 1, (slot - f.base as usize) as u32)
        {
            let m = fmt_msg(ErrMsg::BADOPRT, &[opm.0.as_bytes(), kind.as_bytes(), &name, tname.as_bytes()]);
            return self.err_msgv(&m);
        }
        let m = fmt_msg(ErrMsg::BADOPRV, &[opm.0.as_bytes(), tname.as_bytes()]);
        self.err_msgv(&m)
    }

    /// `lj_err_comp`: ordered comparison of incompatible values.
    pub(crate) fn err_comp(&mut self, o1: Value, o2: Value) -> LuaError {
        let (t1, t2) = (typename(o1), typename(o2));
        let m = if t1 == t2 {
            fmt_msg(ErrMsg::BADCMPV, &[t1.as_bytes()])
        } else {
            fmt_msg(ErrMsg::BADCMPT, &[t1.as_bytes(), t2.as_bytes()])
        };
        self.err_msgv(&m)
    }

    /// `lj_err_callermsg`: `msg` with the position of the Lua code that called the running
    /// native (none if it was called from Rust or by `pcall`). The frame of an FFI metamethod
    /// called from bytecode is removed, so it does not show up in tracebacks.
    pub fn err_callermsg(&mut self, msg: &[u8]) -> LuaError {
        let th = self.cur_thread;
        let idx = self.top_frame_idx();
        let pframe = idx.and_then(|i| debug::caller_frame(th, i));
        if let (Some(i), Some(_)) = (idx, pframe)
            && let Some(f) = th.frame(i)
            && matches!(f.kind, crate::thread::FrameKind::Cont(_))
            && f.func.is_builtin()
            && (super::ffid::FF_FFI_META___INDEX..=super::ffid::FF_FFI_META___TOSTRING).contains(&f.func.ffid())
        {
            // Remove frame for FFI metamethods.
            th.set_top(f.call_slot() as usize);
            th.truncate_frames(i);
        }
        let s = self.addloc(th, pframe, msg);
        self.error_msg_bytes(&s)
    }

    /// `lj_err_caller`: upstream message in the context of the caller.
    pub fn err_caller(&mut self, em: ErrMsg) -> LuaError {
        self.err_callermsg(em.0.as_bytes())
    }

    /// `err_argmsg`: `bad argument #narg to 'fname' (msg)` (or `calling 'f' on bad self`).
    pub fn arg_error(&mut self, narg: i32, msg: &[u8]) -> LuaError {
        let th = self.cur_thread;
        let fname = self.top_frame_idx().and_then(|i| debug::funcname(self, th, i));
        let mut narg = narg;
        if narg < 0 {
            narg += self.nargs() as i32 + 1;
        }
        let m = match &fname {
            Some((kind, name)) if kind.as_bytes().get(3) == Some(&b'h') && narg == 1 => {
                fmt_msg(ErrMsg::BADSELF, &[name, msg])
            }
            Some((kind, name)) => {
                let n = if kind.as_bytes().get(3) == Some(&b'h') { narg - 1 } else { narg };
                fmt_msg(ErrMsg::BADARG, &[n.to_string().as_bytes(), name, msg])
            }
            None => fmt_msg(ErrMsg::BADARG, &[narg.to_string().as_bytes(), b"?", msg]),
        };
        self.err_callermsg(&m)
    }

    /// `lj_err_arg`: argument error with an upstream message.
    pub fn arg_error_em(&mut self, narg: i32, em: ErrMsg) -> LuaError {
        self.arg_error(narg, em.0.as_bytes())
    }

    /// `lj_err_argtype`: `bad argument #n to 'f' (xname expected, got tname)`.
    pub fn type_error(&mut self, narg: i32, xname: &str) -> LuaError {
        let n = self.nargs() as i32;
        let idx = if narg < 0 { n + narg + 1 } else { narg };
        let tname = if idx >= 1 && idx <= n { typename(self.arg(idx as usize)) } else { "no value" };
        let m = fmt_msg(ErrMsg::BADTYPE, &[xname.as_bytes(), tname.as_bytes()]);
        self.arg_error(narg, &m)
    }

    /// Thread-independent helper: message bytes of an error value (strings and numbers).
    pub(crate) fn error_text(&mut self, v: Value) -> Option<Vec<u8>> {
        match v {
            Value::Str(s) => Some(s.as_bytes().to_vec()),
            Value::Num(n) => Some(crate::text::strfmt::fmt_num(n)),
            _ => None,
        }
    }
}

/// Error value of the current thread's position (used by `err_msgv` users that need the
/// thread explicitly).
#[allow(dead_code)]
pub(crate) fn _thread_of(vm: &Vm) -> Gc<Thread> {
    vm.cur_thread
}
