//! Compares every field of the prototypes produced by our parser (constants, template tables,
//! upvalue descriptors, line info, upvalue names, variable info, flags, frame sizes) against the
//! reference parser, decoded from deterministic `luajit -bgd -t raw` dumps of the fixtures
//! (tests/fixtures/parse/*.lua.raw, written by gen.sh).

use std::fs;
use std::path::{Path, PathBuf};

use luajit::cdata::{CTID_COMPLEX_DOUBLE, CTID_INT64, CTID_UINT64};
use luajit::func::{KGc, PROTO_BITOP, PROTO_CHILD, PROTO_FFI, PROTO_VARARG, Proto, VarName};
use luajit::gc::Gc;
use luajit::parse;
use luajit::state::Vm;
use luajit::table::Table;
use luajit::value::{Value, num2int_check};

/// A template table key or value in comparable form.
#[derive(Clone, Debug, PartialEq, PartialOrd)]
enum K {
    Nil,
    False,
    True,
    Num(u64),
    Str(Vec<u8>),
}

#[derive(Debug, PartialEq)]
enum Kgc {
    Child,
    Tab { array: Vec<K>, hash: Vec<(K, K)> },
    I64(u64),
    U64(u64),
    Complex(u64, u64),
    Str(Vec<u8>),
}

#[derive(Debug, PartialEq)]
enum VName {
    Internal(u8),
    Str(Vec<u8>),
}

#[derive(Debug, PartialEq)]
struct DProto {
    flags: u8,
    numparams: u8,
    framesize: u8,
    bc: Vec<u32>,
    uv: Vec<u16>,
    /// In D order (index 0 = D 0).
    kgc: Vec<Kgc>,
    knum: Vec<u64>,
    firstline: u32,
    numline: u32,
    /// Absolute lines of bc[1..].
    lines: Vec<u32>,
    uvnames: Vec<Vec<u8>>,
    varinfo: Vec<(VName, u32, u32)>,
}

struct Rd<'a> {
    b: &'a [u8],
    p: usize,
}

impl Rd<'_> {
    fn u8(&mut self) -> u8 {
        let v = self.b[self.p];
        self.p += 1;
        v
    }
    fn uleb(&mut self) -> u32 {
        let mut v = 0u32;
        let mut sh = 0;
        loop {
            let b = self.u8();
            v |= ((b & 0x7f) as u32) << sh;
            if b < 0x80 {
                return v;
            }
            sh += 7;
        }
    }
    fn bytes(&mut self, n: usize) -> &[u8] {
        let s = &self.b[self.p..self.p + n];
        self.p += n;
        s
    }
    fn cstr(&mut self) -> Vec<u8> {
        let n = self.b[self.p..].iter().position(|&c| c == 0).unwrap();
        let s = self.bytes(n).to_vec();
        self.p += 1;
        s
    }
    /// 33-bit ULEB128 of `lj_bcread`'s `bcread_uleb128_33`.
    fn uleb33(&mut self) -> (u32, bool) {
        let first = self.u8();
        let isnum = first & 1 != 0;
        let mut v = (first >> 1) as u32;
        if v >= 0x40 {
            v &= 0x3f;
            let mut sh = -1i32;
            loop {
                sh += 7;
                let b = self.u8();
                v |= ((b & 0x7f) as u32) << sh;
                if b < 0x80 {
                    break;
                }
            }
        }
        (v, isnum)
    }
}

fn ktabk(r: &mut Rd) -> K {
    match r.uleb() {
        0 => K::Nil,
        1 => K::False,
        2 => K::True,
        3 => K::Num((r.uleb() as i32 as f64).to_bits()),
        4 => {
            let lo = r.uleb() as u64;
            let hi = r.uleb() as u64;
            K::Num(hi << 32 | lo)
        }
        n => K::Str(r.bytes((n - 5) as usize).to_vec()),
    }
}

fn decode(dump: &[u8]) -> Vec<DProto> {
    let mut r = Rd { b: dump, p: 0 };
    assert_eq!(r.bytes(4), b"\x1bLJ\x02");
    let flags = r.uleb();
    assert_eq!(flags & 2, 0, "stripped dump");
    let n = r.uleb() as usize;
    r.bytes(n);
    let mut out = Vec::new();
    loop {
        let len = r.uleb();
        if len == 0 {
            break;
        }
        let pflags = r.u8();
        let numparams = r.u8();
        let framesize = r.u8();
        let sizeuv = r.u8() as usize;
        let sizekgc = r.uleb() as usize;
        let sizekn = r.uleb() as usize;
        let sizebc = r.uleb() as usize;
        let sizedbg = r.uleb();
        let (firstline, numline) = if sizedbg != 0 {
            (r.uleb(), r.uleb())
        } else {
            (0, 0)
        };
        let bc: Vec<u32> = (0..sizebc)
            .map(|_| u32::from_le_bytes(r.bytes(4).try_into().unwrap()))
            .collect();
        let uv: Vec<u16> = (0..sizeuv)
            .map(|_| u16::from_le_bytes(r.bytes(2).try_into().unwrap()))
            .collect();
        let mut kgc = Vec::new();
        for _ in 0..sizekgc {
            let tp = r.uleb();
            kgc.push(match tp {
                0 => Kgc::Child,
                1 => {
                    let narray = r.uleb();
                    let nhash = r.uleb();
                    let array = (0..narray).map(|_| ktabk(&mut r)).collect();
                    let mut hash: Vec<(K, K)> =
                        (0..nhash).map(|_| (ktabk(&mut r), ktabk(&mut r))).collect();
                    hash.sort_by(|a, b| a.partial_cmp(b).unwrap());
                    Kgc::Tab { array, hash }
                }
                2 | 3 => {
                    let lo = r.uleb() as u64;
                    let hi = r.uleb() as u64;
                    if tp == 2 {
                        Kgc::I64(hi << 32 | lo)
                    } else {
                        Kgc::U64(hi << 32 | lo)
                    }
                }
                4 => {
                    let a = r.uleb() as u64;
                    let b = r.uleb() as u64;
                    let c = r.uleb() as u64;
                    let d = r.uleb() as u64;
                    Kgc::Complex(b << 32 | a, d << 32 | c)
                }
                n => Kgc::Str(r.bytes((n - 5) as usize).to_vec()),
            });
        }
        kgc.reverse(); // Written in memory order: highest D first.
        let knum = (0..sizekn)
            .map(|_| {
                let (lo, isnum) = r.uleb33();
                if isnum {
                    let hi = r.uleb() as u64;
                    hi << 32 | lo as u64
                } else {
                    (lo as i32 as f64).to_bits()
                }
            })
            .collect();
        let mut lines = Vec::new();
        let mut uvnames = Vec::new();
        let mut varinfo = Vec::new();
        if sizedbg != 0 {
            let end = r.p + sizedbg as usize;
            for _ in 0..sizebc {
                let d = if numline < 256 {
                    r.u8() as u32
                } else if numline < 65536 {
                    u16::from_le_bytes(r.bytes(2).try_into().unwrap()) as u32
                } else {
                    u32::from_le_bytes(r.bytes(4).try_into().unwrap())
                };
                lines.push(firstline + d);
            }
            for _ in 0..sizeuv {
                uvnames.push(r.cstr());
            }
            let mut lastpc = 0;
            loop {
                let c = r.b[r.p];
                let name = if c < 7 {
                    r.p += 1;
                    if c == 0 {
                        break;
                    }
                    VName::Internal(c)
                } else {
                    VName::Str(r.cstr())
                };
                let startpc = lastpc + r.uleb();
                let endpc = startpc + r.uleb();
                lastpc = startpc;
                varinfo.push((name, startpc, endpc));
            }
            assert_eq!(r.p, end, "debug info size");
        }
        out.push(DProto {
            flags: pflags,
            numparams,
            framesize,
            bc,
            uv,
            kgc,
            knum,
            firstline,
            numline,
            lines,
            uvnames,
            varinfo,
        });
    }
    out
}

fn kval(v: Value, t: Gc<Table>, narrow: bool) -> K {
    match v {
        Value::Nil => K::Nil,
        Value::Bool(false) => K::False,
        Value::Bool(true) => K::True,
        Value::Num(n) => match num2int_check(n) {
            Some(k) if narrow => K::Num((k as f64).to_bits()),
            _ => K::Num(n.to_bits()),
        },
        Value::Str(s) => K::Str(s.as_bytes().to_vec()),
        Value::Table(x) if x == t => K::Nil, // Nil value marker.
        other => panic!("unexpected template table entry {other:?}"),
    }
}

fn ours_tab(t: Gc<Table>) -> Kgc {
    let mut narray = 0;
    for i in (0..t.asize()).rev() {
        if !t.array_get(i).is_nil() {
            narray = i + 1;
            break;
        }
    }
    let array = (0..narray).map(|i| kval(t.array_get(i), t, true)).collect();
    let mut hash: Vec<(K, K)> = (0..t.node_count())
        .map(|i| t.node_get(i))
        .filter(|(_, v)| !v.is_nil())
        .map(|(k, v)| (kval(k, t, false), kval(v, t, true)))
        .collect();
    hash.sort_by(|a, b| a.partial_cmp(b).unwrap());
    Kgc::Tab { array, hash }
}

/// Our prototypes in dump order (children first, in D order, then the parent).
fn flatten(pt: Gc<Proto>, out: &mut Vec<DProto>) {
    if pt.flags() & PROTO_CHILD != 0 {
        for k in pt.kgc.iter() {
            if let KGc::Proto(c) = *k {
                flatten(c, out);
            }
        }
    }
    let kgc = pt
        .kgc
        .iter()
        .map(|k| match *k {
            KGc::Str(s) => Kgc::Str(s.as_bytes().to_vec()),
            KGc::Proto(_) => Kgc::Child,
            KGc::Table(t) => ours_tab(t),
            KGc::CData(cd) => {
                let p = cd.ptr() as *const u64;
                // SAFETY: parser cdata constants own 8 (int64) or 16 (complex) bytes.
                unsafe {
                    match cd.ctypeid() {
                        CTID_INT64 => Kgc::I64(*p),
                        CTID_UINT64 => Kgc::U64(*p),
                        CTID_COMPLEX_DOUBLE => Kgc::Complex(*p, *p.add(1)),
                        id => panic!("bad cdata ctype {id}"),
                    }
                }
            }
        })
        .collect();
    let knum = pt
        .knum
        .iter()
        .map(|&n| match num2int_check(n) {
            Some(k) => (k as f64).to_bits(),
            None => n.to_bits(),
        })
        .collect();
    out.push(DProto {
        flags: pt.flags() & (PROTO_CHILD | PROTO_VARARG | PROTO_FFI | PROTO_BITOP),
        numparams: pt.numparams,
        framesize: pt.framesize,
        bc: pt.bc[1..].iter().map(|i| i.0).collect(),
        uv: pt.uv.to_vec(),
        kgc,
        knum,
        firstline: pt.firstline,
        numline: pt.numline,
        lines: pt.lineinfo[1..].to_vec(),
        uvnames: pt.uvnames.iter().map(|s| s.as_bytes().to_vec()).collect(),
        varinfo: pt
            .varinfo
            .iter()
            .map(|v| {
                let n = match v.name {
                    VarName::Internal(id) => VName::Internal(id),
                    VarName::Str(s) => VName::Str(s.as_bytes().to_vec()),
                };
                (n, v.startpc, v.endpc)
            })
            .collect(),
    });
}

fn check(path: &Path) -> Result<(), String> {
    let name = path.file_name().unwrap().to_str().unwrap();
    let src = fs::read(path).unwrap();
    let raw =
        fs::read(path.with_extension("lua.raw")).map_err(|e| format!("{name}: no dump: {e}"))?;
    let want = decode(&raw);
    let mut vm = Vm::new();
    let pt = parse::parse(&mut vm, &src, format!("@{name}").as_bytes())
        .map_err(|_| format!("{name}: parse failed"))?;
    let mut got = Vec::new();
    flatten(pt, &mut got);
    if want.len() != got.len() {
        return Err(format!(
            "{name}: {} prototypes, want {}",
            got.len(),
            want.len()
        ));
    }
    for (i, (w, g)) in want.iter().zip(&got).enumerate() {
        if w != g {
            let field = if w.bc != g.bc {
                "bc"
            } else if w.kgc != g.kgc {
                "kgc"
            } else if w.knum != g.knum {
                "knum"
            } else if w.uv != g.uv {
                "uv"
            } else if w.lines != g.lines {
                "lineinfo"
            } else if w.varinfo != g.varinfo {
                "varinfo"
            } else if w.uvnames != g.uvnames {
                "uvnames"
            } else {
                "header"
            };
            return Err(format!(
                "{name}: prototype #{i} differs in {field}:\n  want: {w:?}\n  got:  {g:?}"
            ));
        }
    }
    Ok(())
}

#[test]
fn prototypes_match_reference_dumps() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/parse");
    let mut files: Vec<PathBuf> = fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "lua"))
        .collect();
    files.sort();
    assert!(!files.is_empty());
    let failures: Vec<String> = files.iter().filter_map(|p| check(p).err()).collect();
    if !failures.is_empty() {
        let shown: Vec<String> = failures
            .iter()
            .take(10)
            .map(|s| s.chars().take(2000).collect())
            .collect();
        panic!(
            "{} of {} fixtures differ:\n{}",
            failures.len(),
            files.len(),
            shown.join("\n")
        );
    }
}
