//! Port of lj_strscan.c: string to number conversion, plus the base-N integer
//! parsing used by `tonumber(s, base)` (lib_base.c).
//!
//! The scanner is exact: every input converts to the correctly rounded double, and
//! the integer/64-bit/imaginary formats follow upstream bit for bit. Bytes past the
//! end of the input read as `0`, mirroring upstream's reliance on NUL-terminated
//! strings; an embedded NUL therefore ends the number and makes it an error.

use super::chars::{self, isdigit, isspace};
use super::ldexp;

/// Convert to `i32`, if possible.
pub const OPT_TOINT: u32 = 0x01;
/// Always convert to double.
pub const OPT_TONUM: u32 = 0x02;
/// Accept the `i` suffix (imaginary numbers).
pub const OPT_IMAG: u32 = 0x04;
/// Accept the `LL`/`ULL` suffixes (64-bit integers).
pub const OPT_LL: u32 = 0x08;
/// C syntax: octal literals, `U`/`L` suffixes, strict integer overflow.
pub const OPT_C: u32 = 0x10;

/// Scan result (`StrScanFmt` together with the value upstream stores in the `TValue`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum StrScan {
    Error,
    Num(f64),
    /// Imaginary part of a complex constant (`12i`).
    Imag(f64),
    Int(i32),
    U32(u32),
    I64(i64),
    U64(u64),
}

/// Internal format tag; the order matters (upstream does arithmetic on it).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Fmt {
    Error = 0,
    Num = 1,
    Imag = 2,
    Int = 3,
    U32 = 4,
    I64 = 5,
    U64 = 6,
}

impl Fmt {
    fn from_u32(v: u32) -> Fmt {
        match v {
            1 => Fmt::Num,
            2 => Fmt::Imag,
            3 => Fmt::Int,
            4 => Fmt::U32,
            5 => Fmt::I64,
            6 => Fmt::U64,
            _ => Fmt::Error,
        }
    }
    fn add(self, d: u32) -> Fmt {
        Fmt::from_u32(self as u32 + d)
    }
}

/// Value slot (the relevant view of upstream's `TValue`).
#[derive(Clone, Copy)]
enum Val {
    N(f64),
    I(i32),
    U64(u64),
}

const DIG: usize = 1024;
const MAXDIG: u32 = 800;
const DDIG: usize = DIG / 2;
const DMASK: u32 = (DDIG - 1) as u32;
const MAXEXP: u32 = 1 << 20;

#[inline]
fn dnext(a: u32) -> u32 {
    (a.wrapping_add(1)) & DMASK
}
#[inline]
fn dprev(a: u32) -> u32 {
    (a.wrapping_sub(1)) & DMASK
}
#[inline]
fn dlen(lo: u32, hi: u32) -> i32 {
    (lo.wrapping_sub(hi) & DMASK) as i32
}

#[inline]
fn casecmp(c: u8, k: u8) -> bool {
    (c | 0x20) == k
}

#[inline]
fn at(s: &[u8], i: usize) -> u8 {
    s.get(i).copied().unwrap_or(0)
}

/// Read a digit, skipping over a single decimal point (`*p != '.' ? *p : *++p`).
#[inline]
fn digit_skip_dp(s: &[u8], p: &mut usize) -> u8 {
    if at(s, *p) == b'.' {
        *p += 1;
    }
    at(s, *p)
}

/// Final conversion to double.
fn strscan_double(mut x: u64, ex2: i32, neg: bool) -> f64 {
    // Avoid double rounding for denormals.
    if ex2 <= -1075 && x != 0 {
        let b = (x.leading_zeros() ^ 63) as i32;
        if b + ex2 <= -1023 && b + ex2 >= -1075 {
            let rb = 1u64 << (-1075 - ex2);
            if (x & rb) != 0 && (x & (rb + rb + rb - 1)) != 0 {
                x += rb + rb;
            }
            x &= !(rb + rb - 1);
        }
    }
    debug_assert!(x as i64 >= 0, "bad double conversion");
    let mut n = x as i64 as f64;
    if neg {
        n = -n;
    }
    if ex2 != 0 {
        n = ldexp(n, ex2);
    }
    n
}

#[inline]
fn neg_i32(x: u64, neg: bool) -> i32 {
    if neg { (!x).wrapping_add(1) as i32 } else { x as i32 }
}

#[inline]
fn neg_u64(x: u64, neg: bool) -> u64 {
    if neg { (!x).wrapping_add(1) } else { x }
}

/// Parse hexadecimal number.
#[allow(clippy::too_many_arguments)] // Mirrors upstream's parser signature.
fn strscan_hex(
    s: &[u8],
    mut p: usize,
    o: &mut Val,
    mut fmt: Fmt,
    opt: u32,
    mut ex2: i32,
    neg: bool,
    dig: u32,
) -> Fmt {
    let mut x: u64 = 0;

    // Scan hex digits.
    for _ in 0..dig.min(16) {
        let mut d = digit_skip_dp(s, &mut p) as u32;
        if d > b'9' as u32 {
            d += 9;
        }
        x = (x << 4) + (d & 15) as u64;
        p += 1;
    }

    // Summarize rounding-effect of excess digits.
    for _ in 16..dig.max(16) {
        x |= (digit_skip_dp(s, &mut p) != b'0') as u64;
        ex2 += 4;
        p += 1;
    }

    // Format-specific handling.
    match fmt {
        Fmt::Int | Fmt::U32 => {
            if fmt == Fmt::Int {
                if opt & OPT_TONUM == 0 && x < 0x8000_0000u64 + neg as u64 && !(x == 0 && neg) {
                    *o = Val::I(neg_i32(x, neg));
                    return Fmt::Int; // Fast path for 32 bit integers.
                }
                if opt & OPT_C == 0 {
                    fmt = Fmt::Num;
                }
            }
            if fmt != Fmt::Num {
                if dig > 8 {
                    return Fmt::Error;
                }
                *o = Val::I(neg_i32(x, neg));
                return Fmt::U32;
            }
        }
        Fmt::I64 | Fmt::U64 => {
            if dig > 16 {
                return Fmt::Error;
            }
            *o = Val::U64(neg_u64(x, neg));
            return fmt;
        }
        _ => {}
    }

    // Reduce range, then convert to double.
    if x & 0xc000_0000_0000_0000 != 0 {
        x = (x >> 2) | (x & 3);
        ex2 += 2;
    }
    *o = Val::N(strscan_double(x, ex2, neg));
    fmt
}

/// Parse octal number.
fn strscan_oct(s: &[u8], mut p: usize, o: &mut Val, mut fmt: Fmt, neg: bool, mut dig: u32) -> Fmt {
    let mut x: u64 = 0;

    // Scan octal digits.
    if dig > 22 || (dig == 22 && at(s, p) > b'1') {
        return Fmt::Error;
    }
    while dig > 0 {
        dig -= 1;
        let c = at(s, p);
        if !(b'0'..=b'7').contains(&c) {
            return Fmt::Error;
        }
        x = (x << 3) + (c & 7) as u64;
        p += 1;
    }

    // Format-specific handling.
    match fmt {
        Fmt::Int | Fmt::U32 => {
            if fmt == Fmt::Int && x >= 0x8000_0000u64 + neg as u64 {
                fmt = Fmt::U32;
            }
            if x >> 32 != 0 {
                return Fmt::Error;
            }
            let u = x as u32;
            *o = Val::I(if neg { (!u).wrapping_add(1) as i32 } else { u as i32 });
        }
        _ => *o = Val::U64(neg_u64(x, neg)),
    }
    fmt
}

/// Parse decimal number.
#[allow(clippy::too_many_arguments)] // Mirrors upstream's parser signature.
fn strscan_dec(
    s: &[u8],
    mut p: usize,
    o: &mut Val,
    mut fmt: Fmt,
    opt: u32,
    mut ex10: i32,
    neg: bool,
    mut dig: u32,
) -> Fmt {
    let mut xi = [0u8; DDIG];
    let mut xip: usize = 0;

    if dig != 0 {
        let mut i = dig;
        if i > MAXDIG {
            ex10 += (i - MAXDIG) as i32;
            i = MAXDIG;
        }
        // Scan unaligned leading digit.
        if ((ex10 ^ i as i32) & 1) != 0 {
            xi[xip] = digit_skip_dp(s, &mut p) & 15;
            xip += 1;
            i -= 1;
            p += 1;
        }
        // Scan aligned double-digits.
        while i > 1 {
            let d = 10 * (digit_skip_dp(s, &mut p) & 15);
            p += 1;
            xi[xip] = d + (digit_skip_dp(s, &mut p) & 15);
            xip += 1;
            p += 1;
            i -= 2;
        }
        // Scan and realign trailing digit.
        if i != 0 {
            xi[xip] = 10 * (digit_skip_dp(s, &mut p) & 15);
            xip += 1;
            ex10 -= 1;
            dig += 1;
            p += 1;
        }

        // Summarize rounding-effect of excess digits.
        if dig > MAXDIG {
            loop {
                if digit_skip_dp(s, &mut p) != b'0' {
                    xi[xip - 1] |= 1;
                    break;
                }
                p += 1;
                dig -= 1;
                if dig <= MAXDIG {
                    break;
                }
            }
            dig = MAXDIG;
        } else {
            // Simplify exponent.
            while ex10 > 0 && dig <= 18 {
                xi[xip] = 0;
                xip += 1;
                ex10 -= 2;
                dig += 2;
            }
        }
    } else {
        // Only got zeros.
        ex10 = 0;
        xi[0] = 0;
    }

    // Fast path for numbers in integer format (but handles e.g. 1e6, too).
    if dig <= 20 && ex10 == 0 {
        let mut x = xi[0] as u64;
        for &d in &xi[1..xip.max(1)] {
            x = x.wrapping_mul(100).wrapping_add(d as u64);
        }
        if !(dig == 20 && (xi[0] > 18 || x as i64 >= 0)) {
            // No overflow.
            let mut plain = false;
            match fmt {
                Fmt::Int | Fmt::U32 => {
                    if fmt == Fmt::Int {
                        if opt & OPT_TONUM == 0 && x < 0x8000_0000u64 + neg as u64 {
                            *o = Val::I(neg_i32(x, neg));
                            return Fmt::Int; // Fast path for 32 bit integers.
                        }
                        if opt & OPT_C == 0 {
                            fmt = Fmt::Num;
                            plain = true;
                        }
                    }
                    if !plain {
                        if x >> 32 != 0 {
                            return Fmt::Error;
                        }
                        *o = Val::I(neg_i32(x, neg));
                        return Fmt::U32;
                    }
                }
                Fmt::I64 | Fmt::U64 => {
                    *o = Val::U64(neg_u64(x, neg));
                    return fmt;
                }
                _ => plain = true,
            }
            // Fast path for plain numbers < 2^63.
            if plain && (x as i64) >= 0 {
                let mut n = x as i64 as f64;
                if neg {
                    n = -n;
                }
                *o = Val::N(n);
                return fmt;
            }
        }
    }

    // Slow non-integer path.
    if fmt == Fmt::Int {
        if opt & OPT_C != 0 {
            return Fmt::Error;
        }
        fmt = Fmt::Num;
    } else if fmt > Fmt::Int {
        return Fmt::Error;
    }

    let mut hi: u32 = 0;
    let mut lo: u32 = xip as u32;
    let mut ex2: i32 = 0;
    let mut idig: i32 = lo as i32 + (ex10 >> 1);

    debug_assert!(lo > 0 && (ex10 & 1) == 0, "bad lo {lo} ex10 {ex10}");

    // Handle simple overflow/underflow.
    if idig > 310 / 2 {
        *o = Val::N(if neg { f64::NEG_INFINITY } else { f64::INFINITY });
        return fmt;
    } else if idig < -326 / 2 {
        *o = Val::N(if neg { -0.0 } else { 0.0 });
        return fmt;
    }

    // Scale up until we have at least 17 or 18 integer part digits.
    while idig < 9 && idig < dlen(lo, hi) {
        let mut cy: u32 = 0;
        ex2 -= 6;
        let mut i = dprev(lo);
        loop {
            let mut d = ((xi[i as usize] as u32) << 6) + cy;
            cy = ((d >> 2) * 5243) >> 17;
            d -= cy * 100; // Div/mod 100.
            xi[i as usize] = d as u8;
            if i == hi {
                break;
            }
            if d == 0 && i == dprev(lo) {
                lo = i;
            }
            i = dprev(i);
        }
        if cy != 0 {
            hi = dprev(hi);
            if xi[dprev(lo) as usize] == 0 {
                lo = dprev(lo);
            } else if hi == lo {
                lo = dprev(lo);
                xi[dprev(lo) as usize] |= xi[lo as usize];
            }
            xi[hi as usize] = cy as u8;
            idig += 1;
        }
    }

    // Scale down until no more than 17 or 18 integer part digits remain.
    while idig > 9 {
        let mut i = hi;
        let mut cy: u32 = 0;
        ex2 += 6;
        loop {
            cy += xi[i as usize] as u32;
            xi[i as usize] = (cy >> 6) as u8;
            cy = 100 * (cy & 0x3f);
            if xi[i as usize] == 0 && i == hi {
                hi = dnext(hi);
                idig -= 1;
            }
            i = dnext(i);
            if i == lo {
                break;
            }
        }
        while cy != 0 {
            if hi == lo {
                xi[dprev(lo) as usize] |= 1;
                break;
            }
            xi[lo as usize] = (cy >> 6) as u8;
            lo = dnext(lo);
            cy = 100 * (cy & 0x3f);
        }
    }

    // Collect integer part digits and convert to rescaled double.
    let mut x = xi[hi as usize] as u64;
    let mut i = dnext(hi);
    loop {
        idig -= 1;
        if !(idig > 0 && i != lo) {
            break;
        }
        x = x * 100 + xi[i as usize] as u64;
        i = dnext(i);
    }
    if i == lo {
        loop {
            idig -= 1;
            if idig < 0 {
                break;
            }
            x *= 100;
        }
    } else {
        // Gather round bit from remaining digits.
        x <<= 1;
        ex2 -= 1;
        loop {
            if xi[i as usize] != 0 {
                x |= 1;
                break;
            }
            i = dnext(i);
            if i == lo {
                break;
            }
        }
    }
    *o = Val::N(strscan_double(x, ex2, neg));
    fmt
}

/// Parse binary number.
#[allow(clippy::too_many_arguments)] // Mirrors upstream's parser signature.
fn strscan_bin(
    s: &[u8],
    mut p: usize,
    o: &mut Val,
    mut fmt: Fmt,
    opt: u32,
    mut ex2: i32,
    neg: bool,
    dig: u32,
) -> Fmt {
    let mut x: u64 = 0;

    if ex2 != 0 || dig > 64 {
        return Fmt::Error;
    }

    // Scan binary digits.
    for _ in 0..dig {
        let c = at(s, p);
        if (c & !1) != b'0' {
            return Fmt::Error;
        }
        x = (x << 1) | (c & 1) as u64;
        p += 1;
    }

    // Format-specific handling.
    match fmt {
        Fmt::Int | Fmt::U32 => {
            if fmt == Fmt::Int {
                if opt & OPT_TONUM == 0 && x < 0x8000_0000u64 + neg as u64 {
                    *o = Val::I(neg_i32(x, neg));
                    return Fmt::Int; // Fast path for 32 bit integers.
                }
                if opt & OPT_C == 0 {
                    fmt = Fmt::Num;
                }
            }
            if fmt != Fmt::Num {
                if dig > 32 {
                    return Fmt::Error;
                }
                *o = Val::I(neg_i32(x, neg));
                return Fmt::U32;
            }
        }
        Fmt::I64 | Fmt::U64 => {
            *o = Val::U64(neg_u64(x, neg));
            return fmt;
        }
        _ => {}
    }

    // Reduce range, then convert to double.
    if x & 0xc000_0000_0000_0000 != 0 {
        x = (x >> 2) | (x & 3);
        ex2 += 2;
    }
    *o = Val::N(strscan_double(x, ex2, neg));
    fmt
}

/// `lj_strscan_scan`: scan a string containing a number with the given `OPT_*` flags.
pub fn scan(s: &[u8], opt: u32) -> StrScan {
    let mut o = Val::N(0.0);
    let fmt = scan_raw(s, &mut o, opt);
    match (fmt, o) {
        (Fmt::Error, _) => StrScan::Error,
        (Fmt::Num, Val::N(n)) => StrScan::Num(n),
        (Fmt::Imag, Val::N(n)) => StrScan::Imag(n),
        (Fmt::Int, Val::I(i)) => StrScan::Int(i),
        (Fmt::U32, Val::I(i)) => StrScan::U32(i as u32),
        (Fmt::I64, Val::U64(u)) => StrScan::I64(u as i64),
        (Fmt::U64, Val::U64(u)) => StrScan::U64(u),
        (f, _) => unreachable!("strscan: format {f:?} with mismatched value"),
    }
}

fn scan_raw(s: &[u8], o: &mut Val, opt: u32) -> Fmt {
    let pe = s.len();
    let mut p: usize = 0;
    let mut neg = false;

    // Remove leading space, parse sign and non-numbers.
    if !isdigit(at(s, p)) {
        while isspace(at(s, p)) {
            p += 1;
        }
        if at(s, p) == b'+' || at(s, p) == b'-' {
            neg = at(s, p) == b'-';
            p += 1;
        }
        if at(s, p) >= b'A' {
            // Parse "inf", "infinity" or "nan".
            // setnanV: the canonical (x86 default) NaN, sign bit set.
            let mut tmp = f64::from_bits(0xfff8_0000_0000_0000);
            if casecmp(at(s, p), b'i') && casecmp(at(s, p + 1), b'n') && casecmp(at(s, p + 2), b'f') {
                tmp = if neg { f64::NEG_INFINITY } else { f64::INFINITY };
                p += 3;
                if casecmp(at(s, p), b'i')
                    && casecmp(at(s, p + 1), b'n')
                    && casecmp(at(s, p + 2), b'i')
                    && casecmp(at(s, p + 3), b't')
                    && casecmp(at(s, p + 4), b'y')
                {
                    p += 5;
                }
            } else if casecmp(at(s, p), b'n') && casecmp(at(s, p + 1), b'a') && casecmp(at(s, p + 2), b'n') {
                p += 3;
            }
            while isspace(at(s, p)) {
                p += 1;
            }
            if at(s, p) != 0 || p < pe {
                return Fmt::Error;
            }
            *o = Val::N(tmp);
            return Fmt::Num;
        }
    }

    // Parse regular number.
    let mut fmt = Fmt::Int;
    let mut cmask = chars::DIGIT;
    let mut base: u32 = if opt & OPT_C != 0 && at(s, p) == b'0' { 0 } else { 10 };
    let mut dp: Option<usize> = None;
    let mut dig: u32 = 0;
    let mut hasdig = false;
    let mut x: u32 = 0;
    let mut ex: i32 = 0;

    // Determine base and skip leading zeros.
    if at(s, p) <= b'0' {
        if at(s, p) == b'0' {
            if casecmp(at(s, p + 1), b'x') {
                base = 16;
                cmask = chars::XDIGIT;
                p += 2;
            } else if casecmp(at(s, p + 1), b'b') {
                base = 2;
                cmask = chars::DIGIT;
                p += 2;
            }
        }
        loop {
            let c = at(s, p);
            if c == b'0' {
                hasdig = true;
            } else if c == b'.' {
                if dp.is_some() {
                    return Fmt::Error;
                }
                dp = Some(p);
            } else {
                break;
            }
            p += 1;
        }
    }

    // Preliminary digit and decimal point scan.
    let sp = p;
    loop {
        let c = at(s, p);
        if chars::isa(c, cmask) {
            x = x.wrapping_mul(10).wrapping_add((c & 15) as u32); // For fast path below.
            dig += 1;
        } else if c == b'.' {
            if dp.is_some() {
                return Fmt::Error;
            }
            dp = Some(p);
        } else {
            break;
        }
        p += 1;
    }
    if !hasdig && dig == 0 {
        return Fmt::Error;
    }

    // Handle decimal point.
    if let Some(dpos) = dp {
        if base == 2 {
            return Fmt::Error;
        }
        fmt = Fmt::Num;
        if dig != 0 {
            ex = dpos as i32 - (p as i32 - 1);
            let mut d = p as isize - 1;
            // Skip trailing zeros.
            while ex < 0 {
                let c = s[d as usize];
                d -= 1;
                if c != b'0' {
                    break;
                }
                ex += 1;
                dig -= 1;
            }
            if ex <= -(MAXEXP as i32) {
                return Fmt::Error;
            }
            if base == 16 {
                ex *= 4;
            }
        }
    }

    // Parse exponent.
    if base >= 10 && casecmp(at(s, p), if base == 16 { b'p' } else { b'e' }) {
        let mut negx = false;
        fmt = Fmt::Num;
        p += 1;
        if at(s, p) == b'+' || at(s, p) == b'-' {
            negx = at(s, p) == b'-';
            p += 1;
        }
        if !isdigit(at(s, p)) {
            return Fmt::Error;
        }
        let mut xx = (at(s, p) & 15) as u32;
        p += 1;
        while isdigit(at(s, p)) {
            xx = xx * 10 + (at(s, p) & 15) as u32;
            if xx >= MAXEXP {
                return Fmt::Error;
            }
            p += 1;
        }
        ex += if negx { (!xx).wrapping_add(1) as i32 } else { xx as i32 };
    }

    // Parse suffix.
    if at(s, p) != 0 {
        // I (IMAG), U (U32), LL (I64), ULL/LLU (U64), L (long), UL/LU (ulong).
        // NYI upstream: f (float).
        if casecmp(at(s, p), b'i') {
            if opt & OPT_IMAG == 0 {
                return Fmt::Error;
            }
            p += 1;
            fmt = Fmt::Imag;
        } else if fmt == Fmt::Int {
            if casecmp(at(s, p), b'u') {
                p += 1;
                fmt = Fmt::U32;
            }
            if casecmp(at(s, p), b'l') {
                p += 1;
                if casecmp(at(s, p), b'l') {
                    p += 1;
                    fmt = fmt.add(Fmt::I64 as u32 - Fmt::Int as u32);
                } else if opt & OPT_C == 0 {
                    return Fmt::Error;
                } else {
                    // sizeof(long) == 8 on x64 SysV.
                    fmt = fmt.add(Fmt::I64 as u32 - Fmt::Int as u32);
                }
            }
            if casecmp(at(s, p), b'u') && (fmt == Fmt::Int || fmt == Fmt::I64) {
                p += 1;
                fmt = fmt.add(Fmt::U32 as u32 - Fmt::Int as u32);
            }
            if (fmt == Fmt::U32 && opt & OPT_C == 0) || (fmt >= Fmt::I64 && opt & OPT_LL == 0) {
                return Fmt::Error;
            }
        }
        while isspace(at(s, p)) {
            p += 1;
        }
        if at(s, p) != 0 {
            return Fmt::Error;
        }
    }
    if p < pe {
        return Fmt::Error;
    }

    // Fast path for decimal 32 bit integers.
    if fmt == Fmt::Int
        && base == 10
        && (dig < 10 || (dig == 10 && at(s, sp) <= b'2' && (x as u64) < 0x8000_0000u64 + neg as u64))
    {
        if opt & OPT_TONUM != 0 {
            *o = Val::N(if neg { -(x as f64) } else { x as f64 });
            return Fmt::Num;
        } else if x == 0 && neg {
            *o = Val::N(-0.0);
            return Fmt::Num;
        } else {
            *o = Val::I(neg_i32(x as u64, neg));
            return Fmt::Int;
        }
    }

    // Dispatch to base-specific parser.
    if base == 0 && !(fmt == Fmt::Num || fmt == Fmt::Imag) {
        return strscan_oct(s, sp, o, fmt, neg, dig);
    }
    let fmt = match base {
        16 => strscan_hex(s, sp, o, fmt, opt, ex, neg, dig),
        2 => strscan_bin(s, sp, o, fmt, opt, ex, neg, dig),
        _ => strscan_dec(s, sp, o, fmt, opt, ex, neg, dig),
    };

    // Try to convert number to integer, if requested.
    if fmt == Fmt::Num
        && opt & OPT_TOINT != 0
        && let Val::N(n) = *o
        && let Some(i) = super::num2int_check(n)
        && n.to_bits() != (-0.0f64).to_bits()
    {
        *o = Val::I(i);
        return Fmt::Int;
    }
    fmt
}

/// `lj_strscan_num`: convert a string to a double (`tonumber`, string arithmetic coercion).
pub fn scan_num(s: &[u8]) -> Option<f64> {
    match scan(s, OPT_TONUM) {
        StrScan::Num(n) => Some(n),
        StrScan::Error => None,
        other => unreachable!("strscan_num: bad scan format {other:?}"),
    }
}

/// `tonumber(s, base)` for `base != 10` (lib_base.c), using glibc `strtoul` semantics.
///
/// Leading/trailing whitespace is skipped, one sign is accepted, base 16 accepts a
/// `0x` prefix and base 2 a `0b` prefix (glibc C23 `strtoul`), overflow saturates to
/// `2^64-1`. A NUL byte terminates the string. Returns `None` where upstream returns
/// `nil`. `base` must already be validated to lie in `2..=36`.
pub fn parse_int_base(s: &[u8], base: u32) -> Option<f64> {
    debug_assert!((2..=36).contains(&base), "bad base {base}");
    let mut p = 0;
    let mut neg = false;
    while isspace(at(s, p)) {
        p += 1;
    }
    if at(s, p) == b'-' {
        p += 1;
        neg = true;
    } else if at(s, p) == b'+' {
        p += 1;
    }
    if !chars::isalnum(at(s, p)) {
        return None;
    }
    let digit = |c: u8| -> Option<u32> {
        let d = match c {
            b'0'..=b'9' => (c - b'0') as u32,
            b'a'..=b'z' => (c - b'a') as u32 + 10,
            b'A'..=b'Z' => (c - b'A') as u32 + 10,
            _ => return None,
        };
        (d < base).then_some(d)
    };
    // strtoul: the prefix is only consumed if a valid digit follows it.
    if at(s, p) == b'0'
        && ((base == 16 && casecmp(at(s, p + 1), b'x')) || (base == 2 && casecmp(at(s, p + 1), b'b')))
        && digit(at(s, p + 2)).is_some()
    {
        p += 2;
    }
    let start = p;
    let mut ul: u64 = 0;
    let mut overflow = false;
    while let Some(d) = digit(at(s, p)) {
        match ul.checked_mul(base as u64).and_then(|v| v.checked_add(d as u64)) {
            Some(v) => ul = v,
            None => overflow = true,
        }
        p += 1;
    }
    if p == start {
        return None;
    }
    if overflow {
        ul = u64::MAX;
    }
    while isspace(at(s, p)) {
        p += 1;
    }
    if at(s, p) != 0 {
        return None;
    }
    let n = ul as f64;
    Some(if neg { -n } else { n })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hexf(s: &str) -> f64 {
        // Parse the reference's `%a` output: [-]0x<h>[.<hhh>]p<exp>.
        let (neg, s) = match s.strip_prefix('-') {
            Some(r) => (true, r),
            None => (false, s),
        };
        let s = s.strip_prefix("0x").unwrap();
        let (mant, exp) = s.split_once('p').unwrap();
        let (ip, fp) = mant.split_once('.').unwrap_or((mant, ""));
        let mut m = u64::from_str_radix(ip, 16).unwrap() as f64;
        for (i, c) in fp.chars().enumerate() {
            m += c.to_digit(16).unwrap() as f64 * 16f64.powi(-(i as i32 + 1));
        }
        let v = ldexp(m, exp.parse().unwrap());
        if neg { -v } else { v }
    }

    #[test]
    fn tonumber_cases() {
        // luajit -e 'print(string.format("%a", tonumber(s)))'
        let cases: &[(&[u8], Option<&str>)] = &[
            (b"0x10", Some("0x1p+4")),
            (b"0x1p4", Some("0x1p+4")),
            (b"1e", None),
            (b" 10 ", Some("0x1.4p+3")),
            (b"1e400", Some("inf")),
            (b"0x", None),
            (b".5", Some("0x1p-1")),
            (b"5.", Some("0x1.4p+2")),
            (b"0x.8", Some("0x1p-1")),
            (b"0x1P-1074", Some("0x1p-1074")),
            (b"0x1p-1075", Some("0x0p+0")),
            (b"1e-400", Some("0x0p+0")),
            (b"-Infinity", Some("-inf")),
            (b"0b101", Some("0x1.4p+2")),
            (b"0b2", None),
            (b"1..2", None),
            (b"1e+", None),
            (b"2.5e-3", Some("0x1.47ae147ae147bp-9")),
            (b"4.9406564584124654e-324", Some("0x1p-1074")),
            (b"2.4703282292062328e-324", Some("0x1p-1074")),
            (b"2.4703282292062327e-324", Some("0x0p+0")),
            (b"1.7976931348623157e308", Some("0x1.fffffffffffffp+1023")),
            (b"1.7976931348623159e308", Some("inf")),
            (b"123456789012345678901234567890", Some("0x1.8ee90ff6c373ep+96")),
            (b"-0", Some("-0x0p+0")),
            (b"0x7fffffffffffffff", Some("0x1p+63")),
            (b"0xffffffffffffffffff", Some("0x1p+72")),
            (b"\t\n12\x0b", Some("0x1.8p+3")),
            (b"12i", None),
            (b"1LL", None),
            (b"1e5x", None),
            (b".", None),
            (b"", None),
            (b".e1", None),
            (b"0x1.fffffffffffff8p0", Some("0x1p+1")),
            (b"00012", Some("0x1.8p+3")),
            (b"1.", Some("0x1p+0")),
            (b"-.5e1", Some("-0x1.4p+2")),
            (b"1\0", None),
        ];
        for &(s, want) in cases {
            let got = scan_num(s);
            match want {
                None => assert_eq!(got, None, "{:?}", String::from_utf8_lossy(s)),
                Some("inf") => assert_eq!(got, Some(f64::INFINITY)),
                Some("-inf") => assert_eq!(got, Some(f64::NEG_INFINITY)),
                Some(h) => assert_eq!(got.map(f64::to_bits), Some(hexf(h).to_bits()), "{:?}", String::from_utf8_lossy(s)),
            }
        }
        assert!(scan_num(b" nan ").unwrap().is_nan());
        assert_eq!(scan_num(b"inf"), Some(f64::INFINITY));
    }

    #[test]
    fn lexer_and_c_formats() {
        let lex = OPT_TONUM | OPT_LL | OPT_IMAG;
        // luajit -e 'print(12LL, 0x10ULL, 1e2i, 18446744073709551615ULL, 12lu)'
        assert_eq!(scan(b"12LL", lex), StrScan::I64(12));
        assert_eq!(scan(b"0x10ULL", lex), StrScan::U64(16));
        assert_eq!(scan(b"18446744073709551615ULL", lex), StrScan::U64(u64::MAX));
        assert_eq!(scan(b"1e2i", lex), StrScan::Imag(100.0));
        assert_eq!(scan(b"0x8000000000000000LL", lex), StrScan::I64(i64::MIN));
        assert_eq!(scan(b"12lu", lex), StrScan::Error); // `malformed number near '12lu'`
        assert_eq!(scan(b"12u", lex), StrScan::Error);
        assert_eq!(scan(b"1.5LL", lex), StrScan::Error);
        assert_eq!(scan(b"3", lex), StrScan::Num(3.0));
        // Without TONUM, small integers come back as Int.
        assert_eq!(scan(b"-7", 0), StrScan::Int(-7));
        assert_eq!(scan(b"-0", 0), StrScan::Num(-0.0));
        assert_eq!(scan(b"2147483648", 0), StrScan::Num(2147483648.0));
        assert_eq!(scan(b"1.0", OPT_TOINT), StrScan::Int(1));
        // C mode (ffi.cdef 'static const unsigned int X = s;').
        assert_eq!(scan(b"0x1f", OPT_C), StrScan::Int(31));
        assert_eq!(scan(b"017", OPT_C), StrScan::Int(15));
        assert_eq!(scan(b"08", OPT_C), StrScan::Error);
        assert_eq!(scan(b"4294967295", OPT_C), StrScan::U32(u32::MAX));
        assert_eq!(scan(b"4294967296", OPT_C), StrScan::Error);
        assert_eq!(scan(b"0xffffffff", OPT_C), StrScan::U32(u32::MAX));
        assert_eq!(scan(b"037777777777", OPT_C), StrScan::U32(u32::MAX));
        assert_eq!(scan(b"37777777777", OPT_C), StrScan::Error);
        assert_eq!(scan(b"10u", OPT_C), StrScan::U32(10));
        assert_eq!(scan(b"1e5", OPT_C), StrScan::Num(100000.0)); // rejected by cparse
    }

    #[test]
    fn tonumber_base() {
        // luajit -e 'print(tonumber("ff",16), tonumber("  -ff  ",16), tonumber("zz",36), tonumber("0b11",2),
        //   tonumber("0x",16), tonumber("8",8), tonumber("777",8), tonumber("1 1",2), tonumber("",16),
        //   tonumber("ffffffffffffffffffffff",16), tonumber("0x10",16), tonumber("10\0ab",16), tonumber("-0",16))'
        assert_eq!(parse_int_base(b"ff", 16), Some(255.0));
        assert_eq!(parse_int_base(b"  -ff  ", 16), Some(-255.0));
        assert_eq!(parse_int_base(b"zz", 36), Some(1295.0));
        assert_eq!(parse_int_base(b"0b11", 2), Some(3.0));
        assert_eq!(parse_int_base(b"0x", 16), None);
        assert_eq!(parse_int_base(b"8", 8), None);
        assert_eq!(parse_int_base(b"777", 8), Some(511.0));
        assert_eq!(parse_int_base(b"1 1", 2), None);
        assert_eq!(parse_int_base(b"", 16), None);
        assert_eq!(parse_int_base(b"ffffffffffffffffffffff", 16), Some(18446744073709551615.0));
        assert_eq!(parse_int_base(b"0x10", 16), Some(16.0));
        assert_eq!(parse_int_base(b"10\0ab", 16), Some(16.0));
        assert_eq!(parse_int_base(b"-0", 16).map(f64::to_bits), Some((-0.0f64).to_bits()));
    }
}
