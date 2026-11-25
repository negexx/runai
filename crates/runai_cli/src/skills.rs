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

/// Parse the one argument loop `runai build` and `runai skills` share.
///
/// Both commands take exactly one flag — `--out <dir>` or `--out=<dir>` — and both
/// refuse anything else with the same three messages and exit code `2`. The loop
/// lived twice, character for character, which is how the two commands could have
/// drifted into accepting a spelling the other refused; this is the single copy.
///
/// # Returns
///
/// - `Ok(None)` — no `--out` was given; the caller supplies its own default.
/// - `Ok(Some(dir))` — the directory as written, **not** validated: `""`, `.` and
///   `..` are refused by each caller, because a build output directory and a skill
///   output directory have different degeneracy rules.
/// - `Err(2)` — the command was misused. The message and `USAGE` have already been
///   printed, so a caller returns `2` unchanged.
///
/// This lives beside `skills` because `build`'s module cannot be the shared home
/// from here; it is `pub(crate)` so `crate::build` calls it rather than keeping a
/// second copy.
pub(crate) fn parse_out_flag(args: &[String]) -> Result<Option<&str>, i32> {
    let mut out: Option<&str> = None;
    let mut index = 0;

    while index < args.len() {
        let arg = args[index].as_str();
        index += 1;
        match arg {
            // `--out skills` and `--out=skills` are the same flag, and both
            // commands accept either form so a script can reuse one spelling.
            "--out" => {
                let Some(value) = args.get(index) else {
                    eprintln!("error: `--out` needs a directory");
                    eprintln!("{USAGE}");
                    return Err(2);
                };
                out = Some(value.as_str());
                index += 1;
            }
            flag if flag.starts_with("--out=") => out = Some(&flag["--out=".len()..]),
            flag if flag.starts_with('-') => {
                eprintln!("error: unknown flag `{flag}`");
                eprintln!("{USAGE}");
                return Err(2);
            }
            value => {
                eprintln!("error: unexpected argument `{value}`");
                eprintln!("{USAGE}");
                return Err(2);
            }
        }
    }

    Ok(out)
}

/// Run `runai skills …` with the arguments that follow `skills`.
///
/// Returns the process exit code: `0` every file was written, `2` the command was
/// misused, `1` a directory or file could not be written. A `1` names the path
/// that failed, because the repair for a read-only tree is the path, not the
/// command.
pub fn run(args: &[String]) -> i32 {
    // `?` is not available (this returns `i32`, not `Result`), so the exit code is
    // propagated explicitly.
    let out = match parse_out_flag(args) {
        Ok(out) => out,
        Err(code) => return code,
    };

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

#[cfg(test)]
mod tests {
    use super::parse_out_flag;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    /// Every accepted spelling and every refusal, pinned at the parser rather than
    /// through the binary. `build.rs` used to carry its own copy of this loop; these
    /// are the cases it had to keep in step by hand.
    #[test]
    fn parse_out_flag_accepts_both_spellings_and_refuses_the_rest() {
        assert_eq!(parse_out_flag(&args(&[])), Ok(None));
        assert_eq!(parse_out_flag(&args(&["--out", "dist"])), Ok(Some("dist")));
        assert_eq!(parse_out_flag(&args(&["--out=dist"])), Ok(Some("dist")));
        // An empty value parses; the caller refuses it, because `build` and `skills`
        // disagree about which values are degenerate.
        assert_eq!(parse_out_flag(&args(&["--out="])), Ok(Some("")));
        // A value that starts with `-` is still the value: `--out -x` names `-x`.
        assert_eq!(parse_out_flag(&args(&["--out", "-x"])), Ok(Some("-x")));
        // The last spelling wins, as it did before the loop was shared.
        assert_eq!(
            parse_out_flag(&args(&["--out", "a", "--out=b"])),
            Ok(Some("b"))
        );

        assert_eq!(parse_out_flag(&args(&["--out"])), Err(2));
        assert_eq!(parse_out_flag(&args(&["--json"])), Err(2));
        assert_eq!(parse_out_flag(&args(&["dist"])), Err(2));
    }
}
