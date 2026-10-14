//! Export the C API (`lua_*`, `luaL_*`, `luaopen_*`, `luaJIT_*`) from the `luajit` binary's
//! dynamic symbol table, so C modules loaded with `package.loadlib`/`require` resolve against
//! it (upstream links the binary with `-Wl,-E`). The symbol list is collected from the
//! `#[unsafe(no_mangle)]` functions in `src/capi`; each one is also forced into the link
//! (`-u`), since nothing in the binary itself references most of them.

use std::fmt::Write as _;
use std::path::Path;

const PREFIXES: [&str; 4] = ["lua_", "luaL_", "luaopen_", "luaJIT_"];

fn main() {
    println!("cargo:rerun-if-changed=src/capi");
    let dir = Path::new("src/capi");
    let mut syms = Vec::new();
    let mut files: Vec<_> = std::fs::read_dir(dir)
        .expect("src/capi")
        .map(|e| e.expect("dir entry").path())
        .collect();
    files.sort();
    for path in files {
        println!("cargo:rerun-if-changed={}", path.display());
        let src = std::fs::read_to_string(&path).expect("read capi source");
        let mut exported = false;
        for line in src.lines() {
            let l = line.trim_start();
            if l.starts_with("#[unsafe(no_mangle)]") {
                exported = true;
                continue;
            }
            if l.starts_with("#[") || l.starts_with("///") {
                continue;
            }
            if exported && let Some(i) = l.find("fn ") {
                let name: String = l[i + 3..]
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                    .collect();
                if PREFIXES.iter().any(|p| name.starts_with(p)) {
                    syms.push(name);
                }
            }
            // `luaopen!` macro invocations: `luaopen_x => open_...(...)`.
            if let Some(name) = l.split_whitespace().next()
                && name.starts_with("luaopen_")
                && l.contains("=>")
            {
                syms.push(name.to_string());
            }
            exported = false;
        }
    }
    syms.sort();
    syms.dedup();
    let out = std::env::var("OUT_DIR").expect("OUT_DIR");
    let list = Path::new(&out).join("capi-exports.list");
    let mut s = String::from("{\n");
    for n in &syms {
        let _ = writeln!(s, "  {n};");
    }
    s.push_str("};\n");
    std::fs::write(&list, s).expect("write export list");
    println!(
        "cargo:rustc-link-arg-bins=-Wl,--dynamic-list={}",
        list.display()
    );
    for n in &syms {
        println!("cargo:rustc-link-arg-bins=-Wl,-u,{n}");
    }
}
