//! Port of lj_prng.c (Tausworthe PRNG with period 2^223) and the `math.random` /
//! `math.randomseed` conversions of lib_math.c.
//!
//! Based on: Tables of maximally-equidistributed combined LFSR generators,
//! Pierre L'Ecuyer, 1991, table 3, 1st entry. Full-period ME-CF generator with
//! L=64, J=4, k=223, N1=49. NOT suitable for cryptographic use.

/// `PRNGState`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PrngState {
    pub u: [u64; 4],
}

impl Default for PrngState {
    fn default() -> Self {
        PrngState::seed_fixed()
    }
}

/// Update generator i (`TW223_GEN`).
#[inline(always)]
fn tw223_gen(u: &mut u64, k: u32, q: u32, s: u32) -> u64 {
    let z = *u;
    let z = (((z << q) ^ z) >> (k - s)) ^ ((z & (u64::MAX << (64 - k))) << s);
    *u = z;
    z
}

impl PrngState {
    /// `lj_prng_seed_fixed`: precomputed result of `random_seed(rs, 0.0)`; this is the
    /// state of `math.random` at startup.
    pub const fn seed_fixed() -> Self {
        PrngState {
            u: [
                0xa0d27757_0a345b8c,
                0x764a296c_5d4aa64f,
                0x51220704_070adeaa,
                0x2a2717b5_a7b7b927,
            ],
        }
    }

    /// `lj_prng_u64`: step function with 64-bit result.
    pub fn u64(&mut self) -> u64 {
        let mut r = 0;
        r ^= tw223_gen(&mut self.u[0], 63, 31, 18);
        r ^= tw223_gen(&mut self.u[1], 58, 19, 28);
        r ^= tw223_gen(&mut self.u[2], 55, 24, 7);
        r ^= tw223_gen(&mut self.u[3], 47, 21, 8);
        r
    }

    /// `lj_prng_u64d`: step function returning the bit pattern of a double in `[1.0, 2.0)`.
    pub fn u64d(&mut self) -> u64 {
        (self.u64() & 0x000f_ffff_ffff_ffff) | 0x3ff0_0000_0000_0000
    }

    /// Condition seed: ensure k[i] MSB of u[i] are non-zero.
    fn condition(&mut self) {
        if self.u[0] < (1 << 1) {
            self.u[0] += 1 << 1;
        }
        if self.u[1] < (1 << 6) {
            self.u[1] += 1 << 6;
        }
        if self.u[2] < (1 << 9) {
            self.u[2] += 1 << 9;
        }
        if self.u[3] < (1 << 17) {
            self.u[3] += 1 << 17;
        }
    }

    /// `lj_prng_seed_secure`: seed from system entropy (getrandom, falling back to
    /// `/dev/urandom`). Returns `false` on failure (the state is then unspecified).
    pub fn seed_secure(&mut self) -> bool {
        let mut bytes = [0u8; 32];
        let size = bytes.len();
        // SAFETY: getrandom writes at most `size` bytes into the valid, writable `bytes` buffer.
        let got = unsafe { libc::syscall(libc::SYS_getrandom, bytes.as_mut_ptr(), size, 0) };
        let ok = got == size as libc::c_long || {
            // Fallback to /dev/urandom.
            // SAFETY: the path is a NUL-terminated C string literal.
            let fd =
                unsafe { libc::open(c"/dev/urandom".as_ptr(), libc::O_RDONLY | libc::O_CLOEXEC) };
            if fd != -1 {
                // SAFETY: `fd` is an open descriptor; `bytes` is valid for `size` bytes.
                let n = unsafe { libc::read(fd, bytes.as_mut_ptr().cast(), size) };
                // SAFETY: `fd` was opened above and is closed exactly once.
                unsafe { libc::close(fd) };
                n == size as isize
            } else {
                false
            }
        };
        if !ok {
            return false;
        }
        for (u, chunk) in self.u.iter_mut().zip(bytes.as_chunks::<8>().0) {
            *u = u64::from_ne_bytes(*chunk);
        }
        self.condition();
        self.u64();
        true
    }

    /// lib_math.c `random_seed`: `math.randomseed(d)`.
    pub fn seed(&mut self, mut d: f64) {
        let mut r: u32 = 0x11090601; // 64-k[i] as four 8 bit constants.
        for i in 0..4 {
            let m = 1u64 << (r & 255);
            r >>= 8;
            d = d * std::f64::consts::PI + std::f64::consts::E;
            let mut u = d.to_bits();
            if u < m {
                u += m; // Ensure k[i] MSB of u[i] are non-zero.
            }
            self.u[i] = u;
        }
        for _ in 0..10 {
            self.u64();
        }
    }

    /// Draw the next `math.random` base value: a double in `[0, 1)`.
    ///
    /// `math.random` draws *before* checking its arguments, so a failing call still
    /// advances the state; the caller then applies [`random_upto`] / [`random_range`].
    pub fn next_unit(&mut self) -> f64 {
        f64::from_bits(self.u64d()) - 1.0
    }
}

/// `math.random(m)`: map a unit draw `d` to an integer in `[1, m]`.
pub fn random_upto(d: f64, r1: f64) -> f64 {
    (d * r1).floor() + 1.0
}

/// `math.random(m, n)`: map a unit draw `d` to an integer in `[m, n]`.
pub fn random_range(d: f64, r1: f64, r2: f64) -> f64 {
    (d * (r2 - r1 + 1.0)).floor() + r1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_seed_is_seed_zero() {
        let mut rs = PrngState { u: [0; 4] };
        rs.seed(0.0);
        assert_eq!(rs, PrngState::seed_fixed());
    }

    #[test]
    fn sequences_match_reference() {
        // luajit -e 'math.randomseed(42) print(math.random(), math.random(1,100), math.random(10))
        //            print(math.random(), math.random())'
        let mut rs = PrngState::default();
        rs.seed(42.0);
        assert_eq!(
            crate::text::strfmt::fmt_num(rs.next_unit()),
            b"0.95607928791821"
        );
        assert_eq!(random_range(rs.next_unit(), 1.0, 100.0), 1.0);
        assert_eq!(random_upto(rs.next_unit(), 10.0), 6.0);
        assert_eq!(
            crate::text::strfmt::fmt_num(rs.next_unit()),
            b"0.4341133171063"
        );
        assert_eq!(
            crate::text::strfmt::fmt_num(rs.next_unit()),
            b"0.71972511077591"
        );
    }

    #[test]
    fn startup_and_other_seeds() {
        use crate::text::strfmt::fmt_num;
        // luajit -e 'print(math.random(), math.random(1,100), math.random(7))
        //   math.randomseed(1.5) print(math.random(), math.random(-5, 5))
        //   math.randomseed(-1e300) print(math.random(), math.random(3,3))'
        let mut rs = PrngState::default();
        assert_eq!(fmt_num(rs.next_unit()), b"0.79420629243124");
        assert_eq!(random_range(rs.next_unit(), 1.0, 100.0), 70.0);
        assert_eq!(random_upto(rs.next_unit(), 7.0), 5.0);
        rs.seed(1.5);
        assert_eq!(fmt_num(rs.next_unit()), b"0.73848483355984");
        assert_eq!(random_range(rs.next_unit(), -5.0, 5.0), 1.0);
        rs.seed(-1e300);
        assert_eq!(fmt_num(rs.next_unit()), b"0.35984390588359");
        assert_eq!(random_range(rs.next_unit(), 3.0, 3.0), 3.0);
    }

    #[test]
    fn secure_seed() {
        let mut rs = PrngState::default();
        assert!(rs.seed_secure());
        assert!(rs.u[3] >= 1 << 17);
    }
}
