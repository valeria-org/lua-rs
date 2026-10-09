//! The LuaJIT FFI: C type system, C declaration parser, C calls, callbacks and
//! C library namespaces (x86-64 SysV only).
//!
//! This layer works purely on C types, raw bytes and byte-string names. The
//! conversions between Lua values and C data (lj_cconv, lj_carith, lj_cdata,
//! lib_ffi) are built on top of it.

pub mod callback;
pub mod ccall;
pub mod clib;
pub mod cparse;
pub mod ctype;

/// An error raised by the FFI core, classified by how upstream throws it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FfiError {
    /// Thrown with `lj_err_msg`: the message is used as is (e.g. `table overflow`).
    Msg(Vec<u8>),
    /// Thrown with `lj_err_caller`/`lj_err_callermsg`: the integration layer
    /// prefixes the position of the calling Lua function (`chunk:line: `).
    Caller(Vec<u8>),
    /// Thrown with `lj_err_argtype(L, narg, expected)`:
    /// `bad argument #narg to '<fn>' (<expected> expected, got <type>)`.
    ArgType { narg: i32, expected: &'static str },
}

impl FfiError {
    pub(crate) fn caller(msg: impl Into<Vec<u8>>) -> FfiError {
        FfiError::Caller(msg.into())
    }

    /// The message text without any position prefix (for `ArgType` only the
    /// expected-type description).
    pub fn message(&self) -> &[u8] {
        match self {
            FfiError::Msg(m) | FfiError::Caller(m) => m,
            FfiError::ArgType { expected, .. } => expected.as_bytes(),
        }
    }
}

impl std::fmt::Display for FfiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FfiError::Msg(m) | FfiError::Caller(m) => f.write_str(&String::from_utf8_lossy(m)),
            FfiError::ArgType { narg, expected } => {
                write!(f, "bad argument #{narg} ({expected} expected)")
            }
        }
    }
}

impl std::error::Error for FfiError {}

/// Upstream error message texts (lj_errmsg.h) used by the FFI core.
pub mod errmsg {
    pub const TABOV: &str = "table overflow";
    pub const BADVAL: &str = "invalid value";
    pub const XNUMBER: &str = "malformed number";
    pub const XLEVELS: &str = "chunk has too many syntax levels";
    pub const XSTR: &str = "unfinished string";
    pub const XSYMBOL: &str = "unexpected symbol";
    pub const XSYNTAX: &str = "syntax error";
    pub const FFI_INVTYPE: &str = "invalid C type";
    pub const FFI_INVSIZE: &str = "size of C type is unknown or too large";
    pub const FFI_BADSCL: &str = "bad storage class";
    pub const FFI_DECLSPEC: &str = "declaration specifier expected";
    pub const FFI_NUMPARAM: &str = "wrong number of type parameters";
    pub const FFI_NUMARG: &str = "wrong number of arguments for function call";
    pub const FFI_BADCBACK: &str = "bad callback";
    pub const FFI_CBACKOV: &str = "too many callbacks";
    pub const FFI_NYICALL: &str = "NYI: cannot call this C function (yet)";
}

#[cfg(test)]
mod tests;
