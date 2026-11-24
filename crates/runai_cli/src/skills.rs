//! `runai skills`: write one Markdown file per diagnostic code.
//!
//! Every diagnostic carries a `doc` path, `node_modules/runai/skills/<code>.md`,
//! and the file it names may not exist: a project can install the package with
//! its `"files"` filtered, vendor it, or lose `node_modules` to a tool that
//! prunes it. This command is the repair — it writes the same bodies the package
//! ships, from [`runai_compiler::skills::skill_body`], which in turn reads
//! `Code::explain` and `Code::fix_template`. So a regenerated file and the
//! diagnostic that pointed at it cannot disagree.
//!
//! The default `--out` is [`DEFAULT_OUT`], the directory the `doc` path names, so
//! `runai skills` with no arguments fixes the links `runai check` is already
//! printing.
//!
//! The command never deletes: a file for a code that no longer exists is left
//! alone. `skills/` is a directory of documentation, and a repair command that
//! `rm`s things it did not write is how a user loses work.

use std::path::Path;

use runai_compiler::diagnostic::Code;

use crate::USAGE;

/// Where a project's `doc` links point, and therefore where a bare
/// `runai skills` writes. Spec §7.9.
const DEFAULT_OUT: &str = "node_modules/runai/skills";

/// Run `runai skills …` with the arguments that follow `skills`.
///
/// Returns the process exit code: `0` every file was written, `2` the command was
/// misused, `1` a directory or file could not be written. A `1` names the path
/// that failed, because the repair for a read-only tree is the path, not the
/// command.
pub fn run(args: &[String]) -> i32 {
    let mut out: Option<&str> = None;
    let mut index = 0;

    while index < args.len() {
        let arg = args[index].as_str();
        index += 1;
        match arg {
            // `--out skills` and `--out=skills` are the same flag, matching
            // `runai build`'s spelling so a script can reuse either form.
            "--out" => {
                let Some(value) = args.get(index) else {
                    eprintln!("error: `--out` needs a directory");
                    eprintln!("{USAGE}");
                    return 2;
                };
                out = Some(value.as_str());
                index += 1;
            }
            flag if flag.starts_with("--out=") => out = Some(&flag["--out=".len()..]),
            flag if flag.starts_with('-') => {
                eprintln!("error: unknown flag `{flag}`");
                eprintln!("{USAGE}");
                return 2;
            }
            value => {
                eprintln!("error: unexpected argument `{value}`");
                eprintln!("{USAGE}");
                return 2;
            }
        }
    }

    let out = out.unwrap_or(DEFAULT_OUT);
    // An empty value (`--out=`) names no directory; `write` would fail with a
    // confusing OS error rather than telling the agent what is wrong.
    if out.trim().is_empty() {
        eprintln!("error: `--out` needs a directory");
        eprintln!("{USAGE}");
        return 2;
    }

    let dir = Path::new(out);
    if let Err(error) = std::fs::create_dir_all(dir) {
        eprintln!("error: cannot create {}: {error}", dir.display());
        return 1;
    }

    for &code in Code::all() {
        let path = dir.join(code.skill_file());
        if let Err(error) = std::fs::write(&path, runai_compiler::skills::skill_body(code)) {
            eprintln!("error: cannot write {}: {error}", path.display());
            return 1;
        }
    }

    println!(
        "wrote {} skill files to {}",
        Code::all().len(),
        dir.display()
    );
    0
}
