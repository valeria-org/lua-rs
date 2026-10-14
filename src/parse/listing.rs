//! Bytecode listing in the exact format of `luajit -bl` (port of jit/bc.lua `bcdump` with
//! `jit.util.funcinfo`'s `loc` from `lj_debug_pushloc`). Test and developer tooling.

use crate::bc::Op;
use crate::func::{KGc, PROTO_CHILD, Proto};
use crate::gc::Gc;
use crate::text::chars;
use crate::text::numfmt::fmt_num;

/// `lj_debug_pushloc(L, pt, 0)`: location string of a prototype.
fn proto_loc(pt: &Gc<Proto>) -> Vec<u8> {
    let name = pt.chunkname.as_bytes();
    let s = super::lex::cstr(name);
    let line = pt.firstline;
    let mut out = Vec::new();
    if line == !0 {
        out.extend_from_slice(b"builtin:");
        out.extend_from_slice(s);
        return out;
    } else if s.first() == Some(&b'@') {
        let s = &s[1..];
        let len = name.len() - 1;
        // Strip the directory part (upstream scans from the terminator down to index 1).
        let mut start = 0;
        let mut i = len;
        while i > 0 {
            if i < s.len() && (s[i] == b'/' || s[i] == b'\\') {
                start = i + 1;
                break;
            }
            i -= 1;
        }
        out.extend_from_slice(&s[start..]);
    } else if name.len() > 40 {
        out.extend_from_slice(format!("{:#x}", pt.as_ptr() as usize).as_bytes());
    } else if s.first() == Some(&b'=') {
        out.extend_from_slice(&s[1..]);
    } else {
        out.push(b'"');
        out.extend_from_slice(s);
        out.push(b'"');
    }
    out.extend_from_slice(format!(":{line}").as_bytes());
    out
}

/// `ctlsub` applied to every control character (`%c`).
fn escape_ctl(s: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len());
    for &c in s {
        if chars::iscntrl(c) {
            match c {
                b'\n' => out.extend_from_slice(b"\\n"),
                b'\r' => out.extend_from_slice(b"\\r"),
                b'\t' => out.extend_from_slice(b"\\t"),
                _ => out.extend_from_slice(format!("\\{c:03}").as_bytes()),
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn kgc(pt: &Proto, d: u32) -> KGc {
    pt.kgc[d as usize]
}

/// `bcline`: one listing line for instruction `pc`.
fn bcline(pt: &Proto, pc: usize, target: bool, out: &mut Vec<u8>) {
    let ins = pt.bc[pc];
    let op = ins.op();
    let m = op.mode() as u32;
    let (ma, mb, mc) = (m & 7, (m >> 3) & 15, (m >> 7) & 15);
    let a = ins.a();
    let astr = if ma == 0 {
        String::new()
    } else {
        a.to_string()
    };
    let prefix = if target { "=>" } else { "  " };
    let s = format!("{pc:04} {prefix} {:<6} {astr:>3} ", op.name());
    out.extend_from_slice(s.as_bytes());
    let mut d = ins.d();
    if mc == 13 {
        // BCMjump
        out.extend_from_slice(format!("=> {:04}\n", pc as i64 + d as i64 - 0x7fff).as_bytes());
        return;
    }
    if mb != 0 {
        d &= 0xff;
    } else if mc == 0 {
        out.push(b'\n');
        return;
    }
    let mut kc: Option<Vec<u8>> = match mc {
        10 => {
            // BCMstr
            let KGc::Str(s) = kgc(pt, d) else {
                unreachable!("string operand is not a string")
            };
            let e = escape_ctl(s.as_bytes());
            let mut v = vec![b'"'];
            if s.len() > 40 {
                v.extend_from_slice(&e[..e.len().min(40)]);
                v.extend_from_slice(b"\"~");
            } else {
                v.extend_from_slice(&e);
                v.push(b'"');
            }
            Some(v)
        }
        9 => {
            // BCMnum
            let mut n = pt.knum[d as usize];
            if op == Op::TSETM {
                n -= 4503599627370496.0;
            }
            Some(fmt_num(n))
        }
        12 => {
            // BCMfunc
            let KGc::Proto(p) = kgc(pt, d) else {
                unreachable!("function operand is not a prototype")
            };
            Some(proto_loc(&p))
        }
        5 => Some(
            pt.uvnames
                .get(d as usize)
                .map(|s| s.as_bytes().to_vec())
                .unwrap_or_default(),
        ),
        _ => None,
    };
    if ma == 5 {
        // BCMuv
        let mut ka = pt
            .uvnames
            .get(a as usize)
            .map(|s| s.as_bytes().to_vec())
            .unwrap_or_default();
        if let Some(k) = kc {
            ka.extend_from_slice(b" ; ");
            ka.extend_from_slice(&k);
        }
        kc = Some(ka);
    }
    if mb != 0 {
        let b = ins.b();
        match kc {
            Some(k) => {
                out.extend_from_slice(format!("{b:3} {d:3}  ; ").as_bytes());
                out.extend_from_slice(&k);
                out.push(b'\n');
            }
            None => out.extend_from_slice(format!("{b:3} {d:3}\n").as_bytes()),
        }
        return;
    }
    if let Some(k) = kc {
        out.extend_from_slice(format!("{d:3}      ; ").as_bytes());
        out.extend_from_slice(&k);
        out.push(b'\n');
        return;
    }
    let mut dv = d as i64;
    if mc == 7 && d > 32767 {
        dv -= 65536; // BCMlits
    }
    out.extend_from_slice(format!("{dv:3}\n").as_bytes());
}

/// `bcdump(func, out, true)`.
fn bcdump(pt: Gc<Proto>, out: &mut Vec<u8>) {
    if pt.flags() & PROTO_CHILD != 0 {
        for k in pt.kgc.iter() {
            if let KGc::Proto(child) = *k {
                bcdump(child, out);
            }
        }
    }
    out.extend_from_slice(b"-- BYTECODE -- ");
    out.extend_from_slice(&proto_loc(&pt));
    out.extend_from_slice(format!("-{}\n", pt.firstline.wrapping_add(pt.numline)).as_bytes());
    let n = pt.bc.len();
    let mut target = vec![false; n + 1];
    for pc in 1..n {
        let ins = pt.bc[pc];
        if (ins.op().mode() >> 7) & 15 == 13 {
            let t = pc as i64 + ins.d() as i64 - 0x7fff;
            if t >= 0 && (t as usize) < target.len() {
                target[t as usize] = true;
            }
        }
    }
    for (pc, &t) in target.iter().enumerate().take(n).skip(1) {
        bcline(&pt, pc, t, out);
    }
    out.push(b'\n');
}

/// Listing of a prototype and all its children, byte-identical to `luajit -bl`.
#[doc(hidden)]
pub fn bclist(pt: Gc<Proto>) -> Vec<u8> {
    let mut out = Vec::new();
    bcdump(pt, &mut out);
    out
}
