//! FFI C library loader (Linux, dlopen).
//!
//! Port of lj_clib.c. The symbol cache table and the userdata wrapping a
//! [`CLibrary`] belong to the integration layer (`ffi.load`, `ffi.C`).

use std::ffi::{CStr, CString, c_void};

use super::FfiError;
use super::ctype::*;

/// Namespace for C library indexing.
pub const CLNS_INDEX: u32 = (1 << CT_FUNC) | (1 << CT_EXTERN) | (1 << CT_CONSTVAL);

/// C library namespace: an opaque handle for the dynamic library loader.
#[derive(Debug)]
pub struct CLibrary {
    handle: *mut c_void,
}

/// Value of a resolved C library symbol (`lj_clib_index`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ClibValue {
    /// A 32 bit constant: `setintV` value.
    Int(i32),
    /// An unsigned 32 bit constant with the sign bit set: `setnumV` value.
    Num(f64),
    /// A function or extern variable: a new cdata of type `ctid` holding the
    /// pointer `ptr` (a reference for externs, the function address for functions).
    Symbol { ctid: CTypeID, ptr: *mut c_void },
}

/// Truncate at the first NUL (C string semantics) and convert.
fn cstring(s: &[u8]) -> CString {
    let end = s.iter().position(|&b| b == 0).unwrap_or(s.len());
    CString::new(&s[..end]).expect("no interior NUL after truncation")
}

fn dlerror_msg() -> Option<Vec<u8>> {
    // SAFETY: dlerror returns NULL or a valid C string owned by libdl.
    let e = unsafe { libc::dlerror() };
    if e.is_null() {
        None
    } else {
        // SAFETY: non-null result of dlerror is a NUL-terminated string.
        Some(unsafe { CStr::from_ptr(e) }.to_bytes().to_vec())
    }
}

/// Add the `lib` prefix and `.so` suffix to bare library names (`clib_extname`).
pub fn extname(name: &[u8]) -> Vec<u8> {
    let mut name = name.to_vec();
    if !name.contains(&b'/') {
        if !name.contains(&b'.') {
            name.extend_from_slice(b".so");
        }
        if !name.starts_with(b"lib") {
            name.splice(0..0, *b"lib");
        }
    }
    name
}

/// Check for a recognized ld script line (`clib_check_lds`).
fn check_lds(buf: &[u8]) -> Option<Vec<u8>> {
    if !(buf.starts_with(b"GROUP") || buf.starts_with(b"INPUT")) {
        return None;
    }
    let mut p = buf.iter().position(|&b| b == b'(')? + 1;
    while buf.get(p) == Some(&b' ') {
        p += 1;
    }
    let mut e = p;
    while e < buf.len() && buf[e] != 0 && buf[e] != b' ' && buf[e] != b')' {
        e += 1;
    }
    Some(buf[p..e].to_vec())
}

/// Read lines like `fgets(buf, 256, fp)`.
fn fgets_lines(data: &[u8]) -> impl Iterator<Item = &[u8]> {
    let mut pos = 0;
    std::iter::from_fn(move || {
        if pos >= data.len() {
            return None;
        }
        let rest = &data[pos..];
        let mut len = rest.len().min(255);
        if let Some(nl) = rest[..len].iter().position(|&b| b == b'\n') {
            len = nl + 1;
        }
        pos += len;
        Some(&rest[..len])
    })
}

/// Quick and dirty solution to resolve shared library name from ld script
/// (`clib_resolve_lds`).
fn resolve_lds(name: &[u8]) -> Option<Vec<u8>> {
    use std::os::unix::ffi::OsStrExt;
    let path = std::path::Path::new(std::ffi::OsStr::from_bytes(name));
    let data = std::fs::read(path).ok()?;
    let mut lines = fgets_lines(&data);
    let first = lines.next()?;
    if first.starts_with(b"/* GNU ld script") {
        // ld script magic? Check all lines.
        lines.find_map(check_lds)
    } else {
        // Otherwise check only the first line.
        check_lds(first)
    }
}

fn dlopen(name: &[u8], global: bool) -> *mut c_void {
    let flags = libc::RTLD_LAZY
        | if global {
            libc::RTLD_GLOBAL
        } else {
            libc::RTLD_LOCAL
        };
    let c = cstring(name);
    // SAFETY: c is a valid NUL-terminated path.
    unsafe { libc::dlopen(c.as_ptr(), flags) }
}

impl CLibrary {
    /// The default C library namespace (`ffi.C`): `RTLD_DEFAULT`, i.e. the
    /// main program and all libraries loaded with global visibility (libc).
    pub fn default_namespace() -> CLibrary {
        CLibrary {
            handle: libc::RTLD_DEFAULT,
        }
    }

    /// Load a C library (`ffi.load(name, global)`, `clib_loadlib`). The error
    /// message is raised with `lj_err_callermsg`.
    pub fn load(name: &[u8], global: bool) -> Result<CLibrary, FfiError> {
        let h = dlopen(&extname(name), global);
        if !h.is_null() {
            return Ok(CLibrary { handle: h });
        }
        let mut err = dlerror_msg();
        if let Some(e) = &err
            && e.first() == Some(&b'/')
            && let Some(colon) = e.iter().position(|&b| b == b':')
            && let Some(lds) = resolve_lds(&e[..colon])
        {
            let h = dlopen(&lds, global);
            if !h.is_null() {
                return Ok(CLibrary { handle: h });
            }
            err = dlerror_msg();
        }
        Err(FfiError::Caller(
            err.unwrap_or_else(|| b"dlopen failed".to_vec()),
        ))
    }

    /// Whether this is the default namespace.
    pub fn is_default(&self) -> bool {
        self.handle == libc::RTLD_DEFAULT
    }

    /// The raw loader handle.
    pub fn handle(&self) -> *mut c_void {
        self.handle
    }

    /// Look up a symbol (`clib_getsym`). On failure returns the `dlerror()`
    /// message, which upstream raises as `cannot resolve symbol` error text.
    pub fn sym(&self, name: &[u8]) -> Result<*mut c_void, FfiError> {
        let c = cstring(name);
        // SAFETY: clear any stale error state first; dlerror has no preconditions.
        unsafe { libc::dlerror() };
        // SAFETY: handle is RTLD_DEFAULT or a live dlopen handle; c is valid.
        let p = unsafe { libc::dlsym(self.handle, c.as_ptr()) };
        if p.is_null() {
            let msg = dlerror_msg().unwrap_or_else(|| {
                let mut m = b"cannot resolve symbol '".to_vec();
                m.extend_from_slice(c.as_bytes());
                m.push(b'\'');
                m
            });
            return Err(FfiError::Caller(msg));
        }
        Ok(p)
    }

    /// Unload a C library (`lj_clib_unload`). Idempotent.
    pub fn unload(&mut self) {
        if !self.handle.is_null() && self.handle != libc::RTLD_DEFAULT {
            // SAFETY: handle is a live dlopen handle, closed only once.
            unsafe { libc::dlclose(self.handle) };
        }
        self.handle = std::ptr::null_mut();
    }

    /// Resolve a declared name in this library (`lj_clib_index` without the
    /// cache): constants yield numbers, functions/externs yield a cdata of the
    /// declared type pointing to the symbol (honoring `__asm__` redirects).
    pub fn index(&self, cts: &CTState, name: &[u8]) -> Result<ClibValue, ClibError> {
        let id = cts.getname(name, CLNS_INDEX);
        if id == 0 {
            return Err(ClibError::NoDecl);
        }
        let ct = cts.get(id);
        if ctype_isconstval(ct.info) {
            let ctt = cts.get(cts.child(id));
            debug_assert!(
                ctype_isinteger(ctt.info) && ctt.size <= 4,
                "only 32 bit const supported"
            );
            if (ctt.info & CTF_UNSIGNED) != 0 && (ct.size as i32) < 0 {
                Ok(ClibValue::Num(ct.size as f64))
            } else {
                Ok(ClibValue::Int(ct.size as i32))
            }
        } else {
            debug_assert!(
                ctype_isfunc(ct.info) || ctype_isextern(ct.info),
                "unexpected ctype {:08x} in clib",
                ct.info
            );
            let sym = extsym(cts, id, name);
            let p = self.sym(sym).map_err(ClibError::Dl)?;
            Ok(ClibValue::Symbol { ctid: id, ptr: p })
        }
    }
}

impl Drop for CLibrary {
    fn drop(&mut self) {
        self.unload();
    }
}

/// Error from [`CLibrary::index`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClibError {
    /// No declaration: `missing declaration for symbol '<name>'` (raised
    /// with `lj_err_callerv` by the integration layer).
    NoDecl,
    /// Symbol lookup failed with this `dlerror()` message.
    Dl(FfiError),
}

/// Get redirected or mangled external symbol (`clib_extsym`).
pub fn extsym<'a>(cts: &'a CTState, id: CTypeID, name: &'a [u8]) -> &'a [u8] {
    let ct = cts.get(id);
    if ct.sib != 0 {
        let a = ct.sib as CTypeID;
        if ctype_isxattrib(cts.get(a).info, CTA_REDIR) {
            return cts.name_of(a).unwrap_or(name);
        }
    }
    name
}
