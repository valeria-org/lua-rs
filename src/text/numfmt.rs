//! Port of lj_strfmt_num.c: exact formatting of doubles for `%a %e %f %g` (and
//! `%.14g`, the number → string conversion used by `tostring` and concatenation).
//!
//! The algorithm converts the double to a fixed-precision base-1e9 representation
//! ("nd" format) and rounds half-to-even exactly, so results match glibc printf.

use super::strfmt::{F_ALT, F_LEFT, F_PLUS, F_SPACE, F_UPPER, F_ZERO, SFormat, T_FP_E, T_FP_F, wint};

/* -- Precomputed tables -------------------------------------------------- */

/// Rescale factors to push the exponent of a number towards zero.
static RESCALE_E: [i16; 32] = [
    -308, -289, -270, -250, -231, -212, -193, -173, -154, -135, -115, -96, -77, -58, -38, 0, 0, 0, 39,
    58, 77, 96, 116, 135, 154, 174, 193, 212, 231, 251, 270, 289,
];
static RESCALE_N: [f64; 32] = [
    1e308, 1e289, 1e270, 1e250, 1e231, 1e212, 1e193, 1e173, 1e154, 1e135, 1e115, 1e96, 1e77, 1e58,
    1e38, 1e0, 1e0, 1e0, 1e-39, 1e-58, 1e-77, 1e-96, 1e-116, 1e-135, 1e-154, 1e-174, 1e-193, 1e-212,
    1e-231, 1e-251, 1e-270, 1e-289,
];

/// For p in range -70 through 57, pairs (m, e) such that 4*2^p <= (u8)m*10^e,
/// and is the smallest value for which this holds.
static FOUR_ULP_M_E: [i8; 256] = [
    34, -21, 68, -21, 14, -20, 28, -20, 55, -20, 2, -19, 3, -19, 5, -19, 9, -19, -82, -18, 35, -18, 7,
    -17, -117, -17, 28, -17, 56, -17, 112, -16, -33, -16, 45, -16, 89, -16, -78, -15, 36, -15, 72,
    -15, -113, -14, 29, -14, 57, -14, 114, -13, -28, -13, 46, -13, 91, -12, -74, -12, 37, -12, 73,
    -12, 15, -11, 3, -11, 59, -11, 2, -10, 3, -10, 5, -10, 1, -9, -69, -9, 38, -9, 75, -9, 15, -7, 3,
    -7, 6, -7, 12, -6, -17, -7, 48, -7, 96, -7, -65, -6, 39, -6, 77, -6, -103, -5, 31, -5, 62, -5,
    123, -4, -11, -4, 49, -4, 98, -4, -60, -3, 4, -2, 79, -3, 16, -2, 32, -2, 63, -2, 2, -1, 25, 0,
    5, 1, 1, 2, 2, 2, 4, 2, 8, 2, 16, 2, 32, 2, 64, 2, -128, 2, 26, 2, 52, 2, 103, 3, -51, 3, 41, 4,
    82, 4, -92, 4, 33, 4, 66, 4, -124, 5, 27, 5, 53, 5, 105, 6, 21, 6, 42, 6, 84, 6, 17, 7, 34, 7,
    68, 7, 2, 8, 3, 8, 6, 8, 108, 9, -41, 9, 43, 10, 86, 9, -84, 10, 35, 10, 69, 10, -118, 11, 28,
    11, 55, 12, 11, 13, 22, 13, 44, 13, 88, 13, -80, 13, 36, 13, 71, 13, -115, 14, 29, 14, 57, 14,
    113, 15, -30, 15, 46, 15, 91, 15, 19, 16, 37, 16, 73, 16, 2, 17, 3, 17, 6, 17,
];

/// min(2^32-1, 10^e-1) for e in range 0 through 10.
static NDIGITS_DEC_THRESHOLD: [u32; 11] =
    [0, 9, 99, 999, 9999, 99999, 999999, 9999999, 99999999, 999999999, 0xffffffff];

/* -- Helper functions ---------------------------------------------------- */

#[inline]
fn fls(x: u32) -> u32 {
    31 - x.leading_zeros()
}

#[inline]
fn ffs(x: u32) -> u32 {
    x.trailing_zeros()
}

/// Number of digits in the decimal representation of x.
fn ndigits_dec(x: u32) -> u32 {
    let t = ((fls(x | 1) * 77) >> 8) + 1; // 2^8/77 is roughly log2(10)
    t + (x > NDIGITS_DEC_THRESHOLD[t as usize]) as u32
}

/// Write a 9-digit zero-padded unsigned integer.
fn wuint9(buf: &mut [u8], p: usize, mut u: u32) -> usize {
    for i in (0..9).rev() {
        buf[p + i] = b'0' + (u % 10) as u8;
        u /= 10;
    }
    p + 9
}

fn uint9(u: u32) -> [u8; 9] {
    let mut b = [0u8; 9];
    wuint9(&mut b, 0, u);
    b
}

/* -- Extended precision arithmetic --------------------------------------- */

const ND_MUL2K_MAX_SHIFT: u32 = 29;

/// Multiply nd by 2^k and add carry_in (ndlo is assumed to be zero).
fn nd_mul2k(nd: &mut [u32; 64], mut ndhi: u32, mut k: u32, mut carry_in: u32, sf: SFormat) -> u32 {
    let mut ndlo: u32 = 0;
    let mut start: u32 = 1;
    // Performance hacks.
    if k > ND_MUL2K_MAX_SHIFT * 2 && sf.fp() != SFormat(T_FP_F).fp() {
        start = ndhi.wrapping_sub(sf.prec_raw().wrapping_add(17) / 8);
    }
    // Real logic.
    while k >= ND_MUL2K_MAX_SHIFT {
        for i in ndlo..=ndhi {
            let val = ((nd[i as usize] as u64) << ND_MUL2K_MAX_SHIFT) | carry_in as u64;
            carry_in = (val / 1_000_000_000) as u32;
            nd[i as usize] = (val as u32).wrapping_sub(carry_in.wrapping_mul(1_000_000_000));
        }
        if carry_in != 0 {
            ndhi += 1;
            nd[ndhi as usize] = carry_in;
            carry_in = 0;
            if start == ndlo {
                ndlo += 1;
            }
            start = start.wrapping_add(1);
        }
        k -= ND_MUL2K_MAX_SHIFT;
    }
    if k != 0 {
        for i in ndlo..=ndhi {
            let val = ((nd[i as usize] as u64) << k) | carry_in as u64;
            carry_in = (val / 1_000_000_000) as u32;
            nd[i as usize] = (val as u32).wrapping_sub(carry_in.wrapping_mul(1_000_000_000));
        }
        if carry_in != 0 {
            ndhi += 1;
            nd[ndhi as usize] = carry_in;
        }
    }
    ndhi
}

/// Divide nd by 2^k (ndlo is assumed to be zero). Returns the new ndlo.
fn nd_div2k(nd: &mut [u32; 64], mut ndhi: u32, mut k: u32, sf: SFormat) -> u32 {
    let mut ndlo: u32 = 0;
    let mut stop1: u32 = !0;
    let mut stop2: u32 = !0;
    // Performance hacks.
    if ndhi == 0 {
        if nd[0] == 0 {
            return 0;
        }
        let s = ffs(nd[0]);
        if s >= k {
            nd[0] >>= k;
            return 0;
        }
        nd[0] >>= s;
        k -= s;
    }
    if k > 18 && sf.fp() != SFormat(T_FP_F).fp() {
        // (For %f, precision must not be limited here or nd_round cannot round to even.)
        let floorlog2 = (ndhi.wrapping_mul(29).wrapping_add(fls(nd[ndhi as usize])).wrapping_sub(k)) as i32;
        let floorlog10 = (floorlog2 as f64 * 0.30102999566398114) as i32;
        stop1 = (62 + (floorlog10 - sf.prec_raw() as i32) / 9) as u32;
        stop2 = 61u32.wrapping_add(ndhi).wrapping_sub(((sf.prec_raw() as i32) / 8) as u32);
    }
    // Real logic.
    while k >= 9 {
        let mut i = ndhi;
        let mut carry: u32 = 0;
        loop {
            let val = nd[i as usize];
            nd[i as usize] = (val >> 9) + carry;
            carry = (val & 0x1ff) * 1953125;
            if i == ndlo {
                break;
            }
            i = (i.wrapping_sub(1)) & 0x3f;
        }
        if ndlo != stop1 && ndlo != stop2 {
            if carry != 0 {
                ndlo = (ndlo.wrapping_sub(1)) & 0x3f;
                nd[ndlo as usize] = carry;
            }
            if nd[ndhi as usize] == 0 {
                ndhi = (ndhi.wrapping_sub(1)) & 0x3f;
                stop2 = stop2.wrapping_sub(1);
            }
        } else if nd[ndhi as usize] == 0 {
            if ndhi != ndlo {
                ndhi = (ndhi.wrapping_sub(1)) & 0x3f;
                stop2 = stop2.wrapping_sub(1);
            } else {
                return ndlo;
            }
        }
        k -= 9;
    }
    if k != 0 {
        let mask = (1u32 << k) - 1;
        let mul = 1_000_000_000u32 >> k;
        let mut i = ndhi;
        let mut carry: u32 = 0;
        loop {
            let val = nd[i as usize];
            nd[i as usize] = (val >> k) + carry;
            carry = (val & mask) * mul;
            if i == ndlo {
                break;
            }
            i = (i.wrapping_sub(1)) & 0x3f;
        }
        if carry != 0 {
            ndlo = (ndlo.wrapping_sub(1)) & 0x3f;
            nd[ndlo as usize] = carry;
        }
    }
    ndlo
}

/// Add m*10^e to nd (assumes ndlo <= e/9 <= ndhi and 0 <= m <= 9).
fn nd_add_m10e(nd: &mut [u32; 64], mut ndhi: u32, m: u8, e: i32) -> u32 {
    let (mut i, mut carry);
    if e >= 0 {
        i = e as u32 / 9;
        carry = (m as u32).wrapping_mul(NDIGITS_DEC_THRESHOLD[(e - i as i32 * 9) as usize] + 1);
    } else {
        let f = (e - 8) / 9;
        i = (64 + f) as u32;
        carry = (m as u32).wrapping_mul(NDIGITS_DEC_THRESHOLD[(e - f * 9) as usize] + 1);
    }
    loop {
        let mut val = nd[i as usize].wrapping_add(carry);
        if val >= 1_000_000_000 {
            val -= 1_000_000_000;
            nd[i as usize] = val;
            if i == ndhi {
                ndhi = (ndhi + 1) & 0x3f;
                nd[ndhi as usize] = 1;
                break;
            }
            carry = 1;
            i = (i + 1) & 0x3f;
        } else {
            nd[i as usize] = val;
            break;
        }
    }
    ndhi
}

/// Round to even with given precision. Extra digits are not zeroed.
fn nd_round(nd: &mut [u32; 64], ndlo: u32, ndhi: u32, e: i32) -> u32 {
    let mut i: u32;
    let d: i32;
    if e >= 0 {
        i = e as u32 / 9;
        d = 8 - e + i as i32 * 9;
    } else {
        let f = (e - 8) / 9;
        i = (64 + f) as u32;
        d = 8 - e + f * 9;
    }
    let buf = uint9(nd[i as usize]);
    let round_up = if buf[d as usize] < b'5' {
        false
    } else if buf[d as usize] == b'5' {
        // Must check for round to even.
        let odd = if d != 0 { buf[d as usize - 1] & 1 } else { (nd[((i + 1) & 0x3f) as usize] & 1) as u8 };
        // Round up '[13579]5.*' or '[02468]5[^0]*'.
        odd != 0 || buf[(d as usize + 1)..].iter().any(|&c| c != b'0') || {
            // Check remaining fraction.
            let mut up = false;
            while i != ndlo {
                if nd[i as usize] != 0 {
                    up = true;
                    break;
                }
                i = (i.wrapping_sub(1)) & 0x3f;
            }
            up
        }
    } else {
        true
    };
    if round_up { nd_add_m10e(nd, ndhi, 5, e) } else { ndhi }
}

/// Test whether two "nd" values are equal in their most significant digits.
fn nd_similar(nd: &[u32; 64], mut ndhi: u32, mut r: usize, hilen: u32, mut prec: u32) -> bool {
    if hilen <= prec {
        if nd[ndhi as usize] != nd[r] {
            return false;
        }
        prec -= hilen;
        r -= 1;
        ndhi = (ndhi.wrapping_sub(1)) & 0x3f;
        if prec >= 9 {
            if nd[ndhi as usize] != nd[r] {
                return false;
            }
            prec -= 9;
            r -= 1;
            ndhi = (ndhi.wrapping_sub(1)) & 0x3f;
        }
    } else {
        prec = prec.wrapping_sub(hilen.wrapping_sub(9));
    }
    debug_assert!(prec < 9, "bad precision {prec}");
    let nd9 = uint9(nd[ndhi as usize]);
    let ref9 = uint9(nd[r]);
    let prec = prec as usize;
    nd9[..prec] == ref9[..prec] && (nd9[prec] < b'5') == (ref9[prec] < b'5')
}

/* -- Formatted conversions to buffer ------------------------------------- */

/// Output cursor over a pre-sized region of the output vector (mirrors the raw `char *p`
/// of upstream, which may move backwards over over-emitted digits).
struct W<'a> {
    b: &'a mut Vec<u8>,
    p: usize,
}

impl W<'_> {
    /// `lj_buf_more`: make room for `n` bytes (plus slack for over-emitted digits).
    fn more(&mut self, n: u32) {
        let need = self.p + n as usize + 16;
        if self.b.len() < need {
            self.b.resize(need, 0);
        }
    }
    #[inline]
    fn put(&mut self, c: u8) {
        self.b[self.p] = c;
        self.p += 1;
    }
    fn pad(&mut self, width: &mut u32, len: u32, c: u8) {
        while *width > len {
            *width -= 1;
            self.put(c);
        }
    }
    fn wint(&mut self, k: i32) {
        self.p = wint(self.b, self.p, k);
    }
    fn wuint9(&mut self, u: u32) {
        self.p = wuint9(self.b, self.p, u);
    }
    fn back(&self, off: usize) -> u8 {
        self.b[self.p - off]
    }
    fn advance(&mut self, delta: i32) {
        self.p = (self.p as isize + delta as isize) as usize;
    }
}

/// `lj_strfmt_putfnum`: append `n` formatted according to an FP format (`%a %e %f %g`).
pub fn put_fnum(out: &mut Vec<u8>, sf: SFormat, n: f64) {
    let start = out.len();
    let mut w = W { b: out, p: start };
    wfnum(&mut w, sf, n);
    let end = w.p;
    out.truncate(end);
}

/// `lj_strfmt_putnum`: append `n` formatted as `%.14g`.
pub fn put_num(out: &mut Vec<u8>, n: f64) {
    put_fnum(out, SFormat::G14, n);
}

/// `lj_strfmt_num`: number → string conversion (`tostring`, concatenation, `print`).
pub fn fmt_num(n: f64) -> Vec<u8> {
    let mut v = Vec::with_capacity(super::strfmt::MAXBUF_NUM);
    put_num(&mut v, n);
    v
}

fn wfnum(w: &mut W<'_>, sf: SFormat, n: f64) {
    let sfv = sf.0;
    let has = |f: u32| sfv & f != 0;
    let mut width = sf.width();
    let mut prec = sf.prec_raw();
    let len: u32;
    let mut t: u64 = n.to_bits();
    let hi = |t: u64| (t >> 32) as u32;
    let lo = |t: u64| t as u32;

    if (hi(t) << 1) >= 0xffe0_0000 {
        // Handle non-finite values uniformly for %a, %e, %f, %g.
        let mut prefix: u8 = 0;
        let mut ch: u32 = if has(F_UPPER) { 0x202020 } else { 0 };
        if ((hi(t) & 0x000f_ffff) | lo(t)) != 0 {
            ch ^= ((b'n' as u32) << 16) | ((b'a' as u32) << 8) | b'n' as u32;
            if has(F_SPACE) {
                prefix = b' ';
            }
        } else {
            ch ^= ((b'i' as u32) << 16) | ((b'n' as u32) << 8) | b'f' as u32;
            if hi(t) & 0x8000_0000 != 0 {
                prefix = b'-';
            } else if has(F_PLUS) {
                prefix = b'+';
            } else if has(F_SPACE) {
                prefix = b' ';
            }
        }
        len = 3 + (prefix != 0) as u32;
        w.more(width.max(len));
        if !has(F_LEFT) {
            w.pad(&mut width, len, b' ');
        }
        if prefix != 0 {
            w.put(prefix);
        }
        w.put((ch >> 16) as u8);
        w.put((ch >> 8) as u8);
        w.put(ch as u8);
    } else if sf.fp() == SFormat(super::strfmt::T_FP_A).fp() {
        // %a
        let hexdig: &[u8; 18] = if has(F_UPPER) { b"0123456789ABCDEFPX" } else { b"0123456789abcdefpx" };
        let mut e = ((hi(t) >> 20) & 0x7ff) as i32;
        let mut prefix: u8 = 0;
        let mut eprefix = b'+';
        if hi(t) & 0x8000_0000 != 0 {
            prefix = b'-';
        } else if has(F_PLUS) {
            prefix = b'+';
        } else if has(F_SPACE) {
            prefix = b' ';
        }
        t &= 0x000f_ffff_ffff_ffff;
        if e != 0 {
            t |= 0x0010_0000_0000_0000;
            e -= 1023;
        } else if t != 0 {
            // Non-zero denormal - normalise it.
            let shift = if hi(t) != 0 { 20 - fls(hi(t)) } else { 52 - fls(lo(t)) };
            e = -1022 - shift as i32;
            t <<= shift;
        }
        // abs(n) == t * 2^(e - 52); if n != 0, bit 52 of t is the highest set bit.
        if (prec as i32) < 0 {
            // Default precision: use smallest precision giving exact result.
            prec = if lo(t) != 0 { 13 - ffs(lo(t)) / 4 } else { 5 - ffs(hi(t) | 0x100000) / 4 };
        } else if prec < 13 {
            // Precision is sufficiently low as to maybe require rounding.
            t += 1u64 << (51 - prec * 4);
        }
        if e < 0 {
            eprefix = b'-';
            e = -e;
        }
        len = 5 + ndigits_dec(e as u32) + prec + (prefix != 0) as u32 + ((prec | (sfv & F_ALT)) != 0) as u32;
        w.more(width.max(len));
        if !has(F_LEFT | F_ZERO) {
            w.pad(&mut width, len, b' ');
        }
        if prefix != 0 {
            w.put(prefix);
        }
        w.put(b'0');
        w.put(hexdig[17]); // x or X
        if sfv & (F_LEFT | F_ZERO) == F_ZERO {
            w.pad(&mut width, len, b'0');
        }
        w.put(b'0' + (hi(t) >> 20) as u8); // Usually '1', sometimes '0' or '2'.
        if (prec | (sfv & F_ALT)) != 0 {
            // Emit fractional part.
            let q = w.p + 1 + prec as usize;
            let p = w.p;
            w.b[p] = b'.';
            if prec < 13 {
                t >>= 52 - prec * 4;
            } else {
                while prec > 13 {
                    w.b[p + prec as usize] = b'0';
                    prec -= 1;
                }
            }
            while prec != 0 {
                w.b[p + prec as usize] = hexdig[(t & 15) as usize];
                prec -= 1;
                t >>= 4;
            }
            w.p = q;
        }
        w.put(hexdig[16]); // p or P
        w.put(eprefix); // + or -
        w.wint(e);
    } else {
        // %e or %f or %g - begin by converting n to "nd" format.
        let mut nd = [0u32; 64];
        let mut ndhi: u32;
        let mut ndlo: u32;
        let mut e: i32;
        let mut ndebias: i32;
        let mut prefix: u8 = 0;
        if hi(t) & 0x8000_0000 != 0 {
            prefix = b'-';
        } else if has(F_PLUS) {
            prefix = b'+';
        } else if has(F_SPACE) {
            prefix = b' ';
        }
        prec = prec.wrapping_add(((prec as i32 >> 31) & 7) as u32); // Default precision is 6.
        if sf.fp() == SFormat(super::strfmt::T_FP_G).fp() {
            // %g - decrement precision if non-zero (to make it like %e).
            prec = prec.wrapping_sub(1);
            prec ^= (prec as i32 >> 31) as u32;
        }
        let mut try_rescale = has(T_FP_E) && prec < 14 && n != 0.0;
        // Exponent of nd and length of its leading limb (%e/%g only).
        let mut nde: i32 = 0;
        let mut hilen: u32 = 0;
        loop {
            e = ((hi(t) >> 20) & 0x7ff) as i32;
            ndhi = 0;
            ndebias = 0;
            let mut load_lo = false;
            if try_rescale {
                // Precision is sufficiently low that rescaling will probably work.
                ndebias = RESCALE_E[(e >> 6) as usize] as i32;
                if ndebias != 0 {
                    let mut tn = n * RESCALE_N[(e >> 6) as usize];
                    if e == 0 {
                        tn *= 1e10;
                        ndebias -= 10;
                    }
                    t = tn.to_bits();
                    t = t.wrapping_sub(2); // Convert 2ulp below (later we convert 2ulp above).
                    nd[0] = 0x100000 | (hi(t) & 0xfffff);
                    e = ((hi(t) >> 20) & 0x7ff) as i32 - 1075;
                    load_lo = true;
                }
            }
            if !load_lo {
                nd[0] = hi(t) & 0xfffff;
                if e == 0 {
                    e += 1;
                } else {
                    nd[0] |= 0x100000;
                }
                e -= 1043;
                if lo(t) != 0 {
                    e -= 32;
                    load_lo = true;
                }
            }
            if load_lo {
                nd[0] = (nd[0] << 3) | (lo(t) >> 29);
                ndhi = nd_mul2k(&mut nd, ndhi, 29, lo(t) & 0x1fffffff, sf);
            }
            if e >= 0 {
                ndhi = nd_mul2k(&mut nd, ndhi, e as u32, 0, sf);
                ndlo = 0;
            } else {
                ndlo = nd_div2k(&mut nd, ndhi, (-e) as u32, sf);
                if ndhi != 0 && nd[ndhi as usize] == 0 {
                    ndhi -= 1;
                }
            }
            // abs(n) == nd * 10^ndebias (for slightly loose interpretation of ==)
            if has(T_FP_E) {
                // %e or %g - assume %e and start by calculating nd's exponent (nde).
                nde = -1;
                if ndlo != 0 && nd[ndhi as usize] == 0 {
                    ndhi = 64;
                    loop {
                        ndhi -= 1;
                        if nd[ndhi as usize] != 0 {
                            break;
                        }
                    }
                    nde -= 64 * 9;
                }
                hilen = ndigits_dec(nd[ndhi as usize]);
                nde += (ndhi * 9 + hilen) as i32;
                if ndebias != 0 {
                    // Rescaling was performed, but this introduced some error, and might
                    // have pushed us across a rounding boundary. Check whether this error
                    // affected the result by introducing even more error (2ulp in either
                    // direction), and seeing whether a rounding boundary was crossed.
                    let eidx = e + 70 + (lo(t) >= 0xffff_fffe && (!hi(t) << 12) == 0) as i32;
                    debug_assert!((0..128).contains(&eidx), "bad eidx {eidx}");
                    let m = FOUR_ULP_M_E[eidx as usize * 2] as u8;
                    let me = FOUR_ULP_M_E[eidx as usize * 2 + 1] as i32;
                    nd[33] = nd[ndhi as usize];
                    nd[32] = nd[(ndhi.wrapping_sub(1) & 0x3f) as usize];
                    nd[31] = nd[(ndhi.wrapping_sub(2) & 0x3f) as usize];
                    nd_add_m10e(&mut nd, ndhi, m, me);
                    if !nd_similar(&nd, ndhi, 33, hilen, prec + 1) {
                        // rescale_failed: redo the conversion without rescaling.
                        try_rescale = false;
                        t = n.to_bits();
                        continue;
                    }
                }
            }
            break;
        }

        let mut g_like_f = false;
        if has(T_FP_E) {
            let mut eprefix = b'+';
            if ((prec as i32).wrapping_sub(nde)) < ((0x3f & (ndlo as i32).wrapping_neg()) * 9) {
                // Precision is sufficiently low as to maybe require rounding.
                ndhi = nd_round(&mut nd, ndlo, ndhi, nde - prec as i32 - 1);
                nde += (hilen != ndigits_dec(nd[ndhi as usize])) as i32;
            }
            nde += ndebias;
            if has(T_FP_F) {
                // %g
                if prec as i32 >= nde && nde >= -4 {
                    if nde < 0 {
                        ndhi = 0;
                    }
                    prec = prec.wrapping_sub(nde as u32);
                    g_like_f = true;
                } else if !has(F_ALT) && prec != 0 && width > 5 {
                    // Decrease precision in order to strip trailing zeroes.
                    let maxprec = hilen - 1 + (ndhi.wrapping_sub(ndlo) & 0x3f) * 9;
                    if prec >= maxprec {
                        prec = maxprec;
                    } else {
                        ndlo = ndhi.wrapping_sub(((prec.wrapping_sub(hilen) as i32 + 9) / 9) as u32) & 0x3f;
                    }
                    let mut i = prec
                        .wrapping_sub(hilen)
                        .wrapping_sub((ndhi.wrapping_sub(ndlo) & 0x3f) * 9)
                        .wrapping_add(10);
                    let mut tail = uint9(nd[ndlo as usize]);
                    while prec != 0 {
                        i = i.wrapping_sub(1);
                        if tail[i as usize] != b'0' {
                            break;
                        }
                        prec -= 1;
                        if i == 0 {
                            if ndlo == ndhi {
                                prec = 0;
                                break;
                            }
                            ndlo = (ndlo + 1) & 0x3f;
                            tail = uint9(nd[ndlo as usize]);
                            i = 9;
                        }
                    }
                }
            }
            if !g_like_f {
                if nde < 0 {
                    // Make nde non-negative.
                    eprefix = b'-';
                    nde = -nde;
                }
                let l = 3
                    + prec
                    + (prefix != 0) as u32
                    + ndigits_dec(nde as u32)
                    + (nde < 10) as u32
                    + ((prec | (sfv & F_ALT)) != 0) as u32;
                w.more(width.max(l) + 5);
                if !has(F_LEFT | F_ZERO) {
                    w.pad(&mut width, l, b' ');
                }
                if prefix != 0 {
                    w.put(prefix);
                }
                if sfv & (F_LEFT | F_ZERO) == F_ZERO {
                    w.pad(&mut width, l, b'0');
                }
                let p = w.p;
                let q = wint(w.b, p + 1, nd[ndhi as usize] as i32);
                w.b[p] = w.b[p + 1]; // Put leading digit in the correct place.
                if (prec | (sfv & F_ALT)) != 0 {
                    // Emit fractional part.
                    w.b[p + 1] = b'.';
                    w.p = p + 2;
                    prec = prec.wrapping_sub((q - w.p) as u32); // Account for digits already emitted.
                    w.p = q;
                    // Then emit chunks of 9 digits (this may emit 8 digits too many).
                    let mut i = ndhi;
                    while (prec as i32) > 0 && i != ndlo {
                        i = i.wrapping_sub(1) & 0x3f;
                        w.wuint9(nd[i as usize]);
                        prec = prec.wrapping_sub(9);
                    }
                    if has(T_FP_F) && !has(F_ALT) {
                        // %g (and not %#g) - strip trailing zeroes.
                        w.advance((prec as i32) & ((prec as i32) >> 31));
                        while w.back(1) == b'0' {
                            w.p -= 1;
                        }
                        if w.back(1) == b'.' {
                            w.p -= 1;
                        }
                    } else {
                        // %e (or %#g) - emit trailing zeroes.
                        while (prec as i32) > 0 {
                            w.put(b'0');
                            prec -= 1;
                        }
                        w.advance(prec as i32);
                    }
                } else {
                    w.p = p + 1;
                }
                w.put(if has(F_UPPER) { b'E' } else { b'e' });
                w.put(eprefix); // + or -
                if nde < 10 {
                    w.put(b'0'); // Always at least two digits of exponent.
                }
                w.wint(nde);
                len = l;
                if has(F_LEFT) {
                    w.pad(&mut width, len, b' ');
                }
                return;
            }
        }

        // %f (or, shortly, %g in %f style)
        if !g_like_f && prec < (0x3f & (ndlo as i32).wrapping_neg()) as u32 * 9 {
            // Precision is sufficiently low as to maybe require rounding.
            ndhi = nd_round(&mut nd, ndlo, ndhi, 0 - prec as i32 - 1);
        }
        // g_format_like_f:
        if has(T_FP_E) && !has(F_ALT) && prec != 0 && width != 0 {
            // Decrease precision in order to strip trailing zeroes.
            if ndlo != 0 {
                // nd has a fractional part; we need to look at its digits.
                let maxprec = (64 - ndlo) * 9;
                if prec >= maxprec {
                    prec = maxprec;
                } else {
                    ndlo = 64 - prec.div_ceil(9);
                }
                let mut i = prec.wrapping_sub((63 - ndlo) * 9);
                let mut tail = uint9(nd[ndlo as usize]);
                while prec != 0 {
                    i = i.wrapping_sub(1);
                    if tail[i as usize] != b'0' {
                        break;
                    }
                    prec -= 1;
                    if i == 0 {
                        if ndlo == 63 {
                            prec = 0;
                            break;
                        }
                        ndlo += 1;
                        tail = uint9(nd[ndlo as usize]);
                        i = 9;
                    }
                }
            } else {
                // nd has no fractional part, so precision goes straight to zero.
                prec = 0;
            }
        }
        len = ndhi * 9
            + ndigits_dec(nd[ndhi as usize])
            + prec
            + (prefix != 0) as u32
            + ((prec | (sfv & F_ALT)) != 0) as u32;
        w.more(width.max(len) + 8);
        if !has(F_LEFT | F_ZERO) {
            w.pad(&mut width, len, b' ');
        }
        if prefix != 0 {
            w.put(prefix);
        }
        if sfv & (F_LEFT | F_ZERO) == F_ZERO {
            w.pad(&mut width, len, b'0');
        }
        // Emit integer part.
        w.wint(nd[ndhi as usize] as i32);
        let mut i = ndhi;
        while i != 0 {
            i -= 1;
            w.wuint9(nd[i as usize]);
        }
        if (prec | (sfv & F_ALT)) != 0 {
            // Emit fractional part.
            w.put(b'.');
            // Emit chunks of 9 digits (this may emit 8 digits too many).
            while (prec as i32) > 0 && i != ndlo {
                i = i.wrapping_sub(1) & 0x3f;
                w.wuint9(nd[i as usize]);
                prec = prec.wrapping_sub(9);
            }
            if has(T_FP_E) && !has(F_ALT) {
                // %g (and not %#g) - strip trailing zeroes.
                w.advance((prec as i32) & ((prec as i32) >> 31));
                while w.back(1) == b'0' {
                    w.p -= 1;
                }
                if w.back(1) == b'.' {
                    w.p -= 1;
                }
            } else {
                // %f (or %#g) - emit trailing zeroes.
                while (prec as i32) > 0 {
                    w.put(b'0');
                    prec -= 1;
                }
                w.advance(prec as i32);
            }
        }
    }
    if has(F_LEFT) {
        w.pad(&mut width, len, b' ');
    }
}
