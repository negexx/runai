//! The `runai` binary: one subcommand today, `check`.
//!
//! `runai` with no arguments and `runai --version` print the version; `runai
//! check …` runs [`check`]; anything else is a usage error (exit `2`, message on
//! stderr) because the exit-code contract is what tooling reads, and an unknown
//! command must never look like a clean check.

mod check;

/// One usage line, printed for an unknown subcommand. Kept here so every
/// entry point that refuses a command prints the same text.
pub const USAGE: &str = "usage: runai [--version | check [--json] [path]]";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();

    let code = match args.first().map(String::as_str) {
        None | Some("--version") => {
            println!("runai {}", runai_compiler::VERSION);
            0
        }
        Some("check") => check::run(&args[1..]),
        Some(other) => {
            eprintln!("error: unknown command `{other}`");
            eprintln!("{USAGE}");
            2
        }
    };

    std::process::exit(code);
}
