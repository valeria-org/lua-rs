//! Fixtures for the `package`, `debug` and `jit` libraries: each case runs the `luajit` binary
//! (argv[0] = `luajit`) in a fresh temporary copy of `tests/fixtures/libpkg/<dir>/` and
//! compares stdout, stderr and the exit status with the reference binary's.
//!
//! `<case>.expected` holds stdout, then `--- stderr` plus stderr if non-empty, then
//! `--- exit <status>`, with every `0x<hex>` normalized to `0x?`. Optional `<case>.env`
//! (`KEY=VALUE` lines) and `<case>.stdin` provide the environment and standard input. The
//! expectations were generated the same way with `~/projects/luajit-ref/luajit/src/luajit`
//! (with `LUA_PATH` pointing at its `jit/*.lua` for cases using `jit.*` modules, which this
//! binary embeds).
//!
//! Cases marked `needs_string` use `jit.bc` (which needs the `string` and `io` libraries);
//! they are skipped with a note on stderr while those libraries are not linked in.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/libpkg")
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let p = e.path();
        if p.is_dir() {
            copy_dir(&p, &to.join(e.file_name()));
        } else {
            std::fs::copy(&p, to.join(e.file_name())).unwrap();
        }
    }
}

fn command(args: &[&str], dir: &Path) -> Command {
    use std::os::unix::process::CommandExt;
    let mut c = Command::new(env!("CARGO_BIN_EXE_luajit"));
    c.arg0("luajit").args(args).current_dir(dir).env_remove("LUA_INIT").env_remove("LUA_PATH").env_remove("LUA_CPATH");
    c
}

fn have_string_lib() -> bool {
    command(&["-e", "assert(string.format and io.write)"], &fixture_dir())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Replace every `0x<hex digits>` by `0x?`.
fn normalize(s: &str) -> String {
    let mut r = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(p) = rest.find("0x") {
        r.push_str(&rest[..p + 2]);
        let after = &rest[p + 2..];
        let t = after.trim_start_matches(|c: char| c.is_ascii_hexdigit() && !c.is_ascii_uppercase());
        if t.len() != after.len() {
            r.push('?');
        }
        rest = t;
    }
    r.push_str(rest);
    r
}

fn run(case: &str, dir: &str, args: &[&str]) -> String {
    let tmp = std::env::temp_dir().join(format!("ljrs-libpkg-{}-{case}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    copy_dir(&fixture_dir().join(dir), &tmp);
    let mut cmd = command(if args.is_empty() { &["main.lua"] } else { args }, &tmp);
    if let Ok(env) = std::fs::read_to_string(fixture_dir().join(format!("{case}.env"))) {
        for l in env.lines().filter(|l| !l.is_empty()) {
            let (k, v) = l.split_once('=').unwrap();
            cmd.env(k, v);
        }
    }
    let stdin = match std::fs::File::open(fixture_dir().join(format!("{case}.stdin"))) {
        Ok(f) => Stdio::from(f),
        Err(_) => Stdio::null(),
    };
    let out = cmd.stdin(stdin).output().unwrap();
    let _ = std::fs::remove_dir_all(&tmp);
    let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
    if !out.stderr.is_empty() {
        s.push_str("--- stderr\n");
        s.push_str(&String::from_utf8_lossy(&out.stderr));
    }
    s.push_str(&format!("--- exit {}\n", out.status.code().unwrap_or(-1)));
    normalize(&s)
}

fn fixture(case: &str, dir: &str, args: &[&str]) {
    let expected = std::fs::read_to_string(fixture_dir().join(format!("{case}.expected"))).unwrap();
    let got = run(case, dir, args);
    if got != expected {
        for (i, (g, e)) in got.lines().zip(expected.lines()).enumerate() {
            if g != e {
                panic!("{case}: first difference at line {}:\n  got:      {g:?}\n  expected: {e:?}\n\nfull output:\n{got}", i + 1);
            }
        }
        panic!("{case}: output length differs:\n{got}");
    }
}

fn fixture_with_string(case: &str, dir: &str, args: &[&str]) {
    if !have_string_lib() {
        eprintln!("skipped {case}: string/io libraries not available");
        return;
    }
    fixture(case, dir, args);
}

#[test]
fn package_require_module_searchers_and_errors() {
    fixture("require", "require", &[]);
}

#[test]
fn package_loadlib_c_searchers_and_env_paths() {
    fixture("loadlib", "loadlib", &[]);
}

#[test]
fn debug_info_locals_upvalues_metatables_fenv() {
    fixture("debuglib", "debuglib", &[]);
}

#[test]
fn debug_hooks() {
    fixture("hooks", "hooks", &[]);
}

#[test]
fn debug_traceback_forms_and_threads() {
    fixture("traceback", "traceback", &[]);
}

#[test]
fn debug_debug_repl() {
    fixture("debugrepl", "debugrepl", &[]);
}

#[test]
fn jit_functions_util_attach_profile() {
    fixture("jitlib", "jitlib", &[]);
}

#[test]
fn bclist_program_runs() {
    fixture("bclist_run", "bclist", &[]);
}

#[test]
fn jit_bc_listing_matches_reference_bl() {
    fixture_with_string("bclist_l", "bclist", &["-bl", "main.lua"]);
}

#[test]
fn jit_bc_dump_api() {
    fixture_with_string("bclist_dump", "bclist", &["dump.lua"]);
}

#[test]
fn jit_bc_vmevent_listing() {
    fixture_with_string("bclist_jbc", "bclist", &["-jbc=-", "main.lua"]);
}
