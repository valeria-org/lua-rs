//! Loading chunks. Port of lj_load.c (`cpparser`, `lua_loadx`, `luaL_loadfilex`,
//! `luaL_loadbufferx`) on top of the parser and the bytecode reader.

use crate::error::{ErrorKind, LuaError, LuaResult};
use crate::func::Function;
use crate::gc::Gc;
use crate::state::Vm;
use crate::value::Value;

use super::err::ErrMsg;

/// `LJ_MAX_BUF`: maximum size of a chunk read from a reader function.
const LJ_MAX_BUF: usize = 0x7fff_ff00;

impl Vm {
    /// `lua_loadx` for an in-memory chunk: compile source text or load a bytecode dump
    /// (decided by the `\x1b` signature, after an optional BOM/`#` line), check `mode`
    /// (`"b"`, `"t"`, `"bt"`; `None` = any), and return a closure whose environment is the
    /// current thread's environment. Errors: `ErrorKind::Syntax` for syntax errors and wrong
    /// modes (`attempt to load chunk with wrong mode`); bytecode reader errors are runtime
    /// errors to which an active error handler has been applied, as upstream.
    pub fn load(&mut self, src: &[u8], chunkname: &[u8], mode: Option<&[u8]>) -> LuaResult<Gc<Function>> {
        let r = self.load_inner(src, chunkname, mode);
        match r {
            Err(e) if e.kind == ErrorKind::Run => {
                let th = self.cur_thread;
                if self.rt.thrown.is_none_or(|t| !t.raw_equal(e.value)) {
                    let e = self.apply_errfunc(th, e);
                    return Err(e);
                }
                Err(e)
            }
            r => r,
        }
    }

    fn load_inner(&mut self, src: &[u8], chunkname: &[u8], mode: Option<&[u8]>) -> LuaResult<Gc<Function>> {
        let bc = crate::parse::chunk_is_bytecode(self, src)?;
        if let Some(mode) = mode {
            let want = if bc { b'b' } else { b't' };
            if !crate::parse::lex::cstr(mode).contains(&want) {
                let v = self.err_str(ErrMsg::XMODE);
                return Err(LuaError::new(ErrorKind::Syntax, v));
            }
        }
        let pt = if bc { crate::bcdump::read(self, src, chunkname)? } else { crate::parse::parse(self, src, chunkname)? };
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
        Ok(self.new_lua_function(pt, uvs, env))
    }

    /// `lua_load` with a reader: `reader` is called until it returns `None` or an empty piece;
    /// the pieces form the chunk.
    pub fn load_reader(
        &mut self,
        mut reader: impl FnMut(&mut Vm) -> LuaResult<Option<Vec<u8>>>,
        chunkname: &[u8],
        mode: Option<&[u8]>,
    ) -> LuaResult<Gc<Function>> {
        let mut buf = Vec::new();
        loop {
            match reader(self) {
                Ok(Some(p)) if !p.is_empty() => {
                    if buf.len() + p.len() > LJ_MAX_BUF {
                        let v = self.err_str(ErrMsg::ERRMEM);
                        return Err(LuaError::new(ErrorKind::Mem, v));
                    }
                    buf.extend_from_slice(&p);
                }
                Ok(_) => break,
                Err(e) => {
                    if e.kind == ErrorKind::Run && self.rt.thrown.is_none_or(|t| !t.raw_equal(e.value)) {
                        let th = self.cur_thread;
                        let e = self.apply_errfunc(th, e);
                        self.rt.thrown = Some(e.value);
                        return Err(e);
                    }
                    return Err(e);
                }
            }
        }
        self.load(&buf, chunkname, mode)
    }

    /// `luaL_loadfilex`: load a file (`None` = stdin, chunkname `=stdin`). File errors are
    /// `ErrorKind::File` with upstream's `cannot open/read <name>` messages.
    pub fn load_file(&mut self, filename: Option<&[u8]>, mode: Option<&[u8]>) -> LuaResult<Gc<Function>> {
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
