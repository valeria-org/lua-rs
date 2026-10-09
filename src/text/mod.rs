//! Pure byte-level text helpers shared by the lexer, the standard library and the FFI.
//!
//! Nothing in here depends on the VM object model: inputs and outputs are byte slices,
//! `Vec<u8>`, numbers and small enums.

pub mod chars;
pub mod numfmt;
pub mod pattern;
pub mod prng;
pub mod strfmt;
pub mod strscan;

/// `lj_vm_num2int_check`: exact conversion of a double to `i32` (`-0` converts to `0`).
#[inline]
pub fn num2int_check(x: f64) -> Option<i32> {
    // `as` saturates and maps NaN to 0; neither round-trips unless `x` is exact.
    let i = x as i32;
    (i as f64 == x).then_some(i)
}

/// `lj_vm_num2i64` (x64 `cvttsd2si`): truncates; NaN and out-of-range values yield `i64::MIN`.
#[inline]
pub fn num2i64(x: f64) -> i64 {
    if (-9223372036854775808.0..9223372036854775808.0).contains(&x) { x as i64 } else { i64::MIN }
}

/// `lj_vm_num2u64`: accepts the union of the signed and unsigned 64-bit ranges.
#[inline]
pub fn num2u64(x: f64) -> u64 {
    let r = num2i64(x);
    if r != i64::MIN { r as u64 } else { num2i64(x - 18446744073709551616.0) as u64 }
}

/// Correctly rounded `ldexp` (`x * 2^e` with a single rounding), as C's `ldexp`.
pub fn ldexp(mut x: f64, mut e: i32) -> f64 {
    // Same staging as musl's scalbn: keeps the final scale > 2^-1075 so that
    // results in the subnormal range are rounded exactly once.
    const P1023: f64 = f64::from_bits(0x7fe0_0000_0000_0000);
    const PM969: f64 = f64::from_bits(0x0360_0000_0000_0000); // 2^-1022 * 2^53
    if e > 1023 {
        x *= P1023;
        e -= 1023;
        if e > 1023 {
            x *= P1023;
            e -= 1023;
            if e > 1023 {
                e = 1023;
            }
        }
    } else if e < -1022 {
        x *= PM969;
        e += 1022 - 53;
        if e < -1022 {
            x *= PM969;
            e += 1022 - 53;
            if e < -1022 {
                e = -1022;
            }
        }
    }
    x * f64::from_bits(((0x3ff + e) as u64) << 52)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conversions() {
        assert_eq!(num2int_check(-0.0), Some(0));
        assert_eq!(num2int_check(-2147483648.0), Some(i32::MIN));
        assert_eq!(num2int_check(2147483648.0), None);
        assert_eq!(num2int_check(1.5), None);
        assert_eq!(num2int_check(f64::NAN), None);
        assert_eq!(num2i64(f64::NAN), i64::MIN);
        assert_eq!(num2i64(1e19), i64::MIN);
        assert_eq!(num2i64(-1.9), -1);
        assert_eq!(num2u64(1e19), 10000000000000000000);
        assert_eq!(num2u64(-1.0), u64::MAX);
        assert_eq!(num2u64(f64::INFINITY), 1u64 << 63);
    }

    #[test]
    fn ldexp_rounding() {
        assert_eq!(ldexp(1.0, -1074), f64::from_bits(1));
        assert_eq!(ldexp(1.0, -1075), 0.0);
        assert_eq!(ldexp(3.0, -1076), f64::from_bits(1));
        assert_eq!(ldexp(1.0, 1024), f64::INFINITY);
        assert_eq!(ldexp(1.5, 1023), 1.5 * 2f64.powi(1023));
        assert_eq!(ldexp(-1.0, -1022), -f64::MIN_POSITIVE);
    }
}
