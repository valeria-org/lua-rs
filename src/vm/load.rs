//! Loading chunks. Port of lj_load.c (`cpparser`, `lua_loadx`, `luaL_loadfilex`,
//! `luaL_loadbufferx`) on top of the parser and the bytecode reader.

use crate::error::{ErrorKind, LuaError, LuaResult};
use crate::parse::lex::{self, Input, LJ_MAX_BUF};
use crate::state::Vm;
use crate::value::Value;

use super::err::ErrMsg;

impl Vm {
    /// `lua_loadx` for an in-memory chunk (`luaL_loadbufferx`): compile source text or load a
    /// bytecode dump (decided by the `\x1b` signature, after an optional BOM/`#` line), check
    /// `mode` (`"b"`, `"t"`, `"bt"`; `None` = any) and return a closure whose environment is
    /// the current thread's environment. With `W` in `mode`, non-native (non-GC64) bytecode is
    /// generated or accepted and the result is the dumpable but not runnable prototype
    /// (`Value::Proto`). Errors: `ErrorKind::Syntax` for syntax errors and wrong modes
    /// (`attempt to load chunk with wrong mode`); bytecode reader errors are runtime errors to
    /// which an active error handler has been applied, as upstream.
    pub fn load(&mut self, src: &[u8], chunkname: &[u8], mode: Option<&[u8]>) -> LuaResult<Value> {
        if src.len() >= LJ_MAX_BUF {
            return Err(lex::err_mem(self));
        }
        self.load_input(Input::from_slice(src), chunkname, mode)
    }

    /// `lua_load` with a reader: [`Vm::load`] of the chunk formed by the pieces `reader`
    /// returns until it returns `None` or an empty piece. The reader is called only when the
    /// lexer or the bytecode reader needs more input, as upstream.
    pub fn load_reader(
        &mut self,
        reader: impl FnMut(&mut Vm) -> LuaResult<Option<Vec<u8>>>,
        chunkname: &[u8],
        mode: Option<&[u8]>,
    ) -> LuaResult<Value> {
        self.load_input(Input::from_reader(Box::new(reader)), chunkname, mode)
    }

    /// Anchor a GC object created by a load whose reader may run Lua code, until the load ends.
    pub(crate) fn anchor_load(&mut self, v: Value) {
        self.rt.load_anchors.push(v);
    }

    fn load_input(&mut self, input: Input<'_>, chunkname: &[u8], mode: Option<&[u8]>) -> LuaResult<Value> {
        let mark = self.rt.load_anchors.len();
        let r = self.cpparser(input, chunkname, mode);
        self.rt.load_anchors.truncate(mark);
        match r {
            // The error handler is inherited (`cframe_errfunc = -1`): runtime errors raised
            // while loading (by the reader or the bytecode reader) get it applied once.
            Err(e) if e.kind == ErrorKind::Run && self.rt.thrown.is_none_or(|t| !t.raw_equal(e.value)) => {
                let th = self.cur_thread;
                let e = self.apply_errfunc(th, e);
                self.rt.thrown = Some(e.value);
                Err(e)
            }
            r => r,
        }
    }

    /// `cpparser`.
    fn cpparser(&mut self, input: Input<'_>, chunkname: &[u8], mode: Option<&[u8]>) -> LuaResult<Value> {
        let chunkarg = lex::cstr(chunkname);
        lex::init_reserved(self);
        let name = self.new_str(chunkarg);
        let (pt, bc, fr2) = {
            let mut ls = lex::LexState::new(self, input, name);
            let bc = ls.setup()?;
            let mut fr2 = true;
            if let Some(mode) = mode {
                let mut xmode = true;
                for &c in lex::cstr(mode) {
                    if c == if bc { b'b' } else { b't' } {
                        xmode = false;
                    }
                    if c == b'W' {
                        fr2 = false;
                    }
                }
                if xmode {
                    let v = ls.vm.err_str(ErrMsg::XMODE);
                    return Err(LuaError::new(ErrorKind::Syntax, v));
                }
            }
            let pt = if bc {
                crate::bcdump::read_input(ls.vm, ls.input, chunkarg, fr2)?
            } else {
                crate::parse::parse_lex(ls, fr2)?
            };
            (pt, bc, fr2)
        };
        let v = if fr2 {
            super::interp::closure_count(&pt);
            let env = self.cur_thread.env();
            // `lj_func_newL_empty`: fresh closed upvalues (a main chunk has none).
            let uvs: Vec<_> = pt
                .uv
                .iter()
                .map(|&v| {
                    let u = self.new_upval_closed(Value::Nil);
                    u.set_immutable(v & crate::func::PROTO_UV_IMMUTABLE != 0);
                    u
                })
                .collect();
            Value::Func(self.new_lua_function(pt, uvs, env))
        } else {
            // Non-native generation returns a dumpable, but non-runnable prototype.
            Value::Proto(pt)
        };
        if !bc {
            // `lj_vmevent_send(L, BC, ...)` of lj_parse.c's fs_finish, delivered once parsing is done.
            self.vmevent_bc(v);
        }
        Ok(v)
    }

    /// `luaL_loadfilex`: load a file (`None` = stdin, chunkname `=stdin`). File errors are
    /// `ErrorKind::File` with upstream's `cannot open/read <name>` messages.
    pub fn load_file(&mut self, filename: Option<&[u8]>, mode: Option<&[u8]>) -> LuaResult<Value> {
        use std::io::Read;
        let (data, chunkname) = match filename {
            Some(name) => {
                let path = std::ffi::OsStr::from_encoded_bytes_checked(crate::parse::lex::cstr(name));
                let mut cn = b"@".to_vec();
                cn.extend_from_slice(crate::parse::lex::cstr(name));
                let r = std::fs::File::open(path).and_then(|mut f| {
                    let mut v = Vec::new();
                    f.read_to_end(&mut v).map(|_| v)
                });
                match r {
                    Ok(v) => (v, cn),
                    Err(e) => {
                        let what = if std::fs::metadata(path).is_ok() && e.kind() != std::io::ErrorKind::NotFound {
                            "read"
                        } else {
                            "open"
                        };
                        let msg = format!(
                            "cannot {what} {}: {}",
                            String::from_utf8_lossy(crate::parse::lex::cstr(name)),
                            strerror(&e)
                        );
                        let v = Value::Str(self.new_str(msg.as_bytes()));
                        return Err(LuaError::new(ErrorKind::File, v));
                    }
                }
            }
            None => {
                let mut v = Vec::new();
                if let Err(e) = std::io::stdin().read_to_end(&mut v) {
                    let msg = format!("cannot read stdin: {}", strerror(&e));
                    let v = Value::Str(self.new_str(msg.as_bytes()));
                    return Err(LuaError::new(ErrorKind::File, v));
                }
                (v, b"=stdin".to_vec())
            }
        };
        self.load(&data, &chunkname, mode)
    }
}

trait OsStrExt2 {
    fn from_encoded_bytes_checked(b: &[u8]) -> &std::ffi::OsStr;
}
impl OsStrExt2 for std::ffi::OsStr {
    fn from_encoded_bytes_checked(b: &[u8]) -> &std::ffi::OsStr {
        use std::os::unix::ffi::OsStrExt;
        std::ffi::OsStr::from_bytes(b)
    }
}

/// C `strerror(errno)` text for an I/O error.
pub fn strerror(e: &std::io::Error) -> String {
    match e.raw_os_error() {
        Some(code) => {
            // SAFETY: strerror returns a pointer to a static (or thread-local) NUL-terminated string.
            let p = unsafe { libc::strerror(code) };
            if p.is_null() {
                return e.to_string();
            }
            // SAFETY: p is a valid C string as above.
            unsafe { std::ffi::CStr::from_ptr(p) }.to_string_lossy().into_owned()
        }
        None => e.to_string(),
    }
}
