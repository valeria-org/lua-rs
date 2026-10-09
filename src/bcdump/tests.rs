//! Unit tests for cases the reference golden files can't pin down: deterministic behaviour where
//! upstream reads uninitialized memory, deliberate deviations (opcode and debug info
//! validation), the non-native (`X` mode) flavour, big-endian input and encoding edge cases.

use std::cell::Cell;

use super::*;
use crate::bc::{BC_MAX, Ins, Op};
use crate::error::{ErrorKind, LuaResult};
use crate::func::{PROTO_VARARG, Proto, VARNAME_FOR_IDX, VarInfo, VarName};
use crate::gc::Gc;
use crate::state::Vm;
use crate::table::MAX_ASIZE;

const BCBAD: &str = "(binary): cannot load malformed bytecode";
const BCFMT: &str = "(binary): cannot load incompatible bytecode";
const STRIP_FR2: u32 = BCDUMP_F_STRIP | BCDUMP_F_FR2;

fn ret0() -> u32 {
    Ins::ad(Op::RET0, 0, 1).0
}

/// A hand-assembled prototype.
#[derive(Default)]
struct P {
    flags: u8,
    numparams: u8,
    framesize: u8,
    uv: Vec<u16>,
    nkgc: u32,
    nkn: u32,
    bc: Vec<u32>,
    /// Raw constant data (kgc then knum).
    k: Vec<u8>,
    /// Debug info for non-stripped dumps: (firstline, numline, block).
    dbg: Option<(u32, u32, Vec<u8>)>,
    big_endian: bool,
}

impl P {
    fn bytes(&self, strip: bool) -> Vec<u8> {
        let mut b = vec![self.flags, self.numparams, self.framesize.max(1), self.uv.len() as u8];
        put_uleb128(&mut b, self.nkgc);
        put_uleb128(&mut b, self.nkn);
        let bc = if self.bc.is_empty() { vec![ret0()] } else { self.bc.clone() };
        put_uleb128(&mut b, bc.len() as u32);
        if !strip {
            match &self.dbg {
                Some((first, num, block)) => {
                    put_uleb128(&mut b, block.len() as u32);
                    put_uleb128(&mut b, *first);
                    put_uleb128(&mut b, *num);
                }
                None => put_uleb128(&mut b, 0),
            }
        }
        for w in bc {
            b.extend_from_slice(&if self.big_endian { w.to_be_bytes() } else { w.to_le_bytes() });
        }
        for &u in &self.uv {
            b.extend_from_slice(&if self.big_endian { u.to_be_bytes() } else { u.to_le_bytes() });
        }
        b.extend_from_slice(&self.k);
        if !strip && let Some((_, _, block)) = &self.dbg {
            b.extend_from_slice(block);
        }
        let mut out = Vec::new();
        put_uleb128(&mut out, b.len() as u32);
        out.extend_from_slice(&b);
        out
    }
}

fn dump(flags: u32, name: &[u8], protos: &[&P]) -> Vec<u8> {
    let mut d = vec![BCDUMP_HEAD1, BCDUMP_HEAD2, BCDUMP_HEAD3, BCDUMP_VERSION];
    put_uleb128(&mut d, flags);
    let strip = flags & BCDUMP_F_STRIP != 0;
    if !strip {
        put_uleb128(&mut d, name.len() as u32);
        d.extend_from_slice(name);
    }
    for p in protos {
        d.extend_from_slice(&p.bytes(strip));
    }
    d.push(0);
    d
}

fn load(vm: &mut Vm, d: &[u8]) -> LuaResult<Gc<Proto>> {
    read(vm, d, d)
}

fn err(r: LuaResult<Gc<Proto>>) -> (ErrorKind, String) {
    match r {
        Ok(_) => panic!("load succeeded"),
        Err(e) => (e.kind, e.value.as_str().expect("string message").to_string_lossy()),
    }
}

fn assert_bad(vm: &mut Vm, d: &[u8]) {
    assert_eq!(err(load(vm, d)), (ErrorKind::Syntax, BCBAD.to_string()), "input {d:02x?}");
}

#[test]
fn header_cut_after_flags() {
    // Upstream reads the missing bytes from an uninitialized buffer (and may crash); here they
    // read as zero: an empty chunk name, then the missing prototype is reported.
    let mut vm = Vm::new();
    assert_bad(&mut vm, b"\x1bLJ\x02\x08");
    assert_bad(&mut vm, b"\x1bLJ\x02\x0a");
    assert_bad(&mut vm, b"\x1bLJ\x02\x88");
    assert_eq!(err(load(&mut vm, b"\x1bLJ\x02")).1, BCFMT);
    assert_eq!(err(load(&mut vm, b"")).1, ": cannot load incompatible bytecode");
    assert_eq!(err(read(&mut vm, b"LJ", b"=x")).1, "x: cannot load incompatible bytecode");
}

#[test]
fn minimal_dump_round_trips() {
    let mut vm = Vm::new();
    let d = dump(STRIP_FR2, b"", &[&P::default()]);
    let pt = load(&mut vm, &d).expect("load");
    assert_eq!(pt.bc.len(), 2);
    assert_eq!(pt.bc[0], Ins::ad(Op::FUNCF, 1, 0));
    assert_eq!(pt.chunkname.as_bytes(), b"=?");
    assert_eq!((pt.firstline, pt.numline), (0, 0));
    assert!(pt.lineinfo.is_empty() && pt.uvnames.is_empty() && pt.varinfo.is_empty());
    assert_eq!(write(&mut vm, pt, true), d);
    // Unstripped dump of a stripped prototype: chunk name and zero debug sizes.
    let d2 = dump(BCDUMP_F_FR2, b"=?", &[&P::default()]);
    assert_eq!(write(&mut vm, pt, false), d2);
    let pt2 = load(&mut vm, &d2).expect("load");
    assert!(pt2.lineinfo.is_empty());
    assert_eq!(write(&mut vm, pt2, false), d2);
}

#[test]
fn invalid_opcode_is_rejected() {
    // Deviation: upstream loads any instruction word; `Ins::op` requires a valid opcode.
    let mut vm = Vm::new();
    let p = P { bc: vec![BC_MAX as u32, ret0()], ..P::default() };
    assert_bad(&mut vm, &dump(STRIP_FR2, b"", &[&p]));
    let p = P { bc: vec![Op::FUNCCW as u32, ret0()], ..P::default() };
    assert!(load(&mut vm, &dump(STRIP_FR2, b"", &[&p])).is_ok());
}

#[test]
fn varinfo_and_lines_decode() {
    let mut vm = Vm::new();
    let mut block = vec![0u8, 3, 2]; // Line deltas for 3 instructions (1 byte each).
    block.extend_from_slice(b"up\0_ENV\0");
    block.push(VARNAME_FOR_IDX);
    put_uleb128(&mut block, 1); // startpc 1
    put_uleb128(&mut block, 2); // endpc 3
    block.extend_from_slice(b"x\0");
    put_uleb128(&mut block, 0); // startpc 1
    put_uleb128(&mut block, 300); // endpc 301
    block.extend_from_slice(b"y\0");
    put_uleb128(&mut block, u32::MAX); // startpc 0 (wraps)
    put_uleb128(&mut block, 1); // endpc 1
    block.push(0);
    let p = P {
        framesize: 2,
        uv: vec![0x8000, 0xc001],
        bc: vec![Ins::ad(Op::KSHORT, 0, 1).0, Ins::ad(Op::KSHORT, 1, 2).0, ret0()],
        dbg: Some((10, 5, block)),
        ..P::default()
    };
    let d = dump(BCDUMP_F_FR2, b"@t.lua", &[&p]);
    let pt = load(&mut vm, &d).expect("load");
    assert_eq!(pt.chunkname.as_bytes(), b"@t.lua");
    assert_eq!(&*pt.lineinfo, &[10, 10, 13, 12]);
    assert_eq!(&*pt.uv, &[0x8000, 0xc001]);
    let names: Vec<&[u8]> = pt.uvnames.iter().map(|s| s.as_bytes()).collect();
    assert_eq!(names, [&b"up"[..], b"_ENV"]);
    // Strings are interned, so freshly created names compare equal by identity.
    let (x, y) = (vm.new_str(b"x"), vm.new_str(b"y"));
    let want = [
        VarInfo { name: VarName::Internal(VARNAME_FOR_IDX), startpc: 1, endpc: 3 },
        VarInfo { name: VarName::Str(x), startpc: 1, endpc: 301 },
        VarInfo { name: VarName::Str(y), startpc: 0, endpc: 1 },
    ];
    assert_eq!(&*pt.varinfo, &want);
    assert_eq!(write(&mut vm, pt, false), d);
}

#[test]
fn malformed_debug_info_is_rejected() {
    // Deviation: upstream copies the block verbatim and only decodes it when debug info is
    // queried; a block that doesn't decode can't be represented and is rejected.
    let mut vm = Vm::new();
    let bad_blocks: [&[u8]; 6] = [
        b"\0",           // shorter than the line info
        b"\0\0up",       // unterminated upvalue name
        b"\0\0up\0",     // missing varinfo terminator
        b"\0\0up\0x\0",  // varinfo entry without pc range
        b"\0\0up\0\0\0", // trailing byte after the terminator
        b"\0\0up\0x",    // unterminated variable name
    ];
    for block in bad_blocks {
        let p = P { uv: vec![0x8000], bc: vec![ret0(), ret0()], dbg: Some((1, 1, block.to_vec())), ..P::default() };
        assert_bad(&mut vm, &dump(BCDUMP_F_FR2, b"=x", &[&p]));
    }
}

#[test]
fn wide_line_info_and_big_endian_input() {
    let mut vm = Vm::new();
    for (numline, width) in [(255u32, 1usize), (256, 2), (65535, 2), (65536, 4), (1 << 20, 4)] {
        for big_endian in [false, true] {
            let deltas = [0u32, numline / 2, numline];
            let mut block = Vec::new();
            for &dl in &deltas {
                let bytes = if big_endian { dl.to_be_bytes() } else { dl.to_le_bytes() };
                match width {
                    1 => block.push(dl as u8),
                    2 => block.extend_from_slice(if big_endian { &bytes[2..] } else { &bytes[..2] }),
                    _ => block.extend_from_slice(&bytes),
                }
            }
            block.push(0);
            let p = P {
                framesize: 2,
                bc: vec![Ins::ad(Op::KSHORT, 0, 1).0, Ins::ad(Op::KSHORT, 1, 2).0, ret0()],
                dbg: Some((7, numline, block.clone())),
                big_endian,
                ..P::default()
            };
            let flags = BCDUMP_F_FR2 | if big_endian { BCDUMP_F_BE } else { 0 };
            let pt = load(&mut vm, &dump(flags, b"=w", &[&p])).expect("load");
            assert_eq!(&*pt.lineinfo, &[7, 7, 7 + numline / 2, 7 + numline], "numline {numline}");
            assert_eq!(pt.bc[1], Ins::ad(Op::KSHORT, 0, 1));
            // Written back in host (little-endian) order without the BE flag.
            let le = P {
                big_endian: false,
                dbg: Some((7, numline, {
                    let mut b = Vec::new();
                    for &dl in &deltas {
                        b.extend_from_slice(&dl.to_le_bytes()[..width]);
                    }
                    b.push(0);
                    b
                })),
                ..p
            };
            assert_eq!(write(&mut vm, pt, false), dump(BCDUMP_F_FR2, b"=w", &[&le]));
        }
    }
}

#[test]
fn non_native_flavour() {
    let mut vm = Vm::new();
    let p = P { flags: PROTO_VARARG, framesize: 3, ..P::default() };
    let d = dump(BCDUMP_F_STRIP, b"", &[&p]);
    assert_eq!(err(load(&mut vm, &d)).1, BCFMT);
    let pt = read_ex(&mut vm, &d, &d, false).expect("load non-native");
    assert_eq!(pt.bc[0], Ins::ad(Op::NOT, 3, 0));
    assert_eq!(write(&mut vm, pt, true), d);
    let native = dump(STRIP_FR2, b"", &[&p]);
    assert_eq!(err(read_ex(&mut vm, &native, &native, false)).1, BCFMT);
    let pt = load(&mut vm, &native).expect("load native");
    assert_eq!(pt.bc[0], Ins::ad(Op::FUNCV, 3, 0));
}

#[test]
fn oversized_counts_fail_without_huge_allocations() {
    let mut vm = Vm::new();
    let mut k = Vec::new();
    put_uleb128(&mut k, BCDUMP_KGC_STR + 0x7fff_feff); // longest string, missing data
    assert_bad(&mut vm, &dump(STRIP_FR2, b"", &[&P { nkgc: 1, k, ..P::default() }]));
    let mut k = vec![BCDUMP_KGC_TAB as u8];
    put_uleb128(&mut k, MAX_ASIZE); // largest array part, missing entries
    k.push(0);
    assert_bad(&mut vm, &dump(STRIP_FR2, b"", &[&P { nkgc: 1, k, ..P::default() }]));
    let mut k = vec![BCDUMP_KGC_TAB as u8, 0];
    put_uleb128(&mut k, 1 << 26); // largest hash part, missing entries
    let d = dump(STRIP_FR2, b"", &[&P { nkgc: 1, k, ..P::default() }]);
    // The first key is read from the dump's 0 terminator, as upstream does.
    assert_eq!(err(load(&mut vm, &d)), (ErrorKind::Run, "table index is nil".to_string()));
    let mut k = vec![BCDUMP_KGC_TAB as u8, 0];
    put_uleb128(&mut k, 1 << 26);
    k.push(BCDUMP_KTAB_TRUE as u8); // first key, then the value from the terminator
    assert_bad(&mut vm, &dump(STRIP_FR2, b"", &[&P { nkgc: 1, k, ..P::default() }]));
    assert_bad(&mut vm, &dump(STRIP_FR2, b"", &[&P { nkgc: u32::MAX, ..P::default() }]));
    assert_bad(&mut vm, &dump(STRIP_FR2, b"", &[&P { nkn: u32::MAX, ..P::default() }]));
    // Layout: ESC L J version flags | length | flags numparams framesize numuv nkgc nkn nbc | ...
    let mut d = dump(STRIP_FR2, b"", &[&P::default()]);
    assert_eq!(d[12], 1);
    d.splice(12..13, [0xff, 0xff, 0xff, 0xff, 0x0f]); // 2^32 - 1 instructions
    d[5] += 4;
    assert_bad(&mut vm, &d);
}

#[test]
fn constant_errors_are_runtime_errors() {
    let mut vm = Vm::new();
    let mut k = Vec::new();
    put_uleb128(&mut k, BCDUMP_KGC_STR + crate::lstr::MAX_STR as u32);
    let e = err(load(&mut vm, &dump(STRIP_FR2, b"", &[&P { nkgc: 1, k, ..P::default() }])));
    assert_eq!(e, (ErrorKind::Run, "string length overflow".to_string()));
    let k = vec![BCDUMP_KGC_TAB as u8, 0, 1, BCDUMP_KTAB_NIL as u8, BCDUMP_KTAB_TRUE as u8];
    let e = err(load(&mut vm, &dump(STRIP_FR2, b"", &[&P { nkgc: 1, k, ..P::default() }])));
    assert_eq!(e, (ErrorKind::Run, "table index is nil".to_string()));
}

/// Encode a single knum the way the writer does and decode it with the reader.
fn knum_round_trip(vm: &mut Vm, n: f64) -> (Vec<u8>, f64) {
    let empty = vm.empty_str;
    let pt = vm.new_proto(Proto {
        bc: vec![Ins::ad(Op::FUNCF, 1, 0), Ins(ret0())].into_boxed_slice(),
        kgc: Box::default(),
        knum: vec![n].into_boxed_slice(),
        uv: Box::default(),
        numparams: 0,
        framesize: 1,
        flags: Cell::new(0),
        chunkname: empty,
        firstline: 0,
        numline: 0,
        lineinfo: Box::default(),
        uvnames: Box::default(),
        varinfo: Box::default(),
    });
    let d = write(vm, pt, true);
    let back = load(vm, &d).expect("load");
    // Header (5) + length (1) + phead (7) + 1 instruction: the knum follows.
    (d[17..d.len() - 1].to_vec(), back.knum[0])
}

#[test]
fn knum_encoding() {
    let mut vm = Vm::new();
    // Integers: 33-bit ULEB128 of k << 1 with the top bits of negative numbers fixed up.
    assert_eq!(knum_round_trip(&mut vm, 0.0), (vec![0x00], 0.0));
    assert_eq!(knum_round_trip(&mut vm, 1.0), (vec![0x02], 1.0));
    assert_eq!(knum_round_trip(&mut vm, 63.0), (vec![0x7e], 63.0));
    assert_eq!(knum_round_trip(&mut vm, 64.0), (vec![0x80, 0x01], 64.0));
    assert_eq!(knum_round_trip(&mut vm, -1.0), (vec![0xfe, 0xff, 0xff, 0xff, 0x1f], -1.0));
    assert_eq!(knum_round_trip(&mut vm, -2147483648.0), (vec![0x80, 0x80, 0x80, 0x80, 0x10], -2147483648.0));
    assert_eq!(knum_round_trip(&mut vm, 2147483647.0), (vec![0xfe, 0xff, 0xff, 0xff, 0x0f], 2147483647.0));
    // -0 narrows to the integer 0 (the parser never emits it).
    let (b, v) = knum_round_trip(&mut vm, -0.0);
    assert_eq!((b, v.to_bits()), (vec![0x00], 0));
    // Doubles: low word as 33-bit ULEB128 with lsb 1, then the high word.
    let (b, v) = knum_round_trip(&mut vm, 0.5);
    assert_eq!(b, vec![0x01, 0x80, 0x80, 0x80, 0xff, 0x03]);
    assert_eq!(v, 0.5);
    for n in
        [0.1, -0.1, 2147483648.0, -2147483649.0, 1e300, f64::INFINITY, f64::NEG_INFINITY, 5e-324, 4503599627370497.0]
    {
        let (_, v) = knum_round_trip(&mut vm, n);
        assert_eq!(v.to_bits(), n.to_bits(), "{n}");
    }
    // Low words with the top bit set use the 5-byte form with fixed-up top bits.
    let n = f64::from_bits(0x3ff0_0000_8000_0001);
    let (b, v) = knum_round_trip(&mut vm, n);
    assert_eq!(&b[..5], &[0x83, 0x80, 0x80, 0x80, 0x10]);
    assert_eq!(v.to_bits(), n.to_bits());
    let nan = f64::from_bits(0xfff8_0000_ffff_ffff);
    assert_eq!(knum_round_trip(&mut vm, nan).1.to_bits(), nan.to_bits());
}

#[test]
fn uleb128_reader_masks_overlong_shifts() {
    // lj_buf_ruleb128 on x86: shift counts wrap at 32, so in 0x83 0x80 0x80 0x80 0x80 0x01 the
    // sixth byte lands at bit 35 & 31 = 3: 3 | 8 = 11, the length of the prototype.
    let mut vm = Vm::new();
    let mut d = dump(STRIP_FR2, b"", &[&P::default()]);
    assert_eq!(d[5], 11);
    d.splice(5..6, [0x83, 0x80, 0x80, 0x80, 0x80, 0x01]);
    let pt = load(&mut vm, &d).expect("overlong proto length");
    assert_eq!(pt.bc.len(), 2);
}

#[test]
fn read_proto_reads_libdef_lua_functions() {
    // lj_libdef.h (reference build), lj_lib_init_math: LIBINIT_LUA "deg" <proto> LIBINIT_LUA "rad".
    let lib: &[u8] = &[
        249, 3, 100, 101, 103, 0, 1, 2, 0, 0, 1, 2, 24, 1, 0, 0, 76, 1, 2, 0, 241, 135, 158, 166, 3, 220, 203, 178,
        130, 4, 249, 3, 114, 97, 100, 0, 1, 2, 0, 0, 1, 2, 24, 1, 0, 0, 76, 1, 2, 0, 243, 244, 148, 165, 20, 198, 190,
        199, 252, 3, 69,
    ];
    let mut vm = Vm::new();
    let mut p = 0;
    for (name, k) in [(&b"deg"[..], 57.29577951308232), (b"rad", 0.017453292519943295)] {
        assert_eq!(lib[p], 249);
        let len = lib[p + 1] as usize;
        assert_eq!(&lib[p + 2..p + 2 + len], name);
        let s = vm.new_str(name);
        let (pt, n) = read_proto(&mut vm, &lib[p + 2 + len..], s).expect("read_proto");
        assert_eq!(n, 25);
        assert_eq!(pt.chunkname.as_bytes(), name);
        assert_eq!((pt.numparams, pt.framesize), (1, 2));
        assert_eq!(&*pt.bc, &[Ins::ad(Op::FUNCF, 2, 0), Ins::abc(Op::MULVN, 1, 0, 0), Ins::ad(Op::RET1, 1, 2)]);
        assert_eq!(&*pt.knum, &[k]);
        assert!(pt.lineinfo.is_empty());
        p += 2 + len + n;
    }
    assert_eq!(lib[p], 69);
    let s = vm.new_str(b"bad");
    let (kind, msg) = match read_proto(&mut vm, &[0, 1, 2, 0, 0, 0, 1], s) {
        Ok(_) => panic!("truncated prototype loaded"),
        Err(e) => (e.kind, e.value.as_str().unwrap().to_string_lossy()),
    };
    assert_eq!((kind, msg.as_str()), (ErrorKind::Syntax, "bad: cannot load malformed bytecode"));
}
