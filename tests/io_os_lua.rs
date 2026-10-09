//! Runs the io/os fixtures (`tests/fixtures/iolib/*.lua`) with the `luajit` binary and
//! compares stdout, stderr and the exit status with the reference binary's.
//!
//! Each fixture runs as `luajit <name>.lua` in a fresh temporary directory (so chunk names and
//! file names are stable), with `TZ=UTC`, `LJRS_TEST_VAR=hello` and stdin from
//! `<name>.stdin` (or empty). The expectation `<name>.expected` holds stdout, then
//! `--- stderr` plus stderr if non-empty, then `--- exit <status>`; it was generated the same
//! way with `~/projects/luajit-ref/luajit/src/luajit`.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/iolib")
}

fn run(name: &str) -> String {
    let dir = std::env::temp_dir().join(format!("ljrs-iolib-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let src = fixture_dir().join(format!("{name}.lua"));
    std::fs::copy(&src, dir.join(format!("{name}.lua"))).unwrap();
    let stdin = match std::fs::File::open(fixture_dir().join(format!("{name}.stdin"))) {
        Ok(f) => Stdio::from(f),
        Err(_) => Stdio::null(),
    };
    let out = Command::new(env!("CARGO_BIN_EXE_luajit"))
        .arg(format!("{name}.lua"))
        .current_dir(&dir)
        .env("TZ", "UTC")
        .env("LJRS_TEST_VAR", "hello")
        .env_remove("LJRS_TEST_UNSET")
        .stdin(stdin)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
    if !out.stderr.is_empty() {
        s.push_str("--- stderr\n");
        s.push_str(&String::from_utf8_lossy(&out.stderr));
    }
    s.push_str(&format!("--- exit {}\n", out.status.code().unwrap_or(-1)));
    s
}

fn fixture(name: &str) {
    let expected = std::fs::read_to_string(fixture_dir().join(format!("{name}.expected"))).unwrap();
    let got = run(name);
    if got != expected {
        for (i, (g, e)) in got.lines().zip(expected.lines()).enumerate() {
            if g != e {
                panic!("{name}: first difference at line {}:\n  got:      {g:?}\n  expected: {e:?}\n\nfull output:\n{got}", i + 1);
            }
        }
        panic!("{name}: output length differs:\n{got}");
    }
}

#[test]
fn io_files_read_write_seek_lines_popen() {
    fixture("io_basic");
}

#[test]
fn io_stdin_formats_and_lines() {
    fixture("stdin_read");
}

#[test]
fn os_date_time_env_files_execute_locale() {
    fixture("os_basic");
}

#[test]
fn io_os_error_messages() {
    fixture("errnames");
}

#[test]
fn os_exit_true_closes_state() {
    fixture("exit_true");
}

#[test]
fn os_exit_false_skips_finalizers() {
    fixture("exit_false");
}

#[test]
fn os_exit_number_flushes_stdio() {
    fixture("exit_num");
}

#[test]
fn os_exit_bad_argument_and_default() {
    fixture("exit_err");
}
