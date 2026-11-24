//! `runai check`: analyze every route file and report what is wrong.
//!
//! This is the first user-facing command, so it owns the exit-code contract the
//! rest of the tooling reads:
//!
//! - `0` — every file was analyzed and nothing is wrong;
//! - `1` — at least one `error` diagnostic was reported;
//! - `2` — the command was misused, the path does not exist, or nothing could be
//!   read at all. **Nothing is written to stdout** in this case; the message goes
//!   to stderr, because a caller parsing `--json` output must never see prose
//!   where an array belongs.
//!
//! One unreadable file among readable ones does not end the run: it is reported
//! on stderr — naming the path and the OS error — and the walk continues, so the
//! rest of the tree is still analyzed and its diagnostics still print. The exit
//! code is then `1` if any diagnostic was produced and `0` if none was; only when
//! no file could be read at all is it `2`. The message never goes to stdout, so
//! `--json` output stays one parseable array either way.
//!
//! # Paths are relative to the working directory when that is possible
//!
//! A file that canonicalizes to a location under the process's working directory
//! is passed to [`analyze`] as a path relative to that directory, with `/`
//! separators; that relative form is what lands in [`Diagnostic::file`], so two
//! checkouts of the same tree print the same paths. A file outside the working
//! directory keeps the path the user gave — no `../..` chain is invented — so an
//! absolute argument from elsewhere reports an absolute `file`. Either way the
//! separators are `/`.
//!
//! # Output is deterministic
//!
//! Files are walked recursively and sorted by their relative path, and
//! [`analyze`] already sorts its own diagnostics, so two runs on the same tree
//! produce byte-identical output.

use std::path::{Path, PathBuf};

use runai_compiler::analyze::analyze;
use runai_compiler::diagnostic::{Diagnostic, Severity};

use crate::USAGE;

/// Where files are checked when no path argument is given.
const DEFAULT_PATH: &str = "src/routes";

/// Run `runai check …` with the arguments that follow `check`.
///
/// Returns the process exit code: `0` clean, `1` errors found, `2` usage error,
/// missing path, or nothing readable. Every `2` writes to stderr only.
pub fn run(args: &[String]) -> i32 {
    let mut json = false;
    let mut path: Option<&str> = None;

    for arg in args {
        match arg.as_str() {
            // The flag may appear before or after the path; this loop is what
            // makes `check --json`, `check src` and `check --json src` all work.
            "--json" => json = true,
            flag if flag.starts_with('-') => {
                eprintln!("error: unknown flag `{flag}`");
                eprintln!("{USAGE}");
                return 2;
            }
            // A second path is refused rather than silently dropping the first:
            // `check a b` that analyzed only `b` would report a clean tree for a
            // tree full of errors, and the exit code is the whole product here.
            value if path.is_some() => {
                eprintln!("error: unexpected argument `{value}`");
                eprintln!("{USAGE}");
                return 2;
            }
            value => path = Some(value),
        }
    }

    let path = path.unwrap_or(DEFAULT_PATH);
    let root = Path::new(path);

    let files = match collect(path, root) {
        Ok(files) => files,
        Err(message) => {
            eprintln!("error: {message}");
            return 2;
        }
    };

    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    // One entry per file, because `render_human` needs that file's source and
    // the JSON form needs every diagnostic across all files at once.
    let mut checked: Vec<(String, Vec<Diagnostic>)> = Vec::new();
    let mut failed = false;
    let mut unreadable = false;

    for file in files {
        // An unreadable file is reported and skipped, not fatal: aborting here
        // would hide every diagnostic in the same tree behind one bad encoding.
        let source = match std::fs::read_to_string(&file) {
            Ok(source) => source,
            Err(error) => {
                eprintln!("error: cannot read {}: {error}", file.display());
                unreadable = true;
                continue;
            }
        };
        let relative = relative_path(&file, &cwd);
        let diagnostics = analyze(&source, &relative);
        if diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == Severity::Error)
        {
            failed = true;
        }
        checked.push((source, diagnostics));
    }

    // Nothing was read at all: there is no analysis to report, so this is the
    // exit-`2` case and stdout stays untouched for a `--json` caller.
    if checked.is_empty() && unreadable {
        return 2;
    }

    if json {
        let all: Vec<Diagnostic> = checked
            .into_iter()
            .flat_map(|(_, diagnostics)| diagnostics)
            .collect();
        println!("{}", Diagnostic::render_all_json(&all));
    } else {
        for (source, diagnostics) in &checked {
            for diagnostic in diagnostics {
                print!("{}", diagnostic.render_human(source));
            }
        }
    }

    if failed { 1 } else { 0 }
}

/// Every `*.tsx` file under `path`, sorted by relative path.
///
/// `path` may be a directory (walked recursively) or a single `*.tsx` file, in
/// which case that one file is checked. A file with any other extension is a
/// usage error rather than a file to analyze: `analyze` treats its input as TSX,
/// so `package.json` would come back as a syntax diagnostic and a non-route
/// would look like a broken route. A path that does not exist is the exit-`2`
/// message, which names the path the user asked for so the repair is obvious.
fn collect(path: &str, root: &Path) -> Result<Vec<PathBuf>, String> {
    let metadata =
        std::fs::metadata(root).map_err(|_| format!("no such directory or file: `{path}`"))?;

    if metadata.is_file() {
        if root.extension().is_some_and(|ext| ext == "tsx") {
            return Ok(vec![root.to_path_buf()]);
        }
        return Err(format!(
            "not a route file (expected a `*.tsx` file or a directory): `{path}`"
        ));
    }

    let mut files = Vec::new();
    walk(root, &mut files)?;
    files.sort_by_key(|path| separators(path));
    Ok(files)
}

/// Depth-first walk collecting `*.tsx` files.
fn walk(dir: &Path, files: &mut Vec<PathBuf>) -> Result<(), String> {
    let entries = std::fs::read_dir(dir)
        .map_err(|error| format!("cannot read {}: {error}", dir.display()))?;

    for entry in entries {
        let entry = entry.map_err(|error| format!("cannot read {}: {error}", dir.display()))?;
        let path = entry.path();
        // `file_type` does not follow symlinks, so a link loop cannot recurse.
        let file_type = entry
            .file_type()
            .map_err(|error| format!("cannot read {}: {error}", path.display()))?;

        if file_type.is_dir() {
            walk(&path, files)?;
        } else if file_type.is_file() && path.extension().is_some_and(|ext| ext == "tsx") {
            files.push(path);
        }
    }

    Ok(())
}

/// `path` relative to `cwd`, with `/` separators.
///
/// Both sides are canonicalized when possible so an absolute argument still
/// yields a relative diagnostic; if that fails the path is used as given, still
/// with `/` separators, so the output never depends on the platform.
fn relative_path(path: &Path, cwd: &Path) -> String {
    let absolute = path.canonicalize();
    let base = cwd.canonicalize();

    if let (Ok(absolute), Ok(base)) = (absolute, base)
        && let Ok(stripped) = absolute.strip_prefix(&base)
    {
        return separators(stripped);
    }

    separators(path)
}

/// A path rendered with `/` separators.
fn separators(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}
