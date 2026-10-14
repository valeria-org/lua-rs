//! Port of lj_strfmt.c: format string parser and formatted conversions.
//!
//! `string.format` is driven by iterating a [`FormatParser`] over the format string
//! and, for every [`FmtItem::Spec`], fetching the next argument and calling the
//! matching `put_*` function (mirroring upstream `lj_strfmt_putarg`):
//!
//! | `spec.ty()`          | argument handling (lib side)                                     |
//! |----------------------|------------------------------------------------------------------|
//! | `FormatType::Int`    | int64/uint64 cdata → [`put_fxint`] with the raw 64 bits, else [`put_fnum_int`] |
//! | `FormatType::Uint`   | int64/uint64 cdata → [`put_fxint`], else [`put_fnum_uint`]       |
//! | `FormatType::Num`    | [`put_fnum`]                                                   |
//! | `FormatType::Str`    | `spec.is_quoted()` ? [`put_quoted`] : [`put_fstr`]               |
//! | `FormatType::Char`   | [`put_fchar`] with the checked integer                           |
//! | `FormatType::Ptr`    | [`put_ptr`] (no formatting)                                      |
//!
//! [`FmtItem::Err`] carries the offending directive; the caller raises
//! [`invalid_option_message`].

pub use super::numfmt::{fmt_num, put_fnum, put_num};

/// Format type (low 4 bits of an [`SFormat`]).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FormatType {
    Eof = 0,
    Err = 1,
    Lit = 2,
    Int = 3,
    Uint = 4,
    Num = 5,
    Str = 6,
    Char = 7,
    Ptr = 8,
}

/// Packed format indicator (`SFormat` in lj_strfmt.h): type, subtype, flags, width, precision.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SFormat(pub u32);

/// Subtypes (bits are reused between types).
pub const T_HEX: u32 = 0x0010;
pub const T_OCT: u32 = 0x0020;
pub const T_FP_A: u32 = 0x0000;
pub const T_FP_E: u32 = 0x0010;
pub const T_FP_F: u32 = 0x0020;
pub const T_FP_G: u32 = 0x0030;
pub const T_QUOTED: u32 = 0x0010;

/// Flags.
pub const F_LEFT: u32 = 0x0100;
pub const F_PLUS: u32 = 0x0200;
pub const F_ZERO: u32 = 0x0400;
pub const F_SPACE: u32 = 0x0800;
pub const F_ALT: u32 = 0x1000;
pub const F_UPPER: u32 = 0x2000;

pub const SH_WIDTH: u32 = 16;
pub const SH_PREC: u32 = 24;

impl SFormat {
    pub const A: SFormat = SFormat(FormatType::Num as u32 | T_FP_A);
    pub const C: SFormat = SFormat(FormatType::Char as u32);
    pub const D: SFormat = SFormat(FormatType::Int as u32);
    pub const E: SFormat = SFormat(FormatType::Num as u32 | T_FP_E);
    pub const F: SFormat = SFormat(FormatType::Num as u32 | T_FP_F);
    pub const G: SFormat = SFormat(FormatType::Num as u32 | T_FP_G);
    pub const I: SFormat = SFormat::D;
    pub const O: SFormat = SFormat(FormatType::Uint as u32 | T_OCT);
    pub const P: SFormat = SFormat(FormatType::Ptr as u32);
    pub const Q: SFormat = SFormat(FormatType::Str as u32 | T_QUOTED);
    pub const S: SFormat = SFormat(FormatType::Str as u32);
    pub const U: SFormat = SFormat(FormatType::Uint as u32);
    pub const X: SFormat = SFormat(FormatType::Uint as u32 | T_HEX);
    /// `%.14g`, the number → string conversion format.
    pub const G14: SFormat = SFormat(SFormat::G.0 | ((14 + 1) << SH_PREC));

    /// `STRFMT_TYPE`.
    pub fn ty(self) -> FormatType {
        match self.0 & 15 {
            0 => FormatType::Eof,
            1 => FormatType::Err,
            2 => FormatType::Lit,
            3 => FormatType::Int,
            4 => FormatType::Uint,
            5 => FormatType::Num,
            6 => FormatType::Str,
            7 => FormatType::Char,
            8 => FormatType::Ptr,
            t => unreachable!("bad format type {t}"),
        }
    }
    /// `STRFMT_WIDTH` (0..=99).
    pub fn width(self) -> u32 {
        (self.0 >> SH_WIDTH) & 255
    }
    /// `STRFMT_PREC` raw: `u32::MAX` when no precision was given.
    pub fn prec_raw(self) -> u32 {
        ((self.0 >> SH_PREC) & 255).wrapping_sub(1)
    }
    /// Precision, if one was given (0..=99).
    pub fn prec(self) -> Option<u32> {
        let p = self.prec_raw();
        (p != u32::MAX).then_some(p)
    }
    /// `STRFMT_FP`: 0 = `%a`, 1 = `%e`, 2 = `%f`, 3 = `%g`.
    pub fn fp(self) -> u32 {
        (self.0 >> 4) & 3
    }
    pub fn has(self, flag: u32) -> bool {
        self.0 & flag != 0
    }
    /// `%q`.
    pub fn is_quoted(self) -> bool {
        self.ty() == FormatType::Str && self.has(T_QUOTED)
    }
}

/// One element of a parsed format string.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FmtItem<'a> {
    /// Literal text (`%%` yields a literal `%`).
    Lit(&'a [u8]),
    /// A conversion consuming one argument.
    Spec(SFormat),
    /// Invalid directive; the slice is the text reported in the error message.
    Err(&'a [u8]),
}

/// `lj_strfmt_parse` as an iterator. Stops after the first [`FmtItem::Err`].
#[derive(Clone)]
pub struct FormatParser<'a> {
    fmt: &'a [u8],
    p: usize,
}

impl<'a> FormatParser<'a> {
    pub fn new(fmt: &'a [u8]) -> Self {
        FormatParser { fmt, p: 0 }
    }

    #[inline]
    fn at(&self, i: usize) -> u8 {
        // The format string is NUL-terminated upstream.
        self.fmt.get(i).copied().unwrap_or(0)
    }
}

static STRFMT_MAP: [u8; (b'x' - b'A') as usize + 1] = {
    const fn set(mut m: [u8; 56], c: u8, sf: SFormat) -> [u8; 56] {
        m[(c - b'A') as usize] = sf.0 as u8;
        m
    }
    let mut m = [0u8; (b'x' - b'A') as usize + 1];
    // Uppercase forms.
    m = set(m, b'A', SFormat::A);
    m = set(m, b'E', SFormat::E);
    m = set(m, b'F', SFormat::F);
    m = set(m, b'G', SFormat::G);
    m = set(m, b'X', SFormat::X);
    // Lowercase forms.
    m = set(m, b'a', SFormat::A);
    m = set(m, b'c', SFormat::C);
    m = set(m, b'd', SFormat::D);
    m = set(m, b'e', SFormat::E);
    m = set(m, b'f', SFormat::F);
    m = set(m, b'g', SFormat::G);
    m = set(m, b'i', SFormat::I);
    m = set(m, b'o', SFormat::O);
    m = set(m, b'p', SFormat::P);
    m = set(m, b'q', SFormat::Q);
    m = set(m, b's', SFormat::S);
    m = set(m, b'u', SFormat::U);
    m = set(m, b'x', SFormat::X);
    m
};

impl<'a> Iterator for FormatParser<'a> {
    type Item = FmtItem<'a>;

    fn next(&mut self) -> Option<FmtItem<'a>> {
        let e = self.fmt.len();
        let start = self.p;
        let mut p = self.p;
        while p < e {
            if self.at(p) == b'%' {
                if self.at(p + 1) == b'%' {
                    // '%%': literal up to and including the first '%'.
                    p += 1;
                    self.p = p + 1;
                    return Some(FmtItem::Lit(&self.fmt[start..p]));
                }
                if p != start {
                    break;
                }
                let mut sf: u32 = 0;
                p += 1;
                // Parse flags.
                while (self.at(p) as u32).wrapping_sub(b' ' as u32) <= (b'0' - b' ') as u32 {
                    match self.at(p) {
                        b'-' => sf |= F_LEFT,
                        b'+' => sf |= F_PLUS,
                        b'0' => sf |= F_ZERO,
                        b' ' => sf |= F_SPACE,
                        b'#' => sf |= F_ALT,
                        _ => break,
                    }
                    p += 1;
                }
                // Parse width.
                if self.at(p).is_ascii_digit() {
                    let mut width = (self.at(p) - b'0') as u32;
                    p += 1;
                    if self.at(p).is_ascii_digit() {
                        width = (self.at(p) - b'0') as u32 + width * 10;
                        p += 1;
                    }
                    sf |= width << SH_WIDTH;
                }
                // Parse precision.
                if self.at(p) == b'.' {
                    let mut prec: u32 = 0;
                    p += 1;
                    if self.at(p).is_ascii_digit() {
                        prec = (self.at(p) - b'0') as u32;
                        p += 1;
                        if self.at(p).is_ascii_digit() {
                            prec = (self.at(p) - b'0') as u32 + prec * 10;
                            p += 1;
                        }
                    }
                    sf |= (prec + 1) << SH_PREC;
                }
                // Parse conversion.
                let c = (self.at(p) as u32).wrapping_sub(b'A' as u32);
                if c <= (b'x' - b'A') as u32 {
                    let sx = STRFMT_MAP[c as usize] as u32;
                    if sx != 0 {
                        self.p = p + 1;
                        let upper = if c & 0x20 != 0 { 0 } else { F_UPPER };
                        return Some(FmtItem::Spec(SFormat(sf | sx | upper)));
                    }
                }
                // Return error location.
                if self.at(p) >= 32 {
                    p += 1;
                }
                self.p = e;
                return Some(FmtItem::Err(&self.fmt[start..p.min(e)]));
            }
            p += 1;
        }
        self.p = p;
        (p > start).then(|| FmtItem::Lit(&self.fmt[start..p]))
    }
}

/// Error text for [`FmtItem::Err`]: `invalid option '%w' to 'format'`.
pub fn invalid_option_message(spec: &[u8]) -> Vec<u8> {
    let mut m = b"invalid option '".to_vec();
    m.extend_from_slice(spec);
    m.extend_from_slice(b"' to 'format'");
    m
}

/* -- Raw conversions ----------------------------------------------------- */

/// Maximum buffer sizes for conversions.
pub const MAXBUF_XINT: usize = 1 + 22;
pub const MAXBUF_INT: usize = 1 + 10;
pub const MAXBUF_NUM: usize = 32;
pub const MAXBUF_PTR: usize = 2 + 2 * 8;

/// `lj_strfmt_wint`: write `k` in decimal into `buf` at `p`, return the new position.
pub(crate) fn wint(buf: &mut [u8], mut p: usize, k: i32) -> usize {
    let mut u = k as u32;
    if k < 0 {
        u = (!u).wrapping_add(1);
        buf[p] = b'-';
        p += 1;
    }
    let mut tmp = [0u8; 10];
    let mut n = 0;
    loop {
        tmp[n] = b'0' + (u % 10) as u8;
        n += 1;
        u /= 10;
        if u == 0 {
            break;
        }
    }
    for i in (0..n).rev() {
        buf[p] = tmp[i];
        p += 1;
    }
    p
}

/// `lj_strfmt_putint`: append a 32-bit integer in decimal.
pub fn put_int(out: &mut Vec<u8>, k: i32) {
    let mut buf = [0u8; MAXBUF_INT];
    let n = wint(&mut buf, 0, k);
    out.extend_from_slice(&buf[..n]);
}

/// `lj_strfmt_wptr`: append a pointer as `0x` + hex (shortened for 64-bit) or `NULL`.
pub fn put_ptr(out: &mut Vec<u8>, x: usize) {
    if x == 0 {
        out.extend_from_slice(b"NULL");
        return;
    }
    let x = x as u64;
    let hi = (x >> 32) as u32;
    // Shorten output for 64 bit pointers.
    let n = 2
        + 2 * 4
        + if hi != 0 {
            2 + 2 * ((31 - hi.leading_zeros()) >> 3) as usize
        } else {
            0
        };
    let mut buf = [0u8; MAXBUF_PTR];
    buf[0] = b'0';
    buf[1] = b'x';
    let mut v = x;
    for i in (2..n).rev() {
        buf[i] = b"0123456789abcdef"[(v & 15) as usize];
        v >>= 4;
    }
    out.extend_from_slice(&buf[..n]);
}

/// `lj_strfmt_wuleb128`: append an unsigned LEB128 value.
pub fn put_uleb128(out: &mut Vec<u8>, mut v: u32) {
    while v >= 0x80 {
        out.push((v & 0x7f) as u8 | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

/* -- Unformatted / formatted conversions --------------------------------- */

/// `strfmt_putquotedlen`: append `s` quoted for `%q`.
pub fn put_quoted(out: &mut Vec<u8>, s: &[u8]) {
    out.push(b'"');
    for idx in 0..s.len() {
        put_quoted_char(out, s, idx);
    }
    out.push(b'"');
}

/// One step of `strfmt_putquotedlen`: append the (1 to 4 byte) quoted form of `s[idx]`
/// (the following byte decides whether a control character needs three digits).
pub fn put_quoted_char(out: &mut Vec<u8>, s: &[u8], idx: usize) {
    let ch = s[idx];
    let mut c = ch as u32;
    if c == b'"' as u32 || c == b'\\' as u32 || c == b'\n' as u32 {
        out.push(b'\\');
    } else if super::chars::iscntrl(ch) {
        // This can only be 0-31 or 127.
        out.push(b'\\');
        let next = s.get(idx + 1).copied().unwrap_or(0);
        if c >= 100 || super::chars::isdigit(next) {
            out.push(b'0' + (c >= 100) as u8);
            if c >= 100 {
                c -= 100;
            }
            let d = (c * 205) >> 11;
            c -= d * 10;
            out.push(b'0' + d as u8);
        } else if c >= 10 {
            let d = (c * 205) >> 11;
            c -= d * 10;
            out.push(b'0' + d as u8);
        }
        c += b'0' as u32;
    }
    out.push(c as u8);
}

/// `lj_strfmt_putfchar`: append a character with width/left-justification.
pub fn put_fchar(out: &mut Vec<u8>, sf: SFormat, c: i32) {
    let width = sf.width();
    if sf.has(F_LEFT) {
        out.push(c as u8);
    }
    for _ in 1..width.max(1) {
        out.push(b' ');
    }
    if !sf.has(F_LEFT) {
        out.push(c as u8);
    }
}

/// `strfmt_putfstrlen`: append a string with width/precision/left-justification.
pub fn put_fstr(out: &mut Vec<u8>, sf: SFormat, s: &[u8]) {
    let width = sf.width() as usize;
    let len = s.len().min(sf.prec_raw() as usize);
    let s = &s[..len];
    if sf.has(F_LEFT) {
        out.extend_from_slice(s);
    }
    out.resize(out.len() + width.saturating_sub(len), b' ');
    if !sf.has(F_LEFT) {
        out.extend_from_slice(s);
    }
}

/// `lj_strfmt_putfxint`: append a formatted signed (`FormatType::Int`, `k` read as
/// `i64`) or unsigned (`FormatType::Uint`, decimal/hex/octal) 64-bit integer.
pub fn put_fxint(out: &mut Vec<u8>, sf: SFormat, mut k: u64) {
    let mut sf = sf.0;
    let mut buf = [0u8; MAXBUF_XINT];
    let mut q = MAXBUF_XINT;
    let mut prefix: u32 = 0;

    // Figure out signed prefixes.
    if sf & 15 == FormatType::Int as u32 {
        if (k as i64) < 0 {
            k = (!k).wrapping_add(1);
            prefix = 256 + b'-' as u32;
        } else if sf & F_PLUS != 0 {
            prefix = 256 + b'+' as u32;
        } else if sf & F_SPACE != 0 {
            prefix = 256 + b' ' as u32;
        }
    }

    // Convert number and store to fixed-size buffer in reverse order.
    let mut prec = SFormat(sf).prec_raw();
    if (prec as i32) >= 0 {
        sf &= !F_ZERO;
    }
    if k == 0 {
        // Special-case zero argument.
        if prec != 0 || (sf & (T_OCT | F_ALT)) == (T_OCT | F_ALT) {
            q -= 1;
            buf[q] = b'0';
        }
    } else if sf & (T_HEX | T_OCT) == 0 {
        // Decimal.
        loop {
            q -= 1;
            buf[q] = b'0' + (k % 10) as u8;
            k /= 10;
            if k == 0 {
                break;
            }
        }
    } else if sf & T_HEX != 0 {
        let hexdig: &[u8; 16] = if sf & F_UPPER != 0 {
            b"0123456789ABCDEF"
        } else {
            b"0123456789abcdef"
        };
        loop {
            q -= 1;
            buf[q] = hexdig[(k & 15) as usize];
            k >>= 4;
            if k == 0 {
                break;
            }
        }
        if sf & F_ALT != 0 {
            prefix = 512 + if sf & F_UPPER != 0 { b'X' } else { b'x' } as u32;
        }
    } else {
        // Octal.
        loop {
            q -= 1;
            buf[q] = b'0' + (k & 7) as u8;
            k >>= 3;
            if k == 0 {
                break;
            }
        }
        if sf & F_ALT != 0 {
            q -= 1;
            buf[q] = b'0';
        }
    }

    // Calculate sizes.
    let len = (MAXBUF_XINT - q) as u32;
    if len as i32 >= prec as i32 {
        prec = len;
    }
    let mut width = SFormat(sf).width();
    let pprec = prec + (prefix >> 8);

    // Format number with leading/trailing whitespace and zeros.
    if sf & (F_LEFT | F_ZERO) == 0 {
        while width > pprec {
            width -= 1;
            out.push(b' ');
        }
    }
    if prefix != 0 {
        if prefix as u8 >= b'X' {
            out.push(b'0');
        }
        out.push(prefix as u8);
    }
    if sf & (F_LEFT | F_ZERO) == F_ZERO {
        while width > pprec {
            width -= 1;
            out.push(b'0');
        }
    }
    while prec > len {
        prec -= 1;
        out.push(b'0');
    }
    out.extend_from_slice(&buf[q..]); // Add number itself.
    if sf & F_LEFT != 0 {
        while width > pprec {
            width -= 1;
            out.push(b' ');
        }
    }
}

/// `lj_strfmt_putfnum_int`: append a number formatted as a signed integer.
pub fn put_fnum_int(out: &mut Vec<u8>, sf: SFormat, n: f64) {
    let k = super::num2i64(n);
    if k == k as i32 as i64 && sf == SFormat::D {
        put_int(out, k as i32); // Shortcut for plain %d.
    } else {
        put_fxint(out, sf, k as u64);
    }
}

/// `lj_strfmt_putfnum_uint`: append a number formatted as an unsigned integer.
pub fn put_fnum_uint(out: &mut Vec<u8>, sf: SFormat, n: f64) {
    put_fxint(out, sf, super::num2u64(n));
}

/* -- Internal string formatting ------------------------------------------ */

/// Argument for [`format_internal`] (the `va_list` of `lj_strfmt_pushvf`).
#[derive(Clone, Copy, Debug)]
pub enum FmtArg<'a> {
    /// `%d` / `%i`.
    Int(i32),
    /// `%u` / `%o` / `%x`.
    Uint(u32),
    /// Any FP conversion; always formatted as `%.14g`.
    Num(f64),
    /// `%s`; `None` prints `(null)`.
    Str(Option<&'a [u8]>),
    /// `%c`.
    Char(u8),
    /// `%p`.
    Ptr(usize),
}

/// `lj_strfmt_pushvf`: the limited formatter used for `lua_pushfstring` and internal
/// messages. Only `%d %u %o %x` honour flags/width/precision; `%s %c %p` are unformatted.
///
/// # Panics
/// If `args` does not match the conversions in `fmt` (a caller bug, like a bad `va_list`).
pub fn format_internal(fmt: &[u8], args: &[FmtArg<'_>]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut args = args.iter();
    let mut next = || *args.next().expect("format_internal: missing argument");
    for item in FormatParser::new(fmt) {
        match item {
            FmtItem::Lit(s) => out.extend_from_slice(s),
            FmtItem::Err(_) => out.push(b'?'),
            FmtItem::Spec(sf) => match (sf.ty(), next()) {
                (FormatType::Int, FmtArg::Int(k)) => put_fxint(&mut out, sf, k as i64 as u64),
                (FormatType::Uint, FmtArg::Uint(k)) => put_fxint(&mut out, sf, k as u64),
                (FormatType::Num, FmtArg::Num(n)) => put_fnum(&mut out, SFormat::G14, n),
                (FormatType::Str, FmtArg::Str(s)) => out.extend_from_slice(s.unwrap_or(b"(null)")),
                (FormatType::Char, FmtArg::Char(c)) => out.push(c),
                (FormatType::Ptr, FmtArg::Ptr(p)) => put_ptr(&mut out, p),
                (t, a) => panic!("format_internal: conversion {t:?} given {a:?}"),
            },
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `string.format` argument for the test driver.
    #[derive(Clone, Copy)]
    enum A<'a> {
        N(f64),
        S(&'a str),
        I64(i64),
        U64(u64),
    }

    /// Minimal `lj_strfmt_putarg` driver (what lib_string does with real values).
    fn sformat(fmt: &str, args: &[A<'_>]) -> Result<String, String> {
        let mut out = Vec::new();
        let mut args = args.iter();
        for item in FormatParser::new(fmt.as_bytes()) {
            let sf = match item {
                FmtItem::Lit(s) => {
                    out.extend_from_slice(s);
                    continue;
                }
                FmtItem::Err(s) => {
                    return Err(String::from_utf8(invalid_option_message(s)).unwrap());
                }
                FmtItem::Spec(sf) => sf,
            };
            let arg = *args.next().expect("missing arg");
            match (sf.ty(), arg) {
                (FormatType::Int | FormatType::Uint, A::I64(k)) => {
                    put_fxint(&mut out, sf, k as u64)
                }
                (FormatType::Int | FormatType::Uint, A::U64(k)) => put_fxint(&mut out, sf, k),
                (FormatType::Int, A::N(n)) => put_fnum_int(&mut out, sf, n),
                (FormatType::Uint, A::N(n)) => put_fnum_uint(&mut out, sf, n),
                (FormatType::Num, A::N(n)) => put_fnum(&mut out, sf, n),
                (FormatType::Char, A::N(n)) => put_fchar(&mut out, sf, n as i32),
                (FormatType::Str, A::S(s)) if sf.is_quoted() => put_quoted(&mut out, s.as_bytes()),
                (FormatType::Str, A::S(s)) => put_fstr(&mut out, sf, s.as_bytes()),
                (FormatType::Str, A::N(n)) => put_fstr(&mut out, sf, &fmt_num(n)),
                _ => panic!("bad test arg"),
            }
        }
        Ok(String::from_utf8(out).unwrap())
    }

    fn f(fmt: &str, args: &[A<'_>]) -> String {
        sformat(fmt, args).unwrap()
    }

    #[test]
    fn number_to_string() {
        // luajit -e 'print(tostring(v))'
        let cases: &[(f64, &str)] = &[
            (0.1, "0.1"),
            (1e15, "1e+15"),
            (1e16, "1e+16"),
            (2f64.powi(53), "9.007199254741e+15"),
            (-0.0, "-0"),
            (f64::INFINITY, "inf"),
            (f64::NEG_INFINITY, "-inf"),
            (f64::NAN, "nan"),
            (-f64::NAN, "nan"),
            (1e-310, "1e-310"),
            (123.456, "123.456"),
            (1e100, "1e+100"),
            (-1.5e-7, "-1.5e-07"),
            (2f64.powi(63), "9.2233720368548e+18"),
            (1e21, "1e+21"),
            (100.0, "100"),
            (12345678901234567.0, "1.2345678901235e+16"),
            (0.0, "0"),
        ];
        for &(n, s) in cases {
            assert_eq!(String::from_utf8(fmt_num(n)).unwrap(), s, "{n:e}");
        }
    }

    #[test]
    fn float_directives() {
        assert_eq!(
            f(
                "[%5.2f][%a][%A][%.3a][%a][%a]",
                &[
                    A::N(1.23456),
                    A::N(1.0),
                    A::N(0.1),
                    A::N(1.0 / 3.0),
                    A::N(1e-310),
                    A::N(0.0)
                ]
            ),
            "[ 1.23][0x1p+0][0X1.999999999999AP-4][0x1.555p-2][0x1.2688b70e62bp-1030][0x0p+0]"
        );
        assert_eq!(
            f(
                "[%e][%.0e][%#.0f][%g][%g][%g][%.3g][%10.3f][%-10.2e]",
                &[
                    A::N(12345.6789),
                    A::N(5e10),
                    A::N(3.0),
                    A::N(1e-5),
                    A::N(1e20),
                    A::N(100000.0),
                    A::N(0.0001234),
                    A::N(-2.5),
                    A::N(1234.5)
                ]
            ),
            "[1.234568e+04][5e+10][3.][1e-05][1e+20][100000][0.000123][    -2.500][1.23e+03  ]"
        );
        // Round half to even on the exact binary value.
        assert_eq!(
            f(
                "%.3f %.2f %.1f %.0f %.0f %.0f",
                &[
                    A::N(2.0005),
                    A::N(0.125),
                    A::N(0.25),
                    A::N(0.5),
                    A::N(1.5),
                    A::N(2.5)
                ]
            ),
            "2.001 0.12 0.2 0 2 2"
        );
        assert_eq!(
            f(
                "[%f][%F][%E][%G][%e]",
                &[
                    A::N(f64::INFINITY),
                    A::N(f64::NEG_INFINITY),
                    A::N(f64::NAN),
                    A::N(f64::INFINITY),
                    A::N(-0.0)
                ]
            ),
            "[inf][-INF][NAN][INF][-0.000000e+00]"
        );
        let big = f("%f", &[A::N(1e300)]);
        assert!(big.starts_with("10000000000000000525047602552044202487044685811081591549158541"));
        assert!(big.ends_with("459400540160.000000"));
        assert_eq!(big.len(), 301 + 7);
    }

    #[test]
    fn integer_directives() {
        assert_eq!(
            f(
                "[%+d][% d][%05d][%x][%X][%#x][%o][%#o][%u][%c][%5c][%-3c]",
                &[
                    A::N(5.0),
                    A::N(5.0),
                    A::N(-42.0),
                    A::N(255.0),
                    A::N(255.0),
                    A::N(255.0),
                    A::N(8.0),
                    A::N(8.0),
                    A::N(-1.0),
                    A::N(65.0),
                    A::N(66.0),
                    A::N(67.0)
                ]
            ),
            "[+5][ 5][-0042][ff][FF][0xff][10][010][18446744073709551615][A][    B][C  ]"
        );
        assert_eq!(
            f(
                "[%.3d][%8.3x][%-8d][%d][%x][%d][%.0d]",
                &[
                    A::N(7.0),
                    A::N(10.0),
                    A::N(3.0),
                    A::N(2f64.powi(53)),
                    A::N(-1.0),
                    A::N(1e100),
                    A::N(0.0)
                ]
            ),
            "[007][     00a][3       ][9007199254740992][ffffffffffffffff][-9223372036854775808][]"
        );
        // int64_t / uint64_t cdata.
        assert_eq!(
            f(
                "%d %x %d",
                &[A::I64(i64::MIN), A::I64(-1), A::U64(u64::MAX)]
            ),
            "-9223372036854775808 ffffffffffffffff -1"
        );
    }

    #[test]
    fn string_directives() {
        assert_eq!(
            f(
                "[%5s][%.2s][%s][%s][%%][%5.1s][%-10s][%5.3s]",
                &[
                    A::S("abc"),
                    A::S("abc"),
                    A::N(1.5),
                    A::N(10.0),
                    A::S("xyz"),
                    A::S("ab"),
                    A::S("hello")
                ]
            ),
            "[  abc][ab][1.5][10][%][    x][ab        ][  hel]"
        );
        assert_eq!(
            f("%q", &[A::S("a\0b\r\n\"\\\x7f2\0")]),
            "\"a\\0b\\13\\\n\\\"\\\\\\1272\\0\""
        );
        assert_eq!(f("%q", &[A::S("\x011\x01x\x1f")]), "\"\\0011\\1x\\31\"");
    }

    #[test]
    fn parse_errors() {
        // luajit -e 'print(pcall(string.format, fmt, 1))'
        for (fmt, msg) in [
            ("%w", "invalid option '%w' to 'format'"),
            ("%10.123f", "invalid option '%10.123' to 'format'"),
            ("%123d", "invalid option '%123' to 'format'"),
            ("%", "invalid option '%' to 'format'"),
            ("%\x01", "invalid option '%' to 'format'"),
            ("ab%5", "invalid option '%5' to 'format'"),
        ] {
            assert_eq!(sformat(fmt, &[A::N(1.0)]).unwrap_err(), msg, "{fmt:?}");
        }
    }

    #[test]
    fn parser_items() {
        let items: Vec<_> = FormatParser::new(b"a%%b%-5.2fc").collect();
        assert_eq!(items[0], FmtItem::Lit(b"a%"));
        assert_eq!(items[1], FmtItem::Lit(b"b"));
        let FmtItem::Spec(sf) = items[2] else {
            panic!()
        };
        assert_eq!(
            (sf.ty(), sf.width(), sf.prec(), sf.has(F_LEFT), sf.fp()),
            (FormatType::Num, 5, Some(2), true, 2)
        );
        assert_eq!(items[3], FmtItem::Lit(b"c"));
        assert_eq!(items.len(), 4);
    }

    #[test]
    fn raw_conversions() {
        let mut v = Vec::new();
        put_int(&mut v, i32::MIN);
        v.push(b' ');
        put_ptr(&mut v, 0);
        v.push(b' ');
        put_ptr(&mut v, 0x1234);
        v.push(b' ');
        put_ptr(&mut v, 0x7f12_3456_789a);
        assert_eq!(v, b"-2147483648 NULL 0x00001234 0x7f123456789a");
        let mut v = Vec::new();
        put_uleb128(&mut v, 300);
        assert_eq!(v, [0xac, 0x02]);
        assert_eq!(
            format_internal(
                b"%s:%d: %s %c %f%%",
                &[
                    FmtArg::Str(Some(b"x.lua")),
                    FmtArg::Int(-3),
                    FmtArg::Str(None),
                    FmtArg::Char(b'!'),
                    FmtArg::Num(0.5)
                ]
            ),
            b"x.lua:-3: (null) ! 0.5%"
        );
    }
}
