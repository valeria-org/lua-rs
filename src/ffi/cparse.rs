//! C declaration parser.
//!
//! Port of lj_cparse.c. This is NOT a validating C parser, just a minimal C
//! declaration parser for the FFI: it returns correct results for properly
//! formed declarations, but may accept some invalid ones, too. Struct layout
//! follows the x86-64 SysV ABI exactly like upstream.

use super::ctype::*;
use super::{FfiError, errmsg};

/// Max. declaration stack depth.
pub const CPARSE_MAX_DECLSTACK: usize = 100;
/// Max. recursive declaration depth.
pub const CPARSE_MAX_DECLDEPTH: i32 = 20;
/// Max. pack pragma stack depth.
pub const CPARSE_MAX_PACKSTACK: usize = 7;

/// Process multiple declarations.
pub const CPARSE_MODE_MULTI: u32 = 1;
/// Accept abstract declarators.
pub const CPARSE_MODE_ABSTRACT: u32 = 2;
/// Accept direct declarators.
pub const CPARSE_MODE_DIRECT: u32 = 4;
/// Accept field width in bits, too.
pub const CPARSE_MODE_FIELD: u32 = 8;
/// Reject implicit declarations.
pub const CPARSE_MODE_NOIMPLICIT: u32 = 16;
/// Skip definitions, ignore errors.
pub const CPARSE_MODE_SKIP: u32 = 32;

/// Namespaces for resolving identifiers.
pub const CPNS_DEFAULT: u32 =
    (1 << CT_KW) | (1 << CT_TYPEDEF) | (1 << CT_FUNC) | (1 << CT_EXTERN) | (1 << CT_CONSTVAL);
pub const CPNS_STRUCT: u32 = (1 << CT_KW) | (1 << CT_STRUCT) | (1 << CT_ENUM);

/// A `$` type parameter, taken from the extra arguments of `ffi.cdef`/`ffi.typeof`.
#[derive(Clone, Copy, Debug)]
pub enum CParam<'a> {
    /// A string: substituted as an identifier.
    Str(&'a [u8]),
    /// A number (already converted with `lj_num2int`): substituted as integer.
    Num(i32),
    /// A cdata: its ctype id (or, for a `ctype` cdata, the type it holds).
    CType(CTypeID),
    /// Any other value at Lua argument position `narg` (raises
    /// `bad argument #narg (type parameter expected, got ...)`).
    Other(i32),
}

/// Parse C declarations (`lj_cparse`).
///
/// `params` is `None` if `$` parameters are not allowed at all; otherwise all
/// given parameters must be consumed. On error the type table is rolled back.
/// Returns the type id of the declaration (meaningful for single abstract
/// declarations, as used by `ffi.typeof`/`ffi.new`/...).
pub fn cparse(
    cts: &mut CTState,
    src: &[u8],
    mode: u32,
    params: Option<&[CParam<'_>]>,
) -> Result<CTypeID, FfiError> {
    let saved = cts.save();
    let res = CPState::new(cts, src, mode, params).run();
    if res.is_err() {
        cts.restore(&saved);
    }
    res
}

/// Check string against a linear list of matches (`lj_cparse_case`).
pub fn cparse_case(s: &[u8], matches: &[&str]) -> Option<usize> {
    matches.iter().position(|m| m.as_bytes() == s)
}

type PResult<T> = Result<T, FfiError>;

/// C parser internal value representation.
#[derive(Clone, Copy, Default, Debug)]
struct CPValue {
    /// Value bits (`i32` for CTID_INT32, `u32` for CTID_UINT32).
    u32: u32,
    /// C Type ID of the value.
    id: CTypeID,
}

impl CPValue {
    #[inline]
    fn i32(&self) -> i32 {
        self.u32 as i32
    }
    #[inline]
    fn set_i32(&mut self, v: i32) {
        self.u32 = v as u32;
    }
}

/// Type declaration stack element.
#[derive(Clone, Copy, Default)]
struct DeclElem {
    info: CTInfo,
    size: CTSize,
    sib: CTypeID,
    next: usize,
}

/// Type declaration context.
struct CPDecl {
    top: usize,
    pos: usize,
    specpos: usize,
    mode: u32,
    name: Option<Vec<u8>>,
    redir: Option<Vec<u8>>,
    nameid: CTypeID,
    attr: CTInfo,
    fattr: CTInfo,
    specattr: CTInfo,
    specfattr: CTInfo,
    bits: CTSize,
    stack: [DeclElem; CPARSE_MAX_DECLSTACK],
}

impl CPDecl {
    fn new() -> CPDecl {
        CPDecl {
            top: 0,
            pos: 0,
            specpos: 0,
            mode: 0,
            name: None,
            redir: None,
            nameid: 0,
            attr: 0,
            fattr: 0,
            specattr: 0,
            specfattr: 0,
            bits: 0,
            stack: [DeclElem::default(); CPARSE_MAX_DECLSTACK],
        }
    }

    /// Reset declaration state to declaration specifier.
    fn reset(&mut self) {
        self.pos = self.specpos;
        self.top = self.specpos + 1;
        self.stack[self.specpos].next = 0;
        self.attr = self.specattr;
        self.fattr = self.specfattr;
        self.name = None;
        self.redir = None;
    }
}

#[inline]
fn lj_fls(x: u32) -> u32 {
    31 - x.leading_zeros()
}

#[inline]
fn char_isident(c: i32) -> bool {
    matches!(c, 0x30..=0x39 | 0x41..=0x5a | 0x61..=0x7a | 0x5f | 0x80..=0xff)
}
#[inline]
fn char_isdigit(c: i32) -> bool {
    (0x30..=0x39).contains(&c)
}
#[inline]
fn char_isxdigit(c: i32) -> bool {
    matches!(c, 0x30..=0x39 | 0x41..=0x46 | 0x61..=0x66)
}
#[inline]
fn char_iscntrl(c: i32) -> bool {
    (0..0x20).contains(&c) || c == 0x7f
}
#[inline]
fn iseol(c: i32) -> bool {
    c == b'\n' as i32 || c == b'\r' as i32
}

const fn t(c: u8) -> CPToken {
    c as CPToken
}

/// Scan a C integer literal token like `lj_strscan_scan(..., STRSCAN_OPT_C)`
/// restricted to the outcomes the C parser accepts: returns the value and
/// `CTID_INT32` or `CTID_UINT32`, or `None` for anything else (floats,
/// 64 bit suffixes, overflow, bad digits).
fn scan_c_integer(s: &[u8]) -> Option<(u32, CTypeID)> {
    let at = |i: usize| s.get(i).copied().unwrap_or(0);
    let mut p = 0;
    let mut base = if at(0) == b'0' { 0 } else { 10 };
    if at(0) == b'0' {
        match at(1) | 0x20 {
            b'x' => {
                base = 16;
                p = 2;
            }
            b'b' => {
                base = 2;
                p = 2;
            }
            _ => {}
        }
    }
    let mut hasdig = false;
    while at(p) == b'0' {
        hasdig = true;
        p += 1;
    }
    let sp = p;
    let isdig = |c: u8| {
        if base == 16 {
            c.is_ascii_hexdigit()
        } else {
            c.is_ascii_digit()
        }
    };
    while isdig(at(p)) {
        p += 1;
    }
    let digits = &s[sp..p];
    if !hasdig && digits.is_empty() {
        return None;
    }
    // An exponent makes it a floating-point number.
    if base >= 10 && (at(p) | 0x20) == if base == 16 { b'p' } else { b'e' } {
        return None;
    }
    // Suffix: only 'u'/'U' is acceptable ('l' implies a 64 bit type).
    let mut unsigned = false;
    if p < s.len() {
        if at(p) | 0x20 == b'u' {
            unsigned = true;
            p += 1;
        }
        if p < s.len() {
            return None;
        }
    }
    let radix = match base {
        0 => 8,
        b => b,
    };
    let mut x: u64 = 0;
    for &c in digits {
        let d = (c as char).to_digit(radix)? as u64;
        x = x.checked_mul(radix as u64)?.checked_add(d)?;
        if x >> 32 != 0 {
            return None;
        }
    }
    if !unsigned && x < 0x8000_0000 {
        Some((x as u32, CTID_INT32))
    } else {
        Some((x as u32, CTID_UINT32))
    }
}

/// C parser state.
struct CPState<'a, 'p> {
    /// Current character.
    c: i32,
    /// Current token.
    tok: CPToken,
    /// Token value.
    val: CPValue,
    /// String of identifier/keyword/string token.
    str: Vec<u8>,
    /// C type table entry of the last identifier (0 if none).
    ct: CTypeID,
    src: &'a [u8],
    /// Current position in input buffer.
    p: usize,
    /// String buffer for tokens.
    sb: Vec<u8>,
    cts: &'a mut CTState,
    params: Option<&'a [CParam<'p>]>,
    param: usize,
    linenumber: i32,
    depth: i32,
    tmask: u32,
    mode: u32,
    packstack: [u8; CPARSE_MAX_PACKSTACK],
    curpack: usize,
}

impl<'a, 'p> CPState<'a, 'p> {
    fn new(
        cts: &'a mut CTState,
        src: &'a [u8],
        mode: u32,
        params: Option<&'a [CParam<'p>]>,
    ) -> Self {
        CPState {
            c: 0,
            tok: 0,
            val: CPValue::default(),
            str: Vec::new(),
            ct: 0,
            src,
            p: 0,
            sb: Vec::new(),
            cts,
            params,
            param: 0,
            linenumber: 1,
            depth: 0,
            tmask: CPNS_DEFAULT,
            mode,
            packstack: [255, 0, 0, 0, 0, 0, 0],
            curpack: 0,
        }
    }

    fn run(mut self) -> PResult<CTypeID> {
        self.get();
        self.tok = 0;
        self.tmask = CPNS_DEFAULT;
        self.next()?;
        if self.mode & CPARSE_MODE_MULTI != 0 {
            self.decl_multi()?;
        } else {
            self.decl_single()?;
        }
        if let Some(params) = self.params
            && self.param != params.len()
        {
            return Err(self.err(errmsg::FFI_NUMPARAM));
        }
        debug_assert_eq!(self.depth, 0, "unbalanced cparser declaration depth");
        Ok(self.val.id)
    }

    // -- Errors -----------------------------------------------------------

    fn tok2str(tok: CPToken) -> Vec<u8> {
        debug_assert!(tok < CTOK_FIRSTDECL, "bad CPToken {tok}");
        if tok > CTOK_OFS {
            CTOKNAMES[(tok - CTOK_OFS - 1) as usize].as_bytes().to_vec()
        } else if !char_iscntrl(tok) {
            vec![tok as u8]
        } else {
            format!("char({tok})").into_bytes()
        }
    }

    fn errmsg(&mut self, tok: CPToken, msg: &[u8]) -> FfiError {
        let tokstr = if tok == 0 {
            None
        } else if tok == CTOK_IDENT
            || tok == CTOK_INTEGER
            || tok == CTOK_STRING
            || tok >= CTOK_FIRSTDECL
        {
            if self.sb.is_empty() {
                self.sb.push(b'$');
            }
            let end = self
                .sb
                .iter()
                .position(|&b| b == 0)
                .unwrap_or(self.sb.len());
            Some(self.sb[..end].to_vec())
        } else {
            Some(Self::tok2str(tok))
        };
        let mut m = msg.to_vec();
        if let Some(ts) = tokstr {
            m.extend_from_slice(b" near '");
            m.extend_from_slice(&ts);
            m.push(b'\'');
        }
        if self.linenumber > 1 {
            m.extend_from_slice(format!(" at line {}", self.linenumber).as_bytes());
        }
        FfiError::Caller(m)
    }

    fn err_token(&mut self, tok: CPToken) -> FfiError {
        let mut msg = vec![b'\''];
        msg.extend_from_slice(&Self::tok2str(tok));
        msg.extend_from_slice(b"' expected");
        let cur = self.tok;
        self.errmsg(cur, &msg)
    }

    fn err_badidx(&mut self, id: CTypeID) -> FfiError {
        let mut msg = vec![b'\''];
        msg.extend_from_slice(&self.cts.repr(id, None));
        msg.extend_from_slice(b"' cannot be indexed");
        self.errmsg(0, &msg)
    }

    fn err_named(&mut self, prefix: &str, name: &[u8], suffix: &str) -> FfiError {
        let mut msg = prefix.as_bytes().to_vec();
        msg.push(b'\'');
        let end = name.iter().position(|&b| b == 0).unwrap_or(name.len());
        msg.extend_from_slice(&name[..end]);
        msg.push(b'\'');
        msg.extend_from_slice(suffix.as_bytes());
        self.errmsg(0, &msg)
    }

    fn err_redef(&mut self, id: CTypeID) -> FfiError {
        let name = self.cts.name_of(id).unwrap_or_default().to_vec();
        self.err_named("attempt to redefine ", &name, "")
    }

    fn err(&mut self, em: &str) -> FfiError {
        self.errmsg(0, em.as_bytes())
    }

    // -- Lexer ------------------------------------------------------------

    #[inline]
    fn rawpeek(&self) -> i32 {
        self.src.get(self.p).copied().unwrap_or(0) as i32
    }

    /// Get next character.
    #[inline]
    fn get(&mut self) -> i32 {
        self.c = self.rawpeek();
        self.p += 1;
        if self.c != b'\\' as i32 {
            return self.c;
        }
        self.get_bs()
    }

    /// Transparently skip backslash-escaped line breaks.
    fn get_bs(&mut self) -> i32 {
        let c = self.rawpeek();
        if !iseol(c) {
            return self.c;
        }
        self.p += 1;
        let c2 = self.rawpeek();
        if iseol(c2) && c2 != c {
            self.p += 1;
        }
        self.linenumber += 1;
        self.get()
    }

    #[inline]
    fn save(&mut self, c: i32) {
        self.sb.push(c as u8);
    }

    /// Skip line break. Handles "\n", "\r", "\r\n" or "\n\r".
    fn newline(&mut self) {
        let c = self.rawpeek();
        if iseol(c) && c != self.c {
            self.p += 1;
        }
        self.linenumber += 1;
    }

    /// Parse number literal. Only handles int32_t/uint32_t right now.
    fn number(&mut self) -> PResult<CPToken> {
        loop {
            self.save(self.c);
            if !char_isident(self.get()) {
                break;
            }
        }
        match scan_c_integer(&self.sb) {
            Some((v, id)) => {
                self.val.id = id;
                self.val.u32 = v;
            }
            None => {
                if self.mode & CPARSE_MODE_SKIP == 0 {
                    return Err(self.errmsg(CTOK_INTEGER, errmsg::XNUMBER.as_bytes()));
                }
                self.val.u32 = 0;
            }
        }
        Ok(CTOK_INTEGER)
    }

    /// Parse identifier or keyword.
    fn ident(&mut self) -> CPToken {
        loop {
            self.save(self.c);
            if !char_isident(self.get()) {
                break;
            }
        }
        self.str.clear();
        self.str.extend_from_slice(&self.sb);
        self.val.id = self.cts.getname(&self.str, self.tmask);
        self.ct = self.val.id;
        let info = self.cts.get(self.ct).info;
        if ctype_type(info) == CT_KW {
            return ctype_cid(info) as CPToken;
        }
        CTOK_IDENT
    }

    /// Parse parameter.
    fn param(&mut self) -> PResult<CPToken> {
        let c = self.get();
        if char_isident(c) || c == t(b'$') {
            // Reserve $xyz for future extensions.
            return Err(self.errmsg(c, errmsg::XSYNTAX.as_bytes()));
        }
        let o = match self.params {
            Some(params) if self.param < params.len() => params[self.param],
            _ => return Err(self.err(errmsg::FFI_NUMPARAM)),
        };
        self.param += 1;
        match o {
            CParam::Str(s) => {
                self.str.clear();
                self.str.extend_from_slice(s);
                self.val.id = 0;
                self.ct = 0;
                Ok(CTOK_IDENT)
            }
            CParam::Num(n) => {
                self.val.set_i32(n);
                self.val.id = CTID_INT32;
                Ok(CTOK_INTEGER)
            }
            CParam::CType(id) => {
                self.val.id = id;
                Ok(t(b'$'))
            }
            CParam::Other(narg) => Err(FfiError::ArgType {
                narg,
                expected: "type parameter",
            }),
        }
    }

    /// Parse string or character constant.
    fn string(&mut self) -> PResult<CPToken> {
        let delim = self.c;
        self.get();
        while self.c != delim {
            let mut c = self.c;
            if c == 0 {
                return Err(self.errmsg(CTOK_EOF, errmsg::XSTR.as_bytes()));
            }
            if c == t(b'\\') {
                c = self.get();
                match c as u8 {
                    0 => return Err(self.errmsg(CTOK_EOF, errmsg::XSTR.as_bytes())),
                    b'a' => c = 7,
                    b'b' => c = 8,
                    b'f' => c = 12,
                    b'n' => c = 10,
                    b'r' => c = 13,
                    b't' => c = 9,
                    b'v' => c = 11,
                    b'e' => c = 27,
                    b'x' => {
                        c = 0;
                        while char_isxdigit(self.get()) {
                            let d = if char_isdigit(self.c) {
                                self.c - t(b'0')
                            } else {
                                (self.c & 15) + 9
                            };
                            c = (c << 4).wrapping_add(d);
                        }
                        self.save(c & 0xff);
                        continue;
                    }
                    _ => {
                        if char_isdigit(c) {
                            c -= t(b'0');
                            if char_isdigit(self.get()) {
                                c = c * 8 + (self.c - t(b'0'));
                                if char_isdigit(self.get()) {
                                    c = c * 8 + (self.c - t(b'0'));
                                    self.get();
                                }
                            }
                            self.save(c & 0xff);
                            continue;
                        }
                    }
                }
            }
            self.save(c);
            self.get();
        }
        self.get();
        if delim == t(b'"') {
            self.str.clear();
            self.str.extend_from_slice(&self.sb);
            Ok(CTOK_STRING)
        } else {
            if self.sb.len() != 1 {
                return Err(self.err_token(t(b'\'')));
            }
            self.val.set_i32(self.sb[0] as i8 as i32);
            self.val.id = CTID_INT32;
            Ok(CTOK_INTEGER)
        }
    }

    /// Skip C comment.
    fn comment_c(&mut self) {
        loop {
            if self.get() == t(b'*') {
                loop {
                    if self.get() == t(b'/') {
                        self.get();
                        return;
                    }
                    if self.c != t(b'*') {
                        break;
                    }
                }
            }
            if iseol(self.c) {
                self.newline();
            }
            if self.c == 0 {
                break;
            }
        }
    }

    /// Skip C++ comment.
    fn comment_cpp(&mut self) {
        while !iseol(self.get()) && self.c != 0 {}
    }

    /// Lexical scanner for C. Only a minimal subset is implemented.
    fn next_(&mut self) -> PResult<CPToken> {
        self.sb.clear();
        loop {
            if char_isident(self.c) {
                return if char_isdigit(self.c) {
                    self.number()
                } else {
                    Ok(self.ident())
                };
            }
            let c = self.c as u8;
            match c {
                b'\n' | b'\r' | b' ' | b'\t' | 0x0b | 0x0c => {
                    if c == b'\n' || c == b'\r' {
                        self.newline();
                    }
                    self.get();
                }
                b'"' | b'\'' => return self.string(),
                b'/' => {
                    if self.get() == t(b'*') {
                        self.comment_c();
                    } else if self.c == t(b'/') {
                        self.comment_cpp();
                    } else {
                        return Ok(t(b'/'));
                    }
                }
                b'|' => {
                    if self.get() != t(b'|') {
                        return Ok(t(b'|'));
                    }
                    self.get();
                    return Ok(CTOK_OROR);
                }
                b'&' => {
                    if self.get() != t(b'&') {
                        return Ok(t(b'&'));
                    }
                    self.get();
                    return Ok(CTOK_ANDAND);
                }
                b'=' => {
                    if self.get() != t(b'=') {
                        return Ok(t(b'='));
                    }
                    self.get();
                    return Ok(CTOK_EQ);
                }
                b'!' => {
                    if self.get() != t(b'=') {
                        return Ok(t(b'!'));
                    }
                    self.get();
                    return Ok(CTOK_NE);
                }
                b'<' => {
                    if self.get() == t(b'=') {
                        self.get();
                        return Ok(CTOK_LE);
                    } else if self.c == t(b'<') {
                        self.get();
                        return Ok(CTOK_SHL);
                    }
                    return Ok(t(b'<'));
                }
                b'>' => {
                    if self.get() == t(b'=') {
                        self.get();
                        return Ok(CTOK_GE);
                    } else if self.c == t(b'>') {
                        self.get();
                        return Ok(CTOK_SHR);
                    }
                    return Ok(t(b'>'));
                }
                b'-' => {
                    if self.get() != t(b'>') {
                        return Ok(t(b'-'));
                    }
                    self.get();
                    return Ok(CTOK_DEREF);
                }
                b'$' => return self.param(),
                0 => return Ok(CTOK_EOF),
                _ => {
                    let c = self.c;
                    self.get();
                    return Ok(c);
                }
            }
        }
    }

    fn next(&mut self) -> PResult<CPToken> {
        self.tok = self.next_()?;
        Ok(self.tok)
    }

    // -- Parser helpers -----------------------------------------------------

    /// Check and consume optional token.
    fn opt(&mut self, tok: CPToken) -> PResult<bool> {
        if self.tok == tok {
            self.next()?;
            return Ok(true);
        }
        Ok(false)
    }

    /// Check and consume token.
    fn check(&mut self, tok: CPToken) -> PResult<()> {
        if self.tok != tok {
            return Err(self.err_token(tok));
        }
        self.next()?;
        Ok(())
    }

    #[inline]
    fn ctinfo_cur(&self) -> CTInfo {
        self.cts.get(self.ct).info
    }

    /// Check if the next token may start a type declaration.
    fn istypedecl(&self) -> bool {
        (self.tok >= CTOK_FIRSTDECL && self.tok <= CTOK_LASTDECL)
            || (self.tok == CTOK_IDENT && ctype_istypedef(self.ctinfo_cur()))
            || self.tok == t(b'$')
    }

    // -- Constant expression evaluator ---------------------------------------

    /// Parse comma separated expression and return last result.
    fn expr_comma(&mut self, k: &mut CPValue) -> PResult<()> {
        loop {
            self.expr_sub(k, 0)?;
            if !self.opt(t(b','))? {
                return Ok(());
            }
        }
    }

    /// Parse sizeof/alignof operator.
    fn expr_sizeof(&mut self, k: &mut CPValue, wantsz: bool) -> PResult<()> {
        if self.opt(t(b'('))? {
            if self.istypedecl() {
                k.id = self.decl_abstract()?;
            } else {
                self.expr_comma(k)?;
            }
            self.check(t(b')'))?;
        } else {
            self.expr_unary(k)?;
        }
        let (info, sz) = self.cts.info_raw(k.id);
        if wantsz {
            if sz != CTSIZE_INVALID {
                k.u32 = sz;
            } else if k.id != CTID_A_CCHAR {
                // Special case for sizeof("string").
                return Err(self.err(errmsg::FFI_INVSIZE));
            }
        } else {
            k.u32 = 1u32 << ctype_align(info);
        }
        k.id = CTID_UINT32; // Really size_t.
        Ok(())
    }

    /// Parse prefix operators.
    fn expr_prefix(&mut self, k: &mut CPValue) -> PResult<()> {
        if self.tok == CTOK_INTEGER {
            *k = self.val;
            self.next()?;
        } else if self.opt(t(b'+'))? {
            self.expr_unary(k)?; // Nothing to do (well, integer promotion).
        } else if self.opt(t(b'-'))? {
            self.expr_unary(k)?;
            k.u32 = (!k.u32).wrapping_add(1);
        } else if self.opt(t(b'~'))? {
            self.expr_unary(k)?;
            k.u32 = !k.u32;
        } else if self.opt(t(b'!'))? {
            self.expr_unary(k)?;
            k.set_i32((k.i32() == 0) as i32);
            k.id = CTID_INT32;
        } else if self.opt(t(b'('))? {
            if self.istypedecl() {
                // Cast operator.
                let id = self.decl_abstract()?;
                self.check(t(b')'))?;
                self.expr_unary(k)?;
                k.id = id; // No conversion performed.
            } else {
                // Sub-expression.
                self.expr_comma(k)?;
                self.check(t(b')'))?;
            }
        } else if self.opt(t(b'*'))? {
            // Indirection.
            self.expr_unary(k)?;
            let ct = self.cts.rawref(k.id);
            if !ctype_ispointer(self.cts.get(ct).info) {
                return Err(self.err_badidx(ct));
            }
            k.u32 = 0;
            k.id = self.cts.child(ct);
        } else if self.opt(t(b'&'))? {
            // Address operator.
            self.expr_unary(k)?;
            k.id = self
                .cts
                .intern(ctinfo(CT_PTR, CTALIGN_PTR + k.id), CTSIZE_PTR)?;
        } else if self.opt(CTOK_SIZEOF)? {
            self.expr_sizeof(k, true)?;
        } else if self.opt(CTOK_ALIGNOF)? {
            self.expr_sizeof(k, false)?;
        } else if self.tok == CTOK_IDENT {
            let ct = *self.cts.get(self.ct);
            match ctype_type(ct.info) {
                CT_CONSTVAL => {
                    k.u32 = ct.size;
                    k.id = ctype_cid(ct.info);
                }
                CT_EXTERN => {
                    k.u32 = self.val.id;
                    k.id = ctype_cid(ct.info);
                }
                CT_FUNC => {
                    k.u32 = self.val.id;
                    k.id = self.val.id;
                }
                _ => return Err(self.errmsg(self.tok, errmsg::XSYMBOL.as_bytes())),
            }
            self.next()?;
        } else if self.tok == CTOK_STRING {
            let mut sz = self.str.len() as CTSize;
            while self.next()? == CTOK_STRING {
                sz = sz.wrapping_add(self.str.len() as CTSize);
            }
            k.u32 = sz.wrapping_add(1);
            k.id = CTID_A_CCHAR;
        } else {
            return Err(self.errmsg(self.tok, errmsg::XSYMBOL.as_bytes()));
        }
        Ok(())
    }

    /// Parse postfix operators.
    fn expr_postfix(&mut self, k: &mut CPValue) -> PResult<()> {
        loop {
            let ct = if self.opt(t(b'['))? {
                // Array/pointer index.
                let mut k2 = CPValue::default();
                self.expr_comma(&mut k2)?;
                let mut c = self.cts.rawref(k.id);
                if !ctype_ispointer(self.cts.get(c).info) {
                    c = self.cts.rawref(k2.id);
                    if !ctype_ispointer(self.cts.get(c).info) {
                        return Err(self.err_badidx(c));
                    }
                }
                self.check(t(b']'))?;
                k.u32 = 0;
                c
            } else if self.tok == t(b'.') || self.tok == CTOK_DEREF {
                // Struct deref.
                let mut c = self.cts.rawref(k.id);
                if self.tok == CTOK_DEREF {
                    if !ctype_ispointer(self.cts.get(c).info) {
                        return Err(self.err_badidx(c));
                    }
                    c = self.cts.rawref(self.cts.child(c));
                }
                self.next()?;
                if self.tok != CTOK_IDENT {
                    return Err(self.err_token(CTOK_IDENT));
                }
                let sct = *self.cts.get(c);
                let fct = if ctype_isstruct(sct.info) && sct.size != CTSIZE_INVALID {
                    self.cts
                        .getfield(c, &self.str)
                        .filter(|&(f, _)| !ctype_isbitfield(self.cts.get(f).info))
                } else {
                    None
                };
                let Some((f, _)) = fct else {
                    let mut msg = vec![b'\''];
                    msg.extend_from_slice(&self.cts.repr(c, None));
                    msg.extend_from_slice(b"' has no member named '");
                    let s = self.str.clone();
                    let end = s.iter().position(|&b| b == 0).unwrap_or(s.len());
                    msg.extend_from_slice(&s[..end]);
                    msg.push(b'\'');
                    return Err(self.errmsg(0, &msg));
                };
                let fi = *self.cts.get(f);
                k.u32 = if ctype_isconstval(fi.info) {
                    fi.size
                } else {
                    0
                };
                self.next()?;
                f
            } else {
                return Ok(());
            };
            k.id = self.cts.child(ct);
        }
    }

    /// Parse infix operators.
    fn expr_infix(&mut self, k: &mut CPValue, pri: i32) -> PResult<()> {
        let mut k2 = CPValue::default();
        loop {
            if pri <= 0 && self.opt(t(b'?'))? {
                let mut k3 = CPValue::default();
                self.expr_comma(&mut k2)?; // Right-associative.
                self.check(t(b':'))?;
                self.expr_sub(&mut k3, 0)?;
                k.u32 = if k.u32 != 0 { k2.u32 } else { k3.u32 };
                k.id = k2.id.max(k3.id);
                continue;
            }
            if pri <= 1 && self.opt(CTOK_OROR)? {
                self.expr_sub(&mut k2, 2)?;
                k.set_i32((k.u32 != 0 || k2.u32 != 0) as i32);
                k.id = CTID_INT32;
                continue;
            }
            if pri <= 2 && self.opt(CTOK_ANDAND)? {
                self.expr_sub(&mut k2, 3)?;
                k.set_i32((k.u32 != 0 && k2.u32 != 0) as i32);
                k.id = CTID_INT32;
                continue;
            }
            let arith = if pri <= 3 && self.opt(t(b'|'))? {
                self.expr_sub(&mut k2, 4)?;
                k.u32 |= k2.u32;
                true
            } else if pri <= 4 && self.opt(t(b'^'))? {
                self.expr_sub(&mut k2, 5)?;
                k.u32 ^= k2.u32;
                true
            } else if pri <= 5 && self.opt(t(b'&'))? {
                self.expr_sub(&mut k2, 6)?;
                k.u32 &= k2.u32;
                true
            } else {
                false
            };
            if arith {
                if k2.id > k.id {
                    k.id = k2.id; // Trivial promotion to unsigned.
                }
                continue;
            }
            if pri <= 6 {
                if self.opt(CTOK_EQ)? {
                    self.expr_sub(&mut k2, 7)?;
                    k.set_i32((k.u32 == k2.u32) as i32);
                    k.id = CTID_INT32;
                    continue;
                } else if self.opt(CTOK_NE)? {
                    self.expr_sub(&mut k2, 7)?;
                    k.set_i32((k.u32 != k2.u32) as i32);
                    k.id = CTID_INT32;
                    continue;
                }
            }
            if pri <= 7 {
                let op = if self.opt(t(b'<'))? {
                    1
                } else if self.opt(t(b'>'))? {
                    2
                } else if self.opt(CTOK_LE)? {
                    3
                } else if self.opt(CTOK_GE)? {
                    4
                } else {
                    0
                };
                if op != 0 {
                    self.expr_sub(&mut k2, 8)?;
                    let r = if k.id == CTID_INT32 && k2.id == CTID_INT32 {
                        let (a, b) = (k.i32(), k2.i32());
                        match op {
                            1 => a < b,
                            2 => a > b,
                            3 => a <= b,
                            _ => a >= b,
                        }
                    } else {
                        let (a, b) = (k.u32, k2.u32);
                        match op {
                            1 => a < b,
                            2 => a > b,
                            3 => a <= b,
                            _ => a >= b,
                        }
                    };
                    k.set_i32(r as i32);
                    k.id = CTID_INT32;
                    continue;
                }
            }
            if pri <= 8 {
                if self.opt(CTOK_SHL)? {
                    self.expr_sub(&mut k2, 9)?;
                    k.u32 = k.u32.wrapping_shl(k2.u32);
                    continue;
                } else if self.opt(CTOK_SHR)? {
                    self.expr_sub(&mut k2, 9)?;
                    if k.id == CTID_INT32 {
                        k.set_i32(k.i32().wrapping_shr(k2.u32));
                    } else {
                        k.u32 = k.u32.wrapping_shr(k2.u32);
                    }
                    continue;
                }
            }
            if pri <= 9 {
                let arith = if self.opt(t(b'+'))? {
                    self.expr_sub(&mut k2, 10)?;
                    k.u32 = k.u32.wrapping_add(k2.u32);
                    true
                } else if self.opt(t(b'-'))? {
                    self.expr_sub(&mut k2, 10)?;
                    k.u32 = k.u32.wrapping_sub(k2.u32);
                    true
                } else {
                    false
                };
                if arith {
                    if k2.id > k.id {
                        k.id = k2.id; // Trivial promotion to unsigned.
                    }
                    continue;
                }
            }
            if pri <= 10 {
                if self.opt(t(b'*'))? {
                    self.expr_unary(&mut k2)?;
                    k.u32 = k.u32.wrapping_mul(k2.u32);
                    if k2.id > k.id {
                        k.id = k2.id;
                    }
                    continue;
                }
                let op = if self.opt(t(b'/'))? {
                    1
                } else if self.opt(t(b'%'))? {
                    2
                } else {
                    0
                };
                if op != 0 {
                    self.expr_unary(&mut k2)?;
                    if k2.id > k.id {
                        k.id = k2.id; // Trivial promotion to unsigned.
                    }
                    if k2.u32 == 0 || (k.id == CTID_INT32 && k.u32 == 0x8000_0000 && k2.i32() == -1)
                    {
                        return Err(self.err(errmsg::BADVAL));
                    }
                    if k.id == CTID_INT32 {
                        let (a, b) = (k.i32(), k2.i32());
                        k.set_i32(if op == 1 { a / b } else { a % b });
                    } else {
                        let (a, b) = (k.u32, k2.u32);
                        k.u32 = if op == 1 { a / b } else { a % b };
                    }
                    continue;
                }
            }
            return Ok(());
        }
    }

    /// Parse and evaluate unary expression.
    fn expr_unary(&mut self, k: &mut CPValue) -> PResult<()> {
        self.depth += 1;
        if self.depth > CPARSE_MAX_DECLDEPTH {
            return Err(self.err(errmsg::XLEVELS));
        }
        self.expr_prefix(k)?;
        self.expr_postfix(k)?;
        self.depth -= 1;
        Ok(())
    }

    /// Parse and evaluate sub-expression.
    fn expr_sub(&mut self, k: &mut CPValue, pri: i32) -> PResult<()> {
        self.expr_unary(k)?;
        self.expr_infix(k, pri)
    }

    /// Parse constant integer expression.
    fn expr_kint(&mut self, k: &mut CPValue) -> PResult<()> {
        self.expr_sub(k, 0)?;
        let ct = self.cts.raw(k.id);
        if !ctype_isinteger(self.cts.get(ct).info) {
            return Err(self.err(errmsg::BADVAL));
        }
        Ok(())
    }

    /// Parse (non-negative) size expression.
    fn expr_ksize(&mut self) -> PResult<CTSize> {
        let mut k = CPValue::default();
        self.expr_kint(&mut k)?;
        if k.u32 >= 0x8000_0000 {
            return Err(self.err(errmsg::FFI_INVSIZE));
        }
        Ok(k.u32)
    }

    // -- Type declaration stack management -----------------------------------

    /// Add declaration element behind the insertion position.
    fn add(&mut self, decl: &mut CPDecl, info: CTInfo, size: CTSize) -> PResult<usize> {
        let top = decl.top;
        if top >= CPARSE_MAX_DECLSTACK {
            return Err(self.err(errmsg::XLEVELS));
        }
        let next = decl.stack[decl.pos].next;
        decl.stack[top] = DeclElem {
            info,
            size,
            sib: 0,
            next,
        };
        decl.stack[decl.pos].next = top;
        decl.top = top + 1;
        Ok(top)
    }

    /// Push declaration element before the insertion position.
    fn push(&mut self, decl: &mut CPDecl, info: CTInfo, size: CTSize) -> PResult<usize> {
        decl.pos = self.add(decl, info, size)?;
        Ok(decl.pos)
    }

    /// Push or merge attributes.
    fn push_attributes(&mut self, decl: &mut CPDecl) -> PResult<()> {
        let info = decl.stack[decl.pos].info;
        // Function attributes (calling conventions) only matter on x86.
        if !ctype_isfunc(info)
            && (decl.attr & CTFP_ALIGNED) != 0
            && (decl.mode & CPARSE_MODE_FIELD) == 0
        {
            self.push(
                decl,
                ctinfo(CT_ATTRIB, ctattrib(CTA_ALIGN)),
                ctype_align(decl.attr),
            )?;
        }
        Ok(())
    }

    /// Push unrolled type to declaration stack and merge qualifiers.
    fn push_type(&mut self, decl: &mut CPDecl, id: CTypeID) -> PResult<()> {
        let ct = *self.cts.get(id);
        let mut info = ct.info;
        let size = ct.size;
        match ctype_type(info) {
            CT_STRUCT | CT_ENUM => {
                // Don't copy unique types.
                self.push(decl, ctinfo(CT_TYPEDEF, id), 0)?;
                if decl.attr & CTF_QUAL != 0 {
                    // Push unmerged qualifiers.
                    let q = decl.attr & CTF_QUAL;
                    self.push(decl, ctinfo(CT_ATTRIB, ctattrib(CTA_QUAL)), q)?;
                    decl.attr &= !CTF_QUAL;
                }
            }
            CT_ATTRIB => {
                if ctype_isxattrib(info, CTA_QUAL) {
                    decl.attr &= !size; // Remove redundant qualifiers.
                }
                self.push_type(decl, ctype_cid(info))?; // Unroll.
                self.push(decl, info & !CTMASK_CID, size)?; // Copy type.
            }
            CT_ARRAY => {
                if ct.info & (CTF_VECTOR | CTF_COMPLEX) != 0 {
                    info |= decl.attr & CTF_QUAL;
                    decl.attr &= !CTF_QUAL;
                }
                self.push_type(decl, ctype_cid(info))?; // Unroll.
                self.push(decl, info & !CTMASK_CID, size)?; // Copy type.
                // Mark as already checked and sized.
                decl.stack[decl.pos].sib = 1;
            }
            CT_FUNC => {
                // Copy type, link parameters (shared).
                let idx = self.push(decl, info, size)?;
                decl.stack[idx].sib = ct.sib as CTypeID;
            }
            _ => {
                // Copy type, merge common qualifiers.
                self.push(decl, info | (decl.attr & CTF_QUAL), size)?;
                decl.attr &= !CTF_QUAL;
            }
        }
        Ok(())
    }

    /// Consume the declaration element chain and intern the C type.
    fn decl_intern(&mut self, decl: &CPDecl) -> PResult<CTypeID> {
        let mut id: CTypeID = 0;
        let mut idx = 0usize;
        let mut csize = CTSIZE_INVALID;
        let mut cinfo: CTInfo = 0;
        loop {
            let ct = decl.stack[idx];
            let mut info = ct.info;
            let mut size = ct.size;
            // The cid is already part of info for copies of pointers/functions.
            idx = ct.next;
            if ctype_istypedef(info) {
                debug_assert!(id == 0, "typedef not at toplevel");
                id = ctype_cid(info);
                // Always refetch info/size, since struct/enum may have been completed.
                let c = self.cts.get(id);
                cinfo = c.info;
                csize = c.size;
                debug_assert!(
                    ctype_isstruct(cinfo) || ctype_isenum(cinfo),
                    "typedef of bad type"
                );
            } else if ctype_isfunc(info) {
                // Intern function.
                if id != 0 {
                    let refinfo = self.cts.get(self.cts.raw(id)).info;
                    // Reject function or refarray return types.
                    if ctype_isfunc(refinfo) || ctype_isrefarray(refinfo) {
                        return Err(self.err(errmsg::FFI_INVTYPE));
                    }
                }
                // No intervening attributes allowed, skip forward.
                while idx != 0 {
                    let ctn = decl.stack[idx];
                    if !ctype_isattrib(ctn.info) {
                        break;
                    }
                    idx = ctn.next;
                }
                let fid = self.cts.new_type()?;
                csize = CTSIZE_INVALID;
                cinfo = info + id;
                let fct = self.cts.get_mut(fid);
                fct.info = cinfo;
                fct.size = size;
                fct.sib = ct.sib as CTypeID1;
                id = fid;
            } else if ctype_isattrib(info) {
                if ctype_isxattrib(info, CTA_QUAL) {
                    cinfo |= size;
                } else if ctype_isxattrib(info, CTA_ALIGN) {
                    cinfo = ctf_insert(cinfo, CTMASK_ALIGN, CTSHIFT_ALIGN, size);
                }
                id = self.cts.intern(info + id, size)?;
                // Inherit csize/cinfo from original type.
            } else {
                if ctype_isnum(info) {
                    // Handle mode/vector-size attributes.
                    debug_assert!(id == 0, "number not at toplevel");
                    if info & CTF_BOOL == 0 {
                        let msize = ctype_msize_p(decl.attr);
                        let mut vsize = ctype_vsize_p(decl.attr);
                        if msize != 0 && ((info & CTF_FP) == 0 || msize == 4 || msize == 8) {
                            let malign = lj_fls(msize).min(4); // Limit alignment.
                            info = ctf_insert(info, CTMASK_ALIGN, CTSHIFT_ALIGN, malign);
                            size = msize; // Override size via mode.
                        }
                        if vsize != 0 {
                            // Vector size set?
                            let esize = lj_fls(size);
                            if vsize >= esize {
                                // Intern the element type first.
                                id = self.cts.intern(info, size)?;
                                // Then create a vector (array) with vsize alignment.
                                size = 1u32 << vsize;
                                if vsize > 4 {
                                    vsize = 4; // Limit alignment.
                                }
                                if ctype_align(info) > vsize {
                                    vsize = ctype_align(info);
                                }
                                info = ctinfo(
                                    CT_ARRAY,
                                    (info & CTF_QUAL) + CTF_VECTOR + ctalign(vsize),
                                );
                            }
                        }
                    }
                } else if ctype_isptr(info) {
                    // Reject pointer/ref to ref.
                    if id != 0 && ctype_isref(self.cts.get(self.cts.raw(id)).info) {
                        return Err(self.err(errmsg::FFI_INVTYPE));
                    }
                    if ctype_isref(info) {
                        info &= !CTF_VOLATILE; // Refs are always const, never volatile.
                        // No intervening attributes allowed, skip forward.
                        while idx != 0 {
                            let ctn = decl.stack[idx];
                            if !ctype_isattrib(ctn.info) {
                                break;
                            }
                            idx = ctn.next;
                        }
                    }
                } else if ctype_isarray(info) {
                    // Check for valid array size etc.
                    if ct.sib == 0 {
                        // Only check/size arrays not copied by unroll.
                        if ctype_isref(cinfo) {
                            // Reject arrays of refs.
                            return Err(self.err(errmsg::FFI_INVTYPE));
                        }
                        // Reject VLS or unknown-sized types.
                        if ctype_isvltype(cinfo) || csize == CTSIZE_INVALID {
                            return Err(self.err(errmsg::FFI_INVSIZE));
                        }
                        // a[] and a[?] keep their invalid size.
                        if size != CTSIZE_INVALID {
                            let xsz = size as u64 * csize as u64;
                            if xsz >= 0x8000_0000 {
                                return Err(self.err(errmsg::FFI_INVSIZE));
                            }
                            size = xsz as CTSize;
                        }
                    }
                    if (cinfo & CTF_ALIGN) > (info & CTF_ALIGN) {
                        // Find max. align.
                        info = (info & !CTF_ALIGN) | (cinfo & CTF_ALIGN);
                    }
                    info |= cinfo & CTF_QUAL; // Inherit qual.
                } else {
                    debug_assert!(ctype_isvoid(info), "bad ctype {info:08x}");
                }
                csize = size;
                cinfo = info + id;
                id = self.cts.intern(info + id, size)?;
            }
            if idx == 0 {
                return Ok(id);
            }
        }
    }

    // -- C declaration parser ---------------------------------------------

    /// Parse constant initializer.
    fn decl_constinit(&mut self, mut ctypeid: CTypeID) -> PResult<CTypeID> {
        while ctype_isattrib(self.cts.get(ctypeid).info) {
            // Skip attributes.
            ctypeid = self.cts.child(ctypeid); // Update ID, too.
        }
        let ctt = *self.cts.get(ctypeid);
        let info = ctt.info;
        let size = ctt.size;
        if !ctype_isinteger(info) || (info & CTF_CONST) == 0 || size > 4 {
            return Err(self.err(errmsg::FFI_INVTYPE));
        }
        self.check(t(b'='))?;
        let mut k = CPValue::default();
        self.expr_sub(&mut k, 0)?;
        let constid = self.cts.new_type()?;
        let sh = 8 * (4 - size);
        let mut v = k.u32.wrapping_shl(sh);
        if info & CTF_UNSIGNED != 0 {
            v = v.wrapping_shr(sh);
        } else {
            v = (v as i32).wrapping_shr(sh) as u32;
        }
        let c = self.cts.get_mut(constid);
        c.info = ctinfo(CT_CONSTVAL, CTF_CONST | ctypeid);
        c.size = v;
        Ok(constid)
    }

    /// Parse size in parentheses as part of attribute.
    fn decl_sizeattr(&mut self) -> PResult<CTSize> {
        let oldtmask = self.tmask;
        self.tmask = CPNS_DEFAULT; // Required for expression evaluator.
        self.check(t(b'('))?;
        let sz = self.expr_ksize()?;
        self.tmask = oldtmask;
        self.check(t(b')'))?;
        Ok(sz)
    }

    /// Parse alignment attribute.
    fn decl_align(&mut self, decl: &mut CPDecl) -> PResult<()> {
        let mut al = 4; // Unspecified alignment is 16 bytes.
        if self.tok == t(b'(') {
            al = self.decl_sizeattr()?;
            al = if al != 0 { lj_fls(al) } else { 0 };
        }
        decl.attr = ctf_insert(decl.attr, CTMASK_ALIGN, CTSHIFT_ALIGN, al);
        decl.attr |= CTFP_ALIGNED;
        Ok(())
    }

    /// Parse GCC asm("name") redirect.
    fn decl_asm(&mut self, decl: &mut CPDecl) -> PResult<()> {
        self.next()?;
        self.check(t(b'('))?;
        if self.tok == CTOK_STRING {
            let mut s = self.str.clone();
            while self.next()? == CTOK_STRING {
                // lj_strfmt_pushf("%s%s"): C string semantics.
                if let Some(z) = s.iter().position(|&b| b == 0) {
                    s.truncate(z);
                }
                let end = self
                    .str
                    .iter()
                    .position(|&b| b == 0)
                    .unwrap_or(self.str.len());
                s.extend_from_slice(&self.str[..end]);
            }
            decl.redir = Some(s);
        }
        self.check(t(b')'))
    }

    /// Parse GCC __attribute__((mode(...))).
    fn decl_mode(&mut self, decl: &mut CPDecl) -> PResult<()> {
        self.check(t(b'('))?;
        if self.tok == CTOK_IDENT {
            let s = &self.str;
            // C string semantics: reading past the end yields NUL.
            let at = |i: usize| s.get(i).copied().unwrap_or(0) as i8 as i32;
            let mut i = 0;
            let mut vlen: u32 = 0;
            if at(0) == t(b'_') && at(1) == t(b'_') {
                i += 2;
            }
            if at(i) == t(b'V') {
                i += 1;
                vlen = (at(i) - t(b'0')) as u32;
                i += 1;
                if at(i) >= t(b'0') && at(i) <= t(b'9') {
                    vlen = vlen.wrapping_mul(10).wrapping_add((at(i) - t(b'0')) as u32);
                    i += 1;
                }
            }
            let sz: u32 = match at(i) as u8 {
                b'Q' => 1,
                b'H' => 2,
                b'S' => 4,
                b'D' => 8,
                b'T' => 16,
                b'O' => 32,
                _ => 0,
            };
            i += 1;
            if sz != 0 && (at(i) == t(b'I') || at(i) == t(b'F')) {
                decl.attr = ctf_insert(decl.attr, CTMASK_MSIZEP, CTSHIFT_MSIZEP, sz);
                if vlen != 0 {
                    let v = vlen.wrapping_mul(sz);
                    let f = if v != 0 { lj_fls(v) } else { 0 };
                    decl.attr = ctf_insert(decl.attr, CTMASK_VSIZEP, CTSHIFT_VSIZEP, f);
                }
            }
            self.next()?;
        }
        self.check(t(b')'))
    }

    /// Parse GCC __attribute__((...)).
    fn decl_gccattribute(&mut self, decl: &mut CPDecl) -> PResult<()> {
        self.next()?;
        self.check(t(b'('))?;
        self.check(t(b'('))?;
        while self.tok != t(b')') {
            let skip;
            if self.tok == CTOK_IDENT {
                let attrstr = self.str.clone();
                self.next()?;
                match cparse_case(
                    &attrstr,
                    &[
                        "aligned",
                        "__aligned__",
                        "packed",
                        "__packed__",
                        "mode",
                        "__mode__",
                        "vector_size",
                        "__vector_size__",
                    ],
                ) {
                    Some(0 | 1) => {
                        self.decl_align(decl)?;
                        skip = false;
                    }
                    Some(2 | 3) => {
                        decl.attr |= CTFP_PACKED;
                        skip = false;
                    }
                    Some(4 | 5) => {
                        self.decl_mode(decl)?;
                        skip = false;
                    }
                    Some(6 | 7) => {
                        let vsize = self.decl_sizeattr()?;
                        if vsize != 0 {
                            decl.attr =
                                ctf_insert(decl.attr, CTMASK_VSIZEP, CTSHIFT_VSIZEP, lj_fls(vsize));
                        }
                        skip = false;
                    }
                    _ => skip = true, // Skip all other attributes.
                }
            } else if self.tok >= CTOK_FIRSTDECL {
                // For __attribute((const)) etc.
                self.next()?;
                skip = true;
            } else {
                break;
            }
            if skip && self.opt(t(b'('))? {
                while self.tok != t(b')') && self.tok != CTOK_EOF {
                    self.next()?;
                }
                self.check(t(b')'))?;
            }
            if !self.opt(t(b','))? {
                break;
            }
        }
        self.check(t(b')'))?;
        self.check(t(b')'))
    }

    /// Parse MSVC __declspec(...).
    fn decl_msvcattribute(&mut self, decl: &mut CPDecl) -> PResult<()> {
        self.next()?;
        self.check(t(b'('))?;
        while self.tok == CTOK_IDENT {
            let is_align = self.str == b"align";
            self.next()?;
            if is_align {
                self.decl_align(decl)?;
            } else if self.opt(t(b'('))? {
                // Ignore all other attributes.
                while self.tok != t(b')') && self.tok != CTOK_EOF {
                    self.next()?;
                }
                self.check(t(b')'))?;
            }
        }
        self.check(t(b')'))
    }

    /// Parse declaration attributes (and common qualifiers).
    fn decl_attributes(&mut self, decl: &mut CPDecl) -> PResult<()> {
        loop {
            match self.tok {
                CTOK_CONST => decl.attr |= CTF_CONST,
                CTOK_VOLATILE => decl.attr |= CTF_VOLATILE,
                CTOK_RESTRICT | CTOK_EXTENSION => {} // Ignore.
                CTOK_ATTRIBUTE => {
                    self.decl_gccattribute(decl)?;
                    continue;
                }
                CTOK_ASM => {
                    self.decl_asm(decl)?;
                    continue;
                }
                CTOK_DECLSPEC => {
                    self.decl_msvcattribute(decl)?;
                    continue;
                }
                CTOK_CCDECL => {} // Calling conventions only matter on x86.
                CTOK_PTRSZ => {
                    let sz = self.cts.get(self.ct).size;
                    decl.attr = ctf_insert(decl.attr, CTMASK_MSIZEP, CTSHIFT_MSIZEP, sz);
                }
                _ => return Ok(()),
            }
            self.next()?;
        }
    }

    /// Parse struct/union/enum name.
    fn struct_name(&mut self, sdecl: &mut CPDecl, info: CTInfo) -> PResult<CTypeID> {
        let sid;
        self.tmask = CPNS_STRUCT;
        self.next()?;
        self.decl_attributes(sdecl)?;
        self.tmask = CPNS_DEFAULT;
        if self.tok != t(b'{') {
            if self.tok != CTOK_IDENT {
                return Err(self.err_token(CTOK_IDENT));
            }
            if self.val.id != 0 {
                // Name of existing struct/union/enum.
                sid = self.val.id;
                if (self.cts.get(sid).info ^ info) & (CTMASK_NUM | CTF_UNION) != 0 {
                    // Wrong type.
                    return Err(self.err_redef(sid));
                }
            } else {
                // Create named, incomplete struct/union/enum.
                if self.mode & CPARSE_MODE_NOIMPLICIT != 0 {
                    let s = self.str.clone();
                    return Err(self.err_named("undeclared or implicit tag ", &s, ""));
                }
                sid = self.cts.new_type()?;
                let c = self.cts.get_mut(sid);
                c.info = info;
                c.size = CTSIZE_INVALID;
                let s = std::mem::take(&mut self.str);
                self.cts.set_name(sid, &s);
                self.str = s;
                self.cts.addname(sid);
            }
            self.next()?;
        } else {
            // Create anonymous, incomplete struct/union/enum.
            sid = self.cts.new_type()?;
            let c = self.cts.get_mut(sid);
            c.info = info;
            c.size = CTSIZE_INVALID;
        }
        if self.tok == t(b'{') {
            let c = *self.cts.get(sid);
            if c.size != CTSIZE_INVALID || c.sib != 0 {
                return Err(self.err_redef(sid));
            }
            self.cts.get_mut(sid).sib = 1; // Indicate the type is currently being defined.
        }
        Ok(sid)
    }

    /// Layout struct/union fields.
    fn struct_layout(&mut self, sid: CTypeID, sattr: CTInfo) -> PResult<()> {
        let mut bofs: CTSize = 0; // Bit offset.
        let mut bmaxofs: CTSize = 0; // Max. bit offset.
        let mut maxalign = ctype_align(sattr);
        let mut sinfo = self.cts.get(sid).info;
        let mut fieldid = self.cts.get(sid).sib as CTypeID;
        while fieldid != 0 {
            let mut ct = *self.cts.get(fieldid);
            let attr = ct.size; // Field declaration attributes (temp.).

            if ctype_isfield(ct.info) || (ctype_isxattrib(ct.info, CTA_SUBTYPE) && attr != 0) {
                let (info, mut sz) = self.cts.info(ctype_cid(ct.info));
                let mut csz = sz.wrapping_mul(8); // Container size (in bits).
                sinfo |= info & (CTF_QUAL | CTF_VLA); // Merge pseudo-qualifiers.

                // Check for size overflow and determine alignment.
                if sz >= 0x2000_0000 || bofs.wrapping_add(csz) < bofs || (info & CTF_VLA) != 0 {
                    if !(sz == CTSIZE_INVALID && ctype_isarray(info) && (sinfo & CTF_UNION) == 0) {
                        return Err(self.err(errmsg::FFI_INVSIZE));
                    }
                    csz = 0; // Treat a[] and a[?] as zero-sized.
                    sz = 0;
                }
                // The x86-64 ABI uses natural alignment for all fields.
                let mut align = ctype_align(info);
                if ((attr | sattr) & CTFP_PACKED) != 0
                    || ((attr & CTFP_ALIGNED) != 0 && ctype_align(attr) > align)
                {
                    align = ctype_align(attr);
                }
                let pack = self.packstack[self.curpack] as u32;
                if pack < align {
                    align = pack;
                }
                let mut bsz = ctype_bitcsz(ct.info); // Bitfield size (temp.).
                if align > maxalign && bsz != 0 {
                    maxalign = align;
                }
                let amask = (8u32 << align) - 1; // Alignment mask (bits).

                if bsz == CTBSZ_FIELD || !ctype_isfield(ct.info) {
                    bsz = csz; // Regular fields or subtypes always fill the container.
                    bofs = bofs.wrapping_add(amask) & !amask; // Start new aligned field.
                    ct.size = bofs >> 3; // Store field offset.
                    if ctype_isfield(ct.info) {
                        ct.info = ctinfo(CT_FIELD, ctype_cid(ct.info)) + ctalign(align);
                    }
                } else {
                    // Bitfield.
                    if bsz == 0
                        || (attr & CTFP_ALIGNED) != 0
                        || (((attr | sattr) & CTFP_PACKED) == 0 && (bofs & amask) + bsz > csz)
                    {
                        bofs = bofs.wrapping_add(amask) & !amask; // Start new aligned field.
                    }
                    // Prefer regular field over bitfield.
                    if bsz == csz && (bofs & amask) == 0 {
                        ct.info = ctinfo(CT_FIELD, ctype_cid(ct.info)) + ctalign(lj_fls(sz));
                        ct.size = bofs >> 3; // Store field offset.
                    } else {
                        if csz > amask + 1 && bsz <= amask + 1 {
                            csz = amask + 1; // Shrink container of packed bitfield.
                        }
                        ct.info = ctinfo(
                            CT_BITFIELD,
                            (info & (CTF_QUAL | CTF_UNSIGNED | CTF_BOOL))
                                + (csz << (CTSHIFT_BITCSZ - 3))
                                + (bsz << CTSHIFT_BITBSZ),
                        );
                        ct.info += (bofs & (csz - 1)) << CTSHIFT_BITPOS;
                        ct.size = (bofs & !(csz - 1)) >> 3; // Store container offset.
                    }
                }

                // Determine next offset or max. offset.
                if sinfo & CTF_UNION != 0 {
                    if bsz > bmaxofs {
                        bmaxofs = bsz;
                    }
                } else {
                    bofs = bofs.wrapping_add(bsz);
                }
                *self.cts.get_mut(fieldid) = ct;
            } // All other fields in the chain are already set up.

            fieldid = ct.sib as CTypeID;
        }

        // Complete struct/union.
        let sct = self.cts.get_mut(sid);
        sct.info = sinfo + ctalign(maxalign);
        let bofs = if sinfo & CTF_UNION != 0 {
            bmaxofs
        } else {
            bofs
        };
        let maxalign = (8u32 << maxalign) - 1;
        sct.size = (bofs.wrapping_add(maxalign) & !maxalign) >> 3;
        Ok(())
    }

    /// Parse struct/union declaration.
    fn decl_struct(&mut self, sdecl: &mut CPDecl, sinfo: CTInfo) -> PResult<CTypeID> {
        let sid = self.struct_name(sdecl, sinfo)?;
        if self.opt(t(b'{'))? {
            // Struct/union definition.
            let mut lastid = sid;
            let mut lastdecl = false;
            while self.tok != t(b'}') {
                let mut decl = CPDecl::new();
                let scl = self.decl_spec(&mut decl, CDF_STATIC)?;
                decl.mode = if scl != 0 {
                    CPARSE_MODE_DIRECT
                } else {
                    CPARSE_MODE_DIRECT | CPARSE_MODE_ABSTRACT | CPARSE_MODE_FIELD
                };

                loop {
                    if lastdecl {
                        return Err(self.err_token(t(b'}')));
                    }

                    // Parse field declarator.
                    decl.bits = CTSIZE_INVALID;
                    self.declarator(&mut decl)?;
                    let ctypeid = self.decl_intern(&decl)?;

                    if scl & CDF_STATIC != 0 {
                        // Static constant in struct namespace.
                        let fieldid = self.decl_constinit(ctypeid)?;
                        self.cts.get_mut(lastid).sib = fieldid as CTypeID1;
                        lastid = fieldid;
                        if let Some(name) = &decl.name {
                            self.cts.set_name(fieldid, name);
                        }
                    } else {
                        let mut bsz = CTBSZ_FIELD; // Temp. for layout phase.
                        let fieldid = self.cts.new_type()?; // Do this first.
                        let tct = *self.cts.get(self.cts.raw(ctypeid));
                        let mut subtype = false;

                        if decl.bits == CTSIZE_INVALID {
                            // Regular field.
                            if ctype_isarray(tct.info) && tct.size == CTSIZE_INVALID {
                                lastdecl = true; // a[] or a[?] must be the last declared field.
                            }

                            // Accept transparent struct/union/enum.
                            if decl.name.is_none() {
                                if !((ctype_isstruct(tct.info) && (tct.info & CTF_VLA) == 0)
                                    || ctype_isenum(tct.info))
                                {
                                    return Err(self.err_token(CTOK_IDENT));
                                }
                                let ct = self.cts.get_mut(fieldid);
                                ct.info = ctinfo(CT_ATTRIB, ctattrib(CTA_SUBTYPE) + ctypeid);
                                ct.size = if ctype_isstruct(tct.info) {
                                    decl.attr | 0x8000_0000
                                } else {
                                    0
                                }; // For layout phase.
                                subtype = true;
                            }
                        } else {
                            // Bitfield.
                            bsz = decl.bits;
                            let maxbits = if tct.info & CTF_BOOL != 0 {
                                1
                            } else {
                                tct.size.wrapping_mul(8)
                            };
                            if !ctype_isinteger_or_bool(tct.info)
                                || (bsz == 0 && decl.name.is_some())
                                || tct.size.wrapping_mul(8) > CTBSZ_MAX
                                || bsz > maxbits
                            {
                                return Err(self.errmsg(t(b':'), errmsg::BADVAL.as_bytes()));
                            }
                        }

                        if !subtype {
                            // Create temporary field for layout phase.
                            let ct = self.cts.get_mut(fieldid);
                            ct.info = ctinfo(CT_FIELD, ctypeid + (bsz << CTSHIFT_BITCSZ));
                            ct.size = decl.attr;
                            if let Some(name) = &decl.name {
                                self.cts.set_name(fieldid, name);
                            }
                        }

                        self.cts.get_mut(lastid).sib = fieldid as CTypeID1;
                        lastid = fieldid;
                    }
                    if !self.opt(t(b','))? {
                        break;
                    }
                    decl.reset();
                }
                self.check(t(b';'))?;
            }
            self.check(t(b'}'))?;
            self.cts.get_mut(lastid).sib = 0; // Drop sib = 1 for empty structs.
            self.decl_attributes(sdecl)?; // Layout phase needs postfix attributes.
            self.struct_layout(sid, sdecl.attr)?;
        }
        Ok(sid)
    }

    /// Parse enum declaration.
    fn decl_enum(&mut self, sdecl: &mut CPDecl) -> PResult<CTypeID> {
        let eid = self.struct_name(sdecl, ctinfo(CT_ENUM, CTID_VOID))?;
        let mut einfo = ctinfo(CT_ENUM, ctalign(2) + CTID_UINT32);
        let esize = 4; // Only 32 bit enums are supported.
        if self.opt(t(b'{'))? {
            // Enum definition.
            let mut k = CPValue {
                u32: 0,
                id: CTID_INT32,
            };
            let mut lastid = eid;
            loop {
                let name = self.str.clone();
                if self.tok != CTOK_IDENT {
                    return Err(self.err_token(CTOK_IDENT));
                }
                if self.val.id != 0 {
                    return Err(self.err_named("attempt to redefine ", &name, ""));
                }
                self.next()?;
                if self.opt(t(b'='))? {
                    self.expr_kint(&mut k)?;
                    if k.id == CTID_UINT32 {
                        // C99 says that enum constants are always (signed) integers.
                        // But since unsigned constants like 0x80000000 are quite common,
                        // those are left as uint32_t.
                        if k.i32() >= 0 {
                            k.id = CTID_INT32;
                        }
                    } else {
                        // OTOH it's common practice and even mandated by some ABIs
                        // that the enum type itself is unsigned, unless there are any
                        // negative constants.
                        k.id = CTID_INT32;
                        if k.i32() < 0 {
                            einfo = ctinfo(CT_ENUM, ctalign(2) + CTID_INT32);
                        }
                    }
                }
                // Add named enum constant.
                let constid = self.cts.new_type()?;
                self.cts.get_mut(lastid).sib = constid as CTypeID1;
                lastid = constid;
                self.cts.set_name(constid, &name);
                let ct = self.cts.get_mut(constid);
                ct.info = ctinfo(CT_CONSTVAL, CTF_CONST | k.id);
                ct.size = k.u32;
                k.u32 = k.u32.wrapping_add(1);
                if k.u32 == 0x8000_0000 {
                    k.id = CTID_UINT32;
                }
                self.cts.addname(constid);
                if !self.opt(t(b','))? {
                    break;
                }
                if self.tok == t(b'}') {
                    break; // Trailing ',' is ok.
                }
            }
            self.check(t(b'}'))?;
            // Complete enum.
            let e = self.cts.get_mut(eid);
            e.info = einfo;
            e.size = esize;
        }
        Ok(eid)
    }

    /// Parse declaration specifiers. Returns the storage class.
    fn decl_spec(&mut self, decl: &mut CPDecl, scl: u32) -> PResult<u32> {
        let mut cds: u32 = 0;
        let mut sz: u32 = 0;
        let mut tdef: CTypeID = 0;

        decl.mode = self.mode;
        decl.name = None;
        decl.redir = None;
        decl.attr = 0;
        decl.fattr = 0;
        decl.pos = 0;
        decl.top = 0;
        decl.stack[0].next = 0;

        loop {
            // Parse basic types.
            self.decl_attributes(decl)?;
            if self.tok >= CTOK_FIRSTDECL && self.tok <= CTOK_LASTDECLFLAG {
                let ksz = self.cts.get(self.ct).size;
                if ksz != 0 {
                    if sz != 0 {
                        break;
                    }
                    sz = ksz;
                }
                let cbit = 1u32 << (self.tok - CTOK_FIRSTDECL);
                cds = cds | cbit | ((cbit & cds & CDF_LONG) << 1);
                if self.tok >= CTOK_FIRSTSCL {
                    if scl & cbit == 0 {
                        return Err(self.errmsg(self.tok, errmsg::FFI_BADSCL.as_bytes()));
                    }
                } else if tdef != 0 {
                    break;
                }
                self.next()?;
                continue;
            }
            if sz != 0
                || tdef != 0
                || (cds & (CDF_SHORT | CDF_LONG | CDF_SIGNED | CDF_UNSIGNED | CDF_COMPLEX)) != 0
            {
                break;
            }
            match self.tok {
                CTOK_STRUCT => {
                    tdef = self.decl_struct(decl, ctinfo(CT_STRUCT, 0))?;
                    continue;
                }
                CTOK_UNION => {
                    tdef = self.decl_struct(decl, ctinfo(CT_STRUCT, CTF_UNION))?;
                    continue;
                }
                CTOK_ENUM => {
                    tdef = self.decl_enum(decl)?;
                    continue;
                }
                CTOK_IDENT => {
                    if ctype_istypedef(self.ctinfo_cur()) {
                        tdef = ctype_cid(self.ctinfo_cur()); // Get typedef.
                        self.next()?;
                        continue;
                    }
                }
                tok if tok == t(b'$') => {
                    tdef = self.val.id;
                    self.next()?;
                    continue;
                }
                _ => {}
            }
            break;
        }

        if cds & CDF_COMPLEX != 0 {
            // Use predefined complex types.
            tdef = if sz == 4 {
                CTID_COMPLEX_FLOAT
            } else {
                CTID_COMPLEX_DOUBLE
            };
        }

        if tdef != 0 {
            self.push_type(decl, tdef)?;
        } else if cds & CDF_VOID != 0 {
            self.push(decl, ctinfo(CT_VOID, decl.attr & CTF_QUAL), CTSIZE_INVALID)?;
            decl.attr &= !CTF_QUAL;
        } else {
            // Determine type info and size.
            let mut info = ctinfo(
                CT_NUM,
                if cds & CDF_UNSIGNED != 0 {
                    CTF_UNSIGNED
                } else {
                    0
                },
            );
            if cds & CDF_BOOL != 0 {
                if cds & !(CDF_SCL | CDF_BOOL | CDF_INT | CDF_SIGNED | CDF_UNSIGNED) != 0 {
                    return Err(self.err(errmsg::FFI_INVTYPE));
                }
                info |= CTF_BOOL;
                if cds & CDF_SIGNED == 0 {
                    info |= CTF_UNSIGNED;
                }
                if sz == 0 {
                    sz = 1;
                }
            } else if cds & CDF_FP != 0 {
                info = ctinfo(CT_NUM, CTF_FP);
                if cds & CDF_LONG != 0 {
                    sz = 16; // sizeof(long double)
                }
            } else if cds & CDF_CHAR != 0 {
                if (cds & (CDF_CHAR | CDF_SIGNED | CDF_UNSIGNED)) == CDF_CHAR {
                    info |= CTF_UCHAR; // Handle platforms where char is unsigned.
                }
            } else if cds & CDF_SHORT != 0 {
                sz = 2;
            } else if cds & CDF_LONGLONG != 0 {
                sz = 8;
            } else if cds & CDF_LONG != 0 {
                info |= CTF_LONG;
                sz = 8; // sizeof(long)
            } else if sz == 0 {
                if cds & (CDF_SIGNED | CDF_UNSIGNED) == 0 {
                    return Err(self.errmsg(self.tok, errmsg::FFI_DECLSPEC.as_bytes()));
                }
                sz = 4;
            }
            debug_assert!(sz != 0, "basic ctype with zero size");
            info += ctalign(lj_fls(sz)); // Use natural alignment.
            info += decl.attr & CTF_QUAL; // Merge qualifiers.
            self.push(decl, info, sz)?;
            decl.attr &= !CTF_QUAL;
        }
        decl.specpos = decl.pos;
        decl.specattr = decl.attr;
        decl.specfattr = decl.fattr;
        Ok(cds & CDF_SCL)
    }

    /// Parse array declaration.
    fn decl_array(&mut self, decl: &mut CPDecl) -> PResult<()> {
        let mut info = ctinfo(CT_ARRAY, 0);
        let mut nelem = CTSIZE_INVALID; // Default size for a[] or a[?].
        self.decl_attributes(decl)?;
        if self.opt(t(b'?'))? {
            info |= CTF_VLA; // Create variable-length array a[?].
        } else if self.tok != t(b']') {
            nelem = self.expr_ksize()?;
        }
        self.check(t(b']'))?;
        self.add(decl, info, nelem)?;
        Ok(())
    }

    /// Parse function declaration.
    fn decl_func(&mut self, fdecl: &mut CPDecl) -> PResult<()> {
        let mut nargs: CTSize = 0;
        let mut info = ctinfo(CT_FUNC, 0);
        let mut lastid: CTypeID = 0;
        let mut anchor: CTypeID = 0;
        if self.tok != t(b')') {
            loop {
                if self.opt(t(b'.'))? {
                    // Vararg function.
                    self.check(t(b'.'))?; // Workaround for the minimalistic lexer.
                    self.check(t(b'.'))?;
                    info |= CTF_VARARG;
                    break;
                }
                let mut decl = CPDecl::new();
                self.decl_spec(&mut decl, CDF_REGISTER)?;
                decl.mode = CPARSE_MODE_DIRECT | CPARSE_MODE_ABSTRACT;
                self.declarator(&mut decl)?;
                let mut ctypeid = self.decl_intern(&decl)?;
                let ct = *self.cts.get(self.cts.raw(ctypeid));
                if ctype_isvoid(ct.info) {
                    break;
                } else if ctype_isrefarray(ct.info) {
                    ctypeid = self
                        .cts
                        .intern(ctinfo(CT_PTR, CTALIGN_PTR | ctype_cid(ct.info)), CTSIZE_PTR)?;
                } else if ctype_isfunc(ct.info) {
                    ctypeid = self
                        .cts
                        .intern(ctinfo(CT_PTR, CTALIGN_PTR | ctypeid), CTSIZE_PTR)?;
                }
                // Add new parameter.
                let fieldid = self.cts.new_type()?;
                if anchor != 0 {
                    self.cts.get_mut(lastid).sib = fieldid as CTypeID1;
                } else {
                    anchor = fieldid;
                }
                lastid = fieldid;
                if let Some(name) = &decl.name {
                    self.cts.set_name(fieldid, name);
                }
                let f = self.cts.get_mut(fieldid);
                f.info = ctinfo(CT_FIELD, ctypeid);
                f.size = nargs;
                nargs += 1;
                if !self.opt(t(b','))? {
                    break;
                }
            }
        }
        self.check(t(b')'))?;
        if self.opt(t(b'{'))? {
            // Skip function definition.
            let mut level = 1;
            self.mode |= CPARSE_MODE_SKIP;
            loop {
                if self.tok == t(b'{') {
                    level += 1;
                } else if self.tok == t(b'}') {
                    level -= 1;
                    if level == 0 {
                        break;
                    }
                } else if self.tok == CTOK_EOF {
                    return Err(self.err_token(t(b'}')));
                }
                self.next()?;
            }
            self.mode &= !CPARSE_MODE_SKIP;
            self.tok = t(b';'); // Ok for cp_decl_multi(), error in cp_decl_single().
        }
        info |= fdecl.fattr & !CTMASK_CID;
        fdecl.fattr = 0;
        let idx = self.add(fdecl, info, nargs)?;
        fdecl.stack[idx].sib = anchor;
        Ok(())
    }

    /// Parse declarator.
    fn declarator(&mut self, decl: &mut CPDecl) -> PResult<()> {
        self.depth += 1;
        if self.depth > CPARSE_MAX_DECLDEPTH {
            return Err(self.err(errmsg::XLEVELS));
        }

        loop {
            // Head of declarator.
            if self.opt(t(b'*'))? {
                // Pointer.
                self.decl_attributes(decl)?;
                let mut sz = CTSIZE_PTR;
                let mut info = ctinfo(CT_PTR, CTALIGN_PTR);
                if ctype_msize_p(decl.attr) == 4 {
                    sz = 4;
                    info = ctinfo(CT_PTR, ctalign(2));
                }
                info += decl.attr & (CTF_QUAL | CTF_REF);
                decl.attr &= !(CTF_QUAL | (CTMASK_MSIZEP << CTSHIFT_MSIZEP));
                self.push(decl, info, sz)?;
            } else if self.opt(t(b'&'))? || self.opt(CTOK_ANDAND)? {
                // Reference.
                decl.attr &= !(CTF_QUAL | (CTMASK_MSIZEP << CTSHIFT_MSIZEP));
                self.push(decl, ctinfo_ref(0), CTSIZE_PTR)?;
            } else {
                break;
            }
        }

        let mut func_decl = false;
        if self.opt(t(b'('))? {
            // Inner declarator.
            self.decl_attributes(decl)?;
            // Resolve ambiguity between inner declarator and 1st function parameter.
            if (decl.mode & CPARSE_MODE_ABSTRACT) != 0 && (self.tok == t(b')') || self.istypedecl())
            {
                func_decl = true;
            } else {
                let pos = decl.pos;
                self.declarator(decl)?;
                self.check(t(b')'))?;
                decl.pos = pos;
            }
        } else if self.tok == CTOK_IDENT {
            // Direct declarator.
            if decl.mode & CPARSE_MODE_DIRECT == 0 {
                return Err(self.err_token(CTOK_EOF));
            }
            decl.name = Some(self.str.clone());
            decl.nameid = self.val.id;
            self.next()?;
        } else {
            // Abstract declarator.
            if decl.mode & CPARSE_MODE_ABSTRACT == 0 {
                return Err(self.err_token(CTOK_IDENT));
            }
        }

        loop {
            // Tail of declarator.
            if func_decl {
                func_decl = false;
                self.decl_func(decl)?;
            } else if self.opt(t(b'['))? {
                // Array.
                self.decl_array(decl)?;
            } else if self.opt(t(b'('))? {
                // Function.
                self.decl_func(decl)?;
            } else {
                break;
            }
        }

        if (decl.mode & CPARSE_MODE_FIELD) != 0 && self.opt(t(b':'))? {
            // Field width.
            decl.bits = self.expr_ksize()?;
        }

        // Process postfix attributes.
        self.decl_attributes(decl)?;
        self.push_attributes(decl)?;

        self.depth -= 1;
        Ok(())
    }

    /// Parse an abstract type declaration and return its C type ID.
    fn decl_abstract(&mut self) -> PResult<CTypeID> {
        let mut decl = CPDecl::new();
        self.decl_spec(&mut decl, 0)?;
        decl.mode = CPARSE_MODE_ABSTRACT;
        self.declarator(&mut decl)?;
        self.decl_intern(&decl)
    }

    /// Handle pragmas.
    fn pragma(&mut self, pragmaline: i32) -> PResult<()> {
        self.next()?;
        if self.tok == CTOK_IDENT && self.str == b"pack" {
            self.next()?;
            self.check(t(b'('))?;
            let mut end_pack = false;
            if self.tok == CTOK_IDENT {
                if self.str == b"push" {
                    if self.curpack < CPARSE_MAX_PACKSTACK - 1 {
                        self.packstack[self.curpack + 1] = self.packstack[self.curpack];
                        self.curpack += 1;
                    } else {
                        return Err(self.errmsg(self.tok, errmsg::XLEVELS.as_bytes()));
                    }
                } else if self.str == b"pop" {
                    if self.curpack > 0 {
                        self.curpack -= 1;
                    }
                } else {
                    return Err(self.errmsg(self.tok, errmsg::XSYMBOL.as_bytes()));
                }
                self.next()?;
                if !self.opt(t(b','))? {
                    end_pack = true;
                }
            }
            if !end_pack {
                if self.tok == CTOK_INTEGER {
                    let v = self.val.u32;
                    self.packstack[self.curpack] = if v != 0 { lj_fls(v) as u8 } else { 0 };
                    self.next()?;
                } else {
                    self.packstack[self.curpack] = 255;
                }
            }
            self.check(t(b')'))?;
        } else {
            // Ignore all other pragmas.
            while self.tok != CTOK_EOF && self.linenumber == pragmaline {
                self.next()?;
            }
        }
        Ok(())
    }

    /// Handle line number.
    fn line(&mut self, hashline: i32) -> PResult<()> {
        let newline = self.val.u32 as i32;
        while self.tok != CTOK_EOF && self.linenumber == hashline {
            self.next()?;
        }
        self.linenumber = newline;
        Ok(())
    }

    /// Parse multiple C declarations of types or extern identifiers.
    fn decl_multi(&mut self) -> PResult<()> {
        let mut first = true;
        while self.tok != CTOK_EOF {
            if self.opt(t(b';'))? {
                // Skip empty statements.
                first = false;
                continue;
            }
            if self.tok == t(b'#') {
                // Workaround, since we have no preprocessor, yet.
                let hashline = self.linenumber;
                let tok = self.next()?;
                if tok == CTOK_INTEGER {
                    self.line(hashline)?;
                    continue;
                } else if tok == CTOK_IDENT && self.str == b"line" {
                    if self.next()? != CTOK_INTEGER {
                        return Err(self.err_token(tok));
                    }
                    self.line(hashline)?;
                    continue;
                } else if tok == CTOK_IDENT && self.str == b"pragma" {
                    self.pragma(hashline)?;
                    continue;
                } else {
                    return Err(self.errmsg(self.tok, errmsg::XSYMBOL.as_bytes()));
                }
            }
            let mut decl = CPDecl::new();
            let scl = self.decl_spec(&mut decl, CDF_TYPEDEF | CDF_EXTERN | CDF_STATIC)?;
            let mut empty = false;
            if (self.tok == t(b';') || self.tok == CTOK_EOF) && ctype_istypedef(decl.stack[0].info)
            {
                let info = self
                    .cts
                    .get(self.cts.raw(ctype_cid(decl.stack[0].info)))
                    .info;
                if ctype_isstruct(info) || ctype_isenum(info) {
                    empty = true; // Accept empty declaration of struct/union/enum.
                }
            }
            if !empty {
                loop {
                    self.declarator(&mut decl)?;
                    let ctypeid = self.decl_intern(&decl)?;
                    if let Some(name) = decl.name.clone()
                        && decl.nameid == 0
                    {
                        // NYI: redeclarations are ignored.
                        let id;
                        let mut redir = true;
                        if scl & CDF_TYPEDEF != 0 {
                            // Create new typedef.
                            id = self.cts.new_type()?;
                            self.cts.get_mut(id).info = ctinfo(CT_TYPEDEF, ctypeid);
                            redir = false;
                        } else if ctype_isfunc(self.cts.get(ctypeid).info) {
                            // Treat both static and extern function declarations as extern.
                            // We always get new anonymous functions (typedefs are copied).
                            debug_assert!(
                                self.cts.name_of(ctypeid).is_none(),
                                "unexpected named function"
                            );
                            id = ctypeid; // Just name it.
                        } else if scl & CDF_STATIC != 0 {
                            // Accept static constants.
                            id = self.decl_constinit(ctypeid)?;
                            redir = false;
                        } else {
                            // External references have extern or no storage class.
                            id = self.cts.new_type()?;
                            self.cts.get_mut(id).info = ctinfo(CT_EXTERN, ctypeid);
                        }
                        if redir && let Some(r) = decl.redir.clone() {
                            // Add attribute for redirected symbol name.
                            let aid = self.cts.new_type()?;
                            let sib = self.cts.get(id).sib;
                            let cta = self.cts.get_mut(aid);
                            cta.info = ctinfo(CT_ATTRIB, ctattrib(CTA_REDIR));
                            cta.sib = sib;
                            self.cts.get_mut(id).sib = aid as CTypeID1;
                            self.cts.set_name(aid, &r);
                        }
                        self.cts.set_name(id, &name);
                        self.cts.addname(id);
                    }
                    if !self.opt(t(b','))? {
                        break;
                    }
                    decl.reset();
                }
            }
            if self.tok == CTOK_EOF && first {
                break; // May omit ';' for 1 decl.
            }
            first = false;
            self.check(t(b';'))?;
        }
        Ok(())
    }

    /// Parse a single C type declaration.
    fn decl_single(&mut self) -> PResult<()> {
        let mut decl = CPDecl::new();
        self.decl_spec(&mut decl, 0)?;
        self.declarator(&mut decl)?;
        self.val.id = self.decl_intern(&decl)?;
        if self.tok != CTOK_EOF {
            return Err(self.err_token(CTOK_EOF));
        }
        Ok(())
    }
}
