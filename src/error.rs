//! Error values and status codes (lua.h / lauxlib.h status codes, `lj_err` error kinds).

use std::fmt;

use crate::value::Value;

/// Status codes of the Lua 5.1 C API.
pub const LUA_OK: i32 = 0;
pub const LUA_YIELD: i32 = 1;
pub const LUA_ERRRUN: i32 = 2;
pub const LUA_ERRSYNTAX: i32 = 3;
pub const LUA_ERRMEM: i32 = 4;
pub const LUA_ERRERR: i32 = 5;
/// lauxlib: `LUA_ERRERR + 1`.
pub const LUA_ERRFILE: i32 = 6;

/// Kind of a raised error.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ErrorKind {
    /// Runtime error (`LUA_ERRRUN`).
    Run,
    /// Syntax error from the parser or bytecode loader (`LUA_ERRSYNTAX`).
    Syntax,
    /// Out of memory (`LUA_ERRMEM`).
    Mem,
    /// Error while running an error handler (`LUA_ERRERR`).
    Err,
    /// File open/read error in `luaL_loadfile` (`LUA_ERRFILE`).
    File,
    /// Internal: a coroutine yield unwinding to its resume boundary (`LUA_YIELD`). Never seen
    /// by Lua code.
    Yield,
}

impl ErrorKind {
    /// Status code for the C API / `pcall` machinery.
    pub fn status(self) -> i32 {
        match self {
            ErrorKind::Yield => LUA_YIELD,
            ErrorKind::Run => LUA_ERRRUN,
            ErrorKind::Syntax => LUA_ERRSYNTAX,
            ErrorKind::Mem => LUA_ERRMEM,
            ErrorKind::Err => LUA_ERRERR,
            ErrorKind::File => LUA_ERRFILE,
        }
    }

    /// Inverse of `status` (`None` for `LUA_OK` and unknown codes).
    pub fn from_status(s: i32) -> Option<ErrorKind> {
        Some(match s {
            LUA_YIELD => ErrorKind::Yield,
            LUA_ERRRUN => ErrorKind::Run,
            LUA_ERRSYNTAX => ErrorKind::Syntax,
            LUA_ERRMEM => ErrorKind::Mem,
            LUA_ERRERR => ErrorKind::Err,
            LUA_ERRFILE => ErrorKind::File,
            _ => return None,
        })
    }
}

/// A raised Lua error: kind plus error object.
///
/// Rooting: `value` is not a GC root. Whoever catches the error must put the value on a Lua
/// stack (or otherwise root it) before the next GC safe point.
#[derive(Clone, Copy, Debug)]
pub struct LuaError {
    pub kind: ErrorKind,
    pub value: Value,
}

impl LuaError {
    pub fn new(kind: ErrorKind, value: Value) -> LuaError {
        LuaError { kind, value }
    }
    /// Runtime error with the given error object.
    pub fn run(value: Value) -> LuaError {
        LuaError {
            kind: ErrorKind::Run,
            value,
        }
    }
    #[inline]
    pub fn status(&self) -> i32 {
        self.kind.status()
    }
}

impl fmt::Display for LuaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.value {
            Value::Str(s) => f.write_str(&String::from_utf8_lossy(s.as_bytes())),
            Value::Num(n) => write!(f, "{n}"),
            v => write!(f, "(error object is a {} value)", v.type_name()),
        }
    }
}

impl std::error::Error for LuaError {}

/// Result of fallible VM operations.
pub type LuaResult<T> = Result<T, LuaError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_codes() {
        for k in [
            ErrorKind::Run,
            ErrorKind::Syntax,
            ErrorKind::Mem,
            ErrorKind::Err,
            ErrorKind::File,
            ErrorKind::Yield,
        ] {
            assert_eq!(ErrorKind::from_status(k.status()), Some(k));
        }
        assert_eq!(ErrorKind::Run.status(), 2);
        assert_eq!(ErrorKind::File.status(), 6);
        assert_eq!(ErrorKind::from_status(LUA_OK), None);
    }
}
