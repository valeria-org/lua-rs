//! Port of lj_bcread.c: load a bytecode dump into a prototype tree.
//!
//! The input comes from the chunk [`Input`]: in memory, or piece by piece from a reader that is
//! asked for more data only when `bcread_want`/`bcread_need` require it, as upstream. A
//! prototype is read from the buffer once its announced length is available. Upstream reads a
//! prototype blindly — past its end into the following buffered bytes if it is malformed — and
//! checks the consumed length afterwards; this reader does the same, so errors raised on the way
//! (`table index is nil`, …) match. Upstream would go on reading undefined memory past the end
//! of the buffer; here that is `cannot load malformed bytecode`, except in the header, where
//! missing bytes read as zero.
//!
//! Beyond upstream, every instruction must have a valid opcode (`Ins::op` relies on it) and the
//! debug info block must decode completely (it is stored decoded in [`Proto`]); otherwise the
//! dump is rejected as malformed.

use std::borrow::Cow;
use std::cell::Cell;
use std::ops::Range;

use super::*;
use crate::bc::{BC_MAX, Ins, Op};
use crate::cdata::{CTID_COMPLEX_DOUBLE, CTID_INT64, CTID_UINT64};
use crate::error::{ErrorKind, LuaError, LuaResult};
use crate::func::{KGc, PROTO_VARARG, Proto, VARNAME_END, VARNAME_MAX, VarInfo, VarName};
use crate::gc::Gc;
use crate::lstr::{LuaStr, MAX_STR};
use crate::parse::lex::{self, Input, LJ_MAX_BUF};
use crate::state::Vm;
use crate::table::{MAX_ASIZE, MAX_HBITS, TabError, Table, hsize2hbits};
use crate::value::Value;

/// `LJ_ERR_BCFMT`.
const ERR_BCFMT: &str = "cannot load incompatible bytecode";
/// `LJ_ERR_BCBAD`.
const ERR_BCBAD: &str = "cannot load malformed bytecode";
/// `LJ_ERR_STROV`.
const ERR_STROV: &str = "string length overflow";

/// Load a bytecode dump of native (GC64/FR2) bytecode. `src` starts with the `\x1b` signature.
/// `chunkname` is upstream's `chunkarg`: C string semantics (cut at the first NUL); it names the
/// chunk in error messages (`(binary)` if it starts with `\x1b`, a leading `@`/`=` is skipped)
/// and becomes the chunk name of stripped dumps (`=?` if it starts with `\x1b`).
///
/// Errors: `<name>: cannot load incompatible bytecode` / `<name>: cannot load malformed
/// bytecode` with [`ErrorKind::Syntax`]; constant construction errors (`table overflow`,
/// `table index is nil`, `string length overflow`, …) with [`ErrorKind::Run`], as upstream.
pub fn read(vm: &mut Vm, src: &[u8], chunkname: &[u8]) -> LuaResult<Gc<Proto>> {
    read_ex(vm, src, chunkname, true)
}

/// [`read`] with an explicit bytecode flavour: `fr2 == false` is upstream's load mode `X`
/// (`ls->fr2 = 0`), which only accepts dumps without `BCDUMP_F_FR2` and marks the resulting
/// non-native (dumpable, not runnable) prototypes with a `NOT` header instruction.
pub fn read_ex(vm: &mut Vm, src: &[u8], chunkname: &[u8], fr2: bool) -> LuaResult<Gc<Proto>> {
    let chunkarg = lex::cstr(chunkname);
    if src.first() != Some(&BCDUMP_HEAD1) {
        return Err(error(vm, chunkarg, ERR_BCFMT));
    }
    let mut input = Input::from_slice(src);
    input.p = 1;
    read_input(vm, input, chunkarg, fr2)
}

/// `lj_bcread`: read a dump from `input`, positioned after the leading `\x1b` (consumed by
/// `lj_lex_setup`). `chunkarg` and `fr2` as for [`read_ex`].
pub fn read_input<'a>(vm: &'a mut Vm, input: Input<'a>, chunkarg: &'a [u8], fr2: bool) -> LuaResult<Gc<Proto>> {
    let empty = vm.empty_str;
    let mut ls = Reader { vm, inp: input, eof: false, flags: 0, fr2, chunkarg: lex::cstr(chunkarg), chunkname: empty };
    ls.read_dump()
}

/// `lj_bcread_proto` as lj_lib.c's `lib_read_lfunc` uses it for the Lua-coded builtins embedded
/// in lj_libdef.h: one stripped, native, little-endian prototype at the start of `src` (no dump
/// header, no length prefix, no child prototypes), named `chunkname`. Returns the prototype —
/// not yet allocated, so the caller can adjust it (upstream sets `firstline = !0`) — and the
/// number of bytes it occupies.
pub fn read_proto(vm: &mut Vm, src: &[u8], chunkname: Gc<LuaStr>) -> LuaResult<(Proto, usize)> {
    let name = chunkname.as_bytes();
    let mut ls = Reader {
        vm,
        inp: Input::from_slice(src),
        eof: true,
        flags: BCDUMP_F_STRIP,
        fr2: true,
        chunkarg: name,
        chunkname,
    };
    let pt = ls.proto(&mut Vec::new())?;
    if !valid_opcodes(&pt) {
        return Err(ls.bad());
    }
    Ok((pt, ls.inp.p))
}

/// Every instruction has a valid opcode, which `Ins::op` relies on (upstream doesn't check).
fn valid_opcodes(pt: &Proto) -> bool {
    pt.bc.iter().all(|ins| (ins.op_byte() as usize) < BC_MAX)
}

/// `bcread_error`: `"<name>: <msg>"` as a syntax error.
fn error(vm: &mut Vm, chunkarg: &[u8], msg: &str) -> LuaError {
    let name: &[u8] = match chunkarg.first() {
        Some(&BCDUMP_HEAD1) => b"(binary)",
        Some(b'@' | b'=') => &chunkarg[1..],
        _ => chunkarg,
    };
    let mut s = Vec::with_capacity(name.len() + 2 + msg.len());
    s.extend_from_slice(name);
    s.extend_from_slice(b": ");
    s.extend_from_slice(msg.as_bytes());
    LuaError::new(ErrorKind::Syntax, Value::Str(vm.new_str(&s)))
}

/// Reader state (the `LexState` fields upstream reuses for bytecode).
struct Reader<'a> {
    vm: &'a mut Vm,
    /// Chunk input: the buffered bytes and the read position (`ls->p`, `ls->pe`).
    inp: Input<'a>,
    /// Reader returned EOF (`ls->c < 0`): the next refill is an error.
    eof: bool,
    /// Header `BCDUMP_F_*` flags.
    flags: u32,
    /// Accept native (FR2) bytecode.
    fr2: bool,
    chunkarg: &'a [u8],
    chunkname: Gc<LuaStr>,
}

impl<'a> Reader<'a> {
    fn err(&mut self, msg: &str) -> LuaError {
        error(self.vm, self.chunkarg, msg)
    }

    fn bad(&mut self) -> LuaError {
        self.err(ERR_BCBAD)
    }

    /// Keep a GC object alive while the reader may run Lua code (see [`Input::streaming`]).
    fn anchor(&mut self, v: Value) {
        if self.inp.streaming() {
            self.vm.anchor_load(v);
        }
    }

    #[inline]
    fn remaining(&self) -> usize {
        self.inp.buf.len().saturating_sub(self.inp.p)
    }

    /// `bcread_fill`: get pieces from the reader until `len` bytes are buffered; the unread
    /// rest of the buffer is kept. At the end of the input that is an error if `need`, else
    /// it is recorded and only a further refill is an error.
    #[cold]
    fn fill(&mut self, len: usize, need: bool) -> LuaResult<()> {
        if len > LJ_MAX_BUF || self.eof {
            return Err(self.bad());
        }
        loop {
            let n = self.remaining();
            let Some(piece) = self.inp.read_piece(self.vm)? else {
                if need {
                    return Err(self.bad());
                }
                self.eof = true;
                return Ok(());
            };
            if piece.len() >= LJ_MAX_BUF - n {
                return Err(lex::err_mem(self.vm));
            }
            if n > 0 {
                let mut b = Vec::with_capacity((n + piece.len()).max(len));
                b.extend_from_slice(&self.inp.buf[self.inp.p..]);
                b.extend_from_slice(&piece);
                self.inp.buf = Cow::Owned(b);
            } else {
                self.inp.buf = Cow::Owned(piece);
            }
            self.inp.p = 0;
            if self.remaining() >= len {
                return Ok(());
            }
        }
    }

    /// `bcread_want`: try to buffer `len` bytes; fewer are left only at the end of the input.
    #[inline]
    fn want(&mut self, len: usize) -> LuaResult<()> {
        if self.remaining() < len { self.fill(len, false) } else { Ok(()) }
    }

    /// `bcread_need`: buffer `len` bytes.
    #[inline]
    fn need(&mut self, len: usize) -> LuaResult<()> {
        if self.remaining() < len { self.fill(len, true) } else { Ok(()) }
    }

    /// `bcread_mem`: the buffer range of the next `len` bytes.
    fn mem(&mut self, len: usize) -> LuaResult<Range<usize>> {
        if len == 0 {
            return Ok(0..0);
        }
        if self.remaining() < len {
            return Err(self.bad());
        }
        let p = self.inp.p;
        self.inp.p += len;
        Ok(p..p + len)
    }

    /// `bcread_byte`.
    #[inline]
    fn byte(&mut self) -> LuaResult<u8> {
        match self.inp.buf.get(self.inp.p) {
            Some(&b) => {
                self.inp.p += 1;
                Ok(b)
            }
            None => Err(self.bad()),
        }
    }

    /// `bcread_uleb128` (`lj_buf_ruleb128`, x86 shift-count masking for overlong encodings).
    fn uleb128(&mut self) -> LuaResult<u32> {
        let mut v = self.byte()? as u32;
        if v >= 0x80 {
            let mut sh = 0u32;
            v &= 0x7f;
            loop {
                let b = self.byte()?;
                sh = sh.wrapping_add(7);
                v |= ((b & 0x7f) as u32).wrapping_shl(sh);
                if b < 0x80 {
                    break;
                }
            }
        }
        Ok(v)
    }

    /// `bcread_uleb128_33`: top 32 bits of a 33 bit ULEB128 (the lsb is the int/num tag).
    fn uleb128_33(&mut self) -> LuaResult<u32> {
        let mut v = (self.byte()? >> 1) as u32;
        if v >= 0x40 {
            let mut sh = 0u32.wrapping_sub(1);
            v &= 0x3f;
            loop {
                let b = self.byte()?;
                sh = sh.wrapping_add(7);
                v |= ((b & 0x7f) as u32).wrapping_shl(sh);
                if b < 0x80 {
                    break;
                }
            }
        }
        Ok(v)
    }

    /// Header byte read for `bcread_header`. A truncated header reads as zero bytes, mirroring
    /// upstream's (uninitialized) refill buffer for the deterministic cases: a short magic,
    /// version or flags field fails the compatibility check, a missing name length gives an
    /// empty name and the next read reports a malformed dump.
    fn hbyte(&mut self) -> u8 {
        let b = self.inp.buf.get(self.inp.p).copied().unwrap_or(0);
        self.inp.p += 1;
        b
    }

    /// `lj_buf_ruleb128` over header bytes.
    fn huleb128(&mut self) -> u32 {
        let mut v = self.hbyte() as u32;
        if v >= 0x80 {
            let mut sh = 0u32;
            v &= 0x7f;
            loop {
                let b = self.hbyte();
                sh = sh.wrapping_add(7);
                v |= ((b & 0x7f) as u32).wrapping_shl(sh);
                if b < 0x80 {
                    break;
                }
            }
        }
        v
    }

    /// `bcread_header`: false if the dump is incompatible.
    fn header(&mut self) -> LuaResult<bool> {
        self.want(3 + 5 + 5)?;
        if self.hbyte() != BCDUMP_HEAD2 || self.hbyte() != BCDUMP_HEAD3 || self.hbyte() != BCDUMP_VERSION {
            return Ok(false);
        }
        let flags = self.huleb128();
        self.flags = flags;
        if flags & !BCDUMP_F_KNOWN != 0 {
            return Ok(false);
        }
        if (flags & BCDUMP_F_FR2 != 0) != self.fr2 {
            return Ok(false);
        }
        if flags & BCDUMP_F_FFI != 0 {
            crate::parse::ffi_literal_hook(self.vm)?;
        }
        if flags & BCDUMP_F_STRIP != 0 {
            let name: &[u8] = if self.chunkarg.first() == Some(&BCDUMP_HEAD1) { b"=?" } else { self.chunkarg };
            self.chunkname = self.vm.new_str(name);
        } else {
            let len = self.huleb128() as usize;
            self.need(len)?;
            let r = self.mem(len)?;
            self.chunkname = self.vm.new_str(&self.inp.buf[r]);
        }
        self.anchor(Value::Str(self.chunkname));
        Ok(true)
    }

    /// `lj_bcread`: all prototypes; exactly one (the main chunk) must remain on the stack.
    fn read_dump(&mut self) -> LuaResult<Gc<Proto>> {
        if !self.header()? {
            return Err(self.err(ERR_BCFMT));
        }
        let mut stack: Vec<Gc<Proto>> = Vec::new();
        loop {
            // Shortcut EOF.
            if self.inp.buf.get(self.inp.p) == Some(&0) {
                self.inp.p += 1;
                break;
            }
            self.want(5)?;
            let len = self.uleb128()? as usize;
            if len == 0 {
                break;
            }
            self.need(len)?;
            let start = self.inp.p;
            let pt = self.proto(&mut stack)?;
            if self.inp.p != start + len || !valid_opcodes(&pt) {
                return Err(self.bad());
            }
            let pt = self.vm.new_proto(pt);
            self.anchor(Value::Proto(pt));
            stack.push(pt);
        }
        if self.inp.p != self.inp.buf.len() || stack.len() != 1 {
            return Err(self.bad());
        }
        Ok(stack[0])
    }

    /// `bcread_swap`: the dump was written on a host of the other endianness.
    #[inline]
    fn swap(&self) -> bool {
        self.flags & BCDUMP_F_BE != 0
    }

    /// `lj_bcread_proto`: the prototype is returned unallocated so the caller can check it.
    fn proto(&mut self, stack: &mut Vec<Gc<Proto>>) -> LuaResult<Proto> {
        // Prototype header.
        let flags = self.byte()?;
        let numparams = self.byte()?;
        let framesize = self.byte()?;
        let sizeuv = self.byte()? as usize;
        let sizekgc = self.uleb128()? as usize;
        let sizekn = self.uleb128()? as usize;
        let sizebc = self.uleb128()? as usize + 1;
        let (mut sizedbg, mut firstline, mut numline) = (0usize, 0u32, 0u32);
        if self.flags & BCDUMP_F_STRIP == 0 {
            sizedbg = self.uleb128()? as usize;
            if sizedbg != 0 {
                firstline = self.uleb128()?;
                numline = self.uleb128()?;
            }
        }

        // Bytecode instructions; bc[0] is synthesized.
        let op = if !self.fr2 {
            Op::NOT // Mark non-native prototype.
        } else if flags & PROTO_VARARG != 0 {
            Op::FUNCV
        } else {
            Op::FUNCF
        };
        let swap = self.swap();
        let r = self.mem((sizebc - 1) * 4)?;
        let raw = &self.inp.buf[r];
        let mut bc = Vec::with_capacity(sizebc);
        bc.push(Ins::ad(op, framesize as u32, 0));
        bc.extend(raw.as_chunks::<4>().0.iter().map(|&w| {
            let v = u32::from_le_bytes(w);
            Ins(if swap { v.swap_bytes() } else { v })
        }));

        // Upvalue refs.
        let r = self.mem(sizeuv * 2)?;
        let raw = &self.inp.buf[r];
        let uv: Box<[u16]> = raw
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&w| {
                let v = u16::from_le_bytes(w);
                if swap { v.swap_bytes() } else { v }
            })
            .collect();

        // Constants.
        let kgc = self.kgc(sizekgc, stack)?;
        let knum = self.knum(sizekn)?;

        // Debug info.
        let dbg =
            if sizedbg != 0 { self.debug(sizedbg, sizebc, sizeuv, firstline, numline)? } else { DebugInfo::default() };

        Ok(Proto {
            bc: bc.into_boxed_slice(),
            kgc,
            knum,
            uv,
            numparams,
            framesize,
            flags: Cell::new(flags),
            chunkname: self.chunkname,
            firstline,
            numline,
            lineinfo: dbg.lineinfo,
            uvnames: dbg.uvnames,
            varinfo: dbg.varinfo,
        })
    }

    /// `bcread_kgc`: constants arrive in upstream memory order (highest D first); child
    /// prototypes are popped from the stack.
    fn kgc(&mut self, sizekgc: usize, stack: &mut Vec<Gc<Proto>>) -> LuaResult<Box<[KGc]>> {
        // Every constant takes at least one byte; don't trust the count for the capacity.
        let mut kgc = Vec::with_capacity(sizekgc.min(self.remaining()));
        for _ in 0..sizekgc {
            let tp = self.uleb128()?;
            let k = if tp >= BCDUMP_KGC_STR {
                KGc::Str(self.string(tp - BCDUMP_KGC_STR)?)
            } else if tp == BCDUMP_KGC_TAB {
                KGc::Table(self.ktab()?)
            } else if tp != BCDUMP_KGC_CHILD {
                let (id, sz) = match tp {
                    BCDUMP_KGC_COMPLEX => (CTID_COMPLEX_DOUBLE, 16),
                    BCDUMP_KGC_I64 => (CTID_INT64, 8),
                    _ => (CTID_UINT64, 8),
                };
                let mut q = [0u8; 16];
                for w in q[..sz].as_chunks_mut::<4>().0 {
                    *w = self.uleb128()?.to_le_bytes();
                }
                let cd = self.vm.new_cdata(id, sz, 8);
                // SAFETY: `cd` was just allocated with an owned payload of `sz` bytes; `q` holds
                // at least `sz` bytes and is a separate allocation.
                unsafe { std::ptr::copy_nonoverlapping(q.as_ptr(), cd.ptr(), sz) };
                KGc::CData(cd)
            } else {
                match stack.pop() {
                    Some(pt) => KGc::Proto(pt),
                    None => return Err(self.bad()), // Stack underflow.
                }
            };
            kgc.push(k);
        }
        kgc.reverse(); // Index by the bytecode D operand.
        Ok(kgc.into_boxed_slice())
    }

    /// `lj_str_new` of `len` input bytes (with its length limit check first).
    fn string(&mut self, len: u32) -> LuaResult<Gc<LuaStr>> {
        if len as usize >= MAX_STR {
            return Err(self.vm.msg_error(ERR_STROV));
        }
        let r = self.mem(len as usize)?;
        Ok(self.vm.new_str(&self.inp.buf[r]))
    }

    /// `bcread_knum`.
    fn knum(&mut self, sizekn: usize) -> LuaResult<Box<[f64]>> {
        let mut knum = Vec::with_capacity(sizekn.min(self.remaining()));
        for _ in 0..sizekn {
            let isnum = self.inp.buf.get(self.inp.p).is_some_and(|&b| b & 1 != 0);
            let lo = self.uleb128_33()?;
            knum.push(if isnum {
                let hi = self.uleb128()?;
                f64::from_bits(((hi as u64) << 32) | lo as u64)
            } else {
                lo as i32 as f64
            });
        }
        Ok(knum.into_boxed_slice())
    }

    /// `bcread_ktabk`: the type code and the value (nil for `BCDUMP_KTAB_NIL`).
    fn ktabk(&mut self) -> LuaResult<(u32, Value)> {
        let tp = self.uleb128()?;
        let v = if tp >= BCDUMP_KTAB_STR {
            Value::Str(self.string(tp - BCDUMP_KTAB_STR)?)
        } else if tp == BCDUMP_KTAB_INT {
            Value::Num(self.uleb128()? as i32 as f64)
        } else if tp == BCDUMP_KTAB_NUM {
            let lo = self.uleb128()?;
            let hi = self.uleb128()?;
            Value::Num(f64::from_bits(((hi as u64) << 32) | lo as u64))
        } else {
            match tp {
                BCDUMP_KTAB_NIL => Value::Nil,
                BCDUMP_KTAB_FALSE => Value::Bool(false),
                _ => Value::Bool(true),
            }
        };
        Ok((tp, v))
    }

    /// `bcread_ktab`. The entries are decoded before the table is allocated, so a corrupt size
    /// can't make a short input allocate a huge table; errors arise in upstream's order.
    fn ktab(&mut self) -> LuaResult<Gc<Table>> {
        let narray = self.uleb128()?;
        let nhash = self.uleb128()?;
        let hbits = hsize2hbits(nhash);
        if narray > MAX_ASIZE || hbits > MAX_HBITS {
            return Err(self.vm.tab_error(TabError::Overflow));
        }
        let mut array = Vec::with_capacity((narray as usize).min(self.remaining()));
        for _ in 0..narray {
            array.push(self.ktabk()?.1);
        }
        // Hash values of `None` are the nil value marker (the table itself).
        let mut hash: Vec<(Value, Option<Value>)> = Vec::with_capacity((nhash as usize).min(self.remaining() / 2));
        for _ in 0..nhash {
            let key = self.ktabk()?.1;
            // lj_tab_set rejects the key before the value is read.
            match key {
                Value::Nil => return Err(self.vm.tab_error(TabError::NilIndex)),
                Value::Num(n) if n.is_nan() => return Err(self.vm.tab_error(TabError::NanIndex)),
                _ => {}
            }
            let (tp, val) = self.ktabk()?;
            hash.push((key, if tp == BCDUMP_KTAB_NIL { None } else { Some(val) }));
        }
        let t = self.vm.new_table(narray, hbits)?;
        for (i, v) in array.into_iter().enumerate() {
            t.array_set(i as u32, v);
        }
        for (key, val) in hash {
            if let Err(e) = t.set(key, val.unwrap_or(Value::Table(t))) {
                return Err(self.vm.tab_error(e));
            }
        }
        Ok(t)
    }

    /// Decode the debug info block (`bcread_dbg` + `bcread_varinfo`): line deltas, upvalue
    /// names and the varinfo stream. Upstream copies the block verbatim and decodes it lazily;
    /// a block that does not decode (or has trailing bytes) is rejected as malformed here.
    fn debug(
        &mut self,
        sizedbg: usize,
        sizebc: usize,
        sizeuv: usize,
        firstline: u32,
        numline: u32,
    ) -> LuaResult<DebugInfo> {
        let swap = self.swap();
        let r = self.mem(sizedbg)?;
        // Errors are built from the fields, as `block` borrows the buffer.
        let block = &self.inp.buf[r];
        let shift = lineinfo_shift(numline);
        let sizeli = (sizebc - 1) << shift;
        if sizeli > block.len() {
            return Err(error(self.vm, self.chunkarg, ERR_BCBAD));
        }
        let mut lineinfo = Vec::with_capacity(sizebc);
        lineinfo.push(firstline); // Line of the FUNCF/FUNCV header.
        let li = &block[..sizeli];
        match shift {
            0 => lineinfo.extend(li.iter().map(|&d| firstline.wrapping_add(d as u32))),
            1 => lineinfo.extend(li.as_chunks::<2>().0.iter().map(|&w| {
                let d = u16::from_le_bytes(w);
                firstline.wrapping_add(if swap { d.swap_bytes() } else { d } as u32)
            })),
            _ => lineinfo.extend(li.as_chunks::<4>().0.iter().map(|&w| {
                let d = u32::from_le_bytes(w);
                firstline.wrapping_add(if swap { d.swap_bytes() } else { d })
            })),
        }

        // Upvalue names: zero-terminated strings.
        let mut p = sizeli;
        let mut uvnames = Vec::with_capacity(sizeuv);
        for _ in 0..sizeuv {
            let Some(name) = cstr(block, &mut p) else {
                return Err(error(self.vm, self.chunkarg, ERR_BCBAD));
            };
            uvnames.push(self.vm.new_str(name));
        }

        // Varinfo: (VARNAME_* byte | zero-terminated name) startpc-delta length, until a 0 byte.
        let mut varinfo = Vec::new();
        let mut lastpc = 0u32;
        loop {
            let Some(&vn) = block.get(p) else {
                return Err(error(self.vm, self.chunkarg, ERR_BCBAD));
            };
            let name = if vn < VARNAME_MAX {
                p += 1;
                if vn == VARNAME_END {
                    break;
                }
                VarName::Internal(vn)
            } else {
                let Some(name) = cstr(block, &mut p) else {
                    return Err(error(self.vm, self.chunkarg, ERR_BCBAD));
                };
                VarName::Str(self.vm.new_str(name))
            };
            let (Some(d1), Some(d2)) = (block_uleb128(block, &mut p), block_uleb128(block, &mut p)) else {
                return Err(error(self.vm, self.chunkarg, ERR_BCBAD));
            };
            let startpc = lastpc.wrapping_add(d1);
            lastpc = startpc;
            varinfo.push(VarInfo { name, startpc, endpc: startpc.wrapping_add(d2) });
        }
        if p != block.len() {
            return Err(error(self.vm, self.chunkarg, ERR_BCBAD));
        }
        Ok(DebugInfo {
            lineinfo: lineinfo.into_boxed_slice(),
            uvnames: uvnames.into_boxed_slice(),
            varinfo: varinfo.into_boxed_slice(),
        })
    }
}

/// Decoded debug info of a prototype.
#[derive(Default)]
struct DebugInfo {
    lineinfo: Box<[u32]>,
    uvnames: Box<[Gc<LuaStr>]>,
    varinfo: Box<[VarInfo]>,
}

/// Zero-terminated string at `*p` inside `block`; advances past the terminator.
fn cstr<'b>(block: &'b [u8], p: &mut usize) -> Option<&'b [u8]> {
    let rest = block.get(*p..)?;
    let n = rest.iter().position(|&c| c == 0)?;
    *p += n + 1;
    Some(&rest[..n])
}

/// `lj_buf_ruleb128` bounded by the debug info block.
fn block_uleb128(block: &[u8], p: &mut usize) -> Option<u32> {
    let mut v = *block.get(*p)? as u32;
    *p += 1;
    if v >= 0x80 {
        let mut sh = 0u32;
        v &= 0x7f;
        loop {
            let b = *block.get(*p)?;
            *p += 1;
            sh = sh.wrapping_add(7);
            v |= ((b & 0x7f) as u32).wrapping_shl(sh);
            if b < 0x80 {
                break;
            }
        }
    }
    Some(v)
}
