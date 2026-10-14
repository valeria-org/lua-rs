//! Port of lj_bcwrite.c: serialize a prototype tree into a bytecode dump.

use std::cmp::Ordering;

use super::*;
use crate::bc::Op;
use crate::cdata::{CTID_COMPLEX_DOUBLE, CTID_INT64, CTID_UINT64};
use crate::func::{KGc, PROTO_BITOP, PROTO_CHILD, PROTO_FFI, PROTO_VARARG, Proto, VarName};
use crate::gc::Gc;
use crate::state::Vm;
use crate::table::Table;
use crate::value::{Value, num2int_check};

/// `string.dump(f [, strip])`: dump `pt` and all its children. `strip` omits the chunk name and
/// all debug info (line numbers, upvalue and local variable names).
pub fn write(vm: &mut Vm, pt: Gc<Proto>, strip: bool) -> Vec<u8> {
    write_flags(vm, pt, if strip { BCDUMP_F_STRIP } else { 0 })
}

/// `lj_bcwrite` with explicit flags: any combination of [`BCDUMP_F_STRIP`] and
/// [`BCDUMP_F_DETERMINISTIC`] (as set by `string.dump` mode characters `s` and `d`); other bits
/// are ignored. `BCDUMP_F_FR2` is derived from the prototype like upstream.
pub fn write_flags(_vm: &mut Vm, pt: Gc<Proto>, flags: u32) -> Vec<u8> {
    let mut flags = flags & (BCDUMP_F_STRIP | BCDUMP_F_DETERMINISTIC);
    // Native (GC64/FR2) prototypes start with FUNCF/FUNCV; non-native ones with NOT.
    if pt.bc[0].op_byte() != Op::NOT as u8 {
        flags |= BCDUMP_F_FR2;
    }
    let mut ctx = Ctx {
        out: Vec::with_capacity(1024),
        body: Vec::new(),
        dbg: Vec::new(),
        flags,
    };
    ctx.header(&pt);
    ctx.proto(&pt);
    ctx.out.push(0); // Footer.
    ctx.out
}

/// Writer context (`BCWriteCtx`). `body` and `dbg` are scratch buffers reused per prototype.
struct Ctx {
    out: Vec<u8>,
    body: Vec<u8>,
    dbg: Vec<u8>,
    flags: u32,
}

impl Ctx {
    /// `bcwrite_header`.
    fn header(&mut self, pt: &Proto) {
        let pflags = pt.flags();
        let name = pt.chunkname.as_bytes();
        let out = &mut self.out;
        out.extend_from_slice(&[BCDUMP_HEAD1, BCDUMP_HEAD2, BCDUMP_HEAD3, BCDUMP_VERSION]);
        // Little-endian host: BCDUMP_F_BE is never set.
        let hflags = (self.flags & (BCDUMP_F_STRIP | BCDUMP_F_FR2))
            + if pflags & PROTO_FFI != 0 {
                BCDUMP_F_FFI
            } else {
                0
            }
            + if pflags & PROTO_BITOP != 0 {
                BCDUMP_F_BITOP
            } else {
                0
            };
        put_uleb128(out, hflags);
        if self.flags & BCDUMP_F_STRIP == 0 {
            put_uleb128(out, name.len() as u32);
            out.extend_from_slice(name);
        }
    }

    /// `bcwrite_proto`: children first (in increasing D order), then this prototype prefixed by
    /// its length.
    fn proto(&mut self, pt: &Proto) {
        if pt.flags() & PROTO_CHILD != 0 {
            for k in pt.kgc.iter() {
                if let KGc::Proto(child) = *k {
                    self.proto(&child);
                }
            }
        }

        let strip = self.flags & BCDUMP_F_STRIP != 0;
        self.dbg.clear();
        if !strip && !pt.lineinfo.is_empty() {
            debug_info(&mut self.dbg, pt);
        }

        let b = &mut self.body;
        b.clear();
        // Prototype header.
        b.push(pt.flags() & (PROTO_CHILD | PROTO_VARARG | PROTO_FFI | PROTO_BITOP));
        b.push(pt.numparams);
        b.push(pt.framesize);
        b.push(pt.uv.len() as u8);
        put_uleb128(b, pt.kgc.len() as u32);
        put_uleb128(b, pt.knum.len() as u32);
        put_uleb128(b, (pt.bc.len() - 1) as u32);
        if !strip {
            put_uleb128(b, self.dbg.len() as u32);
            if !self.dbg.is_empty() {
                put_uleb128(b, pt.firstline);
                put_uleb128(b, pt.numline);
            }
        }

        // Bytecode instructions (without the FUNCF/FUNCV header) and upvalue refs.
        b.reserve((pt.bc.len() - 1) * 4 + pt.uv.len() * 2);
        for ins in &pt.bc[1..] {
            b.extend_from_slice(&ins.0.to_le_bytes());
        }
        for &uv in pt.uv.iter() {
            b.extend_from_slice(&uv.to_le_bytes());
        }

        // Constants.
        let deterministic = self.flags & BCDUMP_F_DETERMINISTIC != 0;
        for k in pt.kgc.iter().rev() {
            kgc(b, k, deterministic);
        }
        for &n in pt.knum.iter() {
            knum(b, n);
        }

        // Debug info, if not stripped.
        b.extend_from_slice(&self.dbg);

        put_uleb128(&mut self.out, b.len() as u32);
        self.out.extend_from_slice(b);
    }
}

/// Encode the debug info block exactly as lj_parse lays it out (`fs_fixup_line`,
/// `fs_prep_var`): line deltas relative to `firstline` for `bc[1..]` (1, 2 or 4 bytes each),
/// zero-terminated upvalue names, then the zero-terminated varinfo stream.
fn debug_info(d: &mut Vec<u8>, pt: &Proto) {
    let first = pt.firstline;
    debug_assert_eq!(
        pt.lineinfo.len(),
        pt.bc.len(),
        "line info must cover every instruction"
    );
    let lines = &pt.lineinfo[1..pt.bc.len()];
    match lineinfo_shift(pt.numline) {
        0 => d.extend(lines.iter().map(|&l| l.wrapping_sub(first) as u8)),
        1 => {
            for &l in lines {
                d.extend_from_slice(&(l.wrapping_sub(first) as u16).to_le_bytes());
            }
        }
        _ => {
            for &l in lines {
                d.extend_from_slice(&l.wrapping_sub(first).to_le_bytes());
            }
        }
    }
    for name in pt.uvnames.iter() {
        d.extend_from_slice(name.as_bytes());
        d.push(0);
    }
    let mut lastpc = 0u32;
    for v in pt.varinfo.iter() {
        match v.name {
            VarName::Internal(id) => d.push(id),
            VarName::Str(s) => {
                d.extend_from_slice(s.as_bytes());
                d.push(0);
            }
        }
        put_uleb128(d, v.startpc.wrapping_sub(lastpc));
        put_uleb128(d, v.endpc.wrapping_sub(v.startpc));
        lastpc = v.startpc;
    }
    d.push(0); // Terminator for varinfo.
}

/// `bcwrite_kgc` for a single constant.
fn kgc(b: &mut Vec<u8>, k: &KGc, deterministic: bool) {
    match *k {
        KGc::Str(s) => {
            put_uleb128(b, BCDUMP_KGC_STR + s.len() as u32);
            b.extend_from_slice(s.as_bytes());
        }
        KGc::Proto(_) => put_uleb128(b, BCDUMP_KGC_CHILD),
        KGc::Table(t) => {
            put_uleb128(b, BCDUMP_KGC_TAB);
            ktab(b, &t, deterministic);
        }
        KGc::CData(cd) => {
            let (tp, n) = match cd.ctypeid() {
                CTID_INT64 => (BCDUMP_KGC_I64, 8),
                CTID_UINT64 => (BCDUMP_KGC_U64, 8),
                id => {
                    assert_eq!(id, CTID_COMPLEX_DOUBLE, "bad cdata constant CTID {id}");
                    (BCDUMP_KGC_COMPLEX, 16)
                }
            };
            assert!(cd.size() >= n, "cdata constant payload too small");
            let mut q = [0u8; 16];
            // SAFETY: the payload holds at least `n` bytes (checked above) and `q` has room
            // for 16; the regions belong to different allocations.
            unsafe { std::ptr::copy_nonoverlapping(cd.ptr(), q.as_mut_ptr(), n) };
            put_uleb128(b, tp);
            for &w in q[..n].as_chunks::<4>().0 {
                put_uleb128(b, u32::from_le_bytes(w));
            }
        }
    }
}

/// `bcwrite_knum`: integers as a 33 bit ULEB128 with lsb 0, doubles as the low word in a 33
/// bit ULEB128 with lsb 1 followed by the high word.
fn knum(b: &mut Vec<u8>, n: f64) {
    let bits = n.to_bits();
    let (lo, hi) = (bits as u32, (bits >> 32) as u32);
    if hi != LJ_KEYINDEX {
        // Narrow number constants to integers (-0 is never a constant).
        if let Some(k) = num2int_check(n) {
            let u = k as u32;
            put_uleb128(b, u.wrapping_mul(2) | (u & 0x8000_0000));
            if k < 0 {
                let last = b.last_mut().expect("ULEB128 written");
                *last = (*last & 7) | ((k >> 27) & 0x18) as u8;
            }
            return;
        }
    }
    put_uleb128(b, 1 + (lo.wrapping_mul(2) | (lo & 0x8000_0000)));
    if lo >= 0x8000_0000 {
        let last = b.last_mut().expect("ULEB128 written");
        *last = (*last & 7) | ((lo >> 27) & 0x18) as u8;
    }
    put_uleb128(b, hi);
}

/// `bcwrite_ktabk`: a single key or value of a template table. Values (`narrow`) that are
/// exact int32 numbers are written as integers.
fn ktabk(b: &mut Vec<u8>, o: Value, narrow: bool) {
    match o {
        Value::Str(s) => {
            put_uleb128(b, BCDUMP_KTAB_STR + s.len() as u32);
            b.extend_from_slice(s.as_bytes());
        }
        Value::Num(n) => {
            if narrow && let Some(k) = num2int_check(n) {
                b.push(BCDUMP_KTAB_INT as u8);
                put_uleb128(b, k as u32);
            } else {
                let bits = n.to_bits();
                b.push(BCDUMP_KTAB_NUM as u8);
                put_uleb128(b, bits as u32);
                put_uleb128(b, (bits >> 32) as u32);
            }
        }
        // The nil value marker (the template table itself) is written as nil.
        Value::Table(_) | Value::Nil => b.push(BCDUMP_KTAB_NIL as u8),
        Value::Bool(false) => b.push(BCDUMP_KTAB_FALSE as u8),
        Value::Bool(true) => b.push(BCDUMP_KTAB_TRUE as u8),
        other => unreachable!("bad template table constant {other:?}"),
    }
}

/// GC64 `itype()` of a template table key, which orders keys in deterministic dumps.
fn itype(o: Value) -> u32 {
    match o {
        Value::Num(n) => ((n.to_bits() as i64) >> 47) as u32,
        Value::Bool(false) => !1,
        Value::Bool(true) => !2,
        Value::Str(_) => !4,
        other => unreachable!("bad template table key {other:?}"),
    }
}

/// `bcwrite_ktabk_lt`.
fn ktabk_cmp(a: Value, b: Value) -> Ordering {
    let (at, bt) = (itype(a), itype(b));
    if at != bt {
        // This also handles false and true keys.
        return at.cmp(&bt);
    }
    match (a, b) {
        (Value::Str(x), Value::Str(y)) => x.as_bytes().cmp(y.as_bytes()),
        (Value::Num(x), Value::Num(y)) => x.to_bits().cmp(&y.to_bits()),
        _ => Ordering::Equal,
    }
}

/// `bcwrite_ktab`: array part up to the last non-nil slot, then the used hash slots — from the
/// highest node down, or sorted by key for deterministic dumps.
fn ktab(b: &mut Vec<u8>, t: &Table, deterministic: bool) {
    let asize = t.asize();
    let narray = (0..asize)
        .rev()
        .find(|&i| !t.array_get(i).is_nil())
        .map_or(0, |i| i + 1);
    let hmask = t.hmask();
    let nhash = if hmask > 0 {
        (0..=hmask).filter(|&i| !t.node_get(i).1.is_nil()).count() as u32
    } else {
        0
    };
    put_uleb128(b, narray);
    put_uleb128(b, nhash);
    for i in 0..narray {
        ktabk(b, t.array_get(i), true);
    }
    if nhash == 0 {
        return;
    }
    if deterministic && nhash > 1 {
        // Upstream drains a binary heap; keys are distinct, so this is plain sorted order.
        let mut nodes: Vec<(Value, Value)> = (0..=hmask)
            .rev()
            .map(|i| t.node_get(i))
            .filter(|n| !n.1.is_nil())
            .collect();
        nodes.sort_unstable_by(|x, y| ktabk_cmp(x.0, y.0));
        for (k, v) in nodes {
            ktabk(b, k, false);
            ktabk(b, v, true);
        }
    } else {
        for i in (0..=hmask).rev() {
            let (k, v) = t.node_get(i);
            if !v.is_nil() {
                ktabk(b, k, false);
                ktabk(b, v, true);
            }
        }
    }
}
