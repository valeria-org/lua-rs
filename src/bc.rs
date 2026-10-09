//! Bytecode instruction format. Port of lj_bc.h (plus the MMDEF metamethod list of lj_obj.h).

use std::fmt;

/// Maximum operand values.
pub const BCMAX_A: u32 = 0xff;
pub const BCMAX_B: u32 = 0xff;
pub const BCMAX_C: u32 = 0xff;
pub const BCMAX_D: u32 = 0xffff;
/// Bias of jump offsets stored in D.
pub const BCBIAS_J: u32 = 0x8000;
/// "No register" marker.
pub const NO_REG: u32 = BCMAX_A;
/// "No jump" marker for bytecode positions.
pub const NO_JMP: u32 = !0;

/// Stack slots used by FORI/FORL, relative to operand A.
pub const FORL_IDX: u32 = 0;
pub const FORL_STOP: u32 = 1;
pub const FORL_STEP: u32 = 2;
pub const FORL_EXT: u32 = 3;

/// Upstream `FF_next_N` (builtin id of `next`, used by ISNEXT specialisation).
pub const FF_NEXT_N: u8 = 4;

// -- Metamethods --------------------------------------------------------------------------------

macro_rules! mmdef {
    ($($name:ident = $s:literal),* $(,)?) => {
        /// Metamethods in upstream MMDEF order (FFI and pairs entries enabled). ORDER MM.
        #[derive(Clone, Copy, PartialEq, Eq, Debug, Hash, PartialOrd, Ord)]
        #[repr(u8)]
        pub enum MM { $($name),* }

        impl MM {
            /// All metamethods in order.
            pub const ALL: [MM; MM_MAX] = [$(MM::$name),*];
            /// Metamethod name, e.g. `"__index"`.
            pub fn name(self) -> &'static str {
                match self { $(MM::$name => $s),* }
            }
        }
    };
}

mmdef! {
    Index = "__index", Newindex = "__newindex", Gc = "__gc", Mode = "__mode", Eq = "__eq",
    Len = "__len", Lt = "__lt", Le = "__le", Concat = "__concat", Call = "__call",
    Add = "__add", Sub = "__sub", Mul = "__mul", Div = "__div", Mod = "__mod", Pow = "__pow",
    Unm = "__unm", Metatable = "__metatable", Tostring = "__tostring", New = "__new",
    Pairs = "__pairs", Ipairs = "__ipairs",
}

/// Number of metamethods (`MM__MAX`); also the "no metamethod" code in bytecode modes.
pub const MM_MAX: usize = 22;
/// Last metamethod covered by the negative cache (`MM_FAST`).
pub const MM_FAST: MM = MM::Len;

impl MM {
    #[inline]
    pub fn from_u8(x: u8) -> Option<MM> {
        MM::ALL.get(x as usize).copied()
    }
}

// -- Operand modes ------------------------------------------------------------------------------

/// Bytecode operand modes. ORDER BCMode.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum BCMode {
    None,
    Dst,
    Base,
    Var,
    Rbase,
    Uv,
    Lit,
    Lits,
    Pri,
    Num,
    Str,
    Tab,
    Func,
    Jump,
    Cdata,
}

impl BCMode {
    const ALL: [BCMode; 15] = [
        BCMode::None,
        BCMode::Dst,
        BCMode::Base,
        BCMode::Var,
        BCMode::Rbase,
        BCMode::Uv,
        BCMode::Lit,
        BCMode::Lits,
        BCMode::Pri,
        BCMode::Num,
        BCMode::Str,
        BCMode::Tab,
        BCMode::Func,
        BCMode::Jump,
        BCMode::Cdata,
    ];
    #[inline]
    fn from_bits(x: u16) -> BCMode {
        BCMode::ALL[x as usize]
    }
}

// -- Opcodes ------------------------------------------------------------------------------------

macro_rules! bcm {
    (___) => { BCMode::None };
    (dst) => { BCMode::Dst };
    (base) => { BCMode::Base };
    (var) => { BCMode::Var };
    (rbase) => { BCMode::Rbase };
    (uv) => { BCMode::Uv };
    (lit) => { BCMode::Lit };
    (lits) => { BCMode::Lits };
    (pri) => { BCMode::Pri };
    (num) => { BCMode::Num };
    (str) => { BCMode::Str };
    (tab) => { BCMode::Tab };
    (func) => { BCMode::Func };
    (jump) => { BCMode::Jump };
    (cdata) => { BCMode::Cdata };
}

macro_rules! bcmm {
    (___) => { MM_MAX as u16 };
    (lt) => { MM::Lt as u16 };
    (le) => { MM::Le as u16 };
    (eq) => { MM::Eq as u16 };
    (unm) => { MM::Unm as u16 };
    (len) => { MM::Len as u16 };
    (add) => { MM::Add as u16 };
    (sub) => { MM::Sub as u16 };
    (mul) => { MM::Mul as u16 };
    (div) => { MM::Div as u16 };
    (mod) => { MM::Mod as u16 };
    (pow) => { MM::Pow as u16 };
    (concat) => { MM::Concat as u16 };
    (gc) => { MM::Gc as u16 };
    (index) => { MM::Index as u16 };
    (newindex) => { MM::Newindex as u16 };
    (call) => { MM::Call as u16 };
}

macro_rules! bcdef {
    ($($name:ident, $ma:tt, $mb:tt, $mc:tt, $mm:tt;)*) => {
        /// Bytecode opcodes in exact upstream order (BCDEF).
        #[derive(Clone, Copy, PartialEq, Eq, Debug, Hash, PartialOrd, Ord)]
        #[repr(u8)]
        #[allow(clippy::upper_case_acronyms)]
        pub enum Op { $($name),* }

        impl Op {
            /// All opcodes in order.
            pub const ALL: &'static [Op] = &[$(Op::$name),*];
            /// Opcode name as printed by `luajit -bl` (e.g. `"ISLT"`).
            pub const NAMES: &'static [&'static str] = &[$(stringify!($name)),*];
            /// Packed operand modes (`lj_bc_mode`): `a | b<<3 | c<<7 | mm<<11`.
            pub const MODES: &'static [u16] = &[$(
                bcm!($ma) as u16 | ((bcm!($mb) as u16) << 3) | ((bcm!($mc) as u16) << 7) | (bcmm!($mm) << 11)
            ),*];
        }
    };
}

bcdef! {
    ISLT, var, ___, var, lt;
    ISGE, var, ___, var, lt;
    ISLE, var, ___, var, le;
    ISGT, var, ___, var, le;
    ISEQV, var, ___, var, eq;
    ISNEV, var, ___, var, eq;
    ISEQS, var, ___, str, eq;
    ISNES, var, ___, str, eq;
    ISEQN, var, ___, num, eq;
    ISNEN, var, ___, num, eq;
    ISEQP, var, ___, pri, eq;
    ISNEP, var, ___, pri, eq;
    ISTC, dst, ___, var, ___;
    ISFC, dst, ___, var, ___;
    IST, ___, ___, var, ___;
    ISF, ___, ___, var, ___;
    ISTYPE, var, ___, lit, ___;
    ISNUM, var, ___, lit, ___;
    MOV, dst, ___, var, ___;
    NOT, dst, ___, var, ___;
    UNM, dst, ___, var, unm;
    LEN, dst, ___, var, len;
    ADDVN, dst, var, num, add;
    SUBVN, dst, var, num, sub;
    MULVN, dst, var, num, mul;
    DIVVN, dst, var, num, div;
    MODVN, dst, var, num, mod;
    ADDNV, dst, var, num, add;
    SUBNV, dst, var, num, sub;
    MULNV, dst, var, num, mul;
    DIVNV, dst, var, num, div;
    MODNV, dst, var, num, mod;
    ADDVV, dst, var, var, add;
    SUBVV, dst, var, var, sub;
    MULVV, dst, var, var, mul;
    DIVVV, dst, var, var, div;
    MODVV, dst, var, var, mod;
    POW, dst, var, var, pow;
    CAT, dst, rbase, rbase, concat;
    KSTR, dst, ___, str, ___;
    KCDATA, dst, ___, cdata, ___;
    KSHORT, dst, ___, lits, ___;
    KNUM, dst, ___, num, ___;
    KPRI, dst, ___, pri, ___;
    KNIL, base, ___, base, ___;
    UGET, dst, ___, uv, ___;
    USETV, uv, ___, var, ___;
    USETS, uv, ___, str, ___;
    USETN, uv, ___, num, ___;
    USETP, uv, ___, pri, ___;
    UCLO, rbase, ___, jump, ___;
    FNEW, dst, ___, func, gc;
    TNEW, dst, ___, lit, gc;
    TDUP, dst, ___, tab, gc;
    GGET, dst, ___, str, index;
    GSET, var, ___, str, newindex;
    TGETV, dst, var, var, index;
    TGETS, dst, var, str, index;
    TGETB, dst, var, lit, index;
    TGETR, dst, var, var, index;
    TSETV, var, var, var, newindex;
    TSETS, var, var, str, newindex;
    TSETB, var, var, lit, newindex;
    TSETM, base, ___, num, newindex;
    TSETR, var, var, var, newindex;
    CALLM, base, lit, lit, call;
    CALL, base, lit, lit, call;
    CALLMT, base, ___, lit, call;
    CALLT, base, ___, lit, call;
    ITERC, base, lit, lit, call;
    ITERN, base, lit, lit, call;
    VARG, base, lit, lit, ___;
    ISNEXT, base, ___, jump, ___;
    RETM, base, ___, lit, ___;
    RET, rbase, ___, lit, ___;
    RET0, rbase, ___, lit, ___;
    RET1, rbase, ___, lit, ___;
    FORI, base, ___, jump, ___;
    JFORI, base, ___, jump, ___;
    FORL, base, ___, jump, ___;
    IFORL, base, ___, jump, ___;
    JFORL, base, ___, lit, ___;
    ITERL, base, ___, jump, ___;
    IITERL, base, ___, jump, ___;
    JITERL, base, ___, lit, ___;
    LOOP, rbase, ___, jump, ___;
    ILOOP, rbase, ___, jump, ___;
    JLOOP, rbase, ___, lit, ___;
    JMP, rbase, ___, jump, ___;
    BNOT, dst, ___, var, ___;
    BAND, dst, var, var, ___;
    BOR, dst, var, var, ___;
    BXOR, dst, var, var, ___;
    BSHL, dst, var, var, ___;
    BSHR, dst, var, var, ___;
    BSAR, dst, var, var, ___;
    FUNCF, rbase, ___, ___, ___;
    IFUNCF, rbase, ___, ___, ___;
    JFUNCF, rbase, ___, lit, ___;
    FUNCV, rbase, ___, ___, ___;
    IFUNCV, rbase, ___, ___, ___;
    JFUNCV, rbase, ___, lit, ___;
    FUNCC, rbase, ___, ___, ___;
    FUNCCW, rbase, ___, ___, ___;
}

/// Number of opcodes (`BC__MAX`).
pub const BC_MAX: usize = 104;

const _: () = assert!(Op::FUNCCW as usize + 1 == BC_MAX);
const _: () = assert!((Op::ISEQV as u8 ^ 1) == Op::ISNEV as u8);
const _: () = assert!((Op::ISEQS as u8 ^ 1) == Op::ISNES as u8);
const _: () = assert!((Op::ISEQN as u8 ^ 1) == Op::ISNEN as u8);
const _: () = assert!((Op::ISEQP as u8 ^ 1) == Op::ISNEP as u8);
const _: () = assert!((Op::ISLT as u8 ^ 1) == Op::ISGE as u8);
const _: () = assert!((Op::ISLE as u8 ^ 1) == Op::ISGT as u8);
const _: () = assert!((Op::ISLT as u8 ^ 3) == Op::ISGT as u8);
const _: () = assert!(Op::IST as u8 - Op::ISTC as u8 == Op::ISF as u8 - Op::ISFC as u8);
const _: () = assert!(Op::CALLT as u8 - Op::CALL as u8 == Op::CALLMT as u8 - Op::CALLM as u8);
const _: () = assert!(Op::RETM as u8 + 1 == Op::RET as u8);
const _: () = assert!(Op::FORL as u8 + 2 == Op::JFORL as u8);
const _: () = assert!(Op::ITERL as u8 + 2 == Op::JITERL as u8);
const _: () = assert!(Op::LOOP as u8 + 2 == Op::JLOOP as u8);
const _: () = assert!(Op::FUNCF as u8 + 2 == Op::JFUNCF as u8);
const _: () = assert!(Op::FUNCV as u8 + 2 == Op::JFUNCV as u8);

impl Op {
    #[inline]
    pub fn from_u8(x: u8) -> Option<Op> {
        if (x as usize) < Op::ALL.len() {
            // SAFETY: `Op` is `repr(u8)` with implicit discriminants `0..Op::ALL.len()`.
            Some(unsafe { std::mem::transmute::<u8, Op>(x) })
        } else {
            None
        }
    }
    #[inline]
    pub fn name(self) -> &'static str {
        Op::NAMES[self as usize]
    }
    /// Packed mode word (`lj_bc_mode[op]`), as returned by `jit.util.funcbc`.
    #[inline]
    pub fn mode(self) -> u16 {
        Op::MODES[self as usize]
    }
    #[inline]
    pub fn mode_a(self) -> BCMode {
        BCMode::from_bits(self.mode() & 7)
    }
    #[inline]
    pub fn mode_b(self) -> BCMode {
        BCMode::from_bits((self.mode() >> 3) & 15)
    }
    #[inline]
    pub fn mode_c(self) -> BCMode {
        BCMode::from_bits((self.mode() >> 7) & 15)
    }
    #[inline]
    pub fn mode_d(self) -> BCMode {
        self.mode_c()
    }
    /// True if the instruction has a D operand (B mode is none).
    #[inline]
    pub fn has_d(self) -> bool {
        (self.mode() & (15 << 3)) == 0
    }
    /// Metamethod used by the instruction, if any.
    #[inline]
    pub fn mm(self) -> Option<MM> {
        MM::from_u8((self.mode() >> 11) as u8)
    }
    /// `bc_isret`.
    #[inline]
    pub fn is_ret(self) -> bool {
        matches!(self, Op::RETM | Op::RET | Op::RET0 | Op::RET1)
    }
    /// `bc_isret_or_tail`.
    #[inline]
    pub fn is_ret_or_tail(self) -> bool {
        matches!(self, Op::CALLMT | Op::CALLT) || self.is_ret()
    }
}

// -- Instructions -------------------------------------------------------------------------------

/// A 32-bit instruction: op in bits 0-7, A 8-15, C 16-23, B 24-31, D 16-31.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
#[repr(transparent)]
pub struct Ins(pub u32);

impl Ins {
    /// `BCINS_ABC`.
    #[inline]
    pub const fn abc(op: Op, a: u32, b: u32, c: u32) -> Ins {
        Ins((op as u32) | (a << 8) | (b << 24) | (c << 16))
    }
    /// `BCINS_AD`.
    #[inline]
    pub const fn ad(op: Op, a: u32, d: u32) -> Ins {
        Ins((op as u32) | (a << 8) | (d << 16))
    }
    /// `BCINS_AJ`: jump offset `j` relative to the next instruction.
    #[inline]
    pub const fn aj(op: Op, a: u32, j: i32) -> Ins {
        Ins::ad(op, a, (j + BCBIAS_J as i32) as u32)
    }

    /// Raw opcode byte.
    #[inline]
    pub const fn op_byte(self) -> u8 {
        self.0 as u8
    }
    /// Opcode. Instructions are validated on construction/load, so the byte is always valid.
    #[inline]
    pub fn op(self) -> Op {
        Op::ALL[self.op_byte() as usize]
    }
    #[inline]
    pub const fn a(self) -> u32 {
        (self.0 >> 8) & 0xff
    }
    #[inline]
    pub const fn b(self) -> u32 {
        self.0 >> 24
    }
    #[inline]
    pub const fn c(self) -> u32 {
        (self.0 >> 16) & 0xff
    }
    #[inline]
    pub const fn d(self) -> u32 {
        self.0 >> 16
    }
    /// Signed jump offset.
    #[inline]
    pub const fn j(self) -> i32 {
        self.d() as i32 - BCBIAS_J as i32
    }

    #[inline]
    pub fn set_op(&mut self, op: Op) {
        self.0 = (self.0 & !0xff) | op as u32;
    }
    #[inline]
    pub fn set_a(&mut self, a: u32) {
        self.0 = (self.0 & !0xff00) | ((a & 0xff) << 8);
    }
    #[inline]
    pub fn set_b(&mut self, b: u32) {
        self.0 = (self.0 & 0x00ff_ffff) | ((b & 0xff) << 24);
    }
    #[inline]
    pub fn set_c(&mut self, c: u32) {
        self.0 = (self.0 & !0x00ff_0000) | ((c & 0xff) << 16);
    }
    #[inline]
    pub fn set_d(&mut self, d: u32) {
        self.0 = (self.0 & 0xffff) | ((d & 0xffff) << 16);
    }
    #[inline]
    pub fn set_j(&mut self, j: i32) {
        self.set_d((j + BCBIAS_J as i32) as u32);
    }
}

impl fmt::Debug for Ins {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match Op::from_u8(self.op_byte()) {
            Some(op) if op.has_d() => write!(f, "{} {} {}", op.name(), self.a(), self.d()),
            Some(op) => write!(f, "{} {} {} {}", op.name(), self.a(), self.b(), self.c()),
            None => write!(f, "<bad {:#010x}>", self.0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modes_match_reference() {
        // Values from the reference binary: jit.util.funcbc returns lj_bc_mode[op].
        assert_eq!(Op::FUNCF as u8, 96);
        assert_eq!(Op::FUNCF.mode(), 45060);
        assert_eq!(Op::ADDVN as u8, 22);
        assert_eq!(Op::ADDVN.mode(), 21657);
        assert_eq!(Op::RET1.mode(), 45828);
        assert_eq!(Op::ADDVN.mm(), Some(MM::Add));
        assert_eq!(Op::FUNCF.mm(), None);
        assert_eq!(Op::ISLT.name(), "ISLT");
        assert_eq!(Op::FUNCCW.name(), "FUNCCW");
        assert_eq!(MM::Index.name(), "__index");
        assert_eq!(MM::Ipairs as usize, MM_MAX - 1);
        assert!(Op::KSTR.has_d() && !Op::ADDVV.has_d());
        assert_eq!(Op::TGETS.mode_c(), BCMode::Str);
    }

    #[test]
    fn ins_fields() {
        let i = Ins::abc(Op::ADDVV, 1, 2, 3);
        assert_eq!((i.op(), i.a(), i.b(), i.c()), (Op::ADDVV, 1, 2, 3));
        let mut j = Ins::aj(Op::JMP, 4, -5);
        assert_eq!((j.op(), j.a(), j.j()), (Op::JMP, 4, -5));
        j.set_j(100);
        j.set_a(7);
        assert_eq!((j.a(), j.j()), (7, 100));
        // `RET1 1 2` from the reference: 131404
        assert_eq!(Ins::ad(Op::RET1, 1, 2).0, 131404);
    }
}
