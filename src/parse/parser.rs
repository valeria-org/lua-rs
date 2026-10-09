//! Port of lj_parse.c: Lua parser (source code -> bytecode).

use std::collections::HashMap;

use super::lex::*;
use crate::bc::{BCBIAS_J, BCMAX_B, BCMAX_C, BCMAX_D, FORL_EXT, FORL_IDX, FORL_STEP, FORL_STOP, Ins, NO_JMP, NO_REG, Op};
use crate::cdata::{CData, CTID_COMPLEX_DOUBLE};
use crate::error::{LuaError, LuaResult};
use crate::func::{
    KGc, PROTO_BITOP, PROTO_CHILD, PROTO_FFI, PROTO_FIXUP_RETURN, PROTO_HAS_RETURN, PROTO_UV_IMMUTABLE,
    PROTO_UV_LOCAL, PROTO_VARARG, Proto, VARNAME_FOR_CTL, VARNAME_FOR_GEN, VARNAME_FOR_IDX, VARNAME_FOR_STATE,
    VARNAME_FOR_STEP, VARNAME_FOR_STOP, VarInfo as ProtoVarInfo, VarName,
};
use crate::gc::Gc;
use crate::lstr::LuaStr;
use crate::table::{Table, hsize2hbits};
use crate::value::{Value, num2int_check};

// -- Limits (lj_def.h) --------------------------------------------------------------------------

const LJ_MAX_XLEVEL: u32 = 200;
const LJ_MAX_BCINS: usize = 1 << 26;
const LJ_MAX_SLOTS: u32 = 250;
const LJ_MAX_LOCVAR: u32 = 200;
const LJ_MAX_UPVAL: u32 = 60;
const LJ_MAX_VSTACK: usize = 65536 - LJ_MAX_UPVAL as usize;
const LJ_MIN_VECSZ: usize = 8;
/// Native frame layout (`LJ_FR2`).
pub const LJ_FR2: u32 = 1;

// -- Bytecode helpers on raw instruction words --------------------------------------------------

macro_rules! bc {
    ($op:ident) => {
        (Op::$op as u32)
    };
}

#[inline]
fn bc_op(i: u32) -> u32 {
    i & 0xff
}
#[inline]
fn bc_a(i: u32) -> u32 {
    (i >> 8) & 0xff
}
#[inline]
fn bc_b(i: u32) -> u32 {
    i >> 24
}
#[inline]
fn bc_c(i: u32) -> u32 {
    (i >> 16) & 0xff
}
#[inline]
fn bc_d(i: u32) -> u32 {
    i >> 16
}
#[inline]
fn bc_j(i: u32) -> i32 {
    bc_d(i) as i32 - BCBIAS_J as i32
}
#[inline]
fn setbc_op(i: &mut u32, x: u32) {
    *i = (*i & !0xff) | (x & 0xff);
}
#[inline]
fn setbc_a(i: &mut u32, x: u32) {
    *i = (*i & !0xff00) | ((x & 0xff) << 8);
}
#[inline]
fn setbc_b(i: &mut u32, x: u32) {
    *i = (*i & 0x00ff_ffff) | ((x & 0xff) << 24);
}
#[inline]
fn setbc_d(i: &mut u32, x: u32) {
    *i = (*i & 0xffff) | ((x & 0xffff) << 16);
}
#[inline]
fn setbc_j(i: &mut u32, j: u32) {
    setbc_d(i, j.wrapping_add(BCBIAS_J));
}
#[inline]
fn ins_abc(o: u32, a: u32, b: u32, c: u32) -> u32 {
    o | (a << 8) | (b << 24) | (c << 16)
}
#[inline]
fn ins_ad(o: u32, a: u32, d: u32) -> u32 {
    o | (a << 8) | (d << 16)
}
#[inline]
fn ins_aj(o: u32, a: u32, j: u32) -> u32 {
    ins_ad(o, a, j.wrapping_add(BCBIAS_J) & 0xffff)
}
#[inline]
fn is_ret_or_tail(op: u32) -> bool {
    op == bc!(CALLMT) || op == bc!(CALLT) || op == bc!(RETM) || op == bc!(RET) || op == bc!(RET0) || op == bc!(RET1)
}
#[inline]
fn checki16(k: i32) -> bool {
    k == k as i16 as i32
}
#[inline]
fn checku8(k: i32) -> bool {
    k == k as u8 as i32
}

/// `lj_vm_foldarith` for the operators the parser folds (ORDER ARITH).
fn vm_foldarith(x: f64, y: f64, op: u32) -> f64 {
    match op {
        0 => x + y,
        1 => x - y,
        2 => x * y,
        3 => x / y,
        4 => x - (x / y).floor() * y,
        5 => x.powf(y),
        _ => x,
    }
}

/// `lj_num2bit` (`lj_vm_tobit`).
#[inline]
fn num2bit(x: f64) -> i32 {
    (x + 6755399441055744.0).to_bits() as u32 as i32
}

// -- Parser structures and definitions ----------------------------------------------------------

/// Expression kinds. Constant expressions come first and in this order.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
#[repr(u8)]
enum ExpKind {
    KNil,
    KFalse,
    KTrue,
    /// sval = string value
    KStr,
    /// nval = number value
    KNum,
    /// cdata = cdata value, not treated as a constant expression
    KCData,
    /// info = local register, aux = vstack index
    Local,
    /// info = upvalue index, aux = vstack index
    Upval,
    /// sval = string value
    Global,
    /// info = table register, aux = index reg/byte/string const
    Indexed,
    /// info = instruction PC
    Jmp,
    /// info = instruction PC
    Relocable,
    /// info = result register
    NonReloc,
    /// info = instruction PC, aux = base
    Call,
    /// info = instruction PC, aux = base
    CallNav,
    Void,
}
use ExpKind as K;

const VKLAST: ExpKind = K::KNum;

/// Expression descriptor.
#[derive(Clone, Copy, Debug)]
struct ExpDesc {
    k: ExpKind,
    info: u32,
    aux: u32,
    sval: Option<Gc<LuaStr>>,
    nval: f64,
    cdata: Option<Gc<CData>>,
    /// True condition jump list.
    t: u32,
    /// False condition jump list.
    f: u32,
}

impl ExpDesc {
    fn new(k: ExpKind, info: u32) -> ExpDesc {
        ExpDesc { k, info, aux: 0, sval: None, nval: 0.0, cdata: None, t: NO_JMP, f: NO_JMP }
    }
    /// `expr_init`.
    #[inline]
    fn init(&mut self, k: ExpKind, info: u32) {
        self.k = k;
        self.info = info;
        self.t = NO_JMP;
        self.f = NO_JMP;
    }
    #[inline]
    fn hasjump(&self) -> bool {
        self.t != self.f
    }
    #[inline]
    fn isk(&self) -> bool {
        self.k <= VKLAST
    }
    #[inline]
    fn isk_nojump(&self) -> bool {
        self.isk() && !self.hasjump()
    }
    #[inline]
    fn isnumk(&self) -> bool {
        self.k == K::KNum
    }
    #[inline]
    fn isnumk_nojump(&self) -> bool {
        self.isnumk() && !self.hasjump()
    }
    #[inline]
    fn isstrk(&self) -> bool {
        self.k == K::KStr
    }
    #[inline]
    fn bitv(&self) -> i32 {
        num2bit(self.nval)
    }
    /// Check number constant for +-0.
    #[inline]
    fn numiszero(&self) -> bool {
        self.nval == 0.0
    }
    fn str(&self) -> Gc<LuaStr> {
        self.sval.expect("string expression without string")
    }
}

// Expression flags.
const EXPR_F_NORES: u32 = 0x01;
const EXPR_F_NOCOLON: u32 = 0x02;
const EXPR_F_NONAV: u32 = 0x04;
const EXPR_F_RET1: u32 = 0x08;

/// Per-function scope block.
#[derive(Clone, Copy)]
struct FuncScope {
    /// Start of block-local variables.
    vstart: usize,
    /// Number of active vars outside the scope.
    nactvar: u32,
    flags: u8,
}

const FSCOPE_LOOP: u8 = 0x01;
const FSCOPE_BREAK: u8 = 0x02;
const FSCOPE_GOLA: u8 = 0x04;
const FSCOPE_UPVAL: u8 = 0x08;
const FSCOPE_NOCLOSE: u8 = 0x10;
const FSCOPE_CONT: u8 = 0x20;

/// Name of a variable stack entry: a string, one of the small fixed ids (`VARNAME_*`, or the
/// break/continue pseudo labels), or null (invalidated goto/label). Compared by identity like
/// upstream's `GCRef`s.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum VName {
    Null,
    Fixed(u8),
    Str(Gc<LuaStr>),
}

const NAME_BREAK: VName = VName::Fixed(1);
const NAME_CONT: VName = VName::Fixed(2);

const VINDEX_NONE: u16 = 0xffff;
const LJ_VINDEX_HSIZE: usize = 32;
const LJ_VINDEX_MASK: u32 = (LJ_VINDEX_HSIZE - 1) as u32;

const VSTACK_VAR_RW: u8 = 0x01;
const VSTACK_GOTO: u8 = 0x02;
const VSTACK_LABEL: u8 = 0x04;
const VSTACK_CONST: u8 = 0x08;

/// Info for local variables, gotos and labels (`VarInfo` of lj_lex.h).
#[derive(Clone, Copy)]
struct VarInfo {
    name: VName,
    startpc: u32,
    endpc: u32,
    slot: u8,
    info: u8,
    prev: u16,
}

impl VarInfo {
    #[inline]
    fn isgoto(&self) -> bool {
        self.info & VSTACK_GOTO != 0
    }
    #[inline]
    fn islabel(&self) -> bool {
        self.info & VSTACK_LABEL != 0
    }
    #[inline]
    fn isgotolabel(&self) -> bool {
        self.info & (VSTACK_GOTO | VSTACK_LABEL) != 0
    }
}

/// Hash of a variable name.
#[inline]
fn var_hash(name: VName) -> Option<usize> {
    match name {
        VName::Str(s) => Some((s.sid() & LJ_VINDEX_MASK) as usize),
        _ => None,
    }
}

/// Combined bytecode instruction and line.
#[derive(Clone, Copy, Default)]
struct BCInsLine {
    ins: u32,
    line: u32,
}

/// A collectable constant of a function under construction.
enum KSlot {
    Str(Gc<LuaStr>),
    /// Finished child prototype, with raw `uv` entries still to be fixed up by the parent
    /// (`fs_fixup_uv2`).
    Proto(Proto),
    Table(Gc<Table>),
    CData(Gc<CData>),
}

/// Table of constants (`fs->kt`): slot assignment for numbers and strings, in slot order.
#[derive(Default)]
struct KTab {
    nums: HashMap<u64, u32>,
    strs: HashMap<Gc<LuaStr>, u32>,
    knum: Vec<f64>,
    kgc: Vec<KSlot>,
}

/// Per-function state.
struct FuncState {
    kt: KTab,
    /// Scope stack (`fs->bl` is the last entry).
    bl: Vec<FuncScope>,
    pc: u32,
    lasttarget: u32,
    jpc: u32,
    freereg: u32,
    nactvar: u32,
    linedefined: u32,
    /// Base of this function's bytecode in the shared bytecode stack.
    bcbase: usize,
    vbase: usize,
    flags: u8,
    numparams: u8,
    framesize: u8,
    nuv: u32,
    varmap: [u16; LJ_MAX_LOCVAR as usize],
    uvmap: [u16; LJ_MAX_UPVAL as usize],
    uvtmp: [u16; LJ_MAX_UPVAL as usize],
}

// Binary operators. ORDER OPR.
const OPR_ADD: u32 = 0;
const OPR_POW: u32 = 5;
const OPR_BAND: u32 = 6;
const OPR_BOR: u32 = 7;
const OPR_BXOR: u32 = 8;
const OPR_BSHL: u32 = 9;
const OPR_BSHR: u32 = 10;
const OPR_BSAR: u32 = 11;
const OPR_CONCAT: u32 = 12;
const OPR_NE: u32 = 13;
const OPR_EQ: u32 = 14;
const OPR_LT: u32 = 15;
const OPR_GE: u32 = 16;
const OPR_LE: u32 = 17;
const OPR_GT: u32 = 18;
const OPR_AND: u32 = 19;
const OPR_OR: u32 = 20;
const OPR_COAL: u32 = 21;
const OPR_NOBINOPR: u32 = 22;

const _: () = assert!(Op::ISGE as u32 - Op::ISLT as u32 == OPR_GE - OPR_LT);
const _: () = assert!(Op::ISLE as u32 - Op::ISLT as u32 == OPR_LE - OPR_LT);
const _: () = assert!(Op::ISGT as u32 - Op::ISLT as u32 == OPR_GT - OPR_LT);
const _: () = assert!(Op::MODVV as u32 - Op::ADDVV as u32 == 4);
const _: () = assert!(Op::BSAR as u32 - Op::BAND as u32 == OPR_BSAR - OPR_BAND);

/// Priorities for each binary operator. ORDER OPR. (left, right)
const PRIORITY: [(u32, u32); 22] = [
    (10, 10), (10, 10), (11, 11), (11, 11), (11, 11), // ADD SUB MUL DIV MOD
    (14, 13),                                         // POW (right associative)
    (6, 6), (4, 4), (5, 5),                           // BAND BOR BXOR
    (7, 7), (7, 7), (7, 7),                           // BSHL BSHR BSAR
    (9, 8),                                           // CONCAT (right associative)
    (3, 3), (3, 3),                                   // EQ NE
    (3, 3), (3, 3), (3, 3), (3, 3),                   // LT GE GT LE
    (2, 2), (1, 1), (1, 1),                           // AND OR COAL
];

const UNARY_PRIORITY: u32 = 12;

/// Convert token to binary operator.
fn token2binop(tok: LexToken) -> u32 {
    match tok {
        0x2b => 0,          // '+'
        0x2d => 1,          // '-'
        0x2a => 2,          // '*'
        0x2f => 3,          // '/'
        0x25 => 4,          // '%'
        0x5e => OPR_POW,    // '^'
        0x26 => OPR_BAND,   // '&'
        0x7c => OPR_BOR,    // '|'
        0x7e => OPR_BXOR,   // '~'
        TK_SHL => OPR_BSHL,
        TK_SHR => OPR_BSHR,
        TK_SAR => OPR_BSAR,
        TK_CONCAT => OPR_CONCAT,
        TK_NE | TK_NE_ => OPR_NE,
        TK_EQ => OPR_EQ,
        0x3c => OPR_LT,
        TK_LE => OPR_LE,
        0x3e => OPR_GT,
        TK_GE => OPR_GE,
        TK_AND | TK_AND_ => OPR_AND,
        TK_OR | TK_OR_ => OPR_OR,
        TK_COAL => OPR_COAL,
        _ => OPR_NOBINOPR,
    }
}

/// Check for a name, including soft keywords.
#[inline]
fn lex_isname(tok: LexToken) -> bool {
    tok == TK_NAME || tok == TK_GOTO || tok == TK_CONTINUE || tok == TK_CONST
}

/// Check for end of block.
#[inline]
fn parse_isend(tok: LexToken) -> bool {
    matches!(tok, TK_ELSE | TK_ELSEIF | TK_END | TK_UNTIL | TK_EOF)
}

fn cat(parts: &[&[u8]]) -> Vec<u8> {
    parts.concat()
}

type R<T = ()> = LuaResult<T>;

/// Parser state: lexer plus the parts of `LexState` used for code generation, and the stack of
/// `FuncState`s (innermost last).
pub struct Parser<'a> {
    pub ls: LexState<'a>,
    fss: Vec<FuncState>,
    vstack: Vec<VarInfo>,
    vtop: usize,
    vhash: [u16; LJ_VINDEX_HSIZE],
    bcstack: Vec<BCInsLine>,
    level: u32,
    fr2: u32,
}

impl<'a> Parser<'a> {
    pub fn new(ls: LexState<'a>, fr2: bool) -> Parser<'a> {
        Parser {
            ls,
            fss: Vec::new(),
            vstack: Vec::new(),
            vtop: 0,
            vhash: [VINDEX_NONE; LJ_VINDEX_HSIZE],
            bcstack: Vec::new(),
            level: 0,
            fr2: fr2 as u32,
        }
    }

    // -- Accessors --

    #[inline]
    fn fs(&self) -> &FuncState {
        self.fss.last().expect("no FuncState")
    }
    #[inline]
    fn fsm(&mut self) -> &mut FuncState {
        self.fss.last_mut().expect("no FuncState")
    }
    #[inline]
    fn bl(&mut self) -> &mut FuncScope {
        self.fsm().bl.last_mut().expect("no scope")
    }
    #[inline]
    fn ins(&self, pc: u32) -> u32 {
        self.bcstack[self.fs().bcbase + pc as usize].ins
    }
    #[inline]
    fn insm(&mut self, pc: u32) -> &mut u32 {
        let i = self.fs().bcbase + pc as usize;
        &mut self.bcstack[i].ins
    }
    #[inline]
    fn set_line(&mut self, pc: u32, line: u32) {
        let i = self.fs().bcbase + pc as usize;
        self.bcstack[i].line = line;
    }
    #[inline]
    fn line_at(&self, pc: u32) -> u32 {
        self.bcstack[self.fs().bcbase + pc as usize].line
    }
    /// `var_get(ls, fs, i)` as a vstack index.
    #[inline]
    fn var_idx(&self, i: u32) -> usize {
        self.fs().varmap[i as usize] as usize
    }

    // -- Error handling --

    fn err_syntax(&mut self, em: &[u8]) -> LuaError {
        let tok = self.ls.tok;
        self.ls.error(tok, em)
    }

    fn err_token(&mut self, tok: LexToken) -> LuaError {
        let msg = cat(&[&qs(&self.ls.token2str(tok)), b" expected"]);
        let cur = self.ls.tok;
        self.ls.error(cur, &msg)
    }

    fn err_limit_at(&mut self, fsi: usize, limit: u32, what: &str) -> LuaError {
        let ld = self.fss[fsi].linedefined;
        let msg = if ld == 0 {
            format!("main function has more than {limit} {what}")
        } else {
            format!("function at line {ld} has more than {limit} {what}")
        };
        self.ls.error(0, msg.as_bytes())
    }

    fn err_limit(&mut self, limit: u32, what: &str) -> LuaError {
        let fsi = self.fss.len() - 1;
        self.err_limit_at(fsi, limit, what)
    }

    fn checklimit(&mut self, v: u32, l: u32, m: &str) -> R {
        if v >= l { Err(self.err_limit(l, m)) } else { Ok(()) }
    }

    // -- Management of constants --

    /// Add a number constant.
    fn const_num(&mut self, e: &ExpDesc) -> u32 {
        debug_assert!(e.isnumk());
        let n = if e.nval == 0.0 { 0.0 } else { e.nval };
        let kt = &mut self.fsm().kt;
        if let Some(&k) = kt.nums.get(&n.to_bits()) {
            return k;
        }
        let k = kt.knum.len() as u32;
        kt.nums.insert(n.to_bits(), k);
        kt.knum.push(n);
        k
    }

    /// Add a string constant.
    fn const_str_s(&mut self, s: Gc<LuaStr>) -> u32 {
        let kt = &mut self.fsm().kt;
        if let Some(&k) = kt.strs.get(&s) {
            return k;
        }
        let k = kt.kgc.len() as u32;
        kt.strs.insert(s, k);
        kt.kgc.push(KSlot::Str(s));
        k
    }

    fn const_str(&mut self, e: &ExpDesc) -> u32 {
        debug_assert!(e.isstrk() || e.k == K::Global);
        self.const_str_s(e.str())
    }

    /// Add a non-string GC object constant (always a new object, hence a new slot).
    fn const_gc_new(&mut self, k: KSlot) -> u32 {
        let kt = &mut self.fsm().kt;
        let idx = kt.kgc.len() as u32;
        kt.kgc.push(k);
        idx
    }

    // -- Jump list handling --

    /// Get next element in jump list.
    fn jmp_next(&self, pc: u32) -> u32 {
        let delta = bc_j(self.ins(pc));
        if delta == -1 { NO_JMP } else { (pc as i32 + 1 + delta) as u32 }
    }

    /// Check if any of the instructions on the jump list produce no value.
    fn jmp_novalue(&self, mut list: u32) -> bool {
        while list != NO_JMP {
            let p = self.ins(if list >= 1 { list - 1 } else { list });
            if !(bc_op(p) == bc!(ISTC) || bc_op(p) == bc!(ISFC) || bc_a(p) == NO_REG) {
                return true;
            }
            list = self.jmp_next(list);
        }
        false
    }

    /// Patch register of test instructions.
    fn jmp_patchtestreg(&mut self, pc: u32, reg: u32) -> bool {
        let ip = if pc >= 1 { pc - 1 } else { pc };
        let ins = self.ins(ip);
        let op = bc_op(ins);
        if op == bc!(ISTC) || op == bc!(ISFC) {
            let p = self.insm(ip);
            if reg != NO_REG && reg != bc_d(ins) {
                setbc_a(p, reg);
            } else {
                // Nothing to store or already in the right register.
                setbc_op(p, op + (bc!(IST) - bc!(ISTC)));
                setbc_a(p, 0);
            }
        } else if bc_a(ins) == NO_REG {
            if reg == NO_REG {
                let a = bc_a(self.ins(pc));
                *self.insm(ip) = ins_aj(bc!(JMP), a, 0);
            } else {
                setbc_a(self.insm(ip), reg);
                let next = self.ins(ip + 1);
                if reg >= bc_a(next) {
                    setbc_a(self.insm(ip + 1), reg + 1);
                }
            }
        } else {
            return false; // Cannot patch other instructions.
        }
        true
    }

    /// Drop values for all instructions on jump list.
    fn jmp_dropval(&mut self, mut list: u32) {
        while list != NO_JMP {
            self.jmp_patchtestreg(list, NO_REG);
            list = self.jmp_next(list);
        }
    }

    /// Patch jump instruction to target.
    fn jmp_patchins(&mut self, pc: u32, dest: u32) -> R {
        debug_assert!(dest != NO_JMP, "uninitialized jump target");
        let offset = dest.wrapping_sub(pc + 1).wrapping_add(BCBIAS_J);
        if offset > BCMAX_D {
            return Err(self.err_syntax(b"control structure too long"));
        }
        setbc_d(self.insm(pc), offset);
        Ok(())
    }

    /// Append to jump list.
    fn jmp_append(&mut self, l1: &mut u32, l2: u32) -> R {
        if l2 == NO_JMP {
            return Ok(());
        } else if *l1 == NO_JMP {
            *l1 = l2;
        } else {
            let mut list = *l1;
            loop {
                let next = self.jmp_next(list);
                if next == NO_JMP {
                    break;
                }
                list = next;
            }
            self.jmp_patchins(list, l2)?;
        }
        Ok(())
    }

    /// Patch jump list and preserve produced values.
    fn jmp_patchval(&mut self, mut list: u32, vtarget: u32, reg: u32, dtarget: u32) -> R {
        while list != NO_JMP {
            let next = self.jmp_next(list);
            if self.jmp_patchtestreg(list, reg) {
                self.jmp_patchins(list, vtarget)?; // Jump to target with value.
            } else {
                self.jmp_patchins(list, dtarget)?; // Jump to default target.
            }
            list = next;
        }
        Ok(())
    }

    /// Jump to following instruction. Append to list of pending jumps.
    fn jmp_tohere(&mut self, list: u32) -> R {
        let pc = self.fs().pc;
        self.fsm().lasttarget = pc;
        let mut jpc = self.fs().jpc;
        self.jmp_append(&mut jpc, list)?;
        self.fsm().jpc = jpc;
        Ok(())
    }

    /// Patch jump list to target.
    fn jmp_patch(&mut self, list: u32, target: u32) -> R {
        if target == self.fs().pc {
            self.jmp_tohere(list)
        } else {
            debug_assert!(target < self.fs().pc, "bad jump target");
            self.jmp_patchval(list, target, NO_REG, target)
        }
    }

    // -- Bytecode register allocator --

    /// Bump frame size.
    fn bcreg_bump(&mut self, n: u32) -> R {
        let sz = self.fs().freereg + n;
        if sz > self.fs().framesize as u32 {
            if sz >= LJ_MAX_SLOTS {
                return Err(self.err_syntax(b"function or expression too complex"));
            }
            self.fsm().framesize = sz as u8;
        }
        Ok(())
    }

    /// Reserve registers.
    fn bcreg_reserve(&mut self, n: u32) -> R {
        self.bcreg_bump(n)?;
        self.fsm().freereg += n;
        Ok(())
    }

    /// Free register.
    fn bcreg_free(&mut self, reg: u32) {
        let fs = self.fsm();
        if reg >= fs.nactvar {
            fs.freereg -= 1;
            debug_assert!(reg == fs.freereg, "bad regfree");
        }
    }

    /// Free register for expression.
    fn expr_free(&mut self, e: &ExpDesc) {
        if e.k == K::NonReloc {
            self.bcreg_free(e.info);
        }
    }

    // -- Bytecode emitter --

    /// Emit bytecode instruction.
    fn bcemit_ins(&mut self, ins: u32) -> R<u32> {
        let pc = self.fs().pc;
        let jpc = self.fs().jpc;
        self.jmp_patchval(jpc, pc, NO_REG, pc)?;
        self.fsm().jpc = NO_JMP;
        let idx = self.fs().bcbase + pc as usize;
        if idx >= self.bcstack.len() {
            let size = self.bcstack.len();
            if size >= LJ_MAX_BCINS {
                return Err(self.err_limit(LJ_MAX_BCINS as u32, "bytecode instructions"));
            }
            let nsize = (size << 1).clamp(LJ_MIN_VECSZ, LJ_MAX_BCINS);
            self.bcstack.resize(nsize, BCInsLine::default());
        }
        self.bcstack[idx] = BCInsLine { ins, line: self.ls.lastline };
        self.fsm().pc = pc + 1;
        Ok(pc)
    }

    fn bcemit_abc(&mut self, o: u32, a: u32, b: u32, c: u32) -> R<u32> {
        self.bcemit_ins(ins_abc(o, a, b, c))
    }
    fn bcemit_ad(&mut self, o: u32, a: u32, d: u32) -> R<u32> {
        self.bcemit_ins(ins_ad(o, a, d))
    }
    fn bcemit_aj(&mut self, o: u32, a: u32, j: u32) -> R<u32> {
        self.bcemit_ins(ins_aj(o, a, j))
    }

    // -- Bytecode emitter for expressions --

    /// Discharge non-constant expression to any register.
    fn expr_discharge(&mut self, e: &mut ExpDesc) -> R {
        let ins;
        match e.k {
            K::Upval => ins = ins_ad(bc!(UGET), 0, e.info),
            K::Global => ins = ins_ad(bc!(GGET), 0, self.const_str(e)),
            K::Indexed => {
                let rc = e.aux;
                if (rc as i32) < 0 {
                    ins = ins_abc(bc!(TGETS), 0, e.info, !rc);
                } else if rc > BCMAX_C {
                    ins = ins_abc(bc!(TGETB), 0, e.info, rc - (BCMAX_C + 1));
                } else {
                    self.bcreg_free(rc);
                    ins = ins_abc(bc!(TGETV), 0, e.info, rc);
                }
                self.bcreg_free(e.info);
            }
            K::Call | K::CallNav => {
                e.info = e.aux;
                e.k = K::NonReloc;
                return Ok(());
            }
            K::Local => {
                e.k = K::NonReloc;
                return Ok(());
            }
            _ => return Ok(()),
        }
        e.info = self.bcemit_ins(ins)?;
        e.k = K::Relocable;
        Ok(())
    }

    /// Emit bytecode to set a range of registers to nil.
    fn bcemit_nil(&mut self, mut from: u32, mut n: u32) -> R {
        let fs = self.fs();
        if fs.pc > fs.lasttarget {
            // No jumps to current position?
            let ppc = fs.pc - 1;
            let ip = self.ins(ppc);
            let pfrom = bc_a(ip);
            let op = bc_op(ip);
            if op == bc!(KPRI) {
                if bc_d(ip) == 0 {
                    let mut merge = true;
                    if from == pfrom {
                        if n == 1 {
                            return Ok(());
                        }
                    } else if from == pfrom + 1 {
                        from = pfrom;
                        n += 1;
                    } else {
                        merge = false;
                    }
                    if merge {
                        *self.insm(ppc) = ins_ad(bc!(KNIL), from, from + n - 1); // Replace KPRI.
                        return Ok(());
                    }
                }
            } else if op == bc!(KNIL) {
                let pto = bc_d(ip);
                if pfrom <= from && from <= pto + 1 {
                    // Can we connect both ranges?
                    if from + n - 1 > pto {
                        setbc_d(self.insm(ppc), from + n - 1); // Patch previous instruction range.
                    }
                    return Ok(());
                }
            }
        }
        // Emit new instruction or replace old instruction.
        let ins = if n == 1 { ins_ad(bc!(KPRI), from, K::KNil as u32) } else { ins_ad(bc!(KNIL), from, from + n - 1) };
        self.bcemit_ins(ins)?;
        Ok(())
    }

    /// Discharge an expression to a specific register. Ignore branches.
    fn expr_toreg_nobranch(&mut self, e: &mut ExpDesc, reg: u32) -> R {
        self.expr_discharge(e)?;
        let ins = match e.k {
            K::KStr => ins_ad(bc!(KSTR), reg, self.const_str(e)),
            K::KNum => match num2int_check(e.nval) {
                Some(k) if checki16(k) => ins_ad(bc!(KSHORT), reg, k as u16 as u32),
                _ => ins_ad(bc!(KNUM), reg, self.const_num(e)),
            },
            K::KCData => {
                self.fsm().flags |= PROTO_FFI;
                let cd = e.cdata.expect("cdata expression without cdata");
                ins_ad(bc!(KCDATA), reg, self.const_gc_new(KSlot::CData(cd)))
            }
            K::Relocable => {
                setbc_a(self.insm(e.info), reg);
                e.info = reg;
                e.k = K::NonReloc;
                return Ok(());
            }
            K::NonReloc => {
                if reg == e.info {
                    e.info = reg;
                    e.k = K::NonReloc;
                    return Ok(());
                }
                ins_ad(bc!(MOV), reg, e.info)
            }
            K::KNil => {
                self.bcemit_nil(reg, 1)?;
                e.info = reg;
                e.k = K::NonReloc;
                return Ok(());
            }
            K::KFalse | K::KTrue => ins_ad(bc!(KPRI), reg, e.k as u32),
            _ => {
                debug_assert!(e.k == K::Void || e.k == K::Jmp, "bad expr type {:?}", e.k);
                return Ok(());
            }
        };
        self.bcemit_ins(ins)?;
        e.info = reg;
        e.k = K::NonReloc;
        Ok(())
    }

    /// Discharge an expression to a specific register.
    fn expr_toreg(&mut self, e: &mut ExpDesc, reg: u32) -> R {
        self.expr_toreg_nobranch(e, reg)?;
        if e.k == K::Jmp {
            let mut t = e.t;
            self.jmp_append(&mut t, e.info)?; // Add it to the true jump list.
            e.t = t;
        }
        if e.hasjump() {
            // Discharge expression with branches.
            let mut jfalse = NO_JMP;
            let mut jtrue = NO_JMP;
            if self.jmp_novalue(e.t) || self.jmp_novalue(e.f) {
                let jval = if e.k == K::Jmp { NO_JMP } else { self.bcemit_jmp()? };
                jfalse = self.bcemit_ad(bc!(KPRI), reg, K::KFalse as u32)?;
                let freereg = self.fs().freereg;
                self.bcemit_aj(bc!(JMP), freereg, 1)?;
                jtrue = self.bcemit_ad(bc!(KPRI), reg, K::KTrue as u32)?;
                self.jmp_tohere(jval)?;
            }
            let jend = self.fs().pc;
            self.fsm().lasttarget = jend;
            self.jmp_patchval(e.f, jend, reg, jfalse)?;
            self.jmp_patchval(e.t, jend, reg, jtrue)?;
        }
        e.f = NO_JMP;
        e.t = NO_JMP;
        e.info = reg;
        e.k = K::NonReloc;
        Ok(())
    }

    /// Discharge an expression to the next free register.
    fn expr_tonextreg(&mut self, e: &mut ExpDesc) -> R {
        self.expr_discharge(e)?;
        self.expr_free(e);
        self.bcreg_reserve(1)?;
        let reg = self.fs().freereg - 1;
        self.expr_toreg(e, reg)
    }

    /// Discharge an expression to any register.
    fn expr_toanyreg(&mut self, e: &mut ExpDesc) -> R<u32> {
        self.expr_discharge(e)?;
        if e.k == K::NonReloc {
            if !e.hasjump() {
                return Ok(e.info); // Already in a register.
            }
            if e.info >= self.fs().nactvar {
                self.expr_toreg(e, e.info)?; // Discharge to temp. register.
                return Ok(e.info);
            }
        }
        self.expr_tonextreg(e)?; // Discharge to next register.
        Ok(e.info)
    }

    /// Partially discharge expression to a value.
    fn expr_toval(&mut self, e: &mut ExpDesc) -> R {
        if e.hasjump() {
            self.expr_toanyreg(e)?;
            Ok(())
        } else {
            self.expr_discharge(e)
        }
    }

    /// Emit store for LHS expression.
    fn bcemit_store(&mut self, var: &ExpDesc, e: &mut ExpDesc) -> R {
        let ins = match var.k {
            K::Local => {
                debug_assert!(self.vstack[var.aux as usize].info & VSTACK_CONST == 0, "unchecked const assignment");
                self.vstack[var.aux as usize].info |= VSTACK_VAR_RW;
                self.expr_free(e);
                return self.expr_toreg(e, var.info);
            }
            K::Upval => {
                debug_assert!(self.vstack[var.aux as usize].info & VSTACK_CONST == 0, "unchecked const assignment");
                self.vstack[var.aux as usize].info |= VSTACK_VAR_RW;
                self.expr_toval(e)?;
                if e.k <= K::KTrue {
                    ins_ad(bc!(USETP), var.info, e.k as u32)
                } else if e.k == K::KStr {
                    ins_ad(bc!(USETS), var.info, self.const_str(e))
                } else if e.k == K::KNum {
                    ins_ad(bc!(USETN), var.info, self.const_num(e))
                } else {
                    let r = self.expr_toanyreg(e)?;
                    ins_ad(bc!(USETV), var.info, r)
                }
            }
            K::Global => {
                let ra = self.expr_toanyreg(e)?;
                ins_ad(bc!(GSET), ra, self.const_str(var))
            }
            _ => {
                debug_assert!(var.k == K::Indexed, "bad expr type {:?}", var.k);
                let ra = self.expr_toanyreg(e)?;
                let rc = var.aux;
                if (rc as i32) < 0 {
                    ins_abc(bc!(TSETS), ra, var.info, !rc)
                } else if rc > BCMAX_C {
                    ins_abc(bc!(TSETB), ra, var.info, rc - (BCMAX_C + 1))
                } else {
                    // Free late alloced key reg to avoid a bad regfree on free of the value reg.
                    // This can only happen when called from expr_table(), which resets freereg
                    // after the store (upstream does this in LUA_USE_ASSERT builds only).
                    if e.k == K::NonReloc && ra >= self.fs().nactvar && rc >= ra {
                        self.bcreg_free(rc);
                    }
                    ins_abc(bc!(TSETV), ra, var.info, rc)
                }
            }
        };
        self.bcemit_ins(ins)?;
        self.expr_free(e);
        Ok(())
    }

    /// Emit method lookup expression.
    fn bcemit_method(&mut self, e: &mut ExpDesc, key: &ExpDesc) -> R {
        let obj = self.expr_toanyreg(e)?;
        self.expr_free(e);
        let func = self.fs().freereg;
        let fr2 = self.fr2;
        self.bcemit_ad(bc!(MOV), func + 1 + fr2, obj)?; // Copy object to 1st argument.
        debug_assert!(key.isstrk(), "bad usage");
        let idx = self.const_str(key);
        if idx <= BCMAX_C {
            self.bcreg_reserve(2 + fr2)?;
            self.bcemit_abc(bc!(TGETS), func, obj, idx)?;
        } else {
            self.bcreg_reserve(3 + fr2)?;
            self.bcemit_ad(bc!(KSTR), func + 2 + fr2, idx)?;
            self.bcemit_abc(bc!(TGETV), func, obj, func + 2 + fr2)?;
            self.fsm().freereg -= 1;
        }
        e.info = func;
        e.k = K::NonReloc;
        Ok(())
    }

    // -- Bytecode emitter for branches --

    /// Emit unconditional branch.
    fn bcemit_jmp(&mut self) -> R<u32> {
        let jpc = self.fs().jpc;
        let mut j = self.fs().pc.wrapping_sub(1);
        self.fsm().jpc = NO_JMP;
        let lasttarget = self.fs().lasttarget;
        if (j as i32) >= (lasttarget as i32) && bc_op(self.ins(j)) == bc!(UCLO) {
            setbc_j(self.insm(j), NO_JMP);
            self.fsm().lasttarget = j + 1;
        } else {
            let freereg = self.fs().freereg;
            j = self.bcemit_aj(bc!(JMP), freereg, NO_JMP)?;
        }
        self.jmp_append(&mut j, jpc)?;
        Ok(j)
    }

    /// Invert branch condition of bytecode instruction.
    fn invertcond(&mut self, e: &ExpDesc) {
        let p = self.insm(e.info - 1);
        let op = bc_op(*p) ^ 1;
        setbc_op(p, op);
    }

    /// Emit conditional branch.
    fn bcemit_branch(&mut self, e: &mut ExpDesc, cond: bool) -> R<u32> {
        if e.k == K::Relocable {
            let ip = self.ins(e.info);
            if bc_op(ip) == bc!(NOT) {
                *self.insm(e.info) = ins_ad(if cond { bc!(ISF) } else { bc!(IST) }, 0, bc_d(ip));
                return self.bcemit_jmp();
            }
        }
        if e.k != K::NonReloc {
            self.bcreg_reserve(1)?;
            let reg = self.fs().freereg - 1;
            self.expr_toreg_nobranch(e, reg)?;
        }
        self.bcemit_ad(if cond { bc!(ISTC) } else { bc!(ISFC) }, NO_REG, e.info)?;
        let pc = self.bcemit_jmp()?;
        self.expr_free(e);
        Ok(pc)
    }

    /// Emit branch on true condition.
    fn bcemit_branch_t(&mut self, e: &mut ExpDesc) -> R {
        self.expr_discharge(e)?;
        let pc = if e.k == K::KStr || e.k == K::KNum || e.k == K::KTrue {
            NO_JMP // Never jump.
        } else if e.k == K::Jmp {
            self.invertcond(e);
            e.info
        } else if e.k == K::KFalse || e.k == K::KNil {
            self.expr_toreg_nobranch(e, NO_REG)?;
            self.bcemit_jmp()?
        } else {
            self.bcemit_branch(e, false)?
        };
        let mut f = e.f;
        self.jmp_append(&mut f, pc)?;
        e.f = f;
        self.jmp_tohere(e.t)?;
        e.t = NO_JMP;
        Ok(())
    }

    /// Emit branch on false condition.
    fn bcemit_branch_f(&mut self, e: &mut ExpDesc) -> R {
        self.expr_discharge(e)?;
        let pc = if e.k == K::KNil || e.k == K::KFalse {
            NO_JMP // Never jump.
        } else if e.k == K::Jmp {
            e.info
        } else if e.k == K::KStr || e.k == K::KNum || e.k == K::KTrue {
            self.expr_toreg_nobranch(e, NO_REG)?;
            self.bcemit_jmp()?
        } else {
            self.bcemit_branch(e, true)?
        };
        let mut t = e.t;
        self.jmp_append(&mut t, pc)?;
        e.t = t;
        self.jmp_tohere(e.f)?;
        e.f = NO_JMP;
        Ok(())
    }

    // -- Bytecode emitter for operators --

    /// Try constant-folding of arithmetic operators.
    fn foldarith(opr: u32, e1: &mut ExpDesc, e2: &ExpDesc) -> bool {
        if !e1.isnumk_nojump() || !e2.isnumk_nojump() {
            return false;
        }
        let n = vm_foldarith(e1.nval, e2.nval, opr - OPR_ADD);
        if n.is_nan() || (n == 0.0 && n.is_sign_negative()) {
            return false; // Avoid NaN and -0 as consts.
        }
        e1.nval = n;
        true
    }

    /// Try constant-folding of bit operators.
    fn foldbitop(opr: u32, e1: &mut ExpDesc, e2: &ExpDesc) -> bool {
        if e1.isnumk_nojump() && e2.isnumk_nojump() {
            let mut k1 = e1.bitv();
            let k2 = e2.bitv();
            let s = (k2 & 31) as u32;
            match opr {
                OPR_BAND => k1 &= k2,
                OPR_BOR => k1 |= k2,
                OPR_BXOR => k1 ^= k2,
                OPR_BSHL => k1 = k1.wrapping_shl(s),
                OPR_BSHR => k1 = ((k1 as u32) >> s) as i32,
                OPR_BSAR => k1 >>= s,
                _ => unreachable!("bad OPR {opr}"),
            }
            e1.nval = k1 as f64;
            return true;
        }
        false
    }

    /// Emit arithmetic operator.
    fn bcemit_arith(&mut self, opr: u32, e1: &mut ExpDesc, e2: &mut ExpDesc) -> R {
        let (op, rb, rc);
        if opr == OPR_POW {
            op = bc!(POW);
            rc = self.expr_toanyreg(e2)?;
            rb = self.expr_toanyreg(e1)?;
        } else if opr >= OPR_BAND {
            op = opr - OPR_BAND + bc!(BAND);
            rc = self.expr_toanyreg(e2)?;
            rb = self.expr_toanyreg(e1)?;
        } else {
            let mut o = opr - OPR_ADD + bc!(ADDVV);
            // Must discharge 2nd operand first since VINDEXED might free regs.
            self.expr_toval(e2)?;
            let mut c;
            if e2.isnumk() && {
                c = self.const_num(e2);
                c <= BCMAX_C
            } {
                o -= bc!(ADDVV) - bc!(ADDVN);
            } else {
                c = self.expr_toanyreg(e2)?;
            }
            // 1st operand discharged by bcemit_binop_left, but need KNUM/KSHORT.
            debug_assert!(e1.isnumk() || e1.k == K::NonReloc, "bad expr type {:?}", e1.k);
            self.expr_toval(e1)?;
            // Avoid two consts to satisfy bytecode constraints.
            let mut b;
            if e1.isnumk() && !e2.isnumk() && {
                b = self.const_num(e1);
                b <= BCMAX_B
            } {
                std::mem::swap(&mut b, &mut c);
                o -= bc!(ADDVV) - bc!(ADDNV);
            } else {
                b = self.expr_toanyreg(e1)?;
            }
            op = o;
            rb = b;
            rc = c;
        }
        // Using expr_free might cause asserts if the order is wrong.
        if e1.k == K::NonReloc && e1.info >= self.fs().nactvar {
            self.fsm().freereg -= 1;
        }
        if e2.k == K::NonReloc && e2.info >= self.fs().nactvar {
            self.fsm().freereg -= 1;
        }
        e1.info = self.bcemit_abc(op, 0, rb, rc)?;
        e1.k = K::Relocable;
        Ok(())
    }

    /// Emit comparison operator.
    fn bcemit_comp(&mut self, opr: u32, eret: &mut ExpDesc, eother: &mut ExpDesc) -> R {
        self.expr_toval(eret)?;
        // e1/e2 may be swapped; `swapped` tracks whether e1 is `eother`.
        let (ins, swapped) = if opr == OPR_EQ || opr == OPR_NE {
            let op = if opr == OPR_EQ { bc!(ISEQV) } else { bc!(ISNEV) };
            let swapped = eret.isk(); // Need constant in 2nd arg.
            let (e1, e2) = if swapped { (&mut *eother, &mut *eret) } else { (&mut *eret, &mut *eother) };
            let ra = self.expr_toanyreg(e1)?; // First arg must be in a reg.
            self.expr_toval(e2)?;
            let ins = match e2.k {
                K::KNil | K::KFalse | K::KTrue => ins_ad(op + (bc!(ISEQP) - bc!(ISEQV)), ra, e2.k as u32),
                K::KStr => ins_ad(op + (bc!(ISEQS) - bc!(ISEQV)), ra, self.const_str(e2)),
                K::KNum => ins_ad(op + (bc!(ISEQN) - bc!(ISEQV)), ra, self.const_num(e2)),
                _ => {
                    let rd = self.expr_toanyreg(e2)?;
                    ins_ad(op, ra, rd)
                }
            };
            (ins, swapped)
        } else {
            let mut op = opr - OPR_LT + bc!(ISLT);
            let (ra, rd);
            let swapped = (op - bc!(ISLT)) & 1 != 0;
            if swapped {
                // GT -> LT, GE -> LE
                op = ((op - bc!(ISLT)) ^ 3) + bc!(ISLT);
                self.expr_toval(eother)?;
                ra = self.expr_toanyreg(eother)?;
                rd = self.expr_toanyreg(eret)?;
            } else {
                rd = self.expr_toanyreg(eother)?;
                ra = self.expr_toanyreg(eret)?;
            }
            (ins_ad(op, ra, rd), swapped)
        };
        // Using expr_free might cause asserts if the order is wrong.
        let (e1, e2) = if swapped { (&*eother, &*eret) } else { (&*eret, &*eother) };
        let nactvar = self.fs().nactvar;
        let f1 = e1.k == K::NonReloc && e1.info >= nactvar;
        let f2 = e2.k == K::NonReloc && e2.info >= nactvar;
        if f1 {
            self.fsm().freereg -= 1;
        }
        if f2 {
            self.fsm().freereg -= 1;
        }
        self.bcemit_ins(ins)?;
        eret.info = self.bcemit_jmp()?;
        eret.k = K::Jmp;
        Ok(())
    }

    /// Fixup left side of binary operator.
    fn bcemit_binop_left(&mut self, op: u32, e: &mut ExpDesc) -> R {
        if op == OPR_AND {
            self.bcemit_branch_t(e)
        } else if op == OPR_OR {
            self.bcemit_branch_f(e)
        } else if op == OPR_COAL {
            self.expr_tonextreg(e)?;
            let reg = e.info;
            self.bcemit_ins(ins_ad(bc!(ISNEP), reg, K::KNil as u32))?;
            e.aux = self.bcemit_jmp()?;
            self.bcreg_free(reg);
            Ok(())
        } else if op == OPR_CONCAT {
            self.expr_tonextreg(e)
        } else if op == OPR_EQ || op == OPR_NE {
            if !e.isk_nojump() {
                self.expr_toanyreg(e)?;
            }
            Ok(())
        } else {
            if !e.isnumk_nojump() {
                self.expr_toanyreg(e)?;
            }
            Ok(())
        }
    }

    /// Emit binary operator.
    fn bcemit_binop(&mut self, op: u32, e1: &mut ExpDesc, e2: &mut ExpDesc) -> R {
        if op <= OPR_POW {
            if !Self::foldarith(op, e1, e2) {
                self.bcemit_arith(op, e1, e2)?;
            }
        } else if op <= OPR_BSAR {
            if !Self::foldbitop(op, e1, e2) {
                self.fsm().flags |= PROTO_BITOP;
                self.bcemit_arith(op, e1, e2)?;
            }
        } else if op == OPR_AND {
            debug_assert!(e1.t == NO_JMP, "jump list not closed");
            self.expr_discharge(e2)?;
            let mut f = e2.f;
            self.jmp_append(&mut f, e1.f)?;
            e2.f = f;
            *e1 = *e2;
        } else if op == OPR_OR {
            debug_assert!(e1.f == NO_JMP, "jump list not closed");
            self.expr_discharge(e2)?;
            let mut t = e2.t;
            self.jmp_append(&mut t, e1.t)?;
            e2.t = t;
            *e1 = *e2;
        } else if op == OPR_COAL {
            self.expr_tonextreg(e2)?;
            self.jmp_tohere(e1.aux)?;
        } else if op == OPR_CONCAT {
            self.expr_toval(e2)?;
            if e2.k == K::Relocable && bc_op(self.ins(e2.info)) == bc!(CAT) {
                debug_assert!(e1.info == bc_b(self.ins(e2.info)) - 1, "bad CAT stack layout");
                self.expr_free(e1);
                setbc_b(self.insm(e2.info), e1.info);
                e1.info = e2.info;
            } else {
                self.expr_tonextreg(e2)?;
                self.expr_free(e2);
                self.expr_free(e1);
                e1.info = self.bcemit_abc(bc!(CAT), 0, e1.info, e2.info)?;
            }
            e1.k = K::Relocable;
        } else {
            debug_assert!((OPR_NE..=OPR_GT).contains(&op), "bad binop {op}");
            self.bcemit_comp(op, e1, e2)?;
        }
        Ok(())
    }

    /// Emit unary operator.
    fn bcemit_unop(&mut self, op: u32, e: &mut ExpDesc) -> R {
        if op == bc!(NOT) {
            // Swap true and false lists.
            std::mem::swap(&mut e.f, &mut e.t);
            self.jmp_dropval(e.f);
            self.jmp_dropval(e.t);
            self.expr_discharge(e)?;
            if e.k == K::KNil || e.k == K::KFalse {
                e.k = K::KTrue;
                return Ok(());
            } else if e.isk() || e.k == K::KCData {
                e.k = K::KFalse;
                return Ok(());
            } else if e.k == K::Jmp {
                self.invertcond(e);
                return Ok(());
            } else if e.k == K::Relocable {
                self.bcreg_reserve(1)?;
                let r = self.fs().freereg - 1;
                setbc_a(self.insm(e.info), r);
                e.info = r;
                e.k = K::NonReloc;
            } else {
                debug_assert!(e.k == K::NonReloc, "bad expr type {:?}", e.k);
            }
        } else {
            debug_assert!(op == bc!(UNM) || op == bc!(LEN) || op == bc!(BNOT), "bad unop {op}");
            if !e.hasjump() {
                if op == bc!(UNM) {
                    // Constant-fold negations.
                    if e.k == K::KCData {
                        // Fold in-place since cdata is not interned.
                        let cd = e.cdata.expect("cdata expression without cdata");
                        let p = cd.ptr() as *mut u64;
                        // SAFETY: lexer-created cdata constants own 8 (int64/uint64) or 16
                        // (complex) bytes, aligned to at least 16.
                        unsafe {
                            if cd.ctypeid() == CTID_COMPLEX_DOUBLE {
                                *p.add(1) ^= 0x8000_0000_0000_0000;
                            } else {
                                *p = (!*p).wrapping_add(1);
                            }
                        }
                        return Ok(());
                    } else if e.isnumk() && !e.numiszero() {
                        // Avoid folding to -0.
                        e.nval = f64::from_bits(e.nval.to_bits() ^ 0x8000_0000_0000_0000);
                        return Ok(());
                    }
                } else if op == bc!(BNOT) && e.isnumk() {
                    // Constant-fold bitwise not.
                    e.nval = (!e.bitv()) as f64;
                    return Ok(());
                }
            }
            if op == bc!(BNOT) {
                self.fsm().flags |= PROTO_BITOP;
            }
            self.expr_toanyreg(e)?;
        }
        self.expr_free(e);
        e.info = self.bcemit_ad(op, 0, e.info)?;
        e.k = K::Relocable;
        Ok(())
    }

    // -- Lexer support --

    #[inline]
    fn next(&mut self) -> R {
        self.ls.next_token()
    }

    /// Check and consume optional token.
    fn lex_opt(&mut self, tok: LexToken) -> R<bool> {
        if self.ls.tok == tok {
            self.next()?;
            return Ok(true);
        }
        Ok(false)
    }

    /// Check and consume token.
    fn lex_check(&mut self, tok: LexToken) -> R {
        if self.ls.tok != tok {
            return Err(self.err_token(tok));
        }
        self.next()
    }

    /// Check for matching token.
    fn lex_match(&mut self, what: LexToken, who: LexToken, line: u32) -> R {
        if !self.lex_opt(what)? {
            if line == self.ls.linenumber {
                return Err(self.err_token(what));
            }
            let swhat = self.ls.token2str(what);
            let swho = self.ls.token2str(who);
            let msg = cat(&[&qs(&swhat), b" expected (to close ", &qs(&swho), format!(" at line {line})").as_bytes()]);
            let tok = self.ls.tok;
            return Err(self.ls.error(tok, &msg));
        }
        Ok(())
    }

    /// Check for string token.
    fn lex_str(&mut self) -> R<Gc<LuaStr>> {
        if !lex_isname(self.ls.tok) {
            return Err(self.err_token(TK_NAME));
        }
        let s = self.ls.tokval.str();
        self.next()?;
        Ok(s)
    }

    // -- Variable handling --

    /// Grow the variable stack if `vtop` reached its size (`lj_mem_growvec` semantics).
    fn vstack_grow(&mut self) -> R {
        if self.vtop >= self.vstack.len() {
            let size = self.vstack.len();
            if size >= LJ_MAX_VSTACK {
                let msg = format!("chunk has more than {LJ_MAX_VSTACK} local variables");
                return Err(self.ls.error(0, msg.as_bytes()));
            }
            let nsize = (size << 1).clamp(LJ_MIN_VECSZ, LJ_MAX_VSTACK);
            self.vstack.resize(nsize, VarInfo { name: VName::Null, startpc: 0, endpc: 0, slot: 0, info: 0, prev: 0 });
        }
        Ok(())
    }

    /// Define a new local variable.
    fn var_new(&mut self, n: u32, name: VName) -> R<usize> {
        if let Some(h) = var_hash(name) {
            // Check for const re-declaration.
            let mut vidx = self.vhash[h];
            while vidx != VINDEX_NONE {
                let v = self.vstack[vidx as usize];
                if v.name == name && v.info & VSTACK_CONST != 0 {
                    let VName::Str(s) = name else { unreachable!() };
                    let msg = cat(&[b"attempt to re-declare const variable ", &qs(s.as_bytes())]);
                    return Err(self.ls.error(0, &msg));
                }
                vidx = v.prev;
            }
        }
        let nactvar = self.fs().nactvar;
        self.checklimit(nactvar + n, LJ_MAX_LOCVAR, "local variables")?;
        self.vstack_grow()?;
        let vtop = self.vtop;
        self.vstack[vtop].name = name;
        self.vstack[vtop].info = 0;
        // The other VarInfo fields are filled in by var_add and var_remove.
        self.fsm().varmap[(nactvar + n) as usize] = vtop as u16;
        self.vtop = vtop + 1;
        Ok(vtop)
    }

    fn var_new_lit(&mut self, n: u32, s: &[u8]) -> R<usize> {
        let name = self.ls.vm.new_str(s);
        self.ls.anchor(Value::Str(name));
        self.var_new(n, VName::Str(name))
    }

    /// Add local variables.
    fn var_add(&mut self, mut nvars: u32) {
        let mut nactvar = self.fs().nactvar;
        let pc = self.fs().pc;
        while nvars > 0 {
            nvars -= 1;
            let vidx = self.fs().varmap[nactvar as usize];
            let v = &mut self.vstack[vidx as usize];
            v.startpc = pc;
            v.slot = nactvar as u8;
            nactvar += 1;
            if let Some(h) = var_hash(v.name) {
                v.prev = self.vhash[h];
                self.vhash[h] = vidx;
            }
        }
        self.fsm().nactvar = nactvar;
    }

    /// Remove local variables.
    fn var_remove(&mut self, tolevel: u32) {
        let pc = self.fs().pc;
        while self.fs().nactvar > tolevel {
            self.fsm().nactvar -= 1;
            let i = self.var_idx(self.fs().nactvar);
            let v = &mut self.vstack[i];
            v.endpc = pc;
            if let Some(h) = var_hash(v.name) {
                self.vhash[h] = v.prev;
            }
        }
    }

    /// Lookup variable name.
    fn var_lookup(&mut self, e: &mut ExpDesc, name: Gc<LuaStr>) -> R {
        let vn = VName::Str(name);
        let mut vidx = self.vhash[var_hash(vn).expect("string name")];
        while vidx != VINDEX_NONE {
            let v = self.vstack[vidx as usize];
            if v.name == vn {
                let mut fsi = self.fss.len() - 1;
                if vidx as usize >= self.fss[fsi].vbase {
                    e.init(K::Local, v.slot as u32);
                    e.aux = vidx as u32;
                } else {
                    let mut nuv = self.fss[fsi].nuv;
                    e.aux = vidx as u32;
                    for uvidx in 0..nuv {
                        if self.fss[fsi].uvmap[uvidx as usize] == vidx {
                            // Upvalue already exists.
                            e.init(K::Upval, uvidx);
                            return Ok(());
                        }
                    }
                    e.init(K::Upval, nuv);
                    loop {
                        // Create a new upvalue.
                        if nuv >= LJ_MAX_UPVAL {
                            return Err(self.err_limit_at(fsi, LJ_MAX_UPVAL, "upvalues"));
                        }
                        self.fss[fsi].uvmap[nuv as usize] = vidx;
                        self.fss[fsi].nuv = nuv + 1;
                        let puvtmp = (fsi, nuv as usize); // Set below.
                        fsi -= 1; // Continue in parent.
                        if vidx as usize >= self.fss[fsi].vbase {
                            // Local in that function.
                            self.fss[puvtmp.0].uvtmp[puvtmp.1] = vidx;
                            self.fscope_uvmark(fsi, v.slot as u32);
                            return Ok(());
                        }
                        // Not a local in that function. Find or create upvalue.
                        nuv = self.fss[fsi].nuv;
                        for uvidx in 0..nuv {
                            if self.fss[fsi].uvmap[uvidx as usize] == vidx {
                                // Upvalue already exists.
                                self.fss[puvtmp.0].uvtmp[puvtmp.1] = (LJ_MAX_VSTACK as u32 + uvidx) as u16;
                                return Ok(());
                            }
                        }
                        // Not yet an upvalue. Create it and continue.
                        self.fss[puvtmp.0].uvtmp[puvtmp.1] = (LJ_MAX_VSTACK as u32 + nuv) as u16;
                    }
                }
                return Ok(());
            }
            vidx = v.prev;
        }
        e.init(K::Global, 0);
        e.sval = Some(name);
        Ok(())
    }

    /// Check for const variable assignment.
    fn var_assign(&mut self, e: &ExpDesc) -> R {
        if e.k == K::Local || e.k == K::Upval {
            let v = self.vstack[e.aux as usize];
            if v.info & VSTACK_CONST != 0 {
                let VName::Str(s) = v.name else { unreachable!("const variable without name") };
                let msg = cat(&[b"attempt to assign to const variable ", &qs(s.as_bytes())]);
                return Err(self.ls.error(0, &msg));
            }
        }
        Ok(())
    }

    // -- Goto and label handling --

    /// Add a new goto or label.
    fn gola_new(&mut self, name: VName, info: u8, pc: u32) -> R<usize> {
        self.vstack_grow()?;
        let vtop = self.vtop;
        let nactvar = self.fs().nactvar;
        let v = &mut self.vstack[vtop];
        v.name = name;
        v.startpc = pc;
        v.slot = nactvar as u8;
        v.info = info;
        self.vtop = vtop + 1;
        Ok(vtop)
    }

    /// Patch goto to jump to label.
    fn gola_patch(&mut self, vg: usize, vl: usize) -> R {
        let pc = self.vstack[vg].startpc;
        self.vstack[vg].name = VName::Null; // Invalidate pending goto.
        let slot = self.vstack[vl].slot as u32;
        setbc_a(self.insm(pc), slot);
        let target = self.vstack[vl].startpc;
        self.jmp_patch(pc, target)
    }

    /// Patch goto to close upvalues.
    fn gola_close(&mut self, vg: usize) -> R {
        let pc = self.vstack[vg].startpc;
        debug_assert!(self.vstack[vg].isgoto(), "expected goto");
        let slot = self.vstack[vg].slot as u32;
        setbc_a(self.insm(pc), slot);
        if bc_op(self.ins(pc)) == bc!(JMP) {
            let next = self.jmp_next(pc);
            if next != NO_JMP {
                self.jmp_patch(next, pc)?; // Jump to UCLO.
            }
            let p = self.insm(pc);
            setbc_op(p, bc!(UCLO)); // Turn into UCLO.
            setbc_j(p, NO_JMP);
        }
        Ok(())
    }

    /// Resolve pending forward gotos for label.
    fn gola_resolve(&mut self, vstart: usize, idx: usize) -> R {
        for vg in vstart..idx {
            let g = self.vstack[vg];
            let l = self.vstack[idx];
            if g.name == l.name && g.isgoto() {
                if g.slot < l.slot {
                    let vi = self.var_idx(g.slot as u32);
                    let VName::Str(name) = self.vstack[vi].name else { unreachable!("expected goto name") };
                    self.ls.linenumber = self.line_at(g.startpc);
                    debug_assert!(g.name != NAME_BREAK, "unexpected break");
                    let msg = if g.name == NAME_CONT {
                        cat(&[b"<continue> jumps into the scope of local ", &qs(name.as_bytes())])
                    } else {
                        let VName::Str(gname) = g.name else { unreachable!("goto without name") };
                        cat(&[b"<goto ", cstr(gname.as_bytes()), b"> jumps into the scope of local ", &qs(name.as_bytes())])
                    };
                    return Err(self.ls.error(0, &msg));
                }
                self.gola_patch(vg, idx)?;
            }
        }
        Ok(())
    }

    /// Fixup remaining gotos and labels for scope `bl` (already popped; its `prev` is the current
    /// top scope, if any).
    fn gola_fixup(&mut self, bl: &FuncScope) -> R {
        let ve = self.vtop;
        for v in bl.vstart..ve {
            let name = self.vstack[v].name;
            if name == VName::Null {
                continue; // Only consider remaining valid gotos/labels.
            }
            if self.vstack[v].islabel() {
                self.vstack[v].name = VName::Null; // Invalidate label that goes out of scope.
                for vg in v + 1..ve {
                    // Resolve pending backward gotos.
                    if self.vstack[vg].name == name && self.vstack[vg].isgoto() {
                        if bl.flags & FSCOPE_UPVAL != 0 && self.vstack[vg].slot > self.vstack[v].slot {
                            self.gola_close(vg)?;
                        }
                        self.gola_patch(vg, v)?;
                    }
                }
            } else if self.vstack[v].isgoto() {
                if !self.fs().bl.is_empty() {
                    // Propagate goto, break or continue to outer scope.
                    let f = if name == NAME_BREAK {
                        FSCOPE_BREAK
                    } else if name == NAME_CONT {
                        FSCOPE_CONT
                    } else {
                        FSCOPE_GOLA
                    };
                    self.bl().flags |= f;
                    self.vstack[v].slot = bl.nactvar as u8;
                    if bl.flags & FSCOPE_UPVAL != 0 {
                        self.gola_close(v)?;
                    }
                } else {
                    // No outer scope: undefined goto label or no loop.
                    self.ls.linenumber = self.line_at(self.vstack[v].startpc);
                    let msg = if name == NAME_BREAK {
                        b"no loop to break".to_vec()
                    } else if name == NAME_CONT {
                        b"no loop to continue".to_vec()
                    } else {
                        let VName::Str(s) = name else { unreachable!("goto without name") };
                        cat(&[b"undefined label ", &qs(s.as_bytes())])
                    };
                    return Err(self.ls.error(0, &msg));
                }
            }
        }
        Ok(())
    }

    /// Find existing label.
    fn gola_findlabel(&self, name: VName) -> Option<usize> {
        let vstart = self.fs().bl.last().expect("no scope").vstart;
        (vstart..self.vtop).find(|&v| self.vstack[v].name == name && self.vstack[v].islabel())
    }

    // -- Scope handling --

    /// Begin a scope.
    fn fscope_begin(&mut self, flags: u8) {
        let vtop = self.vtop;
        let fs = self.fsm();
        debug_assert!(fs.freereg == fs.nactvar, "bad regalloc");
        let nactvar = fs.nactvar;
        fs.bl.push(FuncScope { vstart: vtop, nactvar, flags });
    }

    /// End a scope.
    fn fscope_end(&mut self) -> R {
        let mut bl = self.fsm().bl.pop().expect("no scope");
        self.var_remove(bl.nactvar);
        let nactvar = self.fs().nactvar;
        self.fsm().freereg = nactvar;
        debug_assert!(bl.nactvar == nactvar, "bad regalloc");
        if bl.flags & (FSCOPE_UPVAL | FSCOPE_NOCLOSE) == FSCOPE_UPVAL {
            self.bcemit_aj(bc!(UCLO), bl.nactvar, 0)?;
        }
        debug_assert!(bl.flags & (FSCOPE_LOOP | FSCOPE_CONT) != (FSCOPE_LOOP | FSCOPE_CONT), "dangling continue");
        if bl.flags & (FSCOPE_LOOP | FSCOPE_BREAK) == (FSCOPE_LOOP | FSCOPE_BREAK) {
            bl.flags &= !FSCOPE_BREAK;
            let pc = self.fs().pc;
            let idx = self.gola_new(NAME_BREAK, VSTACK_LABEL, pc)?;
            self.vtop = idx; // Drop break label immediately.
            self.gola_resolve(bl.vstart, idx)?;
        }
        if bl.flags & (FSCOPE_GOLA | FSCOPE_BREAK | FSCOPE_CONT) != 0 {
            self.gola_fixup(&bl)?;
        }
        Ok(())
    }

    /// Add continue label.
    fn fscope_continue(&mut self, cont: u32) -> R {
        if self.bl().flags & FSCOPE_CONT != 0 {
            self.bl().flags &= !FSCOPE_CONT;
            let vstart = self.bl().vstart;
            let idx = self.gola_new(NAME_CONT, VSTACK_LABEL, cont)?;
            self.vtop = idx; // Drop continue label immediately.
            self.gola_resolve(vstart, idx)?;
        }
        Ok(())
    }

    /// Mark scope as having an upvalue.
    fn fscope_uvmark(&mut self, fsi: usize, level: u32) {
        if let Some(bl) = self.fss[fsi].bl.iter_mut().rev().find(|bl| bl.nactvar <= level) {
            bl.flags |= FSCOPE_UPVAL;
        }
    }

    // -- Function state management --

    /// Fixup return instruction for prototype.
    fn fs_fixup_ret(&mut self) -> R {
        let lastpc = self.fs().pc;
        if lastpc <= self.fs().lasttarget || !is_ret_or_tail(bc_op(self.ins(lastpc - 1))) {
            if self.bl().flags & FSCOPE_UPVAL != 0 {
                self.bcemit_aj(bc!(UCLO), 0, 0)?;
            }
            self.bcemit_ad(bc!(RET0), 0, 1)?; // Need final return.
        }
        self.bl().flags |= FSCOPE_NOCLOSE; // Handled above.
        self.fscope_end()?;
        debug_assert!(self.fs().bl.is_empty(), "bad scope nesting");
        // May need to fixup returns encoded before first function was created.
        if self.fs().flags & PROTO_FIXUP_RETURN != 0 {
            for pc in 1..lastpc {
                let ins = self.ins(pc);
                let op = bc_op(ins);
                if op == bc!(CALLMT) || op == bc!(CALLT) || op == bc!(RETM) || op == bc!(RET) || op == bc!(RET0) || op == bc!(RET1) {
                    let offset = self.bcemit_ins(ins)?; // Copy original instruction.
                    let line = self.line_at(pc);
                    self.set_line(offset, line);
                    let offset = offset.wrapping_sub(pc + 1).wrapping_add(BCBIAS_J);
                    if offset > BCMAX_D {
                        return Err(self.err_syntax(b"function too long for return fixup"));
                    }
                    // Replace with UCLO plus branch.
                    *self.insm(pc) = ins_ad(bc!(UCLO), 0, offset);
                } else if op == bc!(FNEW) {
                    return Ok(()); // We're done.
                }
            }
        }
        Ok(())
    }

    /// Fixup upvalues for child prototype, step #2 (run by the parent).
    fn fs_fixup_uv2(&self, uv: &mut [u16]) {
        for u in uv.iter_mut() {
            let vidx = *u as usize;
            *u = if vidx >= LJ_MAX_VSTACK {
                (vidx - LJ_MAX_VSTACK) as u16
            } else if self.vstack[vidx].info & VSTACK_VAR_RW != 0 {
                self.vstack[vidx].slot as u16 | PROTO_UV_LOCAL
            } else {
                self.vstack[vidx].slot as u16 | PROTO_UV_LOCAL | PROTO_UV_IMMUTABLE
            };
        }
    }

    /// Finish a FuncState and return the new prototype (`fs_finish`). Child prototypes have
    /// their upvalue descriptors fixed up and are allocated here; the returned prototype itself
    /// is not yet allocated, since its own upvalues are fixed up by the parent.
    fn fs_finish(&mut self, line: u32) -> R<Proto> {
        let numline = line.wrapping_sub(self.fs().linedefined);

        // Apply final fixups.
        self.fs_fixup_ret()?;

        let nkn = self.fs().kt.knum.len() as u32;
        let nkgc = self.fs().kt.kgc.len() as u32;
        if nkn > BCMAX_D + 1 {
            return Err(self.err_limit(BCMAX_D + 1, "constants"));
        }
        if nkgc > BCMAX_D + 1 {
            return Err(self.err_limit(BCMAX_D + 1, "constants"));
        }

        let mut fs = self.fss.pop().expect("no FuncState");
        let n = fs.pc as usize;
        let base = fs.bcbase;

        // Bytecode.
        let hop = if self.fr2 != LJ_FR2 {
            bc!(NOT) // Mark non-native prototype.
        } else if fs.flags & PROTO_VARARG != 0 {
            bc!(FUNCV)
        } else {
            bc!(FUNCF)
        };
        let mut bc = Vec::with_capacity(n);
        bc.push(Ins(ins_ad(hop, fs.framesize as u32, 0)));
        bc.extend(self.bcstack[base + 1..base + n].iter().map(|il| Ins(il.ins)));

        // Constants.
        let mut kgc = Vec::with_capacity(fs.kt.kgc.len());
        for k in std::mem::take(&mut fs.kt.kgc) {
            kgc.push(match k {
                KSlot::Str(s) => KGc::Str(s),
                KSlot::Table(t) => KGc::Table(t),
                KSlot::CData(cd) => KGc::CData(cd),
                KSlot::Proto(mut p) => {
                    self.fs_fixup_uv2(&mut p.uv);
                    let p = self.ls.vm.new_proto(p);
                    self.ls.anchor(Value::Proto(p));
                    KGc::Proto(p)
                }
            });
        }

        // Upvalues (step #1).
        let nuv = fs.nuv as usize;
        let uv: Box<[u16]> = fs.uvtmp[..nuv].into();

        // Line info.
        let mut lineinfo = Vec::with_capacity(n);
        lineinfo.push(fs.linedefined);
        lineinfo.extend(self.bcstack[base + 1..base + n].iter().map(|il| il.line));

        // Upvalue names and variable info.
        let uvnames: Box<[Gc<LuaStr>]> = fs.uvmap[..nuv]
            .iter()
            .map(|&vi| match self.vstack[vi as usize].name {
                VName::Str(s) => s,
                other => unreachable!("upvalue without name: {other:?}"),
            })
            .collect();
        let mut varinfo = Vec::new();
        for v in &self.vstack[fs.vbase..self.vtop] {
            if !v.isgotolabel() {
                let name = match v.name {
                    VName::Str(s) => VarName::Str(s),
                    VName::Fixed(id) => VarName::Internal(id),
                    VName::Null => unreachable!("variable without name"),
                };
                varinfo.push(ProtoVarInfo { name, startpc: v.startpc, endpc: v.endpc });
            }
        }

        let pt = Proto {
            bc: bc.into_boxed_slice(),
            kgc: kgc.into_boxed_slice(),
            knum: std::mem::take(&mut fs.kt.knum).into_boxed_slice(),
            uv,
            numparams: fs.numparams,
            framesize: fs.framesize,
            flags: std::cell::Cell::new(fs.flags & !(PROTO_HAS_RETURN | PROTO_FIXUP_RETURN)),
            chunkname: self.ls.chunkname,
            firstline: fs.linedefined,
            numline,
            lineinfo: lineinfo.into_boxed_slice(),
            uvnames,
            varinfo: varinfo.into_boxed_slice(),
        };

        self.vtop = fs.vbase; // Reset variable stack.
        debug_assert!(!self.fss.is_empty() || self.ls.tok == TK_EOF, "bad parser state");
        Ok(pt)
    }

    /// Initialize a new FuncState.
    fn fs_init(&mut self) {
        self.fss.push(FuncState {
            kt: KTab::default(),
            bl: Vec::new(),
            pc: 0,
            lasttarget: 0,
            jpc: NO_JMP,
            freereg: 0,
            nactvar: 0,
            linedefined: 0,
            bcbase: 0,
            vbase: self.vtop,
            flags: 0,
            numparams: 0,
            framesize: 1, // Minimum frame size.
            nuv: 0,
            varmap: [0; LJ_MAX_LOCVAR as usize],
            uvmap: [0; LJ_MAX_UPVAL as usize],
            uvtmp: [0; LJ_MAX_UPVAL as usize],
        });
    }

    // -- Expressions --

    /// Return string expression.
    fn expr_str(&mut self, e: &mut ExpDesc) -> R {
        e.init(K::KStr, 0);
        e.sval = Some(self.lex_str()?);
        Ok(())
    }

    /// Return index expression.
    fn expr_index(&mut self, t: &mut ExpDesc, e: &mut ExpDesc) -> R {
        // Already called: expr_toval(fs, e).
        t.k = K::Indexed;
        if e.isnumk() {
            if let Some(k) = num2int_check(e.nval)
                && checku8(k)
            {
                t.aux = BCMAX_C + 1 + k as u32; // 256..511: const byte key
                return Ok(());
            }
        } else if e.isstrk() {
            let idx = self.const_str(e);
            if idx <= BCMAX_C {
                t.aux = !idx; // -256..-1: const string key
                return Ok(());
            }
        }
        t.aux = self.expr_toanyreg(e)?; // 0..255: register
        Ok(())
    }

    /// Parse index expression with named field.
    fn expr_field(&mut self, v: &mut ExpDesc) -> R {
        let mut key = ExpDesc::new(K::Void, 0);
        self.expr_toanyreg(v)?;
        self.expr_str(&mut key)?;
        self.expr_index(v, &mut key)
    }

    /// Parse index expression with brackets.
    fn expr_bracket(&mut self, v: &mut ExpDesc) -> R {
        self.next()?; // Skip '['.
        self.expr(v, 0)?;
        self.expr_toval(v)?;
        self.lex_check(b']' as LexToken)
    }

    /// Get value of constant expression.
    fn expr_kvalue(e: &ExpDesc) -> Value {
        match e.k {
            K::KNil => Value::Nil,
            K::KFalse => Value::Bool(false),
            K::KTrue => Value::Bool(true),
            K::KStr => Value::Str(e.str()),
            _ => {
                debug_assert!(e.isnumk(), "bad number constant");
                Value::Num(e.nval)
            }
        }
    }

    /// Parse table constructor expression.
    fn expr_table(&mut self, e: &mut ExpDesc) -> R {
        let line = self.ls.linenumber;
        let mut t: Option<Gc<Table>> = None;
        let mut vcall = false;
        let mut needarr = false;
        let mut narr: u32 = 1; // First array index.
        let mut nhash: u32 = 0; // Number of hash entries.
        let mut freg = self.fs().freereg;
        let pc = self.bcemit_ad(bc!(TNEW), freg, 0)?;
        e.init(K::NonReloc, freg);
        self.bcreg_reserve(1)?;
        freg += 1;
        self.lex_check(b'{' as LexToken)?;
        while self.ls.tok != b'}' as LexToken {
            let mut key = ExpDesc::new(K::Void, 0);
            let mut val = ExpDesc::new(K::Void, 0);
            vcall = false;
            if self.ls.tok == b'[' as LexToken {
                self.expr_bracket(&mut key)?; // Already calls expr_toval.
                if !key.isk() {
                    self.expr_index(e, &mut key)?;
                }
                if key.isnumk() && key.numiszero() {
                    needarr = true;
                } else {
                    nhash += 1;
                }
                self.lex_check(b'=' as LexToken)?;
            } else if lex_isname(self.ls.tok) && self.ls.lookahead()? == b'=' as LexToken {
                self.expr_str(&mut key)?;
                self.lex_check(b'=' as LexToken)?;
                nhash += 1;
            } else {
                key.init(K::KNum, 0);
                key.nval = narr as i32 as f64;
                narr += 1;
                needarr = true;
                vcall = true;
            }
            self.expr(&mut val, 0)?;
            let mut nonconst = true;
            if key.isk() && key.k != K::KNil && (key.k == K::KStr || val.isk_nojump()) {
                let tt = match t {
                    Some(tt) => tt,
                    None => {
                        // Create template table on demand.
                        let tt = self.ls.vm.new_table(if needarr { narr } else { 0 }, hsize2hbits(nhash))?;
                        self.ls.anchor(Value::Table(tt));
                        let kidx = self.const_gc_new(KSlot::Table(tt));
                        *self.insm(pc) = ins_ad(bc!(TDUP), freg - 1, kidx);
                        t = Some(tt);
                        tt
                    }
                };
                vcall = false;
                let k = Self::expr_kvalue(&key);
                if val.isk_nojump() {
                    // Add const key/value to template table.
                    let mut v = Self::expr_kvalue(&val);
                    // Mark nil value with table value itself to preserve the key.
                    if key.k == K::KStr && v.is_nil() {
                        v = Value::Table(tt);
                    }
                    tt.set(k, v).map_err(|err| self.ls.vm.tab_error(err))?;
                    nonconst = false;
                } else {
                    // Preserve the key for the following non-const store.
                    tt.set(k, Value::Table(tt)).map_err(|err| self.ls.vm.tab_error(err))?;
                }
            }
            if nonconst {
                if val.k != K::Call {
                    self.expr_toanyreg(&mut val)?;
                    vcall = false;
                }
                if key.isk() {
                    self.expr_index(e, &mut key)?;
                }
                let var = *e;
                self.bcemit_store(&var, &mut val)?;
            }
            self.fsm().freereg = freg;
            if !self.lex_opt(b',' as LexToken)? && !self.lex_opt(b';' as LexToken)? {
                break;
            }
        }
        self.lex_match(b'}' as LexToken, b'{' as LexToken, line)?;
        if vcall {
            let mut ilp = self.fs().pc - 1;
            debug_assert!(
                bc_a(self.ins(ilp)) == freg && bc_op(self.ins(ilp)) == if narr > 256 { bc!(TSETV) } else { bc!(TSETB) },
                "bad CALL code generation"
            );
            let mut en = ExpDesc::new(K::KNum, 0);
            en.nval = f64::from_bits(0x4330_0000_0000_0000 | (narr - 1) as u64); // Biased integer.
            if narr > 256 {
                self.fsm().pc -= 1;
                ilp -= 1;
            }
            let k = self.const_num(&en);
            *self.insm(ilp) = ins_ad(bc!(TSETM), freg, k);
            setbc_b(self.insm(ilp - 1), 0);
        }
        if pc == self.fs().pc - 1 {
            // Make expr relocable if possible.
            e.info = pc;
            self.fsm().freereg -= 1;
            e.k = K::Relocable;
        } else {
            e.k = K::NonReloc; // May have been changed by expr_index.
        }
        match t {
            None => {
                // Construct TNEW RD: hhhhhaaaaaaaaaaa.
                narr = if needarr { narr.clamp(3, 0x7ff) } else { 0 };
                setbc_d(self.insm(pc), narr | (hsize2hbits(nhash) << 11));
            }
            Some(tt) => {
                if needarr && tt.asize() < narr {
                    tt.reasize(narr - 1).map_err(|err| self.ls.vm.tab_error(err))?;
                }
            }
        }
        Ok(())
    }

    /// Parse function parameters.
    fn parse_params(&mut self, needself: bool, before: LexToken, after: LexToken) -> R<u32> {
        let mut nparams = 0;
        self.lex_check(before)?;
        if needself {
            self.var_new_lit(nparams, b"self")?;
            nparams += 1;
        }
        if self.ls.tok != after {
            loop {
                if lex_isname(self.ls.tok) {
                    let s = self.lex_str()?;
                    self.var_new(nparams, VName::Str(s))?;
                    nparams += 1;
                } else if self.ls.tok == TK_DOTS {
                    self.next()?;
                    self.fsm().flags |= PROTO_VARARG;
                    break;
                } else {
                    return Err(self.err_syntax(b"<name> or '...' expected"));
                }
                if !self.lex_opt(b',' as LexToken)? {
                    break;
                }
            }
        }
        self.var_add(nparams);
        debug_assert!(self.fs().nactvar == nparams, "bad regalloc");
        self.bcreg_reserve(nparams)?;
        self.lex_check(after)?;
        Ok(nparams)
    }

    /// Begin a new function prototype.
    fn proto_begin(&mut self, line: u32, nparams: u32) -> R {
        let n = self.fss.len();
        let pbase = self.fss[n - 2].bcbase + self.fss[n - 2].pc as usize;
        let fs = self.fsm();
        fs.linedefined = line;
        fs.numparams = nparams as u8;
        fs.bcbase = pbase;
        self.bcemit_ad(bc!(FUNCF), 0, 0)?; // Placeholder.
        Ok(())
    }

    /// Finish a function prototype.
    fn proto_finish(&mut self, e: &mut ExpDesc) -> R {
        let flags = self.fs().flags & (PROTO_FFI | PROTO_BITOP);
        self.ls.lastline = self.ls.linenumber;
        let pt = self.fs_finish(self.ls.lastline)?;
        // Store new prototype in the constant array of the parent.
        let k = self.const_gc_new(KSlot::Proto(pt));
        let pc = self.bcemit_ad(bc!(FNEW), 0, k)?;
        e.init(K::Relocable, pc);
        let pfs = self.fsm();
        pfs.flags |= flags; // Inherited flags.
        if pfs.flags & PROTO_CHILD == 0 {
            if pfs.flags & PROTO_HAS_RETURN != 0 {
                pfs.flags |= PROTO_FIXUP_RETURN;
            }
            pfs.flags |= PROTO_CHILD;
        }
        Ok(())
    }

    /// Parse body of a function.
    fn parse_body(&mut self, e: &mut ExpDesc, needself: bool, line: u32) -> R {
        self.fs_init();
        self.fscope_begin(0);
        let np = self.parse_params(needself, b'(' as LexToken, b')' as LexToken)?;
        self.proto_begin(line, np)?;
        self.parse_chunk()?;
        if self.ls.tok != TK_END {
            self.lex_match(TK_END, TK_FUNCTION, line)?;
        }
        self.proto_finish(e)?;
        self.next()
    }

    /// Parse short function.
    fn parse_shortfunc(&mut self, e: &mut ExpDesc, name: Option<Gc<LuaStr>>, eflags: u32, line: u32) -> R {
        let mut nparams = 0;
        self.fs_init();
        self.fscope_begin(0);
        if let Some(name) = name {
            self.var_new(nparams, VName::Str(name))?;
            nparams += 1;
            self.var_add(nparams);
            self.bcreg_reserve(1)?;
        } else if !self.lex_opt(TK_OR_)? {
            nparams = self.parse_params(false, b'|' as LexToken, b'|' as LexToken)?;
        }
        self.lex_check(TK_ARROW)?;
        self.proto_begin(line, nparams)?;
        if self.lex_opt(TK_DO)? {
            self.parse_chunk()?;
            if !self.lex_opt(TK_END)? {
                self.lex_match(TK_END, TK_DO, line)?;
            }
        } else {
            self.parse_return(eflags | EXPR_F_RET1)?;
        }
        self.proto_finish(e)
    }

    /// Parse expression list. Last expression is left open.
    fn expr_list(&mut self, v: &mut ExpDesc) -> R<u32> {
        let mut n = 1;
        self.expr(v, 0)?;
        while self.lex_opt(b',' as LexToken)? {
            self.expr_tonextreg(v)?;
            self.expr(v, 0)?;
            n += 1;
        }
        Ok(n)
    }

    /// Parse function argument list.
    fn parse_args(&mut self, e: &mut ExpDesc) -> R {
        let mut args = ExpDesc::new(K::Void, 0);
        let line = self.ls.linenumber;
        if self.ls.tok == b'(' as LexToken {
            if line != self.ls.lastline {
                return Err(self.err_syntax(b"ambiguous syntax (function call x new statement)"));
            }
            self.next()?;
            if self.ls.tok == b')' as LexToken {
                // f().
                args.k = K::Void;
            } else {
                self.expr_list(&mut args)?;
                if args.k == K::Call {
                    // f(a, b, g()) or f(a, b, ...).
                    setbc_b(self.insm(args.info), 0); // Pass on multiple results.
                }
            }
            self.lex_match(b')' as LexToken, b'(' as LexToken, line)?;
        } else if self.ls.tok == b'{' as LexToken {
            self.expr_table(&mut args)?;
        } else if self.ls.tok == TK_STRING {
            args.init(K::KStr, 0);
            args.sval = Some(self.ls.tokval.str());
            self.next()?;
        } else {
            return Err(self.err_syntax(b"function arguments expected"));
        }
        debug_assert!(e.k == K::NonReloc, "bad expr type {:?}", e.k);
        let base = e.info; // Base register for call.
        let ins = if args.k == K::Call {
            ins_abc(bc!(CALLM), base, 2, args.aux - base - 1 - self.fr2)
        } else {
            if args.k != K::Void {
                self.expr_tonextreg(&mut args)?;
            }
            ins_abc(bc!(CALL), base, 2, self.fs().freereg - base - self.fr2)
        };
        let pc = self.bcemit_ins(ins)?;
        e.init(K::Call, pc);
        e.aux = base;
        self.set_line(pc, line);
        self.fsm().freereg = base + 1; // Leave one result by default.
        Ok(())
    }

    /// Emit a safe navigation nil check on register `reg` and append its jump to `xpc`.
    fn nav_check(&mut self, reg: u32, xpc: &mut u32) -> R {
        self.bcemit_ins(ins_ad(bc!(ISEQP), reg, K::KNil as u32))?;
        let j = self.bcemit_jmp()?;
        self.jmp_append(xpc, j)
    }

    /// Parse primary expression with safe navigation.
    fn expr_primary_nav(&mut self, v: &mut ExpDesc, eflags: u32) -> R<u32> {
        let mut xpc = NO_JMP;
        // Parse prefix expression.
        if self.ls.tok == b'(' as LexToken {
            let line = self.ls.linenumber;
            self.next()?;
            self.expr(v, 0)?; // Don't propagate eflags.
            self.lex_match(b')' as LexToken, b'(' as LexToken, line)?;
            self.expr_discharge(v)?;
        } else if lex_isname(self.ls.tok) {
            let line = self.ls.linenumber;
            let name = self.lex_str()?;
            if eflags & EXPR_F_NORES == 0 && self.ls.tok == TK_ARROW {
                self.parse_shortfunc(v, Some(name), eflags, line)?;
                return Ok(xpc);
            }
            self.var_lookup(v, name)?;
        } else {
            return Err(self.err_syntax(b"unexpected symbol"));
        }
        loop {
            // Parse multiple expression suffixes.
            let mut nav = false;
            if eflags & EXPR_F_NONAV == 0 && self.lex_opt(TK_NAV)? {
                nav = true;
                self.expr_toanyreg(v)?;
                self.nav_check(v.info, &mut xpc)?;
            }
            let tok = self.ls.tok;
            let call = if tok == b'[' as LexToken {
                let mut key = ExpDesc::new(K::Void, 0);
                self.expr_toanyreg(v)?;
                self.expr_bracket(&mut key)?;
                self.expr_index(v, &mut key)?;
                false
            } else if tok == b':' as LexToken {
                if eflags & EXPR_F_NOCOLON != 0 {
                    if nav {
                        return Err(self.err_syntax(b"unexpected symbol"));
                    }
                    break;
                }
                let mut key = ExpDesc::new(K::Void, 0);
                self.next()?; // Skip ':'.
                self.expr_str(&mut key)?;
                self.bcemit_method(v, &key)?;
                nav = false;
                if self.lex_opt(TK_NAV)? {
                    nav = true;
                    self.nav_check(v.info, &mut xpc)?;
                }
                true
            } else if tok == b'(' as LexToken || tok == TK_STRING || tok == b'{' as LexToken {
                self.expr_tonextreg(v)?;
                if self.fr2 != 0 {
                    self.bcreg_reserve(1)?;
                }
                true
            } else if nav || self.lex_opt(b'.' as LexToken)? {
                self.expr_field(v)?;
                false
            } else {
                break;
            };
            if call {
                self.parse_args(v)?;
                // Keep nav VCALL if no suffix follows.
                let t = self.ls.tok;
                if nav
                    && eflags & EXPR_F_NORES == 0
                    && !(t == TK_NAV
                        || t == b'[' as LexToken
                        || t == b':' as LexToken
                        || t == b'(' as LexToken
                        || t == TK_STRING
                        || t == b'{' as LexToken
                        || t == b'.' as LexToken)
                {
                    break;
                }
            }
            if nav && eflags & EXPR_F_NORES == 0 {
                self.expr_tonextreg(v)?;
            }
        }
        Ok(xpc)
    }

    /// Parse primary expression.
    fn expr_primary(&mut self, v: &mut ExpDesc, eflags: u32) -> R {
        let xpc = self.expr_primary_nav(v, eflags)?;
        if xpc != NO_JMP {
            let around = self.bcemit_jmp()?;
            self.jmp_tohere(xpc)?;
            if v.k == K::Call {
                // Change to VCALLNAV. Still points to CALL/CALLM.
                v.k = K::CallNav;
                self.bcemit_ad(bc!(KPRI), v.aux, K::KNil as u32)?;
            } else {
                self.bcemit_ad(bc!(KPRI), v.info, K::KNil as u32)?;
            }
            self.jmp_tohere(around)?;
        }
        Ok(())
    }

    /// Parse simple expression.
    fn expr_simple(&mut self, v: &mut ExpDesc, eflags: u32) -> R {
        match self.ls.tok {
            TK_NUMBER => match self.ls.tokval {
                TokVal::CData(cd) => {
                    v.init(K::KCData, 0);
                    v.cdata = Some(cd);
                }
                TokVal::Num(n) => {
                    v.init(K::KNum, 0);
                    v.nval = n;
                }
                other => unreachable!("number token with value {other:?}"),
            },
            TK_STRING => {
                v.init(K::KStr, 0);
                v.sval = Some(self.ls.tokval.str());
            }
            TK_NIL => v.init(K::KNil, 0),
            TK_TRUE => v.init(K::KTrue, 0),
            TK_FALSE => v.init(K::KFalse, 0),
            TK_DOTS => {
                // Vararg.
                if self.fs().flags & PROTO_VARARG == 0 {
                    return Err(self.err_syntax(b"cannot use '...' outside a vararg function"));
                }
                self.bcreg_reserve(1)?;
                let base = self.fs().freereg - 1;
                let np = self.fs().numparams as u32;
                let pc = self.bcemit_abc(bc!(VARG), base, 2, np)?;
                v.init(K::Call, pc);
                v.aux = base;
            }
            0x7b /* '{' */ => {
                // Table constructor.
                return self.expr_table(v);
            }
            TK_FUNCTION => {
                self.next()?;
                let line = self.ls.linenumber;
                return self.parse_body(v, false, line);
            }
            0x7c /* '|' */ | TK_OR_ => {
                let line = self.ls.linenumber;
                return self.parse_shortfunc(v, None, eflags, line);
            }
            _ => return self.expr_primary(v, eflags),
        }
        self.next()
    }

    /// Manage syntactic levels to avoid blowing up the stack.
    fn synlevel_begin(&mut self) -> R {
        self.level += 1;
        if self.level >= LJ_MAX_XLEVEL {
            return Err(self.ls.error(0, b"chunk has too many syntax levels"));
        }
        Ok(())
    }

    #[inline]
    fn synlevel_end(&mut self) {
        self.level -= 1;
    }

    /// Parse unary expression.
    fn expr_unop(&mut self, v: &mut ExpDesc, eflags: u32) -> R {
        let tok = self.ls.tok;
        let op = if tok == TK_NOT || tok == b'!' as LexToken {
            bc!(NOT)
        } else if tok == b'-' as LexToken {
            bc!(UNM)
        } else if tok == b'#' as LexToken {
            bc!(LEN)
        } else if tok == b'~' as LexToken {
            bc!(BNOT)
        } else {
            return self.expr_simple(v, eflags);
        };
        self.next()?;
        self.expr_binop(v, UNARY_PRIORITY, eflags)?;
        self.bcemit_unop(op, v)
    }

    /// Parse binary expressions with priority higher than the limit.
    fn expr_binop(&mut self, v: &mut ExpDesc, limit: u32, eflags: u32) -> R<u32> {
        self.synlevel_begin()?;
        self.expr_unop(v, eflags)?;
        let mut opr = token2binop(self.ls.tok);
        while opr != OPR_NOBINOPR && PRIORITY[opr as usize].0 > limit {
            let mut v2 = ExpDesc::new(K::Void, 0);
            self.next()?;
            self.bcemit_binop_left(opr, v)?;
            // Parse binary expression with higher priority.
            let nextop = self.expr_binop(&mut v2, PRIORITY[opr as usize].1, eflags)?;
            self.bcemit_binop(opr, v, &mut v2)?;
            opr = nextop;
        }
        self.synlevel_end();
        Ok(opr) // Return unconsumed binary operator (if any).
    }

    /// Parse expression.
    fn expr(&mut self, v: &mut ExpDesc, eflags: u32) -> R {
        self.expr_binop(v, 0, eflags)?; // Priority 0: parse whole expression.
        if self.lex_opt(b'?' as LexToken)? {
            // Ternary ?: conditional operator. Right-assoc.
            let mut escapelist = NO_JMP;
            self.bcemit_branch_t(v)?;
            let cond = v.f;
            self.expr(v, EXPR_F_NOCOLON)?; // Prevent method parsing. Use parentheses.
            self.expr_tonextreg(v)?;
            let reg = v.info;
            let j = self.bcemit_jmp()?;
            self.jmp_append(&mut escapelist, j)?;
            self.jmp_tohere(cond)?;
            self.lex_check(b':' as LexToken)?;
            self.bcreg_free(reg);
            self.expr(v, 0)?;
            self.expr_tonextreg(v)?;
            self.jmp_tohere(escapelist)?;
        }
        Ok(())
    }

    /// Assign expression to the next register.
    fn expr_next(&mut self) -> R {
        let mut e = ExpDesc::new(K::Void, 0);
        self.expr(&mut e, 0)?;
        self.expr_tonextreg(&mut e)
    }

    /// Parse conditional expression.
    fn expr_cond(&mut self) -> R<u32> {
        let mut v = ExpDesc::new(K::Void, 0);
        self.expr(&mut v, 0)?;
        if v.k == K::KNil {
            v.k = K::KFalse;
        }
        self.bcemit_branch_t(&mut v)?;
        Ok(v.f)
    }

    // -- Assignments --

    /// Parse compound assignment.
    fn parse_compound(&mut self, e: &mut ExpDesc) -> R<bool> {
        if !(e.k >= K::Local && e.k <= K::Indexed) {
            return Ok(false);
        }
        let mut opr = token2binop(self.ls.tok);
        // '^=' is deliberately omitted; use 'a ~= b' for xor assignment.
        if opr > OPR_NE || opr == OPR_POW {
            return Ok(false);
        }
        self.var_assign(e)?;
        if opr == OPR_NE {
            if self.ls.tok != TK_NE {
                return Err(self.ls.error(b'!' as LexToken, b"'=' expected"));
            }
            opr = OPR_BXOR;
        } else {
            // Can't use lex_check() here. Only allow '+=', not '+ ='.
            if self.ls.c != b'=' as LexChar {
                return Err(self.err_token(b'=' as LexToken));
            }
            self.next()?; // Skip operator.
        }
        self.next()?; // Skip '=' or '~=' aka TOK_ne.
        let estore = *e;
        if e.k == K::Indexed {
            // Preserve the base and key for the store.
            let freg = self.fs().freereg;
            self.expr_discharge(e)?;
            self.fsm().freereg = freg; // Undo bcreg_free of info and/or aux.
        }
        if opr == OPR_CONCAT {
            self.expr_tonextreg(e)?;
        } else {
            self.expr_toanyreg(e)?;
        }
        let mut v = ExpDesc::new(K::Void, 0);
        self.expr(&mut v, 0)?;
        self.bcemit_binop(opr, e, &mut v)?;
        self.bcemit_store(&estore, e)?;
        // Don't bother to free VINDEXED info+aux. Done by parse_chunk().
        Ok(true)
    }

    /// Eliminate write-after-read hazards for local variable assignment.
    fn assign_hazard(&mut self, lh: &mut [ExpDesc], v: &ExpDesc) -> R {
        let reg = v.info; // Check against this variable.
        let tmp = self.fs().freereg; // Rename to this temp. register (if needed).
        let mut hazard = false;
        for l in lh.iter_mut().rev() {
            if l.k == K::Indexed {
                if l.info == reg {
                    // t[i], t = 1, 2
                    hazard = true;
                    l.info = tmp;
                }
                if l.aux == reg {
                    // t[i], i = 1, 2
                    hazard = true;
                    l.aux = tmp;
                }
            }
        }
        if hazard {
            self.bcemit_ad(bc!(MOV), tmp, reg)?; // Rename conflicting variable.
            self.bcreg_reserve(1)?;
        }
        Ok(())
    }

    /// Adjust LHS/RHS of an assignment.
    fn assign_adjust(&mut self, nvars: u32, nexps: u32, e: &mut ExpDesc) -> R {
        let mut extra = nvars as i32 - nexps as i32;
        if e.k == K::Call || e.k == K::CallNav {
            let ilp = e.info;
            extra += 1; // Compensate for the VCALL itself.
            if extra < 0 {
                extra = 0;
            }
            setbc_b(self.insm(ilp), (extra + 1) as u32); // Fixup call results.
            if extra > 1 {
                self.bcreg_reserve(extra as u32 - 1)?;
            }
            if e.k == K::CallNav {
                // Safe navigation result.
                let base = e.aux;
                debug_assert!(
                    (bc_op(self.ins(ilp)) == bc!(CALL) || bc_op(self.ins(ilp)) == bc!(CALLM))
                        && bc_op(self.ins(ilp + 1)) == bc!(JMP)
                        && bc_op(self.ins(ilp + 2)) == bc!(KPRI),
                    "expected CALL|CALLM, JMP, KPRI inside safe navigation"
                );
                setbc_a(self.insm(ilp + 1), base + extra as u32); // Fixup JMP nactvar.
                if extra > 1 {
                    // Need more nils. Case extra == 0 is harmless.
                    *self.insm(ilp + 2) = ins_ad(bc!(KNIL), base, base + extra as u32 - 1);
                }
            }
        } else {
            if e.k != K::Void {
                self.expr_tonextreg(e)?; // Close last expression.
            }
            if extra > 0 {
                // Leftover LHS are set to nil.
                let reg = self.fs().freereg;
                self.bcreg_reserve(extra as u32)?;
                self.bcemit_nil(reg, extra as u32)?;
            }
        }
        if nexps > nvars {
            self.fsm().freereg -= nexps - nvars; // Drop leftover regs.
        }
        Ok(())
    }

    /// Recursively parse assignment statement. `lh` holds the LHS variables parsed so far
    /// (innermost last).
    fn parse_assignment(&mut self, lh: &mut Vec<ExpDesc>, nvars: u32) -> R {
        let cur = *lh.last().expect("empty LHS list");
        if !(K::Local <= cur.k && cur.k <= K::Indexed) {
            return Err(self.err_syntax(b"syntax error"));
        }
        self.var_assign(&cur)?;
        if self.lex_opt(b',' as LexToken)? {
            // Collect LHS list and recurse upwards.
            let mut v = ExpDesc::new(K::Void, 0);
            self.expr_primary(&mut v, EXPR_F_NONAV)?;
            if v.k == K::Local {
                self.assign_hazard(lh, &v)?;
            }
            self.checklimit(self.level + nvars, LJ_MAX_XLEVEL, "variable names")?;
            lh.push(v);
            let r = self.parse_assignment(lh, nvars + 1);
            lh.pop();
            r?;
        } else {
            // Parse RHS.
            let mut e = ExpDesc::new(K::Void, 0);
            self.lex_check(b'=' as LexToken)?;
            let nexps = self.expr_list(&mut e)?;
            if nexps == nvars {
                if e.k == K::Call {
                    if bc_op(self.ins(e.info)) == bc!(VARG) {
                        // Vararg assignment.
                        self.fsm().freereg -= 1;
                        e.k = K::Relocable;
                    } else {
                        // Multiple call results.
                        e.info = e.aux; // Base of call is not relocatable.
                        e.k = K::NonReloc;
                    }
                }
                let var = *lh.last().expect("empty LHS list");
                return self.bcemit_store(&var, &mut e);
            }
            self.assign_adjust(nvars, nexps, &mut e)?;
        }
        // Assign RHS to LHS and recurse downwards.
        let mut e = ExpDesc::new(K::NonReloc, self.fs().freereg - 1);
        let var = *lh.last().expect("empty LHS list");
        self.bcemit_store(&var, &mut e)
    }

    /// Parse call statement or assignment.
    fn parse_call_assign(&mut self) -> R {
        let mut v = ExpDesc::new(K::Void, 0);
        let xpc = self.expr_primary_nav(&mut v, EXPR_F_NORES)?;
        if v.k == K::Call {
            // Function call statement.
            setbc_b(self.insm(v.info), 1); // No results.
        } else {
            // Start of an assignment.
            debug_assert!(v.k != K::CallNav, "unexpected VCALLNAV in statement");
            // Safe navigation is incompatible with parallel assignment.
            if !(xpc == NO_JMP || self.ls.tok != b',' as LexToken) {
                return Err(self.err_syntax(b"syntax error"));
            }
            if !self.parse_compound(&mut v)? {
                let mut lh = vec![v];
                self.parse_assignment(&mut lh, 1)?;
            }
        }
        if xpc != NO_JMP {
            self.jmp_tohere(xpc)?;
        }
        Ok(())
    }

    /// Parse 'local' or 'const' statement.
    fn parse_local(&mut self, vinfo: u8) -> R {
        self.next()?; // Skip local or const.
        if self.lex_opt(TK_FUNCTION)? {
            // Local function declaration.
            let s = self.lex_str()?;
            let vidx = self.var_new(0, VName::Str(s))?;
            self.vstack[vidx].info = vinfo;
            let freereg = self.fs().freereg;
            let mut v = ExpDesc::new(K::Local, freereg);
            v.aux = self.fs().varmap[freereg as usize] as u32;
            self.bcreg_reserve(1)?;
            self.var_add(1);
            let mut b = ExpDesc::new(K::Void, 0);
            let line = self.ls.linenumber;
            self.parse_body(&mut b, false, line)?;
            // bcemit_store(fs, &v, &b) without setting VSTACK_VAR_RW.
            self.expr_free(&b);
            self.expr_toreg(&mut b, v.info)?;
            // The upvalue is in scope, but the local is only valid after the store.
            let vi = self.var_idx(self.fs().nactvar - 1);
            self.vstack[vi].startpc = self.fs().pc;
        } else {
            // Local variable declaration.
            let mut e = ExpDesc::new(K::Void, 0);
            let mut nvars = 0;
            if vinfo != 0 {
                // Multiple consts need to be checked against each other.
                let vhsave = self.vhash;
                loop {
                    // Collect LHS.
                    let s = self.lex_str()?;
                    let vidx = self.var_new(nvars, VName::Str(s))?;
                    nvars += 1;
                    let h = var_hash(self.vstack[vidx].name).expect("string name");
                    self.vstack[vidx].prev = self.vhash[h]; // Temporarily add to hash.
                    self.vhash[h] = vidx as u16;
                    self.vstack[vidx].info = vinfo;
                    if !self.lex_opt(b',' as LexToken)? {
                        break;
                    }
                }
                self.vhash = vhsave; // Restore hash anchors.
            } else {
                loop {
                    // Collect LHS.
                    let s = self.lex_str()?;
                    self.var_new(nvars, VName::Str(s))?;
                    nvars += 1;
                    if !self.lex_opt(b',' as LexToken)? {
                        break;
                    }
                }
            }
            let nexps = if self.lex_opt(b'=' as LexToken)? {
                // Optional RHS.
                self.expr_list(&mut e)?
            } else {
                // Or implicitly set to nil.
                e.k = K::Void;
                0
            };
            self.assign_adjust(nvars, nexps, &mut e)?;
            self.var_add(nvars);
        }
        Ok(())
    }

    /// Parse 'function' statement.
    fn parse_func(&mut self, line: u32) -> R {
        let mut v = ExpDesc::new(K::Void, 0);
        let mut b = ExpDesc::new(K::Void, 0);
        let mut needself = false;
        self.next()?; // Skip 'function'.
        // Parse function name.
        let s = self.lex_str()?;
        self.var_lookup(&mut v, s)?;
        while self.lex_opt(b'.' as LexToken)? {
            // Multiple dot-separated fields.
            self.expr_field(&mut v)?;
        }
        if self.lex_opt(b':' as LexToken)? {
            // Optional colon to signify method call.
            needself = true;
            self.expr_field(&mut v)?;
        }
        self.var_assign(&v)?;
        self.parse_body(&mut b, needself, line)?;
        self.bcemit_store(&v, &mut b)?;
        let pc = self.fs().pc - 1;
        self.set_line(pc, line); // Set line for the store.
        Ok(())
    }

    // -- Control transfer statements --

    /// Parse 'return' statement.
    fn parse_return(&mut self, eflags: u32) -> R {
        self.fsm().flags |= PROTO_HAS_RETURN;
        let ins;
        if eflags & EXPR_F_RET1 == 0 && (parse_isend(self.ls.tok) || self.ls.tok == b';' as LexToken) {
            ins = ins_ad(bc!(RET0), 0, 1); // Bare return.
        } else {
            // Return with one or more values.
            let mut e = ExpDesc::new(K::Void, 0); // Receives the _last_ expression in the list.
            let nret = if eflags & EXPR_F_RET1 != 0 {
                self.expr(&mut e, eflags)?;
                1
            } else {
                self.expr_list(&mut e)?
            };
            let nactvar = self.fs().nactvar;
            if nret == 1 {
                // Return one result.
                if e.k == K::Call {
                    // Check for tail call.
                    let ip = self.ins(e.info);
                    if bc_op(ip) == bc!(VARG) {
                        // It doesn't pay off to add BC_VARGT just for 'return ...'.
                        setbc_b(self.insm(e.info), 0);
                        ins = ins_ad(bc!(RETM), nactvar, e.aux - nactvar);
                    } else {
                        self.fsm().pc -= 1;
                        ins = ins_ad(bc_op(ip) + bc!(CALLT) - bc!(CALL), bc_a(ip), bc_c(ip));
                    }
                } else {
                    // Can return the result from any register.
                    let r = self.expr_toanyreg(&mut e)?;
                    ins = ins_ad(bc!(RET1), r, 2);
                }
            } else if e.k == K::Call {
                // Append all results from a call.
                setbc_b(self.insm(e.info), 0);
                ins = ins_ad(bc!(RETM), nactvar, e.aux - nactvar);
            } else {
                self.expr_tonextreg(&mut e)?; // Force contiguous registers.
                ins = ins_ad(bc!(RET), nactvar, nret + 1);
            }
        }
        if self.fs().flags & PROTO_CHILD != 0 {
            self.bcemit_aj(bc!(UCLO), 0, 0)?; // May need to close upvalues first.
        }
        self.bcemit_ins(ins)?;
        Ok(())
    }

    /// Parse 'break' statement.
    fn parse_break(&mut self) -> R {
        self.bl().flags |= FSCOPE_BREAK;
        let j = self.bcemit_jmp()?;
        self.gola_new(NAME_BREAK, VSTACK_GOTO, j)?;
        Ok(())
    }

    /// Parse 'continue' statement.
    fn parse_continue(&mut self) -> R {
        self.bl().flags |= FSCOPE_CONT;
        let j = self.bcemit_jmp()?;
        self.gola_new(NAME_CONT, VSTACK_GOTO, j)?;
        Ok(())
    }

    /// Parse 'goto' statement.
    fn parse_goto(&mut self) -> R {
        let name = VName::Str(self.lex_str()?);
        if let Some(vl) = self.gola_findlabel(name) {
            // Treat backwards goto within same scope like a loop.
            let slot = self.vstack[vl].slot as u32;
            self.bcemit_aj(bc!(LOOP), slot, (-1i32) as u32)?; // No BC range check.
        }
        self.bl().flags |= FSCOPE_GOLA;
        let j = self.bcemit_jmp()?;
        self.gola_new(name, VSTACK_GOTO, j)?;
        Ok(())
    }

    /// Parse label.
    fn parse_label(&mut self) -> R {
        let pc = self.fs().pc;
        self.fsm().lasttarget = pc;
        self.bl().flags |= FSCOPE_GOLA;
        self.next()?; // Skip '::'.
        let s = self.lex_str()?;
        let name = VName::Str(s);
        if self.gola_findlabel(name).is_some() {
            let msg = cat(&[b"duplicate label ", &qs(s.as_bytes())]);
            return Err(self.ls.error(0, &msg));
        }
        let pc = self.fs().pc;
        let idx = self.gola_new(name, VSTACK_LABEL, pc)?;
        self.lex_check(TK_LABEL)?;
        // Recursively parse trailing statements: labels and ';' (Lua 5.2 only).
        while self.ls.tok == TK_LABEL {
            self.synlevel_begin()?;
            self.parse_label()?;
            self.synlevel_end();
        }
        // Trailing label is considered to be outside of scope.
        if parse_isend(self.ls.tok) && self.ls.tok != TK_UNTIL {
            let n = self.bl().nactvar;
            self.vstack[idx].slot = n as u8;
        }
        let vstart = self.bl().vstart;
        self.gola_resolve(vstart, idx)
    }

    // -- Blocks, loops and conditional statements --

    /// Parse a block.
    fn parse_block(&mut self) -> R {
        self.fscope_begin(0);
        self.parse_chunk()?;
        self.fscope_end()
    }

    /// Parse 'while' statement.
    fn parse_while(&mut self, line: u32) -> R {
        self.next()?; // Skip 'while'.
        let start = self.fs().pc;
        self.fsm().lasttarget = start;
        let condexit = self.expr_cond()?;
        self.fscope_begin(FSCOPE_LOOP);
        self.lex_check(TK_DO)?;
        let nactvar = self.fs().nactvar;
        let loop_ = self.bcemit_ad(bc!(LOOP), nactvar, 0)?;
        self.parse_block()?;
        let j = self.bcemit_jmp()?;
        self.jmp_patch(j, start)?;
        self.lex_match(TK_END, TK_WHILE, line)?;
        self.fscope_continue(start)?;
        self.fscope_end()?;
        self.jmp_tohere(condexit)?;
        let pc = self.fs().pc;
        self.jmp_patchins(loop_, pc)
    }

    /// Parse 'repeat' statement.
    fn parse_repeat(&mut self, line: u32) -> R {
        let loop_ = self.fs().pc;
        self.fsm().lasttarget = loop_;
        self.fscope_begin(FSCOPE_LOOP); // Breakable loop scope.
        self.fscope_begin(0); // Inner scope.
        self.next()?; // Skip 'repeat'.
        let nactvar = self.fs().nactvar;
        self.bcemit_ad(bc!(LOOP), nactvar, 0)?;
        self.parse_chunk()?;
        self.lex_match(TK_UNTIL, TK_REPEAT, line)?;
        let pc = self.fs().pc;
        self.fscope_continue(pc)?;
        let mut condexit = self.expr_cond()?; // Parse condition (still inside inner scope).
        if self.bl().flags & FSCOPE_UPVAL == 0 {
            // No upvalues? Just end inner scope.
            self.fscope_end()?;
        } else {
            // Otherwise generate: cond: UCLO+JMP out, !cond: UCLO+JMP loop.
            self.parse_break()?; // Break from loop and close upvalues.
            self.jmp_tohere(condexit)?;
            self.fscope_end()?; // End inner scope and close upvalues.
            condexit = self.bcemit_jmp()?;
        }
        self.jmp_patch(condexit, loop_)?; // Jump backwards if !cond.
        let pc = self.fs().pc;
        self.jmp_patchins(loop_, pc)?;
        self.fscope_end() // End loop scope.
    }

    /// Parse numeric 'for'.
    fn parse_for_num(&mut self, varname: Gc<LuaStr>, line: u32) -> R {
        let base = self.fs().freereg;
        // Hidden control variables.
        self.var_new(FORL_IDX, VName::Fixed(VARNAME_FOR_IDX))?;
        self.var_new(FORL_STOP, VName::Fixed(VARNAME_FOR_STOP))?;
        self.var_new(FORL_STEP, VName::Fixed(VARNAME_FOR_STEP))?;
        // Visible copy of index variable.
        self.var_new(FORL_EXT, VName::Str(varname))?;
        self.lex_check(b'=' as LexToken)?;
        self.expr_next()?;
        self.lex_check(b',' as LexToken)?;
        self.expr_next()?;
        if self.lex_opt(b',' as LexToken)? {
            self.expr_next()?;
        } else {
            let r = self.fs().freereg;
            self.bcemit_ad(bc!(KSHORT), r, 1)?; // Default step is 1.
            self.bcreg_reserve(1)?;
        }
        self.var_add(3); // Hidden control variables.
        self.lex_check(TK_DO)?;
        let loop_ = self.bcemit_aj(bc!(FORI), base, NO_JMP)?;
        self.fscope_begin(0); // Scope for visible variables.
        self.var_add(1);
        self.bcreg_reserve(1)?;
        self.parse_block()?;
        self.fscope_end()?;
        let pc = self.fs().pc;
        self.fscope_continue(pc)?;
        // Perform loop inversion. Loop control instructions are at the end.
        let loopend = self.bcemit_aj(bc!(FORL), base, NO_JMP)?;
        self.set_line(loopend, line); // Fix line for control ins.
        self.jmp_patchins(loopend, loop_ + 1)?;
        let pc = self.fs().pc;
        self.jmp_patchins(loop_, pc)
    }

    /// Try to predict whether the iterator is next() and specialize the bytecode.
    fn predict_next(&mut self, pc: u32) -> bool {
        let ins = self.ins(pc);
        let name = match bc_op(ins) {
            op if op == bc!(MOV) => {
                if bc_d(ins) >= self.fs().nactvar {
                    return false;
                }
                self.vstack[self.var_idx(bc_d(ins))].name
            }
            op if op == bc!(UGET) => {
                let vi = self.fs().uvmap[bc_d(ins) as usize];
                self.vstack[vi as usize].name
            }
            op if op == bc!(GGET) => {
                // There's no inverse index (yet), so lookup the strings.
                let d = bc_d(ins);
                for s in [&b"pairs"[..], &b"next"[..]] {
                    let st = self.ls.vm.new_str(s);
                    if self.fs().kt.strs.get(&st) == Some(&d) {
                        return true;
                    }
                }
                return false;
            }
            _ => return false,
        };
        match name {
            VName::Str(s) => s.as_bytes() == b"pairs" || s.as_bytes() == b"next",
            _ => false,
        }
    }

    /// Parse 'for' iterator.
    fn parse_for_iter(&mut self, indexname: Gc<LuaStr>) -> R {
        let mut nvars = 0;
        let base = self.fs().freereg + 3;
        let exprpc = self.fs().pc;
        // Hidden control variables.
        self.var_new(nvars, VName::Fixed(VARNAME_FOR_GEN))?;
        nvars += 1;
        self.var_new(nvars, VName::Fixed(VARNAME_FOR_STATE))?;
        nvars += 1;
        self.var_new(nvars, VName::Fixed(VARNAME_FOR_CTL))?;
        nvars += 1;
        // Visible variables returned from iterator.
        self.var_new(nvars, VName::Str(indexname))?;
        nvars += 1;
        while self.lex_opt(b',' as LexToken)? {
            let s = self.lex_str()?;
            self.var_new(nvars, VName::Str(s))?;
            nvars += 1;
        }
        self.lex_check(TK_IN)?;
        let line = self.ls.linenumber;
        let mut e = ExpDesc::new(K::Void, 0);
        let n = self.expr_list(&mut e)?;
        self.assign_adjust(3, n, &mut e)?;
        // The iterator needs another 3 [4] slots (func [pc] | state ctl).
        self.bcreg_bump(3 + self.fr2)?;
        let isnext = nvars <= 5 && self.fs().pc > exprpc && self.predict_next(exprpc);
        self.var_add(3); // Hidden control variables.
        self.lex_check(TK_DO)?;
        let loop_ = self.bcemit_aj(if isnext { bc!(ISNEXT) } else { bc!(JMP) }, base, NO_JMP)?;
        self.fscope_begin(0); // Scope for visible variables.
        self.var_add(nvars - 3);
        self.bcreg_reserve(nvars - 3)?;
        self.parse_block()?;
        self.fscope_end()?;
        // Perform loop inversion. Loop control instructions are at the end.
        let pc = self.fs().pc;
        self.jmp_patchins(loop_, pc)?;
        self.fscope_continue(pc)?;
        self.bcemit_abc(if isnext { bc!(ITERN) } else { bc!(ITERC) }, base, nvars - 3 + 1, 2 + 1)?;
        let loopend = self.bcemit_aj(bc!(ITERL), base, NO_JMP)?;
        self.set_line(loopend - 1, line); // Fix line for control ins.
        self.set_line(loopend, line);
        self.jmp_patchins(loopend, loop_ + 1)
    }

    /// Parse 'for' statement.
    fn parse_for(&mut self, line: u32) -> R {
        self.fscope_begin(FSCOPE_LOOP);
        self.next()?; // Skip 'for'.
        let varname = self.lex_str()?; // Get first variable name.
        if self.ls.tok == b'=' as LexToken {
            self.parse_for_num(varname, line)?;
        } else if self.ls.tok == b',' as LexToken || self.ls.tok == TK_IN {
            self.parse_for_iter(varname)?;
        } else {
            return Err(self.err_syntax(b"'=' or 'in' expected"));
        }
        self.lex_match(TK_END, TK_FOR, line)?;
        self.fscope_end() // Resolve break list.
    }

    /// Parse condition and 'then' block.
    fn parse_then(&mut self) -> R<u32> {
        self.next()?; // Skip 'if' or 'elseif'.
        let condexit = self.expr_cond()?;
        self.lex_check(TK_THEN)?;
        self.parse_block()?;
        Ok(condexit)
    }

    /// Parse 'if' statement.
    fn parse_if(&mut self, line: u32) -> R {
        let mut escapelist = NO_JMP;
        let mut flist = self.parse_then()?;
        while self.ls.tok == TK_ELSEIF {
            // Parse multiple 'elseif' blocks.
            let j = self.bcemit_jmp()?;
            self.jmp_append(&mut escapelist, j)?;
            self.jmp_tohere(flist)?;
            flist = self.parse_then()?;
        }
        if self.ls.tok == TK_ELSE {
            // Parse optional 'else' block.
            let j = self.bcemit_jmp()?;
            self.jmp_append(&mut escapelist, j)?;
            self.jmp_tohere(flist)?;
            self.next()?; // Skip 'else'.
            self.parse_block()?;
        } else {
            self.jmp_append(&mut escapelist, flist)?;
        }
        self.jmp_tohere(escapelist)?;
        self.lex_match(TK_END, TK_IF, line)
    }

    // -- Parse statements --

    /// Parse a statement. Returns true if it must be the last one in a chunk.
    fn parse_stmt(&mut self) -> R<bool> {
        let line = self.ls.linenumber;
        match self.ls.tok {
            TK_IF => self.parse_if(line)?,
            TK_WHILE => self.parse_while(line)?,
            TK_DO => {
                self.next()?;
                self.parse_block()?;
                self.lex_match(TK_END, TK_DO, line)?;
            }
            TK_FOR => self.parse_for(line)?,
            TK_REPEAT => self.parse_repeat(line)?,
            TK_FUNCTION => self.parse_func(line)?,
            TK_LOCAL => self.parse_local(0)?,
            TK_CONST => {
                let tokx = self.ls.lookahead()?;
                if !(lex_isname(tokx) || tokx == TK_FUNCTION) {
                    self.parse_call_assign()?; // Soft keyword.
                } else {
                    self.parse_local(VSTACK_CONST)?;
                }
            }
            TK_RETURN => {
                self.next()?;
                self.parse_return(0)?;
                return Ok(true); // Must be last.
            }
            TK_BREAK => {
                self.next()?;
                self.parse_break()?;
                return Ok(true); // Must be last in Lua 5.1.
            }
            TK_CONTINUE => {
                if !parse_isend(self.ls.lookahead()?) {
                    self.parse_call_assign()?; // Soft keyword.
                } else {
                    self.next()?;
                    self.parse_continue()?;
                    return Ok(true); // Must be last.
                }
            }
            TK_LABEL => self.parse_label()?,
            TK_GOTO => {
                if lex_isname(self.ls.lookahead()?) {
                    // 5.1 soft keyword.
                    self.next()?;
                    self.parse_goto()?;
                } else {
                    self.parse_call_assign()?;
                }
            }
            _ => self.parse_call_assign()?,
        }
        Ok(false)
    }

    /// A chunk is a list of statements optionally separated by semicolons.
    fn parse_chunk(&mut self) -> R {
        let mut islast = false;
        self.synlevel_begin()?;
        while !islast && !parse_isend(self.ls.tok) {
            islast = self.parse_stmt()?;
            self.lex_opt(b';' as LexToken)?;
            let fs = self.fsm();
            debug_assert!(fs.framesize as u32 >= fs.freereg && fs.freereg >= fs.nactvar, "bad regalloc");
            fs.freereg = fs.nactvar; // Free registers after each stmt.
        }
        self.synlevel_end();
        Ok(())
    }

    /// Entry point of bytecode parser (`lj_parse`). The lexer must be set up already.
    pub fn parse(mut self) -> R<Gc<Proto>> {
        self.level = 0;
        self.fs_init();
        self.fsm().flags |= PROTO_VARARG; // Main chunk is always a vararg func.
        self.fscope_begin(0);
        self.bcemit_ad(bc!(FUNCV), 0, 0)?; // Placeholder.
        self.next()?; // Read-ahead first token.
        self.parse_chunk()?;
        if self.ls.tok != TK_EOF {
            return Err(self.err_token(TK_EOF));
        }
        let line = self.ls.linenumber;
        let pt = self.fs_finish(line)?;
        debug_assert!(self.fss.is_empty(), "mismatched frame nesting");
        debug_assert!(pt.uv.is_empty(), "toplevel proto has upvalues");
        Ok(self.ls.vm.new_proto(pt))
    }
}
