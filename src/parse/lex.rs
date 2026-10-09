//! Port of lj_lex.c / lj_lex.h: lexical analyzer.

use crate::cdata::{CTID_COMPLEX_DOUBLE, CTID_INT64, CTID_UINT64, CData};
use crate::error::{ErrorKind, LuaError, LuaResult};
use crate::gc::Gc;
use crate::lstr::LuaStr;
use crate::state::Vm;
use crate::text::chars;
use crate::text::strscan::{self, StrScan};
use crate::value::Value;

/// Lexical character: a byte, or `LEX_EOF`.
pub type LexChar = i32;
/// Lexical token: a single character, or one of the `TK_*` values.
pub type LexToken = i32;

pub const LEX_EOF: LexChar = -1;

/// Max. source code line number (`LJ_MAX_LINE`).
pub const LJ_MAX_LINE: u32 = 0x7fff_ff00;
/// `LUA_IDSIZE`.
pub const LUA_IDSIZE: usize = 60;

// Tokens (TKDEF order).
pub const TK_OFS: LexToken = 256;
pub const TK_AND: LexToken = 257;
pub const TK_BREAK: LexToken = 258;
pub const TK_CONST: LexToken = 259;
pub const TK_CONTINUE: LexToken = 260;
pub const TK_DO: LexToken = 261;
pub const TK_ELSE: LexToken = 262;
pub const TK_ELSEIF: LexToken = 263;
pub const TK_END: LexToken = 264;
pub const TK_FALSE: LexToken = 265;
pub const TK_FOR: LexToken = 266;
pub const TK_FUNCTION: LexToken = 267;
pub const TK_GOTO: LexToken = 268;
pub const TK_IF: LexToken = 269;
pub const TK_IN: LexToken = 270;
pub const TK_LOCAL: LexToken = 271;
pub const TK_NIL: LexToken = 272;
pub const TK_NOT: LexToken = 273;
pub const TK_OR: LexToken = 274;
pub const TK_REPEAT: LexToken = 275;
pub const TK_RETURN: LexToken = 276;
pub const TK_THEN: LexToken = 277;
pub const TK_TRUE: LexToken = 278;
pub const TK_UNTIL: LexToken = 279;
pub const TK_WHILE: LexToken = 280;
pub const TK_CONCAT: LexToken = 281;
pub const TK_DOTS: LexToken = 282;
pub const TK_EQ: LexToken = 283;
pub const TK_GE: LexToken = 284;
pub const TK_LE: LexToken = 285;
pub const TK_NE: LexToken = 286;
pub const TK_NAV: LexToken = 287;
pub const TK_COAL: LexToken = 288;
pub const TK_SHL: LexToken = 289;
pub const TK_SHR: LexToken = 290;
pub const TK_SAR: LexToken = 291;
pub const TK_AND_: LexToken = 292;
pub const TK_OR_: LexToken = 293;
pub const TK_NE_: LexToken = 294;
pub const TK_ARROW: LexToken = 295;
pub const TK_LABEL: LexToken = 296;
pub const TK_NUMBER: LexToken = 297;
pub const TK_NAME: LexToken = 298;
pub const TK_STRING: LexToken = 299;
pub const TK_EOF: LexToken = 300;
/// Number of reserved words (`TK_RESERVED`).
pub const TK_RESERVED: usize = (TK_WHILE - TK_OFS) as usize;

/// Token names (`tokennames`), indexed by `tok - TK_OFS - 1`.
pub const TOKEN_NAMES: [&str; 44] = [
    "and", "break", "const", "continue", "do", "else", "elseif", "end", "false", "for", "function",
    "goto", "if", "in", "local", "nil", "not", "or", "repeat", "return", "then", "true", "until",
    "while", "..", "...", "==", ">=", "<=", "~=", "?.", "??", "<<", ">>", "~>>", "&&", "||", "!=",
    "->", "::", "<number>", "<name>", "<string>", "<eof>",
];

/// Value of the current token.
#[derive(Clone, Copy, Debug)]
pub enum TokVal {
    None,
    Str(Gc<LuaStr>),
    Num(f64),
    CData(Gc<CData>),
}

impl TokVal {
    /// `strV(&ls->tokval)`: the string of a name/string/keyword token.
    pub fn str(self) -> Gc<LuaStr> {
        match self {
            TokVal::Str(s) => s,
            other => unreachable!("token value is not a string: {other:?}"),
        }
    }
}

/// Bytes of `s` up to the first NUL (C string semantics).
pub fn cstr(s: &[u8]) -> &[u8] {
    match s.iter().position(|&b| b == 0) {
        Some(n) => &s[..n],
        None => s,
    }
}

/// `"'" s "'"` (`LUA_QS`), with C string truncation.
pub fn qs(s: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(s.len() + 2);
    v.push(b'\'');
    v.extend_from_slice(cstr(s));
    v.push(b'\'');
    v
}

/// Port of `lj_debug_shortname`: printable chunk name for error messages. `line == !0` marks a
/// builtin.
pub fn debug_shortname(name: &[u8], line: u32) -> Vec<u8> {
    let src = cstr(name);
    let mut out = Vec::with_capacity(LUA_IDSIZE);
    if src.first() == Some(&b'=') {
        let s = &src[1..];
        out.extend_from_slice(&s[..s.len().min(LUA_IDSIZE - 1)]);
    } else if src.first() == Some(&b'@') {
        let mut s = &src[1..];
        let len = name.len() - 1;
        if len >= LUA_IDSIZE {
            let skip = len - (LUA_IDSIZE - 4);
            s = if skip <= s.len() { &s[skip..] } else { &[] };
            out.extend_from_slice(b"...");
        }
        out.extend_from_slice(s);
    } else {
        let mut len = 0;
        while len < LUA_IDSIZE - 12 {
            if len >= src.len() || src[len] < b' ' {
                break;
            }
            len += 1;
        }
        out.extend_from_slice(if line == !0 { b"[builtin:" } else { b"[string \"" });
        if len < src.len() {
            if len > LUA_IDSIZE - 15 {
                len = LUA_IDSIZE - 15;
            }
            out.extend_from_slice(&src[..len]);
            out.extend_from_slice(b"...");
        } else {
            out.extend_from_slice(src);
        }
        out.extend_from_slice(if line == !0 { b"]" } else { b"\"]" });
    }
    out
}

#[inline]
fn isident(c: LexChar) -> bool {
    chars::bits(c) & chars::IDENT != 0
}
#[inline]
fn isdigit(c: LexChar) -> bool {
    chars::bits(c) & chars::DIGIT != 0
}
#[inline]
fn isxdigit(c: LexChar) -> bool {
    chars::bits(c) & chars::XDIGIT != 0
}
#[inline]
fn isspace(c: LexChar) -> bool {
    chars::bits(c) & chars::SPACE != 0
}

/// Lexer state (`LexState`, lexer part).
pub struct LexState<'a> {
    pub vm: &'a mut Vm,
    src: &'a [u8],
    p: usize,
    /// Current character.
    pub c: LexChar,
    /// Current token.
    pub tok: LexToken,
    pub tokval: TokVal,
    /// Lookahead token (`TK_EOF` if none).
    pub lookahead: LexToken,
    lookaheadval: TokVal,
    /// String buffer for tokens.
    pub sb: Vec<u8>,
    /// Input line counter.
    pub linenumber: u32,
    /// Line of last token.
    pub lastline: u32,
    /// Current chunk name.
    pub chunkname: Gc<LuaStr>,
}

impl<'a> LexState<'a> {
    pub fn new(vm: &'a mut Vm, src: &'a [u8], chunkname: Gc<LuaStr>) -> LexState<'a> {
        LexState {
            vm,
            src,
            p: 0,
            c: LEX_EOF,
            tok: 0,
            tokval: TokVal::None,
            lookahead: TK_EOF,
            lookaheadval: TokVal::None,
            sb: Vec::new(),
            linenumber: 1,
            lastline: 1,
            chunkname,
        }
    }

    // -- Buffer handling --

    #[inline]
    fn next(&mut self) -> LexChar {
        self.c = if self.p < self.src.len() {
            let b = self.src[self.p];
            self.p += 1;
            b as LexChar
        } else {
            LEX_EOF
        };
        self.c
    }

    #[inline]
    fn save(&mut self, c: LexChar) {
        self.sb.push(c as u8);
    }

    #[inline]
    fn savenext(&mut self) -> LexChar {
        self.save(self.c);
        self.next()
    }

    #[inline]
    fn iseol(&self) -> bool {
        self.c == b'\n' as i32 || self.c == b'\r' as i32
    }

    /// Skip line break. Handles "\n", "\r", "\r\n" or "\n\r".
    fn newline(&mut self) -> LuaResult<()> {
        let old = self.c;
        self.next();
        if self.iseol() && self.c != old {
            self.next();
        }
        self.linenumber += 1;
        if self.linenumber >= LJ_MAX_LINE {
            return Err(self.error(self.tok, b"chunk has too many lines"));
        }
        Ok(())
    }

    // -- Scanner for terminals --

    /// Parse a number literal.
    fn number(&mut self) -> LuaResult<TokVal> {
        let mut xp = b'e' as LexChar;
        let mut c = self.c;
        if c == b'0' as LexChar {
            self.save(c);
            loop {
                c = self.next();
                if c != b'_' as LexChar {
                    break;
                }
            }
            if (c | 0x20) == b'x' as LexChar {
                xp = b'p' as LexChar;
            }
        }
        while isident(self.c)
            || self.c == b'.' as LexChar
            || ((self.c == b'-' as LexChar || self.c == b'+' as LexChar) && (c | 0x20) == xp)
        {
            if self.c != b'_' as LexChar {
                c = self.c;
                self.save(self.c);
            }
            self.next();
        }
        self.save(0);
        let len = self.sb.len() - 1;
        match strscan::scan(&self.sb[..len], strscan::OPT_TONUM | strscan::OPT_LL | strscan::OPT_IMAG) {
            StrScan::Num(n) => Ok(TokVal::Num(n)),
            StrScan::Imag(n) => {
                super::ffi_literal_hook(self.vm)?;
                let cd = self.vm.new_cdata(CTID_COMPLEX_DOUBLE, 16, 8);
                // SAFETY: the cdata owns 16 zeroed bytes aligned to at least 16.
                unsafe {
                    let p = cd.ptr() as *mut f64;
                    *p = 0.0;
                    *p.add(1) = n;
                }
                Ok(TokVal::CData(cd))
            }
            StrScan::I64(v) => Ok(TokVal::CData(self.cdata64(CTID_INT64, v as u64)?)),
            StrScan::U64(v) => Ok(TokVal::CData(self.cdata64(CTID_UINT64, v)?)),
            StrScan::Error | StrScan::Int(_) | StrScan::U32(_) => {
                Err(self.error(TK_NUMBER, b"malformed number"))
            }
        }
    }

    fn cdata64(&mut self, ctypeid: u32, v: u64) -> LuaResult<Gc<CData>> {
        super::ffi_literal_hook(self.vm)?;
        let cd = self.vm.new_cdata(ctypeid, 8, 8);
        // SAFETY: the cdata owns 8 zeroed bytes aligned to at least 16.
        unsafe { *(cd.ptr() as *mut u64) = v };
        Ok(cd)
    }

    /// Skip equal signs for "[=...=[" and "]=...=]" and return their count.
    fn skipeq(&mut self) -> i32 {
        let mut count = 0;
        let s = self.c;
        while self.savenext() == b'=' as LexChar && count < 0x2000_0000 {
            count += 1;
        }
        if self.c == s { count } else { -count - 1 }
    }

    /// Parse a long string or long comment (`want` = false).
    fn longstring(&mut self, want: bool, sep: i32) -> LuaResult<Option<Gc<LuaStr>>> {
        self.savenext(); // Skip second '['.
        if self.iseol() {
            self.newline()?; // Skip initial newline.
        }
        loop {
            match self.c {
                LEX_EOF => {
                    let msg: &[u8] = if want { b"unfinished long string" } else { b"unfinished long comment" };
                    return Err(self.error(TK_EOF, msg));
                }
                0x5d /* ']' */ => {
                    if self.skipeq() == sep {
                        self.savenext(); // Skip second ']'.
                        break;
                    }
                }
                0x0a | 0x0d => {
                    self.save(b'\n' as LexChar);
                    self.newline()?;
                    if !want {
                        self.sb.clear(); // Don't waste space for comments.
                    }
                }
                _ => {
                    self.savenext();
                }
            }
        }
        if want {
            let n = (2 + sep) as usize;
            let s = self.vm.new_str(&self.sb[n..self.sb.len() - n]);
            Ok(Some(s))
        } else {
            Ok(None)
        }
    }

    /// Parse a string.
    fn string(&mut self) -> LuaResult<Gc<LuaStr>> {
        let delim = self.c;
        self.savenext();
        while self.c != delim {
            match self.c {
                LEX_EOF => return Err(self.error(TK_EOF, b"unfinished string")),
                0x0a | 0x0d => return Err(self.error(TK_STRING, b"unfinished string")),
                0x5c /* '\\' */ => {
                    let mut c = self.next(); // Skip the '\\'.
                    match c {
                        0x61 => c = 7,  // \a
                        0x62 => c = 8,  // \b
                        0x66 => c = 12, // \f
                        0x6e => c = 10, // \n
                        0x72 => c = 13, // \r
                        0x74 => c = 9,  // \t
                        0x76 => c = 11, // \v
                        0x78 => {
                            // Hexadecimal escape '\xXX'.
                            c = (self.next() & 15) << 4;
                            if !isdigit(self.c) {
                                if !isxdigit(self.c) {
                                    return Err(self.err_xesc());
                                }
                                c += 9 << 4;
                            }
                            c += self.next() & 15;
                            if !isdigit(self.c) {
                                if !isxdigit(self.c) {
                                    return Err(self.err_xesc());
                                }
                                c += 9;
                            }
                        }
                        0x75 => {
                            // Unicode escape '\u{XX...}'.
                            if self.next() != b'{' as LexChar {
                                return Err(self.err_xesc());
                            }
                            self.next();
                            c = 0;
                            loop {
                                c = (c << 4) | (self.c & 15);
                                if !isdigit(self.c) {
                                    if !isxdigit(self.c) {
                                        return Err(self.err_xesc());
                                    }
                                    c += 9;
                                }
                                if c >= 0x110000 {
                                    return Err(self.err_xesc()); // Out of Unicode range.
                                }
                                if self.next() == b'}' as LexChar {
                                    break;
                                }
                            }
                            if c < 0x800 {
                                if c >= 0x80 {
                                    self.save(0xc0 | (c >> 6));
                                    c = 0x80 | (c & 0x3f);
                                }
                            } else {
                                if c >= 0x10000 {
                                    self.save(0xf0 | (c >> 18));
                                    self.save(0x80 | ((c >> 12) & 0x3f));
                                } else {
                                    if (0xd800..0xe000).contains(&c) {
                                        return Err(self.err_xesc()); // No surrogates.
                                    }
                                    self.save(0xe0 | (c >> 12));
                                }
                                self.save(0x80 | ((c >> 6) & 0x3f));
                                c = 0x80 | (c & 0x3f);
                            }
                        }
                        0x7a => {
                            // Skip whitespace.
                            self.next();
                            while isspace(self.c) {
                                if self.iseol() {
                                    self.newline()?;
                                } else {
                                    self.next();
                                }
                            }
                            continue;
                        }
                        0x0a | 0x0d => {
                            self.save(b'\n' as LexChar);
                            self.newline()?;
                            continue;
                        }
                        0x5c | 0x22 | 0x27 => {}
                        LEX_EOF => continue,
                        _ => {
                            if !isdigit(c) {
                                return Err(self.err_xesc());
                            }
                            c -= b'0' as LexChar; // Decimal escape '\ddd'.
                            if isdigit(self.next()) {
                                c = c * 10 + (self.c - b'0' as LexChar);
                                if isdigit(self.next()) {
                                    c = c * 10 + (self.c - b'0' as LexChar);
                                    if c > 255 {
                                        return Err(self.err_xesc());
                                    }
                                    self.next();
                                }
                            }
                            self.save(c);
                            continue;
                        }
                    }
                    self.save(c);
                    self.next();
                }
                _ => {
                    self.savenext();
                }
            }
        }
        self.savenext(); // Skip trailing delimiter.
        let n = self.sb.len();
        Ok(self.vm.new_str(&self.sb[1..n - 1]))
    }

    fn err_xesc(&mut self) -> LuaError {
        self.error(TK_STRING, b"invalid escape sequence")
    }

    // -- Main lexical scanner --

    /// Get next lexical token.
    fn scan(&mut self) -> LuaResult<(LexToken, TokVal)> {
        self.sb.clear();
        loop {
            if isident(self.c) {
                if isdigit(self.c) {
                    // Numeric literal.
                    let tv = self.number()?;
                    return Ok((TK_NUMBER, tv));
                }
                // Identifier or reserved word.
                loop {
                    self.savenext();
                    if !isident(self.c) {
                        break;
                    }
                }
                let s = self.vm.new_str(&self.sb);
                let r = s.reserved();
                if r > 0 {
                    return Ok((TK_OFS + r as LexToken, TokVal::Str(s)));
                }
                return Ok((TK_NAME, TokVal::Str(s)));
            }
            let c = self.c;
            let tok = match c {
                0x0a | 0x0d => {
                    self.newline()?;
                    continue;
                }
                0x20 | 0x09 | 0x0b | 0x0c => {
                    self.next();
                    continue;
                }
                0x2d /* '-' */ => {
                    self.next();
                    if self.c != b'-' as LexChar {
                        if self.c != b'>' as LexChar {
                            return Ok((b'-' as LexToken, TokVal::None));
                        }
                        self.next();
                        return Ok((TK_ARROW, TokVal::None));
                    }
                    self.next();
                    if self.c == b'[' as LexChar {
                        // Long comment "--[=*[...]=*]".
                        let sep = self.skipeq();
                        self.sb.clear(); // `skipeq' may dirty the buffer.
                        if sep >= 0 {
                            self.longstring(false, sep)?;
                            self.sb.clear();
                            continue;
                        }
                    }
                    // Short comment "--.*\n".
                    while !self.iseol() && self.c != LEX_EOF {
                        self.next();
                    }
                    continue;
                }
                0x5b /* '[' */ => {
                    let sep = self.skipeq();
                    if sep >= 0 {
                        let s = self.longstring(true, sep)?.expect("long string value");
                        return Ok((TK_STRING, TokVal::Str(s)));
                    } else if sep == -1 {
                        b'[' as LexToken
                    } else {
                        return Err(self.error(TK_STRING, b"invalid long string delimiter"));
                    }
                }
                0x3d /* '=' */ => {
                    self.next();
                    if self.c != b'=' as LexChar {
                        b'=' as LexToken
                    } else {
                        self.next();
                        TK_EQ
                    }
                }
                0x3c /* '<' */ => {
                    self.next();
                    if self.c == b'=' as LexChar {
                        self.next();
                        TK_LE
                    } else if self.c == b'<' as LexChar {
                        self.next();
                        TK_SHL
                    } else {
                        b'<' as LexToken
                    }
                }
                0x3e /* '>' */ => {
                    self.next();
                    if self.c == b'=' as LexChar {
                        self.next();
                        TK_GE
                    } else if self.c == b'>' as LexChar {
                        self.next();
                        TK_SHR
                    } else {
                        b'>' as LexToken
                    }
                }
                0x7e /* '~' */ => {
                    self.next();
                    if self.c == b'=' as LexChar {
                        self.next();
                        TK_NE
                    } else if self.c == b'>' as LexChar {
                        self.next();
                        if self.c != b'>' as LexChar {
                            return Err(self.error(b'~' as LexToken, b"unexpected symbol"));
                        }
                        self.next();
                        TK_SAR
                    } else {
                        b'~' as LexToken
                    }
                }
                0x21 /* '!' */ => {
                    self.next();
                    if self.c != b'=' as LexChar {
                        b'!' as LexToken
                    } else {
                        self.next();
                        TK_NE_
                    }
                }
                0x3a /* ':' */ => {
                    self.next();
                    if self.c != b':' as LexChar {
                        b':' as LexToken
                    } else {
                        self.next();
                        TK_LABEL
                    }
                }
                0x3f /* '?' */ => {
                    self.next();
                    if self.c == b'.' as LexChar {
                        self.next();
                        TK_NAV
                    } else if self.c == b'?' as LexChar {
                        self.next();
                        TK_COAL
                    } else {
                        b'?' as LexToken
                    }
                }
                0x26 /* '&' */ => {
                    self.next();
                    if self.c != b'&' as LexChar {
                        b'&' as LexToken
                    } else {
                        self.next();
                        TK_AND_
                    }
                }
                0x7c /* '|' */ => {
                    self.next();
                    if self.c != b'|' as LexChar {
                        b'|' as LexToken
                    } else {
                        self.next();
                        TK_OR_
                    }
                }
                0x22 | 0x27 => {
                    let s = self.string()?;
                    return Ok((TK_STRING, TokVal::Str(s)));
                }
                0x2e /* '.' */ => {
                    if self.savenext() == b'.' as LexChar {
                        self.next();
                        if self.c == b'.' as LexChar {
                            self.next();
                            TK_DOTS
                        } else {
                            TK_CONCAT
                        }
                    } else if !isdigit(self.c) {
                        b'.' as LexToken
                    } else {
                        let tv = self.number()?;
                        return Ok((TK_NUMBER, tv));
                    }
                }
                LEX_EOF => TK_EOF,
                _ => {
                    self.next();
                    c // Single-char tokens (+ - / ...).
                }
            };
            return Ok((tok, TokVal::None));
        }
    }

    // -- Lexer API --

    /// `lj_lex_setup`: read the first character, skip a UTF-8 BOM and a `#` header line.
    /// Returns true if the input is a bytecode dump (without header); a dump after a header is
    /// rejected.
    pub fn setup(&mut self) -> LuaResult<bool> {
        let mut header = false;
        self.next(); // Read-ahead first char.
        if self.c == 0xef && self.p + 2 <= self.src.len() && self.src[self.p] == 0xbb && self.src[self.p + 1] == 0xbf {
            // Skip UTF-8 BOM.
            self.p += 2;
            self.next();
            header = true;
        }
        if self.c == b'#' as LexChar {
            // Skip POSIX #! header line.
            loop {
                self.next();
                if self.c == LEX_EOF {
                    return Ok(false);
                }
                if self.iseol() {
                    break;
                }
            }
            self.newline()?;
            header = true;
        }
        if self.c == 0x1b {
            // Bytecode dump.
            if header {
                let s = self.vm.new_str(b"cannot load malformed bytecode");
                return Err(LuaError::new(ErrorKind::Syntax, Value::Str(s)));
            }
            return Ok(true);
        }
        Ok(false)
    }

    /// `lj_lex_next`: advance to the next token.
    pub fn next_token(&mut self) -> LuaResult<()> {
        self.lastline = self.linenumber;
        if self.lookahead == TK_EOF {
            let (tok, tv) = self.scan()?;
            self.tok = tok;
            self.tokval = tv;
        } else {
            self.tok = self.lookahead;
            self.lookahead = TK_EOF;
            self.tokval = self.lookaheadval;
        }
        Ok(())
    }

    /// `lj_lex_lookahead`: look ahead for the next token.
    pub fn lookahead(&mut self) -> LuaResult<LexToken> {
        debug_assert!(self.lookahead == TK_EOF, "double lookahead");
        let (tok, tv) = self.scan()?;
        self.lookahead = tok;
        self.lookaheadval = tv;
        Ok(tok)
    }

    /// `lj_lex_token2str`.
    pub fn token2str(&self, tok: LexToken) -> Vec<u8> {
        if tok > TK_OFS {
            TOKEN_NAMES[(tok - TK_OFS - 1) as usize].as_bytes().to_vec()
        } else if !chars::iscntrl(tok as u8) {
            vec![tok as u8]
        } else {
            format!("char({tok})").into_bytes()
        }
    }

    /// `lj_lex_error`: build a syntax error with position and optional `near` token. `msg` is
    /// the already formatted message.
    pub fn error(&mut self, tok: LexToken, msg: &[u8]) -> LuaError {
        let tokstr = if tok == 0 {
            None
        } else if tok == TK_NAME || tok == TK_STRING || tok == TK_NUMBER {
            self.sb.push(0);
            Some(cstr(&self.sb).to_vec())
        } else {
            Some(self.token2str(tok))
        };
        let line = self.linenumber;
        let mut m = debug_shortname(self.chunkname.as_bytes(), line);
        m.extend_from_slice(format!(":{line}: ").as_bytes());
        m.extend_from_slice(cstr(msg));
        if let Some(t) = tokstr {
            m.extend_from_slice(b" near ");
            m.extend_from_slice(&qs(&t));
        }
        let s = self.vm.new_str(&m);
        LuaError::new(ErrorKind::Syntax, Value::Str(s))
    }
}

/// `lj_lex_init`: mark the reserved words (interned and fixed).
pub fn init_reserved(vm: &mut Vm) {
    for (i, name) in TOKEN_NAMES.iter().take(TK_RESERVED).enumerate() {
        let s = vm.new_str(name.as_bytes());
        if s.reserved() == 0 {
            s.fix();
            s.set_reserved((i + 1) as u8);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::debug_shortname;

    #[test]
    fn shortname_forms() {
        // Reference: luajit -e 'error("x")' style prefixes via loadstring(..., name).
        assert_eq!(debug_shortname(b"=stdin", 1), b"stdin");
        let long_eq = [b'='; 80];
        assert_eq!(debug_shortname(&long_eq, 1).len(), 59);
        assert_eq!(debug_shortname(b"@foo/bar.lua", 1), b"foo/bar.lua");
        let mut long_at = b"@".to_vec();
        long_at.extend((0..70).map(|i| b'a' + (i % 26) as u8));
        let s = debug_shortname(&long_at, 1);
        assert_eq!(&s[..3], b"...");
        assert_eq!(s.len(), 3 + 56);
        assert_eq!(&s[3..], &long_at[long_at.len() - 56..]);
        assert_eq!(debug_shortname(b"return 1", 1), b"[string \"return 1\"]");
        assert_eq!(debug_shortname(b"a\nb", 1), b"[string \"a...\"]");
        assert_eq!(debug_shortname(b"name", !0), b"[builtin:name]");
        let long = [b'x'; 60];
        let mut want = b"[string \"".to_vec();
        want.extend_from_slice(&[b'x'; 45]);
        want.extend_from_slice(b"...\"]");
        assert_eq!(debug_shortname(&long, 1), want);
    }
}
