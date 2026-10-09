//! Port of lj_char.c / lj_char.h: ASCII character classification.
//!
//! The table is hardcoded for ASCII. Bytes 128-255 are classified as
//! identifier characters only, so the lexer accepts any non-ASCII byte in
//! identifiers (as upstream does).

pub const CNTRL: u8 = 0x01;
pub const SPACE: u8 = 0x02;
pub const PUNCT: u8 = 0x04;
pub const DIGIT: u8 = 0x08;
pub const XDIGIT: u8 = 0x10;
pub const UPPER: u8 = 0x20;
pub const LOWER: u8 = 0x40;
pub const IDENT: u8 = 0x80;
pub const ALPHA: u8 = LOWER | UPPER;
pub const ALNUM: u8 = ALPHA | DIGIT;
pub const GRAPH: u8 = ALNUM | PUNCT;

/// `lj_char_bits`: entry 0 is for `-1` (end of stream), entry `c + 1` for byte `c`.
pub static BITS: [u8; 257] = [
    0,
    1, 1, 1, 1, 1, 1, 1, 1, 1, 3, 3, 3, 3, 3, 1, 1,
    1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
    2, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4,
    152, 152, 152, 152, 152, 152, 152, 152, 152, 152, 4, 4, 4, 4, 4, 4,
    4, 176, 176, 176, 176, 176, 176, 160, 160, 160, 160, 160, 160, 160, 160, 160,
    160, 160, 160, 160, 160, 160, 160, 160, 160, 160, 160, 4, 4, 4, 4, 132,
    4, 208, 208, 208, 208, 208, 208, 192, 192, 192, 192, 192, 192, 192, 192, 192,
    192, 192, 192, 192, 192, 192, 192, 192, 192, 192, 192, 4, 4, 4, 4, 1,
    128, 128, 128, 128, 128, 128, 128, 128, 128, 128, 128, 128, 128, 128, 128, 128,
    128, 128, 128, 128, 128, 128, 128, 128, 128, 128, 128, 128, 128, 128, 128, 128,
    128, 128, 128, 128, 128, 128, 128, 128, 128, 128, 128, 128, 128, 128, 128, 128,
    128, 128, 128, 128, 128, 128, 128, 128, 128, 128, 128, 128, 128, 128, 128, 128,
    128, 128, 128, 128, 128, 128, 128, 128, 128, 128, 128, 128, 128, 128, 128, 128,
    128, 128, 128, 128, 128, 128, 128, 128, 128, 128, 128, 128, 128, 128, 128, 128,
    128, 128, 128, 128, 128, 128, 128, 128, 128, 128, 128, 128, 128, 128, 128, 128,
    128, 128, 128, 128, 128, 128, 128, 128, 128, 128, 128, 128, 128, 128, 128, 128,
];

/// Class bits for `c` in `-1..=255` (`-1` is the lexer's end-of-stream marker).
#[inline]
pub fn bits(c: i32) -> u8 {
    debug_assert!((-1..=255).contains(&c), "bad char class index {c}");
    BITS[(c + 1) as usize]
}

/// `lj_char_isa`: whether byte `c` has any of the class bits in `t`.
#[inline]
pub fn isa(c: u8, t: u8) -> bool {
    BITS[c as usize + 1] & t != 0
}

#[inline]
pub fn iscntrl(c: u8) -> bool {
    isa(c, CNTRL)
}
#[inline]
pub fn isspace(c: u8) -> bool {
    isa(c, SPACE)
}
#[inline]
pub fn ispunct(c: u8) -> bool {
    isa(c, PUNCT)
}
#[inline]
pub fn isdigit(c: u8) -> bool {
    isa(c, DIGIT)
}
#[inline]
pub fn isxdigit(c: u8) -> bool {
    isa(c, XDIGIT)
}
#[inline]
pub fn isupper(c: u8) -> bool {
    isa(c, UPPER)
}
#[inline]
pub fn islower(c: u8) -> bool {
    isa(c, LOWER)
}
#[inline]
pub fn isident(c: u8) -> bool {
    isa(c, IDENT)
}
#[inline]
pub fn isalpha(c: u8) -> bool {
    isa(c, ALPHA)
}
#[inline]
pub fn isalnum(c: u8) -> bool {
    isa(c, ALNUM)
}
#[inline]
pub fn isgraph(c: u8) -> bool {
    isa(c, GRAPH)
}

/// `lj_char_toupper`: ASCII-only upper-casing.
#[inline]
pub fn toupper(c: u8) -> u8 {
    c - ((BITS[c as usize + 1] & LOWER) >> 1)
}

/// `lj_char_tolower`: ASCII-only lower-casing.
#[inline]
pub fn tolower(c: u8) -> u8 {
    c + (BITS[c as usize + 1] & UPPER)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classes() {
        for c in 0..=255u8 {
            let ascii = c < 128;
            assert_eq!(isdigit(c), c.is_ascii_digit(), "{c}");
            assert_eq!(isxdigit(c), c.is_ascii_hexdigit(), "{c}");
            assert_eq!(isupper(c), c.is_ascii_uppercase(), "{c}");
            assert_eq!(islower(c), c.is_ascii_lowercase(), "{c}");
            assert_eq!(isalpha(c), c.is_ascii_alphabetic(), "{c}");
            assert_eq!(ispunct(c), c.is_ascii_punctuation(), "{c}");
            assert_eq!(iscntrl(c), c.is_ascii_control(), "{c}");
            // C isspace: '\t' '\n' '\v' '\f' '\r' ' '.
            assert_eq!(isspace(c), matches!(c, 9..=13 | 32), "{c}");
            assert_eq!(isident(c), !ascii || c.is_ascii_alphanumeric() || c == b'_', "{c}");
            assert_eq!(toupper(c), c.to_ascii_uppercase(), "{c}");
            assert_eq!(tolower(c), c.to_ascii_lowercase(), "{c}");
        }
        assert_eq!(bits(-1), 0);
        assert_eq!(bits(b'a' as i32), 208);
    }
}
