//! The `runai` binary: `check` and `build` today.
//!
//! `runai` with no arguments and `runai --version` print the version; `runai
//! check …` runs [`check`]; `runai build …` runs [`build`]; anything else is a
//! usage error (exit `2`, message on stderr) because the exit-code contract is what
//! tooling reads, and an unknown command must never look like a clean run.

mod build;
mod check;

/// One usage line, printed for an unknown subcommand and for a bad flag. Kept here
/// so every entry point that refuses an argument prints the same text.
pub const USAGE: &str = "usage: runai [--version | check [--json] [path] | build [--out dist]]";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();

    let code = match args.first().map(String::as_str) {
        None | Some("--version") => {
            println!("runai {}", runai_compiler::VERSION);
            0
        }
        Some("check") => check::run(&args[1..]),
        Some("build") => build::run(&args[1..]),
        Some(other) => {
            eprintln!("error: unknown command `{other}`");
            eprintln!("{USAGE}");
            2
        }
    };

    std::process::exit(code);
}
