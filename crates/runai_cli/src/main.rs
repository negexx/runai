//! The `runai` binary: `check`, `build`, `dev`, and `skills` today.
//!
//! `runai` with no arguments and `runai --version` print the version; `runai
//! check …` runs [`check`]; `runai build …` runs [`build`]; `runai dev …` runs
//! [`dev`]; `runai skills …` runs [`skills`]; anything else is a usage error
//! (exit `2`, message on stderr) because the exit-code contract is what tooling
//! reads, and an unknown command must never look like a clean run.

mod build;
mod check;
mod dev;
mod skills;

/// One usage block, printed for an unknown subcommand and for a bad flag. Kept here
/// so every entry point that refuses an argument prints the same text.
///
/// The second line documents where `build`'s single-writer lock lives. Two invocations
/// with different `TMPDIR`s therefore take different locks — an assumption stated in the
/// text rather than a silent hole.
pub const USAGE: &str = "usage: runai [--version | check [--json] [path] | build [--out dist] | \
     dev [--port 5173] [--background] | dev stop | skills [--out dir]]\n\
     `build` takes a lock in the system temp directory ($TMPDIR) keyed by the output path\n\
     `dev` serves `dist/` on 127.0.0.1 only and rewrites `.runai/dev.json`\n\
     `skills` writes one file per diagnostic code to node_modules/runai/skills by default";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();

    let code = match args.first().map(String::as_str) {
        None | Some("--version") => {
            println!("runai {}", runai_compiler::VERSION);
            0
        }
        Some("check") => check::run(&args[1..]),
        Some("build") => build::run(&args[1..]),
        Some("dev") => dev::run(&args[1..]),
        Some("skills") => skills::run(&args[1..]),
        Some(other) => {
            eprintln!("error: unknown command `{other}`");
            eprintln!("{USAGE}");
            2
        }
    };

    std::process::exit(code);
}
