//! Lua tables. Port of lj_tab.c (array part incl. slot 0, chained hash part with Brent's
//! variation, upstream rehash counting, border length and traversal order).
//!
//! Invariant inherited from upstream: hash nodes whose value is nil may keep a *dead* key whose
//! GC object has been collected. Such keys are only ever compared by identity, never
//! dereferenced (hashing a key only happens for nodes with a non-nil value, or for live
//! lookup keys).

use std::cell::{Cell, UnsafeCell};
use std::fmt;

use crate::bc::MM;
use crate::gc::{self, Gc, GcBox, GcKind, GcObject, Marker, Trace, WEAK_KEY, WEAK_KV, WEAK_VAL};
use crate::lstr::LuaStr;
use crate::value::{Value, num2int_check};

/// Max. hash bits (`LJ_MAX_HBITS`).
pub const MAX_HBITS: u32 = 26;
/// Max. bits of array key (`LJ_MAX_ABITS`).
pub const MAX_ABITS: u32 = 28;
/// Max. array part size (`LJ_MAX_ASIZE`).
pub const MAX_ASIZE: u32 = (1 << (MAX_ABITS - 1)) + 1;

const NO_NODE: u32 = u32::MAX;

/// Table errors, with upstream messages (lj_errmsg.h).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TabError {
    /// `table index is nil`
    NilIndex,
    /// `table index is NaN`
    NanIndex,
    /// `table overflow`
    Overflow,
    /// `invalid key to 'next'`
    InvalidNextKey,
}

impl TabError {
    pub fn msg(self) -> &'static str {
        match self {
            TabError::NilIndex => "table index is nil",
            TabError::NanIndex => "table index is NaN",
            TabError::Overflow => "table overflow",
            TabError::InvalidNextKey => "invalid key to 'next'",
        }
    }
}

impl fmt::Display for TabError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.msg())
    }
}

impl std::error::Error for TabError {}

/// `hsize2hbits`: number of hash bits needed for `s` entries.
#[inline]
pub fn hsize2hbits(s: u32) -> u32 {
    match s {
        0 => 0,
        1 => 1,
        _ => 1 + fls(s - 1),
    }
}

/// Index of the most significant set bit (`lj_fls`); `x` must be non-zero.
#[inline]
fn fls(x: u32) -> u32 {
    31 - x.leading_zeros()
}

/// Hash constants and scrambler from lj_tab.h (x86/x64 variant).
#[inline]
fn hashrot(mut lo: u32, mut hi: u32) -> u32 {
    lo ^= hi;
    hi = hi.rotate_left(14);
    lo = lo.wrapping_sub(hi);
    hi = hi.rotate_left(5);
    hi ^= lo;
    hi = hi.wrapping_sub(lo.rotate_left(13));
    hi
}

#[inline]
fn hash_num(n: f64) -> u32 {
    let u = n.to_bits();
    hashrot(u as u32, ((u >> 32) as u32) << 1)
}

#[inline]
fn hash_ptr(p: usize) -> u32 {
    hashrot(p as u32, (p as u64 >> 32) as u32)
}

/// Raw hash of a non-nil key (before masking). `hashkey` in lj_tab.c.
#[inline]
fn key_hash(key: Value) -> u32 {
    match key {
        Value::Str(s) => s.sid(),
        Value::Num(n) => hash_num(n),
        Value::Bool(b) => b as u32,
        Value::LightUd(p) => hash_ptr(p),
        Value::Table(t) => hash_ptr(t.addr()),
        Value::Func(f) => hash_ptr(f.addr()),
        Value::Thread(t) => hash_ptr(t.addr()),
        Value::Udata(u) => hash_ptr(u.addr()),
        Value::CData(c) => hash_ptr(c.addr()),
        Value::Nil => 0,
    }
}

#[derive(Clone, Copy)]
struct Node {
    val: Value,
    key: Value,
    next: u32,
}

const NIL_NODE: Node = Node { val: Value::Nil, key: Value::Nil, next: NO_NODE };

/// Mutable table storage (accessed only inside `Table` methods, never across calls).
#[derive(Clone)]
struct Inner {
    /// Array part, `len() == asize`.
    array: Vec<Value>,
    /// Hash part, `len() == hmask + 1`, or empty (upstream's shared nil node).
    node: Vec<Node>,
    /// Free-node search position (`freetop`), scanning downwards.
    freetop: u32,
}

enum Slot {
    Arr(u32),
    Node(u32),
}

impl Inner {
    #[inline]
    fn asize(&self) -> u32 {
        self.array.len() as u32
    }
    #[inline]
    fn hmask(&self) -> u32 {
        if self.node.is_empty() { 0 } else { self.node.len() as u32 - 1 }
    }
    #[inline]
    fn mainpos(&self, key: Value) -> u32 {
        key_hash(key) & self.hmask()
    }
    #[inline]
    fn mainpos_num(&self, n: f64) -> u32 {
        hash_num(n) & self.hmask()
    }

    fn bytes(&self) -> usize {
        self.array.capacity() * std::mem::size_of::<Value>() + self.node.capacity() * std::mem::size_of::<Node>()
    }

    fn new_hpart(&mut self, hbits: u32) -> Result<(), TabError> {
        if hbits > MAX_HBITS {
            return Err(TabError::Overflow);
        }
        let hsize = 1usize << hbits;
        self.node = vec![NIL_NODE; hsize];
        self.freetop = hsize as u32;
        Ok(())
    }

    fn find_int(&self, key: i32) -> Option<u32> {
        if self.node.is_empty() {
            return None;
        }
        let k = key as f64;
        let mut n = self.mainpos_num(k);
        loop {
            let nd = &self.node[n as usize];
            if let Value::Num(x) = nd.key
                && x == k
            {
                return Some(n);
            }
            n = nd.next;
            if n == NO_NODE {
                return None;
            }
        }
    }

    fn find_str(&self, key: Gc<LuaStr>) -> Option<u32> {
        if self.node.is_empty() {
            return None;
        }
        let mut n = key.sid() & self.hmask();
        loop {
            let nd = &self.node[n as usize];
            if let Value::Str(s) = nd.key
                && s == key
            {
                return Some(n);
            }
            n = nd.next;
            if n == NO_NODE {
                return None;
            }
        }
    }

    fn find_generic(&self, key: Value) -> Option<u32> {
        if self.node.is_empty() {
            return None;
        }
        let mut n = self.mainpos(key);
        loop {
            let nd = &self.node[n as usize];
            if nd.key == key {
                return Some(n);
            }
            n = nd.next;
            if n == NO_NODE {
                return None;
            }
        }
    }

    #[inline]
    fn get_int(&self, key: i32) -> Value {
        if (key as u32) < self.asize() {
            self.array[key as usize]
        } else {
            self.find_int(key).map_or(Value::Nil, |n| self.node[n as usize].val)
        }
    }

    fn get(&self, key: Value) -> Value {
        let n = match key {
            Value::Str(s) => self.find_str(s),
            Value::Num(x) => match num2int_check(x) {
                Some(k) => return self.get_int(k),
                None => self.find_generic(key),
            },
            Value::Nil => None,
            _ => self.find_generic(key),
        };
        n.map_or(Value::Nil, |n| self.node[n as usize].val)
    }

    #[inline]
    fn write(&mut self, slot: Slot, val: Value) {
        match slot {
            Slot::Arr(i) => self.array[i as usize] = val,
            Slot::Node(n) => self.node[n as usize].val = val,
        }
    }

    // -- Setters (return the slot to write) --

    fn set_int_slot(&mut self, key: i32) -> Result<Slot, TabError> {
        if (key as u32) < self.asize() {
            return Ok(Slot::Arr(key as u32));
        }
        if let Some(n) = self.find_int(key) {
            return Ok(Slot::Node(n));
        }
        self.newkey(Value::Num(key as f64))
    }

    fn set_str_slot(&mut self, key: Gc<LuaStr>) -> Result<Slot, TabError> {
        if let Some(n) = self.find_str(key) {
            return Ok(Slot::Node(n));
        }
        self.newkey(Value::Str(key))
    }

    /// `lj_tab_set` minus the nomm reset (done by the caller).
    fn set_slot(&mut self, key: Value) -> Result<Slot, TabError> {
        match key {
            Value::Str(s) => return self.set_str_slot(s),
            Value::Num(x) => {
                if let Some(k) = num2int_check(x) {
                    return self.set_int_slot(k);
                }
                if x.is_nan() {
                    return Err(TabError::NanIndex);
                }
            }
            Value::Nil => return Err(TabError::NilIndex),
            _ => {}
        }
        if let Some(n) = self.find_generic(key) {
            return Ok(Slot::Node(n));
        }
        self.newkey(key)
    }

    /// Insert a new key (`lj_tab_newkey`), using Brent's variation to optimize chain length.
    fn newkey(&mut self, key: Value) -> Result<Slot, TabError> {
        if self.node.is_empty() {
            // Upstream: hmask == 0 means the shared nil node; no free node -> rehash.
            self.rehash(key)?;
            return self.set_slot(key);
        }
        let mut n = self.mainpos(key);
        if !self.node[n as usize].val.is_nil() {
            // Main position is taken: find a free node.
            let mut freenode = self.freetop;
            loop {
                if freenode == 0 {
                    self.rehash(key)?;
                    return self.set_slot(key);
                }
                freenode -= 1;
                if self.node[freenode as usize].key.is_nil() {
                    break;
                }
            }
            self.freetop = freenode;
            let collide = self.mainpos(self.node[n as usize].key);
            if collide != n {
                // Colliding node is not in its main position: move it to the free node.
                let mut c = collide;
                while self.node[c as usize].next != n {
                    c = self.node[c as usize].next;
                }
                self.node[c as usize].next = freenode;
                self.node[freenode as usize] = self.node[n as usize];
                self.node[n as usize].next = NO_NODE;
                self.node[n as usize].val = Value::Nil;
                // Rechain pseudo-resurrected string keys with colliding hashes.
                let mut fnode = freenode;
                loop {
                    let nn = self.node[fnode as usize].next;
                    if nn == NO_NODE {
                        break;
                    }
                    if !self.node[nn as usize].val.is_nil() && self.mainpos(self.node[nn as usize].key) == n {
                        self.node[fnode as usize].next = self.node[nn as usize].next;
                        self.node[nn as usize].next = self.node[n as usize].next;
                        self.node[n as usize].next = nn;
                        // Rechain any key that is currently in a non-main position.
                        loop {
                            let nn = self.node[fnode as usize].next;
                            if nn == NO_NODE {
                                break;
                            }
                            if !self.node[nn as usize].val.is_nil() {
                                let mn = self.mainpos(self.node[nn as usize].key);
                                if mn != fnode && mn != nn {
                                    self.node[fnode as usize].next = self.node[nn as usize].next;
                                    self.node[nn as usize].next = self.node[mn as usize].next;
                                    self.node[mn as usize].next = nn;
                                } else {
                                    fnode = nn;
                                }
                            } else {
                                fnode = nn;
                            }
                        }
                        break;
                    }
                    fnode = nn;
                }
            } else {
                // Colliding node is in its main position: chain the free node after it.
                self.node[freenode as usize].next = self.node[n as usize].next;
                self.node[n as usize].next = freenode;
                n = freenode;
            }
        }
        self.node[n as usize].key = key.normalize_key();
        debug_assert!(self.node[n as usize].val.is_nil());
        Ok(Slot::Node(n))
    }

    // -- Resizing --

    /// `lj_tab_resize`.
    fn resize(&mut self, asize: u32, hbits: u32) -> Result<(), TabError> {
        let oldasize = self.asize();
        if asize > oldasize {
            if asize > MAX_ASIZE {
                return Err(TabError::Overflow);
            }
            self.array.resize(asize as usize, Value::Nil);
        }
        if hbits > MAX_HBITS {
            return Err(TabError::Overflow);
        }
        let oldnode = std::mem::take(&mut self.node);
        if hbits > 0 {
            self.new_hpart(hbits)?;
        } else {
            self.freetop = 0;
        }
        if asize < oldasize {
            // Array part shrinks: reinsert old array values into the hash part.
            let tail: Vec<Value> = self.array.split_off(asize as usize);
            self.array.shrink_to_fit();
            for (i, v) in tail.into_iter().enumerate() {
                if !v.is_nil() {
                    let s = self.set_int_slot((asize as usize + i) as i32)?;
                    self.write(s, v);
                }
            }
        }
        // Reinsert pairs from the old hash part.
        for n in oldnode {
            if !n.val.is_nil() {
                let s = self.set_slot(n.key)?;
                self.write(s, n.val);
            }
        }
        Ok(())
    }

    fn countarray(&self, bins: &mut [u32; MAX_ABITS as usize]) -> u32 {
        let asize = self.asize();
        if asize == 0 {
            return 0;
        }
        let mut na = 0;
        let mut i = 0u32;
        for (b, bin) in bins.iter_mut().enumerate() {
            let mut top = 2u32 << b;
            if top >= asize {
                top = asize - 1;
                if i > top {
                    break;
                }
            }
            let mut n = 0;
            while i <= top {
                if !self.array[i as usize].is_nil() {
                    n += 1;
                }
                i += 1;
            }
            *bin += n;
            na += n;
        }
        na
    }

    fn counthash(&self, bins: &mut [u32; MAX_ABITS as usize], narray: &mut u32) -> u32 {
        let mut total = 0;
        let mut na = 0;
        for n in &self.node {
            if !n.val.is_nil() {
                na += countint(n.key, bins);
                total += 1;
            }
        }
        *narray += na;
        total
    }

    /// `rehashtab`: choose new array/hash sizes for the current contents plus `ek`.
    fn rehash(&mut self, ek: Value) -> Result<(), TabError> {
        let mut bins = [0u32; MAX_ABITS as usize];
        let mut asize = self.countarray(&mut bins);
        let mut total = 1 + asize;
        total += self.counthash(&mut bins, &mut asize);
        asize += countint(ek, &mut bins);
        let na = bestasize(&bins, &mut asize);
        total -= na;
        self.resize(asize, hsize2hbits(total))
    }

    // -- Traversal --

    /// `lj_tab_keyindex`: successor traversal index of `key` (`None` = invalid key).
    fn keyindex(&self, key: Value) -> Option<u32> {
        let asize = self.asize();
        if let Value::Num(x) = key
            && let Some(k) = num2int_check(x)
            && (k as u32) < asize
        {
            return Some(k as u32 + 1);
        }
        if key.is_nil() {
            return Some(0);
        }
        let n = self.find_generic(key)?;
        Some(asize + n + 1)
    }

    fn next_from(&self, mut idx: u32) -> Option<(u32, Value, Value)> {
        let asize = self.asize();
        while idx < asize {
            let v = self.array[idx as usize];
            if !v.is_nil() {
                return Some((idx + 1, Value::Num(idx as f64), v));
            }
            idx += 1;
        }
        let mut h = (idx - asize) as usize;
        while h < self.node.len() {
            let n = &self.node[h];
            if !n.val.is_nil() {
                return Some((asize + h as u32 + 1, n.key, n.val));
            }
            h += 1;
        }
        None
    }

    // -- Length --

    fn len_slow(&self, hi: usize) -> usize {
        let mut lo = hi;
        let mut hi = hi + 1;
        // Widening search for an upper bound.
        while !self.get_int(hi as i32).is_nil() {
            lo = hi;
            hi += hi;
            if hi > (0x7fffffff - 2) {
                // Punt and do a linear search.
                lo = 1;
                while !self.get_int(lo as i32).is_nil() {
                    lo += 1;
                }
                return lo - 1;
            }
        }
        // Binary search to find a non-nil to nil transition.
        while hi - lo > 1 {
            let mid = (lo + hi) >> 1;
            if !self.get_int(mid as i32).is_nil() {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        lo
    }

    fn len(&self) -> usize {
        let mut hi = self.array.len().saturating_sub(1);
        // In a growing array the last array element is very likely nil.
        if hi > 0 && self.array[hi].is_nil() {
            let mut lo = 0;
            while hi - lo > 1 {
                let mid = (lo + hi) >> 1;
                if self.array[mid].is_nil() {
                    hi = mid;
                } else {
                    lo = mid;
                }
            }
            return lo;
        }
        // Without a hash part, there's an implicit nil after the last element.
        if !self.node.is_empty() { self.len_slow(hi) } else { hi }
    }
}

fn countint(key: Value, bins: &mut [u32; MAX_ABITS as usize]) -> u32 {
    if let Value::Num(x) = key
        && let Some(k) = num2int_check(x)
        && (k as u32) < MAX_ASIZE
    {
        bins[if k > 2 { fls((k - 1) as u32) as usize } else { 0 }] += 1;
        return 1;
    }
    0
}

fn bestasize(bins: &[u32; MAX_ABITS as usize], narray: &mut u32) -> u32 {
    let nn = *narray;
    let mut sum = 0u32;
    let mut na = 0;
    let mut sz = 0;
    let mut b = 0usize;
    while b < MAX_ABITS as usize && 2 * nn > (1u32 << b) && sum != nn {
        if bins[b] > 0 {
            sum += bins[b];
            if 2 * sum > (1u32 << b) {
                sz = (2u32 << b) + 1;
                na = sum;
            }
        }
        b += 1;
    }
    *narray = sz;
    na
}

// -- Table --------------------------------------------------------------------------------------

/// A Lua table (`GCtab`). All access goes through `&self` methods returning copies.
pub struct Table {
    inner: UnsafeCell<Inner>,
    meta: Cell<Option<Gc<Table>>>,
    nomm: Cell<u8>,
}

impl fmt::Debug for Table {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Table(asize={}, hmask={})", self.asize(), self.hmask())
    }
}

impl Table {
    #[inline]
    fn inner(&self) -> &Inner {
        // SAFETY: no method hands out references into `inner` or calls back into code that could
        // obtain a `&mut Inner` while this shared borrow is alive.
        unsafe { &*self.inner.get() }
    }

    /// Run `f` with exclusive access to the storage and report the size change to the GC.
    #[inline]
    fn mutate<R>(&self, f: impl FnOnce(&mut Inner) -> R) -> R {
        // SAFETY: tables are single-threaded; every `&mut Inner` is created only here, lives
        // for the duration of `f`, and `f` never re-enters this table's methods.
        let inner = unsafe { &mut *self.inner.get() };
        let before = inner.bytes();
        let r = f(inner);
        let after = inner.bytes();
        if before != after {
            gc::account(after as isize - before as isize);
        }
        r
    }

    /// `lj_tab_new`: `asize` array slots (0..asize-1, non-inclusive) and `2^hbits` hash nodes.
    pub fn new(asize: u32, hbits: u32) -> Result<Table, TabError> {
        if asize > MAX_ASIZE {
            return Err(TabError::Overflow);
        }
        let mut inner = Inner { array: vec![Value::Nil; asize as usize], node: Vec::new(), freetop: 0 };
        if hbits > 0 {
            inner.new_hpart(hbits)?;
        }
        Ok(Table { inner: UnsafeCell::new(inner), meta: Cell::new(None), nomm: Cell::new(!0) })
    }

    /// `lj_tab_new_ah`: sizes as in `lua_createtable(L, narr, nrec)`.
    pub fn new_ah(narr: u32, nrec: u32) -> Result<Table, TabError> {
        Table::new(if narr > 0 { narr + 1 } else { 0 }, hsize2hbits(nrec))
    }

    /// `lj_tab_dup`: copy a template table (TDUP). Table values in the template's hash part are
    /// nil markers and become nil. The copy has no metatable and a cleared metamethod cache.
    pub fn dup(&self) -> Table {
        let mut inner = self.inner().clone();
        for n in inner.node.iter_mut() {
            if let Value::Table(_) = n.val {
                n.val = Value::Nil;
            }
        }
        Table { inner: UnsafeCell::new(inner), meta: Cell::new(None), nomm: Cell::new(0) }
    }

    /// Size of the array part (`asize`, includes slot 0).
    #[inline]
    pub fn asize(&self) -> u32 {
        self.inner().asize()
    }
    /// Hash mask (`hmask`); 0 if there is no hash part.
    #[inline]
    pub fn hmask(&self) -> u32 {
        self.inner().hmask()
    }
    /// Number of hash nodes (0 if there is no hash part).
    #[inline]
    pub fn node_count(&self) -> u32 {
        self.inner().node.len() as u32
    }

    /// Raw read of array slot `i < asize`.
    #[inline]
    pub fn array_get(&self, i: u32) -> Value {
        self.inner().array[i as usize]
    }
    /// Raw write of array slot `i < asize`.
    #[inline]
    pub fn array_set(&self, i: u32, v: Value) {
        self.mutate(|t| t.array[i as usize] = v)
    }
    /// Raw `(key, value)` of hash node `i < node_count()`; key may be dead if value is nil.
    #[inline]
    pub fn node_get(&self, i: u32) -> (Value, Value) {
        let n = &self.inner().node[i as usize];
        (n.key, n.val)
    }

    // -- Getters --

    /// `lj_tab_get`: value for `key`, nil if absent.
    #[inline]
    pub fn get(&self, key: Value) -> Value {
        self.inner().get(key)
    }
    /// `lj_tab_getint`.
    #[inline]
    pub fn get_int(&self, key: i32) -> Value {
        self.inner().get_int(key)
    }
    /// `lj_tab_getstr`.
    #[inline]
    pub fn get_str(&self, key: Gc<LuaStr>) -> Value {
        let t = self.inner();
        t.find_str(key).map_or(Value::Nil, |n| t.node[n as usize].val)
    }
    /// True if `key` has a slot (array slot or existing hash node), even if its value is nil.
    pub fn has_slot(&self, key: Value) -> bool {
        let t = self.inner();
        match key {
            Value::Str(s) => t.find_str(s).is_some(),
            Value::Num(x) => match num2int_check(x) {
                Some(k) if (k as u32) < t.asize() => true,
                Some(k) => t.find_int(k).is_some(),
                None => t.find_generic(key).is_some(),
            },
            Value::Nil => false,
            _ => t.find_generic(key).is_some(),
        }
    }

    // -- Setters --

    /// `lj_tab_set` + store: creates the key even when `val` is nil, like upstream. Clears the
    /// negative metamethod cache.
    pub fn set(&self, key: Value, val: Value) -> Result<(), TabError> {
        self.nomm.set(0);
        self.mutate(|t| {
            let s = t.set_slot(key)?;
            t.write(s, val);
            Ok(())
        })
    }
    /// `lj_tab_setint` + store.
    pub fn set_int(&self, key: i32, val: Value) -> Result<(), TabError> {
        self.mutate(|t| {
            let s = t.set_int_slot(key)?;
            t.write(s, val);
            Ok(())
        })
    }
    /// `lj_tab_setstr` + store. Clears the negative metamethod cache.
    pub fn set_str(&self, key: Gc<LuaStr>, val: Value) -> Result<(), TabError> {
        self.nomm.set(0);
        self.mutate(|t| {
            let s = t.set_str_slot(key)?;
            t.write(s, val);
            Ok(())
        })
    }
    /// Overwrite the value of an existing key without inserting. Returns false if the key has
    /// no slot (VM fast paths: store only if `get` found a non-nil value).
    pub fn set_existing(&self, key: Value, val: Value) -> bool {
        self.nomm.set(0);
        self.mutate(|t| {
            let slot = match key {
                Value::Str(s) => t.find_str(s).map(Slot::Node),
                Value::Num(x) => match num2int_check(x) {
                    Some(k) if (k as u32) < t.asize() => Some(Slot::Arr(k as u32)),
                    Some(k) => t.find_int(k).map(Slot::Node),
                    None => t.find_generic(key).map(Slot::Node),
                },
                Value::Nil => None,
                _ => t.find_generic(key).map(Slot::Node),
            };
            match slot {
                Some(s) => {
                    t.write(s, val);
                    true
                }
                None => false,
            }
        })
    }

    // -- Length, traversal, sizing --

    /// `lj_tab_len`: a border of the table.
    #[inline]
    pub fn len(&self) -> usize {
        self.inner().len()
    }
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.next(Value::Nil).ok().flatten().is_none()
    }

    /// `lj_tab_next`: next `(key, value)` after `key` (nil starts), `Ok(None)` at the end.
    pub fn next(&self, key: Value) -> Result<Option<(Value, Value)>, TabError> {
        let t = self.inner();
        let idx = t.keyindex(key).ok_or(TabError::InvalidNextKey)?;
        Ok(t.next_from(idx).map(|(_, k, v)| (k, v)))
    }

    /// `lj_tab_keyindex`: traversal index following `key` (array slots `0..asize`, then hash
    /// nodes), `None` for a key that is not in the table.
    #[inline]
    pub fn key_index(&self, key: Value) -> Option<u32> {
        self.inner().keyindex(key)
    }

    /// Next non-nil entry at traversal index `>= idx`: `(next_idx, key, value)`. Array keys are
    /// returned as numbers. Used by ITERN-style iteration.
    #[inline]
    pub fn next_at(&self, idx: u32) -> Option<(u32, Value, Value)> {
        self.inner().next_from(idx)
    }

    /// `lj_tab_clear`: set every slot to nil, keeping the sizes.
    pub fn clear(&self) {
        self.mutate(|t| {
            t.array.iter_mut().for_each(|v| *v = Value::Nil);
            t.node.iter_mut().for_each(|n| *n = NIL_NODE);
            t.freetop = t.node.len() as u32;
        })
    }

    /// `lj_tab_resize`.
    pub fn resize(&self, asize: u32, hbits: u32) -> Result<(), TabError> {
        self.mutate(|t| t.resize(asize, hbits))
    }

    /// `lj_tab_reasize`: resize the array part to hold indexes up to `nasize`.
    pub fn reasize(&self, nasize: u32) -> Result<(), TabError> {
        self.mutate(|t| {
            let hbits = if t.hmask() > 0 { fls(t.hmask()) + 1 } else { 0 };
            t.resize(nasize + 1, hbits)
        })
    }

    // -- Metatable and metamethod cache --

    #[inline]
    pub fn metatable(&self) -> Option<Gc<Table>> {
        self.meta.get()
    }
    #[inline]
    pub fn set_metatable(&self, mt: Option<Gc<Table>>) {
        self.meta.set(mt);
    }
    /// Negative metamethod cache bits (bit `mm` set = known absent).
    #[inline]
    pub fn nomm(&self) -> u8 {
        self.nomm.get()
    }
    #[inline]
    pub fn set_nomm(&self, v: u8) {
        self.nomm.set(v);
    }

    /// `lj_meta_cache` on this table used as a metatable: look up `name` (the interned name of
    /// `mm`) and set the negative-cache bit if absent.
    pub fn meta_cache(&self, mm: MM, name: Gc<LuaStr>) -> Option<Value> {
        let v = self.get_str(name);
        if v.is_nil() {
            if (mm as u32) < 8 {
                self.nomm.set(self.nomm.get() | (1u8 << mm as u32));
            }
            None
        } else {
            Some(v)
        }
    }

    /// `lj_meta_fastg` body: negative-cache check, then `meta_cache`.
    #[inline]
    pub fn meta_fast(&self, mm: MM, name: Gc<LuaStr>) -> Option<Value> {
        if (mm as u32) < 8 && self.nomm.get() & (1u8 << mm as u32) != 0 {
            None
        } else {
            self.meta_cache(mm, name)
        }
    }

    // -- GC support --

    /// Clear entries of a weak table whose key or value is about to be collected
    /// (`gc_clearweak`). `may_clear(v, is_val)` implements `gc_mayclear`.
    pub fn clear_weak(&self, mode: u8, mut may_clear: impl FnMut(Value, bool) -> bool) {
        self.mutate(|t| {
            if mode & WEAK_VAL != 0 {
                for v in t.array.iter_mut() {
                    if may_clear(*v, true) {
                        *v = Value::Nil;
                    }
                }
            }
            for n in t.node.iter_mut() {
                if !n.val.is_nil() && (may_clear(n.key, false) || may_clear(n.val, true)) {
                    n.val = Value::Nil;
                }
            }
        })
    }
}

impl Trace for Table {
    /// `gc_traverse_tab`.
    fn trace(&self, m: &mut Marker) {
        let mt = self.metatable();
        let mut weak = 0u8;
        if let Some(mt) = mt {
            m.mark(mt);
            if let Some(name) = m.mode_name()
                && let Some(Value::Str(mode)) = mt.meta_fast(MM::Mode, name)
            {
                for &c in mode.as_bytes() {
                    match c {
                        b'k' => weak |= WEAK_KEY,
                        b'v' => weak |= WEAK_VAL,
                        _ => {}
                    }
                }
                if weak != 0 {
                    weak = m.register_weak_current(weak);
                }
            }
        }
        if weak == WEAK_KV {
            return;
        }
        let t = self.inner();
        if weak & WEAK_VAL == 0 {
            for &v in &t.array {
                m.mark_value(v);
            }
        }
        for n in &t.node {
            if !n.val.is_nil() {
                if weak & WEAK_KEY == 0 {
                    m.mark_value(n.key);
                }
                if weak & WEAK_VAL == 0 {
                    m.mark_value(n.val);
                }
            }
        }
    }

    fn size(&self) -> usize {
        std::mem::size_of::<GcBox<Table>>() + self.inner().bytes()
    }
}

impl GcObject for Table {
    const KIND: GcKind = GcKind::Table;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gc::Heap;
    use crate::lstr::StrTab;

    fn n(x: f64) -> Value {
        Value::Num(x)
    }

    fn keys(t: &Table) -> Vec<Value> {
        let mut out = Vec::new();
        let mut k = Value::Nil;
        while let Some((nk, _)) = t.next(k).unwrap() {
            out.push(nk);
            k = nk;
        }
        out
    }

    #[test]
    fn set_get_basic() {
        let mut heap = Heap::new();
        let mut st = StrTab::new(1);
        let t = Table::new(0, 0).unwrap();
        let a = st.intern(&mut heap, b"a");
        t.set(Value::Str(a), n(1.0)).unwrap();
        t.set(n(1.0), n(10.0)).unwrap();
        t.set(n(2.5), n(25.0)).unwrap();
        t.set(Value::Bool(true), n(7.0)).unwrap();
        assert_eq!(t.get(Value::Str(a)), n(1.0));
        assert_eq!(t.get_str(a), n(1.0));
        assert_eq!(t.get(n(1.0)), n(10.0));
        assert_eq!(t.get_int(1), n(10.0));
        assert_eq!(t.get(n(2.5)), n(25.0));
        assert_eq!(t.get(Value::Bool(true)), n(7.0));
        assert_eq!(t.get(Value::Bool(false)), Value::Nil);
        assert_eq!(t.get(Value::Nil), Value::Nil);
        assert_eq!(t.set(Value::Nil, n(1.0)), Err(TabError::NilIndex));
        assert_eq!(t.set(n(f64::NAN), n(1.0)), Err(TabError::NanIndex));
        assert_eq!(TabError::NilIndex.msg(), "table index is nil");
        assert_eq!(TabError::NanIndex.msg(), "table index is NaN");
    }

    #[test]
    fn neg_zero_and_integer_float_keys() {
        let t = Table::new(0, 0).unwrap();
        t.set(n(-0.0), n(1.0)).unwrap();
        assert_eq!(t.get(n(0.0)), n(1.0));
        let (k, _) = t.next(Value::Nil).unwrap().unwrap();
        assert!(k.as_num().unwrap().is_sign_positive());
        t.set(n(3.0), n(3.0)).unwrap();
        assert_eq!(t.get_int(3), n(3.0));
        // 2^40 is integral but not int32: hashed as a double.
        t.set(n(1099511627776.0), n(4.0)).unwrap();
        assert_eq!(t.get(n(1099511627776.0)), n(4.0));
        t.set(n(-5.0), n(5.0)).unwrap();
        assert_eq!(t.get_int(-5), n(5.0));
        assert_eq!(t.get(n(-5.0)), n(5.0));
    }

    #[test]
    fn rehash_growth_and_len() {
        // t = {} ; for i=1,100 do t[i]=i end -> array part sized by rehash.
        let t = Table::new(0, 0).unwrap();
        for i in 1..=100 {
            t.set(n(i as f64), n(i as f64)).unwrap();
        }
        assert_eq!(t.len(), 100);
        assert_eq!(t.asize(), 129);
        assert_eq!(t.hmask(), 0);
        for i in 1..=100 {
            assert_eq!(t.get_int(i), n(i as f64));
        }
        // Reference: `local t={} for i=1,100 do t[i]=i end; for k in pairs(t)` yields 1..100.
        let ks = keys(&t);
        assert_eq!(ks.len(), 100);
        assert_eq!(ks[0], n(1.0));
        assert_eq!(ks[99], n(100.0));
    }

    #[test]
    fn len_with_holes() {
        // luajit: t=table.new(4,0) (asize 5); t[1]=1 t[2]=2 t[4]=4; print(#t) --> 4
        let t = Table::new(5, 0).unwrap();
        for (i, v) in [Some(1.0), Some(2.0), None, Some(4.0)].iter().enumerate() {
            t.set_int(i as i32 + 1, v.map_or(Value::Nil, n)).unwrap();
        }
        assert_eq!(t.len(), 4);
        // luajit: t=table.new(5,0) (asize 6); t[1]=1 t[2]=2 t[4]=4; print(#t) --> 2
        let t = Table::new(6, 0).unwrap();
        for (i, v) in [Some(1.0), Some(2.0), None, Some(4.0), None].iter().enumerate() {
            t.set_int(i as i32 + 1, v.map_or(Value::Nil, n)).unwrap();
        }
        assert_eq!(t.len(), 2);
        // Hash-only integer keys: local t={} t[1]=1 t[2]=2 t[4]=4 print(#t) --> 4
        let t = Table::new(0, 0).unwrap();
        t.set(n(1.0), n(1.0)).unwrap();
        t.set(n(2.0), n(2.0)).unwrap();
        t.set(n(4.0), n(4.0)).unwrap();
        assert_eq!(t.len(), 4);
        assert_eq!(Table::new(0, 0).unwrap().len(), 0);
    }

    #[test]
    fn next_with_nil_assignment_during_traversal() {
        let mut heap = Heap::new();
        let mut st = StrTab::new(1);
        let t = Table::new(0, 0).unwrap();
        let names: Vec<_> = (0..20).map(|i| st.intern(&mut heap, format!("k{i}").as_bytes())).collect();
        for (i, s) in names.iter().enumerate() {
            t.set(Value::Str(*s), n(i as f64)).unwrap();
        }
        for i in 1..=10 {
            t.set_int(i, n(i as f64)).unwrap();
        }
        let mut seen = 0;
        let mut k = Value::Nil;
        while let Some((nk, _)) = t.next(k).unwrap() {
            seen += 1;
            t.set(nk, Value::Nil).unwrap(); // Clearing existing keys keeps traversal valid.
            k = nk;
        }
        assert_eq!(seen, 30);
        assert!(t.next(Value::Nil).unwrap().is_none());
        assert_eq!(t.len(), 0);
        // Dead keys are still valid traversal keys.
        assert!(t.next(Value::Str(names[3])).is_ok());
        let absent = st.intern(&mut heap, b"absent");
        assert_eq!(t.next(Value::Str(absent)), Err(TabError::InvalidNextKey));
        assert_eq!(TabError::InvalidNextKey.msg(), "invalid key to 'next'");
    }

    #[test]
    fn array_then_hash_order() {
        let mut heap = Heap::new();
        let mut st = StrTab::new(1);
        let t = Table::new(4, 1).unwrap();
        let x = st.intern(&mut heap, b"x");
        t.set(Value::Str(x), n(9.0)).unwrap();
        t.set_int(0, n(0.0)).unwrap();
        t.set_int(2, n(2.0)).unwrap();
        let ks = keys(&t);
        assert_eq!(ks, vec![n(0.0), n(2.0), Value::Str(x)]);
    }

    #[test]
    fn many_keys_and_resize_shrink() {
        let t = Table::new(0, 0).unwrap();
        for i in 0..1000 {
            t.set(n(i as f64 + 0.5), n(i as f64)).unwrap();
        }
        for i in 0..1000 {
            assert_eq!(t.get(n(i as f64 + 0.5)), n(i as f64));
        }
        assert_eq!(keys(&t).len(), 1000);
        // Shrinking the array part moves values into the hash part.
        let t = Table::new(10, 0).unwrap();
        for i in 1..10 {
            t.set_int(i, n(i as f64)).unwrap();
        }
        t.resize(2, 4).unwrap();
        assert_eq!(t.asize(), 2);
        for i in 1..10 {
            assert_eq!(t.get_int(i), n(i as f64));
        }
    }

    #[test]
    fn dup_and_clear() {
        let tpl = Table::new(3, 1).unwrap();
        tpl.set_int(1, n(1.0)).unwrap();
        tpl.set(Value::Bool(true), n(2.0)).unwrap();
        let c = tpl.dup();
        assert_eq!(c.get_int(1), n(1.0));
        assert_eq!(c.get(Value::Bool(true)), n(2.0));
        assert_eq!(c.nomm(), 0);
        c.clear();
        assert!(c.is_empty());
        assert_eq!(tpl.get_int(1), n(1.0));
    }

    #[test]
    fn hash_order_matches_reference() {
        // luajit: local t={} for _,k in ipairs{1.5,2.5,0.25,-3,1e10,true,false,7,100,0.5,-0.5,3.75}
        //   do t[k]=1 end for k in pairs(t) do io.write(tostring(k),' ') end
        // --> false true 0.25 3.75 1.5 0.5 -0.5 -3 7 10000000000 2.5 100
        // then t[2.5]=nil t[0.125]=1 t[9.5]=1
        // --> false true 9.5 0.25 3.75 1.5 0.125 0.5 -0.5 -3 7 10000000000 100
        let t = Table::new(0, 0).unwrap();
        let ins = [
            n(1.5),
            n(2.5),
            n(0.25),
            n(-3.0),
            n(1e10),
            Value::Bool(true),
            Value::Bool(false),
            n(7.0),
            n(100.0),
            n(0.5),
            n(-0.5),
            n(3.75),
        ];
        for k in ins {
            t.set(k, n(1.0)).unwrap();
        }
        let b = Value::Bool;
        let expect1 = [b(false), b(true), n(0.25), n(3.75), n(1.5), n(0.5), n(-0.5), n(-3.0), n(7.0), n(1e10), n(2.5), n(100.0)];
        assert_eq!(keys(&t), expect1);
        t.set(n(2.5), Value::Nil).unwrap();
        t.set(n(0.125), n(1.0)).unwrap();
        t.set(n(9.5), n(1.0)).unwrap();
        let expect2 = [b(false), b(true), n(9.5), n(0.25), n(3.75), n(1.5), n(0.125), n(0.5), n(-0.5), n(-3.0), n(7.0), n(1e10), n(100.0)];
        assert_eq!(keys(&t), expect2);
    }

    #[test]
    fn randomized_against_model() {
        // Deterministic LCG; keys mix small ints (array candidates), negative/large ints and
        // fractional numbers; values sometimes nil (deletion).
        let mut seed: u64 = 12345;
        let mut rnd = move || {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (seed >> 33) as u32
        };
        let t = Table::new(0, 0).unwrap();
        let mut model: Vec<(f64, f64)> = Vec::new();
        for step in 0..20000 {
            let r = rnd();
            let k = match r % 4 {
                0 => (r % 64) as f64,
                1 => -((r % 50) as f64),
                2 => (r % 300) as f64 + 0.5,
                _ => (r % 5000) as f64,
            };
            let v = if rnd() % 3 == 0 { None } else { Some(step as f64) };
            t.set(n(k), v.map_or(Value::Nil, n)).unwrap();
            model.retain(|&(mk, _)| mk != k);
            if let Some(v) = v {
                model.push((k, v));
            }
        }
        for &(k, v) in &model {
            assert_eq!(t.get(n(k)), n(v));
        }
        let mut ks = keys(&t);
        assert_eq!(ks.len(), model.len());
        ks.sort_by(|a, b| a.as_num().unwrap().total_cmp(&b.as_num().unwrap()));
        let mut mk: Vec<f64> = model.iter().map(|&(k, _)| k).collect();
        mk.sort_by(f64::total_cmp);
        assert_eq!(ks, mk.into_iter().map(n).collect::<Vec<_>>());
        // The length is a border.
        let l = t.len();
        assert!(l == 0 || !t.get_int(l as i32).is_nil());
        assert!(t.get_int(l as i32 + 1).is_nil());
    }

    #[test]
    fn new_ah_sizes() {
        let t = Table::new_ah(3, 5).unwrap();
        assert_eq!(t.asize(), 4);
        assert_eq!(t.hmask(), 7);
        assert_eq!(hsize2hbits(0), 0);
        assert_eq!(hsize2hbits(1), 1);
        assert_eq!(hsize2hbits(2), 1);
        assert_eq!(hsize2hbits(3), 2);
        assert_eq!(hsize2hbits(64), 6);
    }
}
