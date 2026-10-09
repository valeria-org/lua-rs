//! C API tests: C code compiled with `gcc` at test time against `include/`.
//!
//! * `embed.c` embeds `libluajit.so` (state creation, stack ops, tables, userdata, closures,
//!   registry refs, errors, coroutines with C yields, reader-based loads, dump, debug API,
//!   hooks, tracebacks, buffers, panics). Its output with our library must equal its output
//!   with the reference `libluajit.so`, also when collecting at every GC check point.
//! * `cmod.c` is a C module loaded by the `luajit` binary through `require`/`package.loadlib`
//!   (`test_cmod.lua`).
//! * lua-cjson (vendored in `tests/capi/lua-cjson`) is built from source and exercised through
//!   `require` (`test_cjson.lua`).
//! * The C API tests of the upstream test suite (`tests/capi/upstream`: `ctest.c`,
//!   `cpptest.cpp` with C++ exceptions crossing Lua frames) run with their Lua drivers.
//!
//! Expected outputs come from the reference build (`~/projects/luajit-ref/luajit/src`; its
//! `libluajit.so` is built with `make XCFLAGS=-DLUAJIT_DISABLE_JIT` if missing). Without the
//! reference tree, the committed `tests/capi/*.expected` files (generated from it) are used.
//! The tests are skipped with a message if `gcc` is not available.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn src_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/capi")
}

fn include_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("include")
}

fn have_gcc() -> bool {
    Command::new("gcc").arg("--version").output().is_ok_and(|o| o.status.success())
}

/// Our `luajit` binary and `libluajit.so` (next to it in the target directory).
fn our_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_luajit"))
}

/// Cargo does not build the cdylib for integration tests: build it with the same profile.
fn our_lib() -> PathBuf {
    let bin = our_bin();
    let p = bin.with_file_name("libluajit.so");
    let target_dir = bin.parent().and_then(Path::parent).expect("target dir");
    let release = bin.parent().is_some_and(|d| d.ends_with("release"));
    let mut c = Command::new(env!("CARGO"));
    c.args(["build", "--lib", "--manifest-path", concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml")]).env("CARGO_TARGET_DIR", target_dir);
    if release {
        c.arg("--release");
    }
    let st = c.status().expect("run cargo build --lib");
    assert!(st.success() && p.exists(), "building {} failed", p.display());
    p
}

/// The reference source directory, if present.
fn ref_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    let d = Path::new(&home).join("projects/luajit-ref/luajit/src");
    d.join("luajit.c").exists().then_some(d)
}

/// Reference `libluajit.so`, built if missing.
fn ref_lib() -> Option<PathBuf> {
    let d = ref_dir()?;
    let so = d.join("libluajit.so");
    if !so.exists() {
        let st = Command::new("make")
            .arg("-C")
            .arg(d.parent().expect("luajit dir"))
            .arg("XCFLAGS=-DLUAJIT_DISABLE_JIT")
            .status()
            .expect("run make for the reference build");
        assert!(st.success(), "building the reference libluajit.so failed");
    }
    so.exists().then_some(so)
}

fn ref_bin() -> Option<PathBuf> {
    let b = ref_dir()?.join("luajit");
    b.exists().then_some(b)
}

/// Fresh scratch directory for one test.
fn scratch(name: &str) -> PathBuf {
    let d = Path::new(env!("CARGO_TARGET_TMPDIR")).join("capi").join(name);
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).expect("create scratch dir");
    d
}

fn gcc(args: &[&std::ffi::OsStr]) {
    let o = Command::new("gcc").args(args).output().expect("run gcc");
    assert!(o.status.success(), "gcc {args:?} failed:\n{}", String::from_utf8_lossy(&o.stderr));
}

/// Link `embed.c` against the library `lib` (by path, found at run time through an rpath to a
/// directory containing it under its soname).
fn build_embed(dir: &Path, lib: &Path, soname: Option<&str>, out: &str) -> PathBuf {
    let libdir = dir.join(format!("{out}-lib"));
    std::fs::create_dir_all(&libdir).expect("lib dir");
    let name = soname.unwrap_or("libluajit.so");
    let link = libdir.join(name);
    let _ = std::fs::remove_file(&link);
    std::os::unix::fs::symlink(lib, &link).expect("symlink library");
    let exe = dir.join(out);
    let inc = format!("-I{}", include_dir().display());
    let rpath = format!("-Wl,-rpath,{}", libdir.display());
    gcc(&[
        "-O1".as_ref(),
        "-Wall".as_ref(),
        inc.as_ref(),
        src_dir().join("embed.c").as_os_str(),
        "-o".as_ref(),
        exe.as_os_str(),
        link.as_os_str(),
        rpath.as_ref(),
        "-lm".as_ref(),
    ]);
    exe
}

/// stdout+stderr and exit code of a run.
fn result(o: Output) -> String {
    let mut s = String::from_utf8_lossy(&o.stdout).into_owned();
    s.push_str(&String::from_utf8_lossy(&o.stderr));
    s.push_str(&format!("[exit {}]\n", o.status.code().map_or("signal".to_string(), |c| c.to_string())));
    s
}

/// Compare with the reference output (or the committed expected file). With `LJRS_BLESS`
/// set, the reference output is also written to the expected file.
fn check(name: &str, ours: &str, reference: Option<String>) {
    let exp_path = src_dir().join(format!("{name}.expected"));
    if let (Some(r), Some(_)) = (&reference, std::env::var_os("LJRS_BLESS")) {
        std::fs::write(&exp_path, r).expect("write expected output");
    }
    let expected = match reference {
        Some(r) => r,
        None => std::fs::read_to_string(&exp_path).expect("expected output"),
    };
    if ours != expected {
        let a: Vec<&str> = expected.lines().collect();
        let b: Vec<&str> = ours.lines().collect();
        let first = a.iter().zip(&b).position(|(x, y)| x != y).unwrap_or(a.len().min(b.len()));
        panic!(
            "{name}: output differs from the reference at line {}:\n  expected: {:?}\n  got:      {:?}\n--- full output ---\n{ours}",
            first + 1,
            a.get(first),
            b.get(first)
        );
    }
}

#[test]
fn embed_matches_reference() {
    if !have_gcc() {
        eprintln!("SKIPPED: gcc not found, C API tests need a C compiler");
        return;
    }
    let dir = scratch("embed");
    let ours = build_embed(&dir, &our_lib(), None, "embed_rs");
    let reference = ref_lib().map(|l| build_embed(&dir, &l, Some("libluajit-5.1.so.2"), "embed_ref"));
    for (mode, name) in [(None, "embed"), (Some("panic"), "embed_panic"), (Some("atpanic"), "embed_atpanic")] {
        let run = |exe: &Path| {
            let mut c = Command::new(exe);
            c.current_dir(&dir);
            if let Some(m) = mode {
                c.arg(m);
            }
            result(c.output().expect("run embed"))
        };
        check(name, &run(&ours), reference.as_deref().map(run));
    }
    // Regression check for rooting bugs: collect at every GC check point (EMBED_GCSTRESS sets
    // the pacing, LUAJIT_RS_GCSTRESS the stress mode) and still produce the reference output.
    let o = Command::new(&ours)
        .current_dir(&dir)
        .env("EMBED_GCSTRESS", "1")
        .env("LUAJIT_RS_GCSTRESS", "1")
        .output()
        .expect("run embed");
    let reference = reference.as_deref().map(|r| result(Command::new(r).current_dir(&dir).output().expect("run embed")));
    check("embed", &result(o), reference);
}

/// Run a Lua script from `tests/capi` (copied to `dir`) with `bin`, cwd `dir`.
fn run_script(bin: &Path, dir: &Path, script: &str, pre: &str) -> String {
    let o = Command::new(bin)
        .current_dir(dir)
        .env("LUA_CPATH", "./?.so")
        .env("LUA_PATH", "./?.lua")
        .args(["-e", pre, script])
        .output()
        .expect("run luajit");
    result(o)
}

#[test]
fn c_module_via_require_and_loadlib() {
    if !have_gcc() {
        eprintln!("SKIPPED: gcc not found, C API tests need a C compiler");
        return;
    }
    let dir = scratch("cmod");
    let inc = format!("-I{}", include_dir().display());
    let so = dir.join("cmod.so");
    gcc(&["-shared".as_ref(), "-fPIC".as_ref(), "-O1".as_ref(), "-Wall".as_ref(), inc.as_ref(), src_dir().join("cmod.c").as_os_str(), "-o".as_ref(), so.as_os_str()]);
    std::fs::copy(src_dir().join("test_cmod.lua"), dir.join("test_cmod.lua")).expect("copy script");
    let pre = "CMOD_PATH='./cmod.so'";
    let ours = run_script(&our_bin(), &dir, "test_cmod.lua", pre);
    let reference = ref_bin().map(|b| run_script(&b, &dir, "test_cmod.lua", pre));
    check("cmod", &ours, reference);
}

#[test]
fn lua_cjson_from_source() {
    if !have_gcc() {
        eprintln!("SKIPPED: gcc not found, C API tests need a C compiler");
        return;
    }
    let dir = scratch("cjson");
    let cj = src_dir().join("lua-cjson");
    // LuaJIT 2.1's lauxlib.h declares luaL_setfuncs; rename lua-cjson's static 5.1 fallback
    // (the exported one is used).
    let src = std::fs::read_to_string(cj.join("lua_cjson.c")).expect("lua_cjson.c");
    let patched = src.replace("static void luaL_setfuncs (", "static void cjson_compat_setfuncs (");
    assert_ne!(src, patched, "lua_cjson.c changed: compat function not found");
    let build_src = dir.join("lua_cjson.c");
    std::fs::write(&build_src, patched).expect("write build copy");
    let inc = format!("-I{}", include_dir().display());
    let inc2 = format!("-I{}", cj.display());
    let so = dir.join("cjson.so");
    gcc(&[
        "-shared".as_ref(),
        "-fPIC".as_ref(),
        "-O2".as_ref(),
        "-Wno-unused-function".as_ref(),
        inc.as_ref(),
        inc2.as_ref(),
        build_src.as_os_str(),
        cj.join("strbuf.c").as_os_str(),
        cj.join("fpconv.c").as_os_str(),
        "-o".as_ref(),
        so.as_os_str(),
    ]);
    std::fs::copy(src_dir().join("test_cjson.lua"), dir.join("test_cjson.lua")).expect("copy script");
    let ours = run_script(&our_bin(), &dir, "test_cjson.lua", "");
    let reference = ref_bin().map(|b| run_script(&b, &dir, "test_cjson.lua", ""));
    check("cjson", &ours, reference);
}

fn have_gxx() -> bool {
    Command::new("g++").arg("--version").output().is_ok_and(|o| o.status.success())
}

/// Upstream misc/api_call.lua, misc/hook_active.lua (ctest.c), misc/catch_wrap.lua and
/// sysdep/catch_cpp.lua (cpptest.cpp): they print nothing and exit 0 on success. Then
/// `test_cppexc.lua`: C++ exceptions crossing Lua frames, caught by C++ (unchanged), by `pcall`,
/// coroutines and wrappers, compared with the reference.
#[test]
fn upstream_ctest_cpptest() {
    if !have_gcc() {
        eprintln!("SKIPPED: gcc not found, C API tests need a C compiler");
        return;
    }
    let dir = scratch("upstream");
    let up = src_dir().join("upstream");
    let inc = format!("-I{}", include_dir().display());
    let so = dir.join("ctest.so");
    gcc(&["-shared".as_ref(), "-fPIC".as_ref(), "-O1".as_ref(), inc.as_ref(), up.join("ctest.c").as_os_str(), "-o".as_ref(), so.as_os_str()]);
    let mut scripts = vec!["api_call.lua", "hook_active.lua"];
    let cpp = have_gxx();
    if cpp {
        let so = dir.join("cpptest.so");
        let o = Command::new("g++")
            .args(["-shared".as_ref(), "-fPIC".as_ref(), "-O1".as_ref(), std::ffi::OsStr::new(&inc), up.join("cpptest.cpp").as_os_str(), "-o".as_ref(), so.as_os_str()])
            .output()
            .expect("run g++");
        assert!(o.status.success(), "g++ failed:\n{}", String::from_utf8_lossy(&o.stderr));
        scripts.push("catch_wrap.lua");
        scripts.push("catch_cpp.lua");
    } else {
        eprintln!("SKIPPED catch_wrap.lua, catch_cpp.lua, test_cppexc.lua: g++ not found");
    }
    for s in scripts {
        std::fs::copy(up.join(s), dir.join(s)).expect("copy script");
        let out = run_script(&our_bin(), &dir, s, "");
        assert_eq!(out, "[exit 0]\n", "{s} failed");
    }
    if cpp {
        std::fs::copy(src_dir().join("test_cppexc.lua"), dir.join("test_cppexc.lua")).expect("copy script");
        let ours = run_script(&our_bin(), &dir, "test_cppexc.lua", "");
        let reference = ref_bin().map(|b| run_script(&b, &dir, "test_cppexc.lua", ""));
        check("cppexc", &ours, reference);
    }
}
