//! The `luajit` executable (see [`luajit::cli`]).

fn main() {
    std::process::exit(luajit::cli::main());
}
