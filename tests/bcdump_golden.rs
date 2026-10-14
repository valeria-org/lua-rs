//! Golden tests for the bytecode dump reader/writer: raw dumps, prototype descriptions and
//! `loadstring` results produced by the reference binary (tests/fixtures/bcdump/gen.sh).

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use luajit::bcdump::{self, BCDUMP_F_DETERMINISTIC, BCDUMP_F_STRIP};
use luajit::cdata::{CTID_COMPLEX_DOUBLE, CTID_INT64, CTID_UINT64};
use luajit::func::{KGc, PROTO_CHILD, PROTO_CLC_BITS, PROTO_CLCOUNT, PROTO_VARARG, Proto};
use luajit::gc::Gc;
use luajit::parse;
use luajit::state::Vm;
use luajit::table::Table;
use luajit::value::Value;

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/bcdump")
}

/// Names of the corpus sources (without `.lua`).
fn corpus() -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(fixture_dir().join("corpus"))
        .expect("corpus dir")
        .map(|e| e.expect("dir entry").path())
        .filter(|p| p.extension().is_some_and(|e| e == "lua"))
        .map(|p| p.file_stem().unwrap().to_string_lossy().into_owned())
        .collect();
    v.sort();
    assert!(v.len() >= 8, "corpus missing");
    v
}

fn golden(name: &str, variant: &str, ext: &str) -> Vec<u8> {
    let p = fixture_dir()
        .join("golden")
        .join(format!("{name}.{variant}.{ext}"));
    fs::read(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

/// `loadstring(dump)`: the dump itself is the chunk name argument.
fn load(vm: &mut Vm, dump: &[u8]) -> Gc<Proto> {
    bcdump::read(vm, dump, dump).unwrap_or_else(|e| panic!("read failed: {e}"))
}

fn hex(b: &[u8]) -> String {
    b.iter()
        .fold(String::with_capacity(b.len() * 2), |mut s, c| {
            let _ = write!(s, "{c:02x}");
            s
        })
}

fn unhex(h: &str) -> Vec<u8> {
    (0..h.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&h[i..i + 2], 16).expect("hex"))
        .collect()
}

fn first_diff(want: &[u8], got: &[u8]) -> String {
    match want.iter().zip(got).position(|(a, b)| a != b) {
        Some(i) => format!(
            "first difference at byte {i} (want {:02x}, got {:02x})",
            want[i], got[i]
        ),
        None => format!("length differs: want {}, got {}", want.len(), got.len()),
    }
}

fn assert_bytes(want: &[u8], got: &[u8], what: &str) {
    assert!(want == got, "{what}: {}", first_diff(want, got));
}

const VARIANTS: [(&str, &str, u32); 2] = [("s", "sd", BCDUMP_F_STRIP), ("g", "gd", 0)];

#[test]
fn roundtrip_deterministic_dumps() {
    let mut vm = Vm::new();
    for name in corpus() {
        for (_, det, flags) in VARIANTS {
            let d = golden(&name, det, "raw");
            let pt = load(&mut vm, &d);
            let out = bcdump::write_flags(&mut vm, pt, flags | BCDUMP_F_DETERMINISTIC);
            assert_bytes(&d, &out, &format!("{name}.{det}"));
        }
    }
}

#[test]
fn roundtrip_plain_dumps() {
    let mut vm = Vm::new();
    for name in corpus() {
        for (plain, det, flags) in VARIANTS {
            let n = golden(&name, plain, "raw");
            let d = golden(&name, det, "raw");
            let pt = load(&mut vm, &n);
            // Sorting the template table hash parts reproduces the reference `-d` dump exactly.
            let out = bcdump::write_flags(&mut vm, pt, flags | BCDUMP_F_DETERMINISTIC);
            assert_bytes(
                &d,
                &out,
                &format!("{name}.{plain} written deterministically"),
            );
            let out = bcdump::write_flags(&mut vm, pt, flags);
            if n == d {
                assert_bytes(&n, &out, &format!("{name}.{plain}"));
            } else {
                // Hash part order depends on string ids and insertion history (upstream's own
                // load/dump round trip differs here too); everything else must be identical.
                assert_eq!(n.len(), out.len(), "{name}.{plain}: length");
                let again = load(&mut vm, &out);
                let out2 = bcdump::write_flags(&mut vm, again, flags | BCDUMP_F_DETERMINISTIC);
                assert_bytes(&d, &out2, &format!("{name}.{plain} re-read"));
            }
            let strip = flags & BCDUMP_F_STRIP != 0;
            assert_bytes(
                &out,
                &bcdump::write(&mut vm, pt, strip),
                &format!("{name}.{plain} write()"),
            );
        }
    }
}

#[test]
fn parsed_prototypes_dump_like_reference() {
    let mut vm = Vm::new();
    for name in corpus() {
        let src =
            fs::read(fixture_dir().join("corpus").join(format!("{name}.lua"))).expect("source");
        let chunkname = format!("@{name}.lua");
        let pt = parse::parse(&mut vm, &src, chunkname.as_bytes())
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        for (_, det, flags) in VARIANTS {
            let d = golden(&name, det, "raw");
            let out = bcdump::write_flags(&mut vm, pt, flags | BCDUMP_F_DETERMINISTIC);
            assert_bytes(&d, &out, &format!("parse({name}) dumped as {det}"));
        }
    }
}

/// Template table contents (layout may differ: trailing nil array slots aren't dumped).
fn same_table(a: Gc<Table>, b: Gc<Table>, what: &str) {
    let norm = |v: Value, t: Gc<Table>| match v {
        Value::Table(x) if x == t => None, // nil value marker
        v => Some(v),
    };
    let mut n = 0;
    let mut key = Value::Nil;
    while let Some((k, v)) = a.next(key).expect("next") {
        let w = b.get(k);
        assert!(!w.is_nil(), "{what}: key {k:?} missing");
        assert_eq!(norm(v, a), norm(w, b), "{what}: value of {k:?}");
        key = k;
        n += 1;
    }
    let mut m = 0;
    let mut key = Value::Nil;
    while let Some((k, _)) = b.next(key).expect("next") {
        key = k;
        m += 1;
    }
    assert_eq!(n, m, "{what}: entry count");
}

/// Everything the dump format carries must come back unchanged.
fn same_proto(a: &Proto, b: &Proto, debug: bool, what: &str) {
    assert_eq!(a.bc, b.bc, "{what}: bc");
    assert_eq!(a.uv, b.uv, "{what}: uv");
    assert_eq!(
        (a.numparams, a.framesize),
        (b.numparams, b.framesize),
        "{what}: sizes"
    );
    assert_eq!(a.flags() & 0x87, b.flags(), "{what}: flags");
    let abits: Vec<u64> = a.knum.iter().map(|n| n.to_bits()).collect();
    let bbits: Vec<u64> = b.knum.iter().map(|n| n.to_bits()).collect();
    assert_eq!(abits, bbits, "{what}: knum");
    assert_eq!(a.kgc.len(), b.kgc.len(), "{what}: kgc count");
    for (d, (&x, &y)) in a.kgc.iter().zip(b.kgc.iter()).enumerate() {
        let what = format!("{what}.{d}");
        match (x, y) {
            (KGc::Str(s), KGc::Str(t)) => assert!(s == t, "{what}: string"),
            (KGc::Proto(p), KGc::Proto(q)) => same_proto(&p, &q, debug, &what),
            (KGc::Table(s), KGc::Table(t)) => same_table(s, t, &what),
            (KGc::CData(c), KGc::CData(e)) => {
                assert_eq!(c.ctypeid(), e.ctypeid(), "{what}: ctype");
                assert_eq!(payload(&c), payload(&e), "{what}: payload");
            }
            _ => panic!("{what}: constant kind differs"),
        }
    }
    if debug {
        assert_eq!(a.chunkname, b.chunkname, "{what}: chunkname");
        assert_eq!(
            (a.firstline, a.numline),
            (b.firstline, b.numline),
            "{what}: lines"
        );
        assert_eq!(a.lineinfo, b.lineinfo, "{what}: lineinfo");
        assert_eq!(a.uvnames, b.uvnames, "{what}: uvnames");
        assert_eq!(a.varinfo, b.varinfo, "{what}: varinfo");
    } else {
        assert_eq!(b.chunkname.as_bytes(), b"=?", "{what}: chunkname");
        assert_eq!((b.firstline, b.numline), (0, 0), "{what}: lines");
        assert!(
            b.lineinfo.is_empty() && b.uvnames.is_empty() && b.varinfo.is_empty(),
            "{what}: debug info"
        );
    }
}

#[test]
fn parsed_prototypes_survive_round_trip() {
    let mut vm = Vm::new();
    for name in corpus() {
        let src =
            fs::read(fixture_dir().join("corpus").join(format!("{name}.lua"))).expect("source");
        let pt = parse::parse(&mut vm, &src, format!("@{name}.lua").as_bytes()).expect("parse");
        for strip in [false, true] {
            let d = bcdump::write(&mut vm, pt, strip);
            let back = load(&mut vm, &d);
            same_proto(&pt, &back, !strip, &name);
        }
    }
}

// -- Prototype descriptions (format of describe.lua) ----------------------------------------------

fn nbits(n: f64) -> String {
    format!("{:016x}", n.to_bits())
}

fn tval(v: Value, t: Gc<Table>) -> String {
    match v {
        Value::Str(s) => format!("s:{}", hex(s.as_bytes())),
        Value::Num(n) => format!("n:{}", nbits(n)),
        Value::Bool(b) => b.to_string(),
        Value::Table(x) if x == t => "marker".to_string(),
        other => panic!("unexpected template value {other:?}"),
    }
}

fn payload(cd: &luajit::cdata::CData) -> Vec<u8> {
    // SAFETY: test-only read of the cdata payload, which owns `size()` bytes.
    unsafe { std::slice::from_raw_parts(cd.ptr(), cd.size()).to_vec() }
}

fn kgc_desc(k: KGc) -> String {
    match k {
        KGc::Str(s) => format!("str {}", hex(s.as_bytes())),
        KGc::Proto(_) => "proto".to_string(),
        KGc::Table(t) => {
            let mut e = Vec::new();
            let mut key = Value::Nil;
            while let Some((k, v)) = t.next(key).expect("next") {
                e.push(format!("{}={}", tval(k, t), tval(v, t)));
                key = k;
            }
            e.sort();
            format!("tab {{{}}}", e.join(","))
        }
        KGc::CData(cd) => {
            let b = payload(&cd);
            let w = |i: usize| u64::from_le_bytes(b[i..i + 8].try_into().unwrap());
            match cd.ctypeid() {
                CTID_INT64 => format!("i64 {}ULL", w(0)),
                CTID_UINT64 => format!("u64 {}ULL", w(0)),
                CTID_COMPLEX_DOUBLE => {
                    format!(
                        "complex {} {}",
                        nbits(f64::from_bits(w(0))),
                        nbits(f64::from_bits(w(8)))
                    )
                }
                id => panic!("unexpected cdata ctype {id}"),
            }
        }
    }
}

fn describe(out: &mut String, pt: Gc<Proto>, path: &str) {
    let flags = pt.flags();
    let _ = writeln!(out, "proto {path}");
    let _ = writeln!(
        out,
        "params={} framesize={} vararg={} children={} firstline={} numline={}",
        pt.numparams,
        pt.framesize,
        flags & PROTO_VARARG != 0,
        flags & PROTO_CHILD != 0,
        pt.firstline,
        pt.numline
    );
    let _ = writeln!(out, "source {}", hex(pt.chunkname.as_bytes()));
    let bc: Vec<String> = pt.bc.iter().map(|i| format!("{:08x}", i.0)).collect();
    let _ = writeln!(out, "bc {}", bc.join(" "));
    if !pt.lineinfo.is_empty() {
        assert_eq!(
            pt.lineinfo.len(),
            pt.bc.len(),
            "line info covers every instruction"
        );
    }
    let lines: Vec<String> = (0..pt.bc.len())
        .map(|pc| pt.line(pc).unwrap_or(0).to_string())
        .collect();
    let _ = writeln!(out, "lines {}", lines.join(" "));
    for i in 0..pt.uv.len() {
        let name = pt.uvnames.get(i).map_or(&b""[..], |s| s.as_bytes());
        let _ = writeln!(out, "uvname {i} {}", hex(name));
    }
    let mut children = Vec::new();
    for (d, &k) in pt.kgc.iter().enumerate() {
        let _ = writeln!(out, "kgc {d} {}", kgc_desc(k));
        if let KGc::Proto(c) = k {
            children.push((c, format!("{path}.{d}")));
        }
    }
    for (i, &n) in pt.knum.iter().enumerate() {
        let _ = writeln!(out, "knum {i} {}", nbits(n));
    }
    for (c, p) in children {
        describe(out, c, &p);
    }
}

#[test]
fn prototype_structure_matches_reference() {
    let mut vm = Vm::new();
    for name in corpus() {
        for variant in ["s", "g"] {
            let d = golden(&name, variant, "raw");
            let want = golden(&name, variant, "txt");
            let pt = load(&mut vm, &d);
            let mut got = String::new();
            describe(&mut got, pt, "0");
            let want = String::from_utf8(want).expect("utf8");
            if want != got {
                let line = want.lines().zip(got.lines()).position(|(a, b)| a != b);
                panic!(
                    "{name}.{variant}: description differs at line {:?}\n want: {:?}\n got:  {:?}",
                    line.map(|l| l + 1),
                    line.and_then(|l| want.lines().nth(l)),
                    line.and_then(|l| got.lines().nth(l))
                );
            }
        }
    }
}

// -- lua_load() results for valid and malformed input ----------------------------------------------

#[test]
fn load_results_match_reference() {
    let text = fs::read_to_string(fixture_dir().join("golden/errors.txt")).expect("errors.txt");
    let mut vm = Vm::new();
    let mut n = 0;
    for line in text.lines() {
        let mut f = line.splitn(3, '\t');
        let (input, name, want) = (f.next().unwrap(), f.next().unwrap(), f.next().unwrap());
        let input = unhex(input);
        let name = if name == "-" {
            input.clone()
        } else {
            unhex(name)
        };
        let got = match bcdump::read(&mut vm, &input, &name) {
            Ok(pt) => {
                // lua_load wraps the prototype in a closure; func_newL bumps the saturating closure
                // counter in the flags (bits 5-7, overlapping PROTO_BITOP), which a dump shows.
                let count = pt.flags() as u32 + PROTO_CLCOUNT as u32;
                pt.set_flags((count - ((count >> PROTO_CLC_BITS) & PROTO_CLCOUNT as u32)) as u8);
                // The reference also records string.dump(f, "d") of the loaded function.
                let redump = bcdump::write_flags(&mut vm, pt, BCDUMP_F_DETERMINISTIC);
                format!("OK 0 {} {}", hex(pt.chunkname.as_bytes()), hex(&redump))
            }
            Err(e) => {
                let msg = e.value.as_str().expect("string error").to_string_lossy();
                format!("ERR {} {msg}", e.kind.status())
            }
        };
        assert_eq!(want, got, "input {}", hex(&input));
        n += 1;
    }
    assert!(n > 300, "too few cases: {n}");
}
