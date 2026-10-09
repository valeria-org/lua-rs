//! Port of the Lua 5.1 pattern matcher in lib_string.c (`string.find`, `string.match`,
//! `string.gmatch`, `string.gsub`), plus lj_str.c `lj_str_find` / `lj_str_haspattern`.
//!
//! Positions are 0-based byte offsets into the subject. Reading past the end of the
//! subject or the pattern yields `0`, mirroring upstream's NUL-terminated strings, so a
//! NUL byte inside a pattern terminates it exactly as in LuaJIT.

use super::chars::{self, isdigit};

/// `LUA_MAXCAPTURES`.
pub const MAXCAPTURES: usize = 32;
/// `LJ_MAX_XLEVEL`: recursion limit of the matcher.
pub const MAX_XLEVEL: u32 = 200;

const L_ESC: u8 = b'%';
const CAP_UNFINISHED: isize = -1;
const CAP_POSITION: isize = -2;

/// Pattern errors (raised upstream with `lj_err_caller`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PatError {
    /// `LJ_ERR_STRPATB`
    MissingFrontierBracket,
    /// `LJ_ERR_STRPATC`
    InvalidPatternCapture,
    /// `LJ_ERR_STRPATE`
    EndsWithEscape,
    /// `LJ_ERR_STRPATM`
    MissingBracket,
    /// `LJ_ERR_STRPATU`
    Unbalanced,
    /// `LJ_ERR_STRPATX`
    TooComplex,
    /// `LJ_ERR_STRCAPI`
    InvalidCaptureIndex,
    /// `LJ_ERR_STRCAPN`
    TooManyCaptures,
    /// `LJ_ERR_STRCAPU`
    UnfinishedCapture,
}

impl PatError {
    /// Exact upstream message text (lj_errmsg.h).
    pub fn message(self) -> &'static str {
        match self {
            PatError::MissingFrontierBracket => "missing '[' after '%f' in pattern",
            PatError::InvalidPatternCapture => "invalid pattern capture",
            PatError::EndsWithEscape => "malformed pattern (ends with '%')",
            PatError::MissingBracket => "malformed pattern (missing ']')",
            PatError::Unbalanced => "unbalanced pattern",
            PatError::TooComplex => "pattern too complex",
            PatError::InvalidCaptureIndex => "invalid capture index",
            PatError::TooManyCaptures => "too many captures",
            PatError::UnfinishedCapture => "unfinished capture",
        }
    }
}

impl std::fmt::Display for PatError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

impl std::error::Error for PatError {}

type PResult<T> = Result<T, PatError>;

/// A capture value, as pushed by upstream `push_onecapture`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Capture {
    /// Substring `src[start..end]`.
    Span { start: usize, end: usize },
    /// Position capture `()`: the 1-based position (Lua integer).
    Position(usize),
}

impl Capture {
    /// The captured bytes, or `None` for a position capture.
    pub fn bytes<'a>(&self, src: &'a [u8]) -> Option<&'a [u8]> {
        match *self {
            Capture::Span { start, end } => Some(&src[start..end]),
            Capture::Position(_) => None,
        }
    }
}

#[derive(Clone, Copy)]
struct Cap {
    init: usize,
    len: isize,
}

/// `MatchState`: matcher state over one subject string.
pub struct MatchState<'a> {
    src: &'a [u8],
    level: usize,
    depth: u32,
    capture: [Cap; MAXCAPTURES],
}

#[inline]
fn at(s: &[u8], i: usize) -> u8 {
    s.get(i).copied().unwrap_or(0)
}

fn match_class(c: u8, cl: u8) -> bool {
    static MATCH_CLASS_MAP: [u8; 32] = [
        0,
        chars::ALPHA,
        0,
        chars::CNTRL,
        chars::DIGIT,
        0,
        0,
        chars::GRAPH,
        0,
        0,
        0,
        0,
        chars::LOWER,
        0,
        0,
        0,
        chars::PUNCT,
        0,
        0,
        chars::SPACE,
        0,
        chars::UPPER,
        0,
        chars::ALNUM,
        chars::XDIGIT,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
    ];
    if (cl & 0xc0) == 0x40 {
        let t = MATCH_CLASS_MAP[(cl & 0x1f) as usize];
        if t != 0 {
            let r = chars::isa(c, t);
            return if cl & 0x20 != 0 { r } else { !r };
        }
        if cl == b'z' {
            return c == 0;
        }
        if cl == b'Z' {
            return c != 0;
        }
    }
    cl == c
}

/// `p` points at `[`, `ec` at the closing `]`.
fn matchbracketclass(c: u8, pat: &[u8], mut p: usize, ec: usize) -> bool {
    let mut sig = true;
    if at(pat, p + 1) == b'^' {
        sig = false;
        p += 1; // skip the `^'
    }
    loop {
        p += 1;
        if p >= ec {
            break;
        }
        if at(pat, p) == L_ESC {
            p += 1;
            if match_class(c, at(pat, p)) {
                return sig;
            }
        } else if at(pat, p + 1) == b'-' && p + 2 < ec {
            p += 2;
            if at(pat, p - 2) <= c && c <= at(pat, p) {
                return sig;
            }
        } else if at(pat, p) == c {
            return sig;
        }
    }
    !sig
}

fn singlematch(c: u8, pat: &[u8], p: usize, ep: usize) -> bool {
    match at(pat, p) {
        b'.' => true, // matches any char
        L_ESC => match_class(c, at(pat, p + 1)),
        b'[' => matchbracketclass(c, pat, p, ep - 1),
        pc => pc == c,
    }
}

fn classend(pat: &[u8], mut p: usize) -> PResult<usize> {
    let c = at(pat, p);
    p += 1;
    match c {
        L_ESC => {
            if at(pat, p) == 0 {
                return Err(PatError::EndsWithEscape);
            }
            Ok(p + 1)
        }
        b'[' => {
            if at(pat, p) == b'^' {
                p += 1;
            }
            // look for a `]'
            loop {
                if at(pat, p) == 0 {
                    return Err(PatError::MissingBracket);
                }
                let c = at(pat, p);
                p += 1;
                if c == L_ESC && at(pat, p) != 0 {
                    p += 1; // skip escapes (e.g. `%]')
                }
                if at(pat, p) == b']' {
                    break;
                }
            }
            Ok(p + 1)
        }
        _ => Ok(p),
    }
}

impl<'a> MatchState<'a> {
    pub fn new(src: &'a [u8]) -> Self {
        MatchState { src, level: 0, depth: 0, capture: [Cap { init: 0, len: 0 }; MAXCAPTURES] }
    }

    /// The subject string.
    pub fn src(&self) -> &'a [u8] {
        self.src
    }

    /// Number of captures (finished or unfinished) of the last match.
    pub fn level(&self) -> usize {
        self.level
    }

    /// Try to match `pat` (without `^` handling) at subject offset `s`.
    /// Resets the capture state first. Returns the end offset of the match.
    pub fn match_at(&mut self, s: usize, pat: &[u8]) -> PResult<Option<usize>> {
        self.level = 0;
        self.depth = 0;
        self.do_match(s, pat, 0)
    }

    fn check_capture(&self, l: u8) -> PResult<usize> {
        let l = l as i32 - b'1' as i32;
        if l < 0 || l as usize >= self.level || self.capture[l as usize].len == CAP_UNFINISHED {
            return Err(PatError::InvalidCaptureIndex);
        }
        Ok(l as usize)
    }

    fn capture_to_close(&self) -> PResult<usize> {
        (0..self.level)
            .rev()
            .find(|&l| self.capture[l].len == CAP_UNFINISHED)
            .ok_or(PatError::InvalidPatternCapture)
    }

    fn matchbalance(&self, s: usize, pat: &[u8], p: usize) -> PResult<Option<usize>> {
        if at(pat, p) == 0 || at(pat, p + 1) == 0 {
            return Err(PatError::Unbalanced);
        }
        if at(self.src, s) != at(pat, p) {
            return Ok(None);
        }
        let b = at(pat, p);
        let e = at(pat, p + 1);
        let mut cont = 1;
        let mut s = s;
        loop {
            s += 1;
            if s >= self.src.len() {
                break;
            }
            let c = self.src[s];
            if c == e {
                cont -= 1;
                if cont == 0 {
                    return Ok(Some(s + 1));
                }
            } else if c == b {
                cont += 1;
            }
        }
        Ok(None) // string ends out of balance
    }

    fn max_expand(&mut self, s: usize, pat: &[u8], p: usize, ep: usize) -> PResult<Option<usize>> {
        let mut i = 0usize; // counts maximum expand for item
        while s + i < self.src.len() && singlematch(self.src[s + i], pat, p, ep) {
            i += 1;
        }
        // keeps trying to match with the maximum repetitions
        loop {
            if let Some(res) = self.do_match(s + i, pat, ep + 1)? {
                return Ok(Some(res));
            }
            if i == 0 {
                return Ok(None);
            }
            i -= 1; // else didn't match; reduce 1 repetition to try again
        }
    }

    fn min_expand(&mut self, mut s: usize, pat: &[u8], p: usize, ep: usize) -> PResult<Option<usize>> {
        loop {
            if let Some(res) = self.do_match(s, pat, ep + 1)? {
                return Ok(Some(res));
            } else if s < self.src.len() && singlematch(self.src[s], pat, p, ep) {
                s += 1; // try with one more repetition
            } else {
                return Ok(None);
            }
        }
    }

    fn start_capture(&mut self, s: usize, pat: &[u8], p: usize, what: isize) -> PResult<Option<usize>> {
        let level = self.level;
        if level >= MAXCAPTURES {
            return Err(PatError::TooManyCaptures);
        }
        self.capture[level] = Cap { init: s, len: what };
        self.level = level + 1;
        let res = self.do_match(s, pat, p)?;
        if res.is_none() {
            self.level -= 1; // undo capture
        }
        Ok(res)
    }

    fn end_capture(&mut self, s: usize, pat: &[u8], p: usize) -> PResult<Option<usize>> {
        let l = self.capture_to_close()?;
        self.capture[l].len = (s - self.capture[l].init) as isize; // close capture
        let res = self.do_match(s, pat, p)?;
        if res.is_none() {
            self.capture[l].len = CAP_UNFINISHED; // undo capture
        }
        Ok(res)
    }

    fn match_capture(&self, s: usize, l: u8) -> PResult<Option<usize>> {
        let l = self.check_capture(l)?;
        let len = self.capture[l].len as usize;
        let init = self.capture[l].init;
        if self.src.len() - s >= len && self.src[init..init + len] == self.src[s..s + len] {
            Ok(Some(s + len))
        } else {
            Ok(None)
        }
    }

    fn do_match(&mut self, mut s: usize, pat: &[u8], mut p: usize) -> PResult<Option<usize>> {
        self.depth += 1;
        if self.depth > MAX_XLEVEL {
            return Err(PatError::TooComplex);
        }
        let res: Option<usize> = 'init: loop {
            // using loops to optimize tail recursion
            match at(pat, p) {
                b'(' => {
                    // start capture
                    break if at(pat, p + 1) == b')' {
                        // position capture?
                        self.start_capture(s, pat, p + 2, CAP_POSITION)?
                    } else {
                        self.start_capture(s, pat, p + 1, CAP_UNFINISHED)?
                    };
                }
                b')' => break self.end_capture(s, pat, p + 1)?, // end capture
                0 => break Some(s),                             // end of pattern: match succeeded
                b'$' if at(pat, p + 1) == 0 => {
                    // the `$' is the last char in pattern: check end of string
                    break (s == self.src.len()).then_some(s);
                }
                L_ESC if at(pat, p + 1) == b'b' => {
                    // balanced string?
                    match self.matchbalance(s, pat, p + 2)? {
                        None => break None,
                        Some(ns) => {
                            s = ns;
                            p += 4;
                            continue 'init; // else s = match(ms, s, p+4);
                        }
                    }
                }
                L_ESC if at(pat, p + 1) == b'f' => {
                    // frontier?
                    p += 2;
                    if at(pat, p) != b'[' {
                        return Err(PatError::MissingFrontierBracket);
                    }
                    let ep = classend(pat, p)?; // points to what is next
                    let previous = if s == 0 { 0 } else { self.src[s - 1] };
                    if matchbracketclass(previous, pat, p, ep - 1)
                        || !matchbracketclass(at(self.src, s), pat, p, ep - 1)
                    {
                        break None;
                    }
                    p = ep;
                    continue 'init; // else s = match(ms, s, ep);
                }
                L_ESC if isdigit(at(pat, p + 1)) => {
                    // capture results (%0-%9)?
                    match self.match_capture(s, at(pat, p + 1))? {
                        None => break None,
                        Some(ns) => {
                            s = ns;
                            p += 2;
                            continue 'init; // else s = match(ms, s, p+2)
                        }
                    }
                }
                _ => {
                    // it is a pattern item
                    let ep = classend(pat, p)?; // points to what is next
                    let m = s < self.src.len() && singlematch(self.src[s], pat, p, ep);
                    match at(pat, ep) {
                        b'?' => {
                            // optional
                            if m && let Some(res) = self.do_match(s + 1, pat, ep + 1)? {
                                break Some(res);
                            }
                            p = ep + 1;
                            continue 'init; // else s = match(ms, s, ep+1);
                        }
                        b'*' => break self.max_expand(s, pat, p, ep)?, // 0 or more repetitions
                        b'+' => {
                            // 1 or more repetitions
                            break if m { self.max_expand(s + 1, pat, p, ep)? } else { None };
                        }
                        b'-' => break self.min_expand(s, pat, p, ep)?, // 0 or more repetitions (minimum)
                        _ => {
                            if m {
                                s += 1;
                                p = ep;
                                continue 'init; // else s = match(ms, s+1, ep);
                            }
                            break None;
                        }
                    }
                }
            }
        };
        self.depth -= 1;
        Ok(res)
    }

    /// `push_onecapture`: capture `i` of the last match `src[s..e]`.
    /// With no explicit captures, index 0 is the whole match.
    pub fn capture(&self, i: usize, s: usize, e: usize) -> PResult<Capture> {
        if i >= self.level {
            if i == 0 {
                Ok(Capture::Span { start: s, end: e }) // add whole match
            } else {
                Err(PatError::InvalidCaptureIndex)
            }
        } else {
            let c = self.capture[i];
            match c.len {
                CAP_UNFINISHED => Err(PatError::UnfinishedCapture),
                CAP_POSITION => Ok(Capture::Position(c.init + 1)),
                l => Ok(Capture::Span { start: c.init, end: c.init + l as usize }),
            }
        }
    }

    /// Number of values `push_captures` pushes: the capture count, or 1 (the whole
    /// match) when there are no captures and `whole_match` is set. `string.find`
    /// passes `whole_match = false` (upstream passes `s = NULL`).
    pub fn num_captures(&self, whole_match: bool) -> usize {
        if self.level == 0 && whole_match { 1 } else { self.level }
    }

    /// `push_captures`: all captures of the last match `src[s..e]`.
    pub fn captures(&self, whole_match: Option<(usize, usize)>) -> PResult<Vec<Capture>> {
        let (s, e) = whole_match.unwrap_or((0, 0));
        (0..self.num_captures(whole_match.is_some())).map(|i| self.capture(i, s, e)).collect()
    }
}

/// Result of [`find`]: the match span plus the state holding its captures.
pub struct Found<'a> {
    pub start: usize,
    pub end: usize,
    pub state: MatchState<'a>,
}

/// Pattern search loop of `str_find_aux` (`string.find` / `string.match`): handles a
/// leading `^` anchor and tries every start offset from `init` (already clamped to
/// `0..=src.len()`).
pub fn find<'a>(src: &'a [u8], pat: &[u8], init: usize) -> PResult<Option<Found<'a>>> {
    let (pat, anchor) = match pat.first() {
        Some(b'^') => (&pat[1..], true),
        _ => (pat, false),
    };
    let mut ms = MatchState::new(src);
    let mut s = init;
    loop {
        if let Some(e) = ms.match_at(s, pat)? {
            return Ok(Some(Found { start: s, end: e, state: ms }));
        }
        let cont = s < src.len() && !anchor;
        s += 1;
        if !cont {
            return Ok(None);
        }
    }
}

/// One step of `string.gmatch`: search from byte offset `pos` (the iterator's saved
/// position). Returns the match span and the next saved position. Note that gmatch
/// does not treat `^` as an anchor (Lua 5.1 semantics).
pub fn gmatch_next(ms: &mut MatchState<'_>, pat: &[u8], pos: usize) -> PResult<Option<(usize, usize, usize)>> {
    let mut src = pos;
    while src <= ms.src.len() {
        if let Some(e) = ms.match_at(src, pat)? {
            let next = if e == src { e + 1 } else { e }; // Ensure progress for empty match.
            return Ok(Some((src, e, next)));
        }
        src += 1;
    }
    Ok(None)
}

/// `add_s`: expand a `string.gsub` replacement string for the match `src[s..e]`.
/// `%0` is the whole match, `%1`-`%9` captures (a position capture expands to its
/// decimal value), `%x` for any other byte is `x`.
pub fn expand_replacement(ms: &MatchState<'_>, repl: &[u8], s: usize, e: usize, out: &mut Vec<u8>) -> PResult<()> {
    let mut i = 0;
    while i < repl.len() {
        if repl[i] != L_ESC {
            out.push(repl[i]);
        } else {
            i += 1; // skip ESC
            let c = at(repl, i);
            if !isdigit(c) {
                // Upstream reads the terminating NUL for a trailing '%'.
                out.push(c);
            } else if c == b'0' {
                out.extend_from_slice(&ms.src[s..e]);
            } else {
                match ms.capture((c - b'1') as usize, s, e)? {
                    Capture::Span { start, end } => out.extend_from_slice(&ms.src[start..end]),
                    Capture::Position(pos) => super::strfmt::put_int(out, pos as i32),
                }
            }
        }
        i += 1;
    }
    Ok(())
}

/// `lj_str_haspattern`: whether `p` contains any pattern special character.
pub fn has_pattern(p: &[u8]) -> bool {
    p.iter().any(|&c| chars::ispunct(c) && b"^$*+?.([%-".contains(&c))
}

/// `lj_str_find`: plain substring search; returns the offset of the first occurrence.
pub fn find_plain(s: &[u8], p: &[u8]) -> Option<usize> {
    if p.is_empty() {
        return Some(0);
    }
    if p.len() > s.len() {
        return None;
    }
    let (first, rest) = (p[0], &p[1..]);
    let last_start = s.len() - p.len();
    let mut i = 0;
    while i <= last_start {
        let q = i + s[i..=last_start].iter().position(|&c| c == first)?;
        if &s[q + 1..q + p.len()] == rest {
            return Some(q);
        }
        i = q + 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `string.find` / `string.match` as lib_string's str_find_aux, results rendered like
    /// `print(...)` joined with `|`.
    fn find_aux(s: &str, p: &str, init: i64, find_mode: bool, plain: bool) -> Result<String, PatError> {
        let (s, p) = (s.as_bytes(), p.as_bytes());
        let len = s.len() as i64;
        let mut start = if init < 0 { init + len } else { init - 1 };
        if start < 0 {
            start = 0;
        }
        let st = start.min(len) as usize;
        if find_mode && (plain || !has_pattern(p)) {
            return Ok(match find_plain(&s[st..], p) {
                Some(q) => format!("{}|{}", st + q + 1, st + q + p.len()),
                None => "nil".into(),
            });
        }
        let Some(f) = find(s, p, st)? else { return Ok("nil".into()) };
        let mut out = Vec::new();
        let caps = if find_mode {
            out.push(format!("{}|{}", f.start + 1, f.end));
            f.state.captures(None)?
        } else {
            f.state.captures(Some((f.start, f.end)))?
        };
        for c in caps {
            out.push(match c {
                Capture::Span { start, end } => String::from_utf8_lossy(&s[start..end]).into_owned(),
                Capture::Position(p) => p.to_string(),
            });
        }
        Ok(out.join("|"))
    }

    fn sfind(s: &str, p: &str) -> String {
        find_aux(s, p, 1, true, false).unwrap()
    }
    fn smatch(s: &str, p: &str) -> String {
        find_aux(s, p, 1, false, false).unwrap()
    }

    fn gsub(src: &str, p: &str, repl: &str, max: Option<usize>) -> Result<(String, usize), PatError> {
        let src = src.as_bytes();
        let p = p.as_bytes();
        let (p, anchor) = if p.first() == Some(&b'^') { (&p[1..], true) } else { (p, false) };
        let max_s = max.unwrap_or(src.len() + 1);
        let mut ms = MatchState::new(src);
        let mut out = Vec::new();
        let (mut s, mut n) = (0, 0);
        while n < max_s {
            let e = ms.match_at(s, p)?;
            if let Some(e) = e {
                n += 1;
                expand_replacement(&ms, repl.as_bytes(), s, e, &mut out)?;
            }
            match e {
                Some(e) if e > s => s = e,
                _ if s < src.len() => {
                    out.push(src[s]);
                    s += 1;
                }
                _ => break,
            }
            if anchor {
                break;
            }
        }
        out.extend_from_slice(&src[s..]);
        Ok((String::from_utf8(out).unwrap(), n))
    }

    fn gmatch(src: &str, p: &str) -> Vec<String> {
        let mut ms = MatchState::new(src.as_bytes());
        let mut pos = 0;
        let mut v = Vec::new();
        while let Some((s, e, next)) = gmatch_next(&mut ms, p.as_bytes(), pos).unwrap() {
            pos = next;
            let caps = ms.captures(Some((s, e))).unwrap();
            let parts: Vec<String> =
                caps.iter().map(|c| String::from_utf8_lossy(c.bytes(src.as_bytes()).unwrap()).into_owned()).collect();
            v.push(parts.concat());
        }
        v
    }

    #[test]
    fn find_and_match() {
        // Expected values: luajit -e 'print(string.find/match(...))'
        assert_eq!(sfind("hello world", "o w"), "5|7");
        assert_eq!(sfind("hello", "l+"), "3|4");
        assert_eq!(sfind("THE (quick) fox", "%((%a+)%)"), "5|11|quick");
        assert_eq!(smatch("hello", "()ll()"), "3|5");
        assert_eq!(find_aux("abc", "b", -1, true, false).unwrap(), "nil");
        assert_eq!(find_aux("abc", "", 10, true, false).unwrap(), "4|3");
        assert_eq!(find_aux("a.b", ".", 1, true, true).unwrap(), "2|2");
        assert_eq!(smatch("key = value", "(%w+)%s*=%s*(%w+)"), "key|value");
        assert_eq!(smatch("2024-01-02", "(%d+)-(%d+)-(%d+)"), "2024|01|02");
        assert_eq!(smatch("abcabc", "(abc)%1"), "abc");
        assert_eq!(sfind("x^y", "^"), "1|0");
        assert_eq!(sfind("x^y", "%^"), "2|2");
        assert_eq!(smatch("hello", "^(h)(e)"), "h|e");
        assert_eq!(sfind("abc", "x)"), "nil");
        assert_eq!(smatch("f(a(b)c)d", "%b()"), "(a(b)c)");
        assert_eq!(find_aux("THE (quick) fox", "%f[%a]%a+", 5, false, false).unwrap(), "quick");
        assert_eq!(smatch(" x", "%f[%w]%w+"), "x");
        assert_eq!(smatch("[[x]]", "%b[]"), "[[x]]");
        assert_eq!(smatch("a\0b", "%z"), "\0");
        assert_eq!(smatch("aaa", "a-b"), "nil");
        assert_eq!(smatch("aaab", "a-b"), "aaab");
        assert_eq!(sfind("aXb", "[%u]"), "2|2");
        assert_eq!(sfind("a]b", "[]]"), "2|2");
        assert_eq!(find_aux("a-b", "[a-]", 2, true, false).unwrap(), "2|2");
        assert_eq!(smatch("a$b", "a$b"), "a$b");
        assert_eq!(smatch("ab", "b$"), "b");
        assert_eq!(find_plain(b"hello", b"ll"), Some(2));
        assert_eq!(find_plain(b"aab", b"ab"), Some(1));
        assert_eq!(find_plain(b"ab", b"abc"), None);
        assert!(!has_pattern(b"b)"));
        assert!(has_pattern(b"a.b"));
    }

    #[test]
    fn errors() {
        // luajit -e 'print(pcall(string.find, "abc", p))'
        for (p, e) in [
            ("%", PatError::EndsWithEscape),
            ("[a", PatError::MissingBracket),
            ("%f", PatError::MissingFrontierBracket),
            ("%1", PatError::InvalidCaptureIndex),
            ("(()", PatError::UnfinishedCapture),
            ("%b", PatError::Unbalanced),
            ("(%1)", PatError::InvalidCaptureIndex),
        ] {
            assert_eq!(find_aux("abc", p, 1, true, false), Err(e), "{p:?}");
        }
        assert_eq!(PatError::EndsWithEscape.message(), "malformed pattern (ends with '%')");
        assert_eq!(PatError::MissingBracket.message(), "malformed pattern (missing ']')");
        assert_eq!(PatError::MissingFrontierBracket.message(), "missing '[' after '%f' in pattern");
        // pcall(string.gsub, "abc", "b", "%2") / ("abc", "b)", "")
        assert_eq!(gsub("abc", "b", "%2", None), Err(PatError::InvalidCaptureIndex));
        assert_eq!(gsub("abc", "b)", "", None), Err(PatError::InvalidPatternCapture));
        // Recursion and capture limits.
        let a300 = "a".repeat(300);
        assert_eq!(find_aux(&a300, &"a?".repeat(250), 1, true, false), Err(PatError::TooComplex));
        assert_eq!(find_aux(&a300, &"a?".repeat(199), 1, true, false).unwrap(), "1|199");
        assert_eq!(find_aux("a", &"()".repeat(33), 1, true, false), Err(PatError::TooManyCaptures));
    }

    #[test]
    fn gsub_and_gmatch() {
        // luajit -e 'print(string.gsub(...))'
        assert_eq!(gsub("hello world", "(%w+)", "<%1>", None).unwrap(), ("<hello> <world>".into(), 2));
        assert_eq!(gsub("abc", "", "-", None).unwrap(), ("-a-b-c-".into(), 4));
        assert_eq!(gsub("abc", "^a", "x", None).unwrap(), ("xbc".into(), 1));
        assert_eq!(gsub("hello", "l", "%0%0", Some(1)).unwrap(), ("helllo".into(), 1));
        assert_eq!(gsub("abc", "(b)()", "[%2%1%%]", None).unwrap(), ("a[3b%]c".into(), 1));
        // luajit -e 'for k, v in string.gmatch("a=1, b=2", "(%w+)=(%w+)") do ... end'
        assert_eq!(gmatch("a=1, b=2", "(%w+)=(%w+)"), ["a1", "b2"]);
        assert_eq!(gmatch("one two", "%a*"), ["one", "", "two", ""]);
    }
}
