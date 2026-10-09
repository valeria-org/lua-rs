//! LuaJIT 2.1 re-implemented in Rust: interpreter, standard library and FFI.
//!
//! Behaviour reference: upstream LuaJIT v2.1 built with `-DLUAJIT_DISABLE_JIT` (x86-64, GC64).

pub mod gc;
pub mod value;
pub mod lstr;
pub mod table;
pub mod func;
pub mod udata;
pub mod cdata;
pub mod thread;
pub mod bc;
pub mod error;
pub mod state;
