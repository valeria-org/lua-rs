//! Lua source parser: port of lj_lex.c (lexer) and lj_parse.c (bytecode compiler), plus a
//! bytecode listing in the format of `luajit -bl` (jit/bc.lua).

pub mod lex;
pub mod listing;
mod parser;

use std::sync::OnceLock;

use crate::error::LuaResult;
use crate::func::Proto;
use crate::gc::Gc;
use crate::state::Vm;

pub use lex::debug_shortname;
pub use listing::bclist;

/// Parse Lua source text into a prototype (`lj_lex_setup` + `lj_parse`). Skips a UTF-8 BOM and a
/// `#` header line. Syntax errors are `ErrorKind::Syntax` errors carrying upstream's message.
///
/// The input is always treated as source text: chunk loading ([`Vm::load`]) dispatches bytecode
/// dumps (leading `\x1b`) to the bytecode reader. A dump after a BOM/`#` header is rejected with
/// "cannot load malformed bytecode", as upstream does.
pub fn parse(vm: &mut Vm, src: &[u8], chunkname: &[u8]) -> LuaResult<Gc<Proto>> {
    parse_ex(vm, src, chunkname, true)
}

/// [`parse`] with a choice of frame layout: `fr2 = false` generates the non-native (2-slot
/// frame) bytecode selected by the `W` load mode; such prototypes are dumpable but not runnable
/// (their header op is `NOT`, as upstream marks them).
pub fn parse_ex(vm: &mut Vm, src: &[u8], chunkname: &[u8], fr2: bool) -> LuaResult<Gc<Proto>> {
    lex::init_reserved(vm);
    let name = vm.new_str(lex::cstr(chunkname));
    let mut ls = lex::LexState::new(vm, lex::Input::from_slice(src), name);
    ls.setup()?;
    parse_lex(ls, fr2)
}

/// `lj_parse` on a lexer already set up (`lj_lex_setup` found source text).
pub fn parse_lex(ls: lex::LexState<'_>, fr2: bool) -> LuaResult<Gc<Proto>> {
    parser::Parser::new(ls, fr2).parse()
}

/// Loader for the `ffi` library, registered by the FFI implementation.
static FFI_LOADER: OnceLock<fn(&mut Vm) -> LuaResult<()>> = OnceLock::new();

/// Register the function that loads the `ffi` library into a VM (`luaopen_ffi`). Called once by
/// the FFI implementation; later registrations are ignored.
pub fn set_ffi_loader(f: fn(&mut Vm) -> LuaResult<()>) {
    let _ = FFI_LOADER.set(f);
}

/// `ctype_loadffi`: make sure the FFI library is loaded before a 64-bit integer or imaginary
/// number literal (KCDATA constant) is created. Also used by the bytecode reader for dumps with
/// FFI constants.
pub fn ffi_literal_hook(vm: &mut Vm) -> LuaResult<()> {
    if vm.ffi.is_none()
        && let Some(load) = FFI_LOADER.get()
    {
        load(vm)?;
    }
    Ok(())
}
