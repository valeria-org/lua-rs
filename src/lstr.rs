//! Interned byte strings. Port of lj_str.c (hashing and interning).

use std::cell::Cell;
use std::fmt;

use crate::gc::{Gc, GcBox, GcKind, GcObject, Heap, Marker, Trace};

/// An interned, immutable Lua string (`GCstr`).
pub struct LuaStr {
    hash: u32,
    sid: u32,
    reserved: Cell<u8>,
    chain: Cell<Option<Gc<LuaStr>>>,
    /// Content followed by a NUL byte (not part of the string).
    bytes: Box<[u8]>,
}

impl LuaStr {
    #[inline]
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.bytes.len() - 1]
    }
    /// Content followed by a NUL byte, for C strings (`strdata`).
    #[inline]
    pub fn as_bytes_with_nul(&self) -> &[u8] {
        &self.bytes
    }
    #[inline]
    pub fn len(&self) -> usize {
        self.bytes.len() - 1
    }
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.bytes.len() == 1
    }
    /// Content hash used by the intern table.
    #[inline]
    pub fn hash(&self) -> u32 {
        self.hash
    }
    /// String ID assigned at interning; used for table hashing (`hashstr`).
    #[inline]
    pub fn sid(&self) -> u32 {
        self.sid
    }
    /// Reserved-word index for the lexer (0 = not reserved).
    #[inline]
    pub fn reserved(&self) -> u8 {
        self.reserved.get()
    }
    #[inline]
    pub fn set_reserved(&self, r: u8) {
        self.reserved.set(r);
    }
    /// Content as UTF-8, if valid.
    pub fn to_str(&self) -> Option<&str> {
        std::str::from_utf8(self.as_bytes()).ok()
    }
    /// Content as UTF-8 with invalid sequences replaced.
    pub fn to_string_lossy(&self) -> String {
        String::from_utf8_lossy(self.as_bytes()).into_owned()
    }
}

impl fmt::Debug for LuaStr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", String::from_utf8_lossy(self.as_bytes()))
    }
}

impl Trace for LuaStr {
    fn trace(&self, _m: &mut Marker) {}
    fn size(&self) -> usize {
        std::mem::size_of::<GcBox<LuaStr>>() + self.bytes.len()
    }
}

impl GcObject for LuaStr {
    const KIND: GcKind = GcKind::Str;
}

#[inline]
fn getu32(s: &[u8], i: usize) -> u32 {
    u32::from_le_bytes([s[i], s[i + 1], s[i + 2], s[i + 3]])
}

/// Keyed sparse ARX string hash (`hash_sparse` in lj_str.c). Constant time.
pub fn hash_sparse(seed: u64, s: &[u8]) -> u32 {
    let len = s.len();
    let mut h = (len as u32) ^ (seed as u32);
    let mut a;
    let mut b;
    if len >= 4 {
        a = getu32(s, 0);
        h ^= getu32(s, len - 4);
        b = getu32(s, (len >> 1) - 2);
        h ^= b;
        h = h.wrapping_sub(b.rotate_left(14));
        b = b.wrapping_add(getu32(s, (len >> 2) - 1));
    } else if len > 0 {
        a = s[0] as u32;
        h ^= s[len - 1] as u32;
        b = s[len >> 1] as u32;
        h ^= b;
        h = h.wrapping_sub(b.rotate_left(14));
    } else {
        // Upstream never hashes the empty string (it is a fixed global object).
        a = 0;
        b = 0;
    }
    a ^= h;
    a = a.wrapping_sub(h.rotate_left(11));
    b ^= a;
    b = b.wrapping_sub(a.rotate_left(25));
    h ^= b;
    h = h.wrapping_sub(b.rotate_left(16));
    h
}

/// Minimum intern table size (`LJ_MIN_STRTAB`).
pub const MIN_STRTAB: usize = 256;
/// Maximum string length (`LJ_MAX_STR`).
pub const MAX_STR: usize = 0x7fffff00;

/// Weak string intern table (`g->str`). Chains are threaded through the strings themselves.
pub struct StrTab {
    tab: Box<[Option<Gc<LuaStr>>]>,
    mask: usize,
    num: usize,
    next_sid: u32,
    seed: u64,
}

impl StrTab {
    /// Create an empty table with a fixed seed (deterministic hashing and string IDs).
    pub fn new(seed: u64) -> StrTab {
        StrTab { tab: vec![None; MIN_STRTAB].into_boxed_slice(), mask: MIN_STRTAB - 1, num: 0, next_sid: 0, seed }
    }

    /// Number of interned strings.
    #[inline]
    pub fn len(&self) -> usize {
        self.num
    }
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.num == 0
    }
    /// Number of hash chains (`g->str.mask + 1`).
    #[inline]
    pub fn chains(&self) -> usize {
        self.mask + 1
    }

    /// Intern `s`, allocating a new string object in `heap` if not present (`lj_str_new`).
    pub fn intern(&mut self, heap: &mut Heap, s: &[u8]) -> Gc<LuaStr> {
        let hash = hash_sparse(self.seed, s);
        let mut o = self.tab[hash as usize & self.mask];
        while let Some(sx) = o {
            if sx.hash == hash && sx.as_bytes() == s {
                return sx;
            }
            o = sx.chain.get();
        }
        let sid = self.next_sid;
        self.next_sid = self.next_sid.wrapping_add(1);
        let g = heap.alloc(LuaStr {
            hash,
            sid,
            reserved: Cell::new(0),
            chain: Cell::new(None),
            bytes: {
                // Trailing NUL (not part of the content) so the C API can hand out C strings.
                let mut b = Vec::with_capacity(s.len() + 1);
                b.extend_from_slice(s);
                b.push(0);
                b.into_boxed_slice()
            },
        });
        let slot = hash as usize & self.mask;
        g.chain.set(self.tab[slot]);
        self.tab[slot] = Some(g);
        let n = self.num;
        self.num += 1;
        if n > self.mask {
            // Allow a 100% load factor.
            self.resize((self.mask << 1) + 1);
        }
        g
    }

    /// Look up an already interned string without allocating.
    pub fn find(&self, s: &[u8]) -> Option<Gc<LuaStr>> {
        let hash = hash_sparse(self.seed, s);
        let mut o = self.tab[hash as usize & self.mask];
        while let Some(sx) = o {
            if sx.hash == hash && sx.as_bytes() == s {
                return Some(sx);
            }
            o = sx.chain.get();
        }
        None
    }

    fn resize(&mut self, newmask: usize) {
        if newmask >= (1 << 26) - 1 {
            return;
        }
        let mut newtab: Box<[Option<Gc<LuaStr>>]> = vec![None; newmask + 1].into_boxed_slice();
        for i in (0..=self.mask).rev() {
            let mut o = self.tab[i];
            while let Some(s) = o {
                o = s.chain.get();
                let h = s.hash as usize & newmask;
                s.chain.set(newtab[h]);
                newtab[h] = Some(s);
            }
        }
        self.tab = newtab;
        self.mask = newmask;
    }

    /// Unlink every string that is neither marked nor fixed. Must run after marking and before
    /// `Heap::sweep` frees them (`gc_sweepstr`). Shrinks the table when sparsely used.
    pub fn purge(&mut self) {
        for i in 0..=self.mask {
            let mut prev: Option<Gc<LuaStr>> = None;
            let mut o = self.tab[i];
            while let Some(s) = o {
                let next = s.chain.get();
                let h = s.header();
                if h.is_marked() || h.is_fixed() {
                    prev = Some(s);
                } else {
                    match prev {
                        Some(p) => p.chain.set(next),
                        None => self.tab[i] = next,
                    }
                    self.num -= 1;
                }
                o = next;
            }
        }
        if self.num <= (self.mask >> 2) && self.mask > MIN_STRTAB * 2 - 1 {
            self.resize(self.mask >> 1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interning_identity() {
        let mut heap = Heap::new();
        let mut st = StrTab::new(0x1234);
        let a = st.intern(&mut heap, b"hello");
        let b = st.intern(&mut heap, b"hello");
        let c = st.intern(&mut heap, b"hellO");
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_eq!(a.as_bytes(), b"hello");
        assert_eq!(st.find(b"hellO"), Some(c));
        assert_eq!(st.find(b"nope"), None);
        // Grow beyond the initial size and check every string is still found.
        let v: Vec<_> = (0..2000).map(|i| st.intern(&mut heap, format!("k{i}").as_bytes())).collect();
        for (i, s) in v.iter().enumerate() {
            assert_eq!(st.intern(&mut heap, format!("k{i}").as_bytes()), *s);
        }
        assert_eq!(st.len(), 2002);
        let e = st.intern(&mut heap, b"");
        assert!(e.is_empty());
        assert_eq!(st.intern(&mut heap, b""), e);
        assert_eq!(st.intern(&mut heap, b"a\0b").len(), 3);
    }

    #[test]
    fn purge_removes_unmarked() {
        let mut heap = Heap::new();
        let mut st = StrTab::new(0);
        let keep = st.intern(&mut heap, b"keep");
        let fixed = st.intern(&mut heap, b"fixed");
        fixed.fix();
        st.intern(&mut heap, b"drop");
        let mut m = Marker::new(None, None);
        m.mark(keep);
        st.purge();
        heap.sweep();
        assert_eq!(st.len(), 2);
        assert_eq!(heap.object_count(), 2);
        assert_eq!(st.find(b"keep"), Some(keep));
        assert_eq!(st.find(b"fixed"), Some(fixed));
        assert!(st.find(b"drop").is_none());
    }
}
