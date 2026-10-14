//! LuaJIT 2.1 re-implemented in Rust: interpreter, standard library and FFI.
//!
//! Behaviour reference: upstream LuaJIT v2.1 built with `-DLUAJIT_DISABLE_JIT` (x86-64, GC64).
//! [`syntax`] provides owned Lua 5.4 and Luau ASTs for analysis, independently of the VM.

pub mod bc;
pub mod bcdump;
pub mod capi;
pub mod cdata;
pub mod cli;
pub mod error;
pub mod ffi;
pub mod func;
pub mod gc;
pub mod lstr;
pub mod parse;
pub mod state;
pub mod stdlib;
pub mod syntax;
pub mod table;
pub mod text;
pub mod thread;
pub mod udata;
pub mod value;
pub mod vm;
