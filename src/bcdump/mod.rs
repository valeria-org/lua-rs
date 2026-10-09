//! Port of lj_bcdump.h, lj_bcwrite.c and lj_bcread.c: the LuaJIT 2.1 bytecode dump format
//! (`string.dump` output, accepted by `load`).
//!
//! ```text
//! dump   = header proto+ 0U
//! header = ESC 'L' 'J' versionB flagsU [namelenU nameB*]
//! proto  = lengthU pdata
//! pdata  = phead bcinsW* uvdataH* kgc* knum* [debugB*]
//! phead  = flagsB numparamsB framesizeB numuvB numkgcU numknU numbcU
//!          [debuglenU [firstlineU numlineU]]
//! kgc    = kgctypeU { ktab | (loU hiU) | (rloU rhiU iloU ihiU) | strB* }
//! knum   = intU0 | (loU1 hiU)
//! ktab   = narrayU nhashU karray* khash*
//! karray = ktabk
//! khash  = ktabk ktabk
//! ktabk  = ktabtypeU { intU | (loU hiU) | strB* }
//!
//! B = 8 bit, H = 16 bit, W = 32 bit, U = ULEB128 of W, U0/U1 = ULEB128 of W+1
//! ```
//!
//! Child prototypes precede their parent; the reader keeps them on a stack and the parent's
//! `BCDUMP_KGC_CHILD` constants pop them. GC constants are stored in upstream memory order
//! (highest bytecode D operand first), i.e. the reverse of [`Proto::kgc`](crate::func::Proto).

mod read;
mod write;

pub use read::{read, read_ex, read_input, read_proto};
pub use write::{write, write_flags};

/// Bytecode dump header bytes.
pub const BCDUMP_HEAD1: u8 = 0x1b;
pub const BCDUMP_HEAD2: u8 = 0x4c;
pub const BCDUMP_HEAD3: u8 = 0x4a;

/// Dump format version.
pub const BCDUMP_VERSION: u8 = 2;

/// Compatibility flags (header `flagsU`).
pub const BCDUMP_F_BE: u32 = 0x01;
pub const BCDUMP_F_STRIP: u32 = 0x02;
pub const BCDUMP_F_FFI: u32 = 0x04;
pub const BCDUMP_F_FR2: u32 = 0x08;
pub const BCDUMP_F_BITOP: u32 = 0x10;
pub const BCDUMP_F_KNOWN: u32 = BCDUMP_F_BITOP * 2 - 1;

/// Writer-only flag: emit template table hash parts in sorted key order (`string.dump(f, "d")`).
pub const BCDUMP_F_DETERMINISTIC: u32 = 0x8000_0000;

/// Type codes for the GC constants of a prototype. Plus length for strings.
pub const BCDUMP_KGC_CHILD: u32 = 0;
pub const BCDUMP_KGC_TAB: u32 = 1;
pub const BCDUMP_KGC_I64: u32 = 2;
pub const BCDUMP_KGC_U64: u32 = 3;
pub const BCDUMP_KGC_COMPLEX: u32 = 4;
pub const BCDUMP_KGC_STR: u32 = 5;

/// Type codes for the keys/values of a constant table. Plus length for strings.
pub const BCDUMP_KTAB_NIL: u32 = 0;
pub const BCDUMP_KTAB_FALSE: u32 = 1;
pub const BCDUMP_KTAB_TRUE: u32 = 2;
pub const BCDUMP_KTAB_INT: u32 = 3;
pub const BCDUMP_KTAB_NUM: u32 = 4;
pub const BCDUMP_KTAB_STR: u32 = 5;

/// `LJ_KEYINDEX`: high word of a traversal-index marker; never narrowed to an integer.
const LJ_KEYINDEX: u32 = 0xfffe_7fff;

/// `lj_strfmt_wuleb128`.
#[inline]
fn put_uleb128(out: &mut Vec<u8>, mut v: u32) {
    while v >= 0x80 {
        out.push((v & 0x7f) as u8 | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

/// Width in bytes (log2) of each line info entry for a prototype spanning `numline` lines.
#[inline]
fn lineinfo_shift(numline: u32) -> u32 {
    if numline < 256 {
        0
    } else if numline < 65536 {
        1
    } else {
        2
    }
}

#[cfg(test)]
mod tests;
