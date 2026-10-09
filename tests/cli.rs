//! Command-line frontend tests: runs the built `luajit` binary (argv[0] = `luajit`, cwd =
//! `tests/fixtures/cli`) and compares stdout, stderr and exit status with the reference
//! binary's (expectations generated from it; `[C]: at 0x...` addresses normalized).
//!
//! Cases marked `libs` need the `package`, `os` and `jit` libraries; they are skipped (with
//! a note on stderr) while those libraries are not linked into the binary.

use std::io::Write;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};

struct Case {
    args: &'static [&'static str],
    stdin: Option<&'static str>,
    env: &'static [(&'static str, &'static str)],
    libs: bool,
    out: &'static str,
    err: &'static str,
    code: i32,
}

const DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/cli");

fn command(args: &[&str]) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_luajit"));
    c.arg0("luajit").args(args).current_dir(DIR).env_remove("LUA_INIT");
    c
}

fn have_libs() -> bool {
    command(&["-e", "assert(package and os and jit)"]).stderr(Stdio::null()).status().is_ok_and(|s| s.success())
}

fn normalize(s: &[u8]) -> String {
    let s = String::from_utf8_lossy(s);
    let mut r = String::new();
    let mut rest = &*s;
    while let Some(p) = rest.find("[C]: at 0x") {
        r.push_str(&rest[..p + 10]);
        rest = rest[p + 10..].trim_start_matches(|c: char| c.is_ascii_hexdigit());
        r.push('?');
    }
    r.push_str(rest);
    r
}

fn check(c: Case) {
    if c.libs && !have_libs() {
        eprintln!("skipped: package/os/jit libraries not available");
        return;
    }
    let mut cmd = command(c.args);
    cmd.envs(c.env.iter().copied()).stdout(Stdio::piped()).stderr(Stdio::piped()).stdin(Stdio::piped());
    let mut child = cmd.spawn().expect("spawn luajit");
    let input = c.stdin.map(|f| std::fs::read(format!("{DIR}/{f}")).unwrap()).unwrap_or_default();
    let mut si = child.stdin.take().unwrap();
    let w = std::thread::spawn(move || {
        let _ = si.write_all(&input);
    });
    let o = child.wait_with_output().unwrap();
    w.join().unwrap();
    assert_eq!(normalize(&o.stdout), c.out, "stdout");
    assert_eq!(normalize(&o.stderr), c.err, "stderr");
    assert_eq!(o.status.code(), Some(c.code), "exit status");
}

#[test]
fn script_args() {
    check(Case { args: &["args.lua", "a", "b c"], stdin: None, env: &[], libs: false, out: r###"a	b c
-3	nil
-2	nil
-1	luajit
0	args.lua
1	a
2	b c
"###, err: r###""###, code: 0 });
}
#[test]
fn e_chain() {
    check(Case { args: &["-e", "x=1", "-e", "print(x)", "-eprint(x+1)", "args.lua", "1"], stdin: None, env: &[], libs: false, out: r###"1
2
1
-3	-e
-2	print(x)
-1	-eprint(x+1)
0	args.lua
1	1
"###, err: r###""###, code: 0 });
}
#[test]
fn e_changes_arg() {
    check(Case { args: &["-e", "arg[1]=5 arg[3]='z'", "args.lua", "1", "2"], stdin: None, env: &[], libs: false, out: r###"5	2	z
-3	luajit
-2	-e
-1	arg[1]=5 arg[3]='z'
0	args.lua
1	5
2	2
3	z
"###, err: r###""###, code: 0 });
}
#[test]
fn version() {
    check(Case { args: &["-v"], stdin: None, env: &[], libs: false, out: r###"LuaJIT 2.1.1788856981 -- Copyright (C) 2005-2026 Mike Pall. https://luajit.org/
"###, err: r###""###, code: 0 });
}
#[test]
fn version_e() {
    check(Case { args: &["-v", "-e", "print(1)"], stdin: None, env: &[], libs: false, out: r###"LuaJIT 2.1.1788856981 -- Copyright (C) 2005-2026 Mike Pall. https://luajit.org/
1
"###, err: r###""###, code: 0 });
}
#[test]
fn bad_option() {
    check(Case { args: &["-x"], stdin: None, env: &[], libs: false, out: r###""###, err: r###"usage: luajit [options]... [script [args]...].
Available options are:
  -e chunk  Execute string 'chunk'.
  -l name   Require library 'name'.
  -b ...    Save or list bytecode.
  -j cmd    Perform LuaJIT control command.
  -O[opt]   Control LuaJIT optimizations.
  -i        Enter interactive mode after executing 'script'.
  -v        Show version information.
  -E        Ignore environment variables.
  --        Stop handling options.
  -         Execute stdin and stop handling options.
"###, code: 1 });
}
#[test]
fn bad_tail() {
    check(Case { args: &["-vv"], stdin: None, env: &[], libs: false, out: r###""###, err: r###"usage: luajit [options]... [script [args]...].
Available options are:
  -e chunk  Execute string 'chunk'.
  -l name   Require library 'name'.
  -b ...    Save or list bytecode.
  -j cmd    Perform LuaJIT control command.
  -O[opt]   Control LuaJIT optimizations.
  -i        Enter interactive mode after executing 'script'.
  -v        Show version information.
  -E        Ignore environment variables.
  --        Stop handling options.
  -         Execute stdin and stop handling options.
"###, code: 1 });
}
#[test]
fn missing_e_arg() {
    check(Case { args: &["-e"], stdin: None, env: &[], libs: false, out: r###""###, err: r###"usage: luajit [options]... [script [args]...].
Available options are:
  -e chunk  Execute string 'chunk'.
  -l name   Require library 'name'.
  -b ...    Save or list bytecode.
  -j cmd    Perform LuaJIT control command.
  -O[opt]   Control LuaJIT optimizations.
  -i        Enter interactive mode after executing 'script'.
  -v        Show version information.
  -E        Ignore environment variables.
  --        Stop handling options.
  -         Execute stdin and stop handling options.
"###, code: 1 });
}
#[test]
fn b_after_option() {
    check(Case { args: &["-v", "-b"], stdin: None, env: &[], libs: false, out: r###""###, err: r###"usage: luajit [options]... [script [args]...].
Available options are:
  -e chunk  Execute string 'chunk'.
  -l name   Require library 'name'.
  -b ...    Save or list bytecode.
  -j cmd    Perform LuaJIT control command.
  -O[opt]   Control LuaJIT optimizations.
  -i        Enter interactive mode after executing 'script'.
  -v        Show version information.
  -E        Ignore environment variables.
  --        Stop handling options.
  -         Execute stdin and stop handling options.
"###, code: 1 });
}
#[test]
fn error_traceback() {
    check(Case { args: &["err.lua"], stdin: None, env: &[], libs: false, out: r###""###, err: r###"luajit: err.lua:1: boom
stack traceback:
	[C]: in function 'error'
	err.lua:1: in function 'f'
	err.lua:2: in main chunk
	[C]: at 0x?
"###, code: 1 });
}
#[test]
fn error_table() {
    check(Case { args: &["-e", "error({})"], stdin: None, env: &[], libs: false, out: r###""###, err: r###"luajit: (error object is not a string)
"###, code: 1 });
}
#[test]
fn error_nil() {
    check(Case { args: &["-e", "error()"], stdin: None, env: &[], libs: false, out: r###""###, err: r###""###, code: 1 });
}
#[test]
fn error_tostring() {
    check(Case { args: &["-e", "error(setmetatable({}, {__tostring=function() return 'TS' end}))"], stdin: None, env: &[], libs: false, out: r###""###, err: r###"luajit: TS
stack traceback:
	[C]: in function 'error'
	(command line):1: in main chunk
	[C]: at 0x?
"###, code: 1 });
}
#[test]
fn syntax_error() {
    check(Case { args: &["-e", "x x"], stdin: None, env: &[], libs: false, out: r###""###, err: r###"luajit: (command line):1: '=' expected near 'x'
"###, code: 1 });
}
#[test]
fn missing_file() {
    check(Case { args: &["nofile.lua"], stdin: None, env: &[], libs: false, out: r###""###, err: r###"luajit: cannot open nofile.lua: No such file or directory
"###, code: 1 });
}
#[test]
fn stdin_dash() {
    check(Case { args: &["-", "x", "y"], stdin: Some("stdin.lua"), env: &[], libs: false, out: r###"from stdin	x	y
"###, err: r###"luajit: stdin:2: in stdin
stack traceback:
	[C]: in function 'error'
	stdin:2: in main chunk
	[C]: at 0x?
"###, code: 1 });
}
#[test]
fn stdin_noargs() {
    check(Case { args: &[], stdin: Some("stdin.lua"), env: &[], libs: false, out: r###"from stdin
"###, err: r###"luajit: stdin:2: in stdin
stack traceback:
	[C]: in function 'error'
	stdin:2: in main chunk
	[C]: at 0x?
"###, code: 0 });
}
#[test]
fn dashdash() {
    check(Case { args: &["--", "args.lua", "q"], stdin: None, env: &[], libs: false, out: r###"q
-3	nil
-2	luajit
-1	--
0	args.lua
1	q
"###, err: r###""###, code: 0 });
}
#[test]
fn lua_init() {
    check(Case { args: &["-e", "print(2)"], stdin: None, env: &[("LUA_INIT", "print('init')")], libs: false, out: r###"init
2
"###, err: r###""###, code: 0 });
}
#[test]
fn lua_init_file() {
    check(Case { args: &["args.lua", "z"], stdin: None, env: &[("LUA_INIT", "@init.lua")], libs: false, out: r###"init file
z
-3	nil
-2	nil
-1	luajit
0	args.lua
1	z
"###, err: r###""###, code: 0 });
}
#[test]
fn lua_init_missing() {
    check(Case { args: &["args.lua"], stdin: None, env: &[("LUA_INIT", "@nofile")], libs: false, out: r###""###, err: r###"luajit: cannot open nofile: No such file or directory
"###, code: 1 });
}
#[test]
fn lua_init_error() {
    check(Case { args: &["args.lua"], stdin: None, env: &[("LUA_INIT", "error('ie')")], libs: false, out: r###""###, err: r###"luajit: LUA_INIT:1: ie
stack traceback:
	[C]: in function 'error'
	LUA_INIT:1: in main chunk
	[C]: at 0x?
"###, code: 1 });
}
#[test]
fn ignore_env() {
    check(Case { args: &["-E", "-e", "print(2)"], stdin: None, env: &[("LUA_INIT", "print('init')")], libs: false, out: r###"2
"###, err: r###""###, code: 0 });
}
#[test]
fn finalizer_at_exit() {
    check(Case { args: &["gc.lua"], stdin: None, env: &[], libs: false, out: r###"end
"###, err: r###""###, code: 0 });
}
#[test]
fn opt_o3() {
    check(Case { args: &["-O3", "-e", "print(1)"], stdin: None, env: &[], libs: false, out: r###""###, err: r###"luajit: attempt to index a nil value
"###, code: 1 });
}
#[test]
fn require_lib() {
    check(Case { args: &["-l", "string", "-e", "print(1)"], stdin: None, env: &[], libs: true, out: r###"1
"###, err: r###""###, code: 0 });
}
#[test]
fn require_missing() {
    check(Case { args: &["-lnosuch"], stdin: None, env: &[], libs: true, out: r###""###, err: r###"luajit: module 'nosuch' not found:
	no field package.preload['nosuch']
	no file './nosuch.lua'
	no file '/usr/local/share/luajit-2.1/nosuch.lua'
	no file '/usr/local/share/lua/5.1/nosuch.lua'
	no file '/usr/local/share/lua/5.1/nosuch/init.lua'
	no file './nosuch.so'
	no file '/usr/local/lib/lua/5.1/nosuch.so'
	no file '/usr/local/lib/lua/5.1/loadall.so'
stack traceback:
	[C]: at 0x?
	[C]: at 0x?
"###, code: 1 });
}
#[test]
fn exit_code() {
    check(Case { args: &["-e", "os.exit(3)"], stdin: None, env: &[], libs: true, out: r###""###, err: r###""###, code: 3 });
}
#[test]
fn repl() {
    check(Case { args: &["-i"], stdin: Some("repl.txt"), env: &[], libs: true, out: r###"LuaJIT 2.1.1788856981 -- Copyright (C) 2005-2026 Mike Pall. https://luajit.org/
JIT: OFF
> >> > 3
> 3
> 1	2	nil
> > > > >> >> 1
2
> > P P nil
P 
"###, err: r###"stdin:1: e
stack traceback:
	[C]: in function 'error'
	stdin:1: in main chunk
	[C]: at 0x?
(error object is not a string)
TS
stack traceback:
	[C]: in function 'error'
	stdin:1: in main chunk
	[C]: at 0x?
stdin:1: unexpected symbol near ')'
stdin:1: unexpected symbol near '1'
"###, code: 0 });
}
#[test]
fn repl_after_script() {
    check(Case { args: &["-i", "args.lua", "q"], stdin: Some("repl.txt"), env: &[], libs: true, out: r###"LuaJIT 2.1.1788856981 -- Copyright (C) 2005-2026 Mike Pall. https://luajit.org/
q
-3	nil
-2	luajit
-1	-i
0	args.lua
1	q
JIT: OFF
> >> > 3
> 3
> 1	2	nil
> > > > >> >> 1
2
> > P P nil
P 
"###, err: r###"stdin:1: e
stack traceback:
	[C]: in function 'error'
	stdin:1: in main chunk
	[C]: at 0x?
(error object is not a string)
TS
stack traceback:
	[C]: in function 'error'
	stdin:1: in main chunk
	[C]: at 0x?
stdin:1: unexpected symbol near ')'
stdin:1: unexpected symbol near '1'
"###, code: 0 });
}
#[test]
fn repl_eof() {
    check(Case { args: &["-i"], stdin: Some("repl_eof.txt"), env: &[], libs: true, out: r###"LuaJIT 2.1.1788856981 -- Copyright (C) 2005-2026 Mike Pall. https://luajit.org/
JIT: OFF
> >> > --[[<eof>
> > >> 
"###, err: r###""###, code: 0 });
}
#[test]
fn repl_long_line() {
    check(Case { args: &["-i"], stdin: Some("repl_long.txt"), env: &[], libs: true, out: r###"LuaJIT 2.1.1788856981 -- Copyright (C) 2005-2026 Mike Pall. https://luajit.org/
JIT: OFF
> >> > 
"###, err: r###"stdin:1: unfinished string near '"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa'
"###, code: 0 });
}
#[test]
fn repl_print_error() {
    check(Case { args: &["-i"], stdin: Some("repl_print.txt"), env: &[], libs: true, out: r###"LuaJIT 2.1.1788856981 -- Copyright (C) 2005-2026 Mike Pall. https://luajit.org/
JIT: OFF
> > > > 
"###, err: r###"error calling 'print' (attempt to call a nil value)
"###, code: 0 });
}
#[test]
fn jit_on() {
    check(Case { args: &["-jon", "-e", "1"], stdin: None, env: &[], libs: true, out: r###""###, err: r###"luajit: JIT compiler permanently disabled by build option
"###, code: 1 });
}
#[test]
fn jit_off() {
    check(Case { args: &["-joff", "-e", "print(jit.status())"], stdin: None, env: &[], libs: true, out: r###"false
"###, err: r###""###, code: 0 });
}
#[test]
fn jit_unknown() {
    check(Case { args: &["-jfoo"], stdin: None, env: &[], libs: true, out: r###""###, err: r###"luajit: unknown luaJIT command or jit.* modules not installed
"###, code: 1 });
}
#[test]
fn bc_list() {
    check(Case { args: &["-bl", "-e", "x=1"], stdin: None, env: &[], libs: true, out: r###""###, err: r###"luajit: unknown luaJIT command or jit.* modules not installed
"###, code: 1 });
}
#[test]
fn bc_usage() {
    check(Case { args: &["-b"], stdin: None, env: &[], libs: true, out: r###""###, err: r###"luajit: unknown luaJIT command or jit.* modules not installed
"###, code: 1 });
}
#[test]
fn sigint_interrupts() {
    let child = command(&["-e", "local function f() while true do end end f()"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn luajit");
    std::thread::sleep(std::time::Duration::from_millis(300));
    // SAFETY: sending a signal to our child process.
    unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGINT) };
    let o = child.wait_with_output().unwrap();
    assert_eq!(
        normalize(&o.stderr),
        "luajit: (command line):1: interrupted!\nstack traceback:\n\t(command line):1: in function 'f'\n\t(command line):1: in main chunk\n\t[C]: at 0x?\n"
    );
    assert_eq!(o.status.code(), Some(1));
}
