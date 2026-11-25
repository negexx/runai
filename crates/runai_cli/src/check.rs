//! `runai check`: analyze every route file and report what is wrong.
//!
//! This is the first user-facing command, so it owns the exit-code contract the
//! rest of the tooling reads:
//!
//! - `0` — every route the walk found was read and analyzed, and nothing is
//!   wrong;
//! - `1` — every route was read and at least one `error` diagnostic was reported;
//! - `2` — the command was misused, the path does not exist, or **any** file or
//!   directory could not be read. **Nothing is written to stdout** in this case;
//!   every reason goes to stderr, because a caller parsing `--json` output must
//!   never see prose where an array belongs.
//!
//! A clean verdict is a promise about the whole tree: `0` means every route the
//! walk reached was analyzed. So one unreadable file — or one unreadable
//! subdirectory — is enough to make the run incomplete, because reporting `[]`
//! next to a warning leaves a CI free to read whichever channel is green while a
//! route was never analyzed. Two things follow, and they are deliberate:
//!
//! - the walk *continues* past an unreadable entry instead of aborting, so one
//!   bad permission bit cannot hide the diagnostics the other files produced;
//!   and
//! - those diagnostics are rendered to **stderr** rather than dropped. They
//!   cannot go to stdout (that is the exit-`2` promise), but they are real
//!   findings about readable files, and losing them would hide a broken route
//!   behind an unrelated permission error.
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
//! # `check` refuses what `build` refuses
//!
//! `analyze` sees a file's text and the path to display, not the routes root its
//! segments are relative to, so the route-set rules the builder enforces on a
//! page's **path** are applied by [`run`] as well: a dot-prefixed segment anywhere
//! in the route path (the output would be `dist/.hidden/index.html`), and an island
//! route whose asset path is `assets/runtime.js` (the runtime copy would overwrite
//! its module). Both are `RA014`, both exit `1`, and both carry the builder's own
//! reason — a CI that runs only `check` must not go green on a tree `build` will
//! not produce.
//!
//! # Output is deterministic
//!
//! Files are walked recursively and sorted by their relative path, and
//! [`analyze`] already sorts its own diagnostics, so two runs on the same tree
//! produce byte-identical output.

use std::path::{Path, PathBuf};

use runai_compiler::analyze::analyze;
use runai_compiler::diagnostic::{Code, Diagnostic, Severity};
use runai_compiler::ir::lower_route;
use runai_compiler::source::Mode;

use crate::USAGE;

/// Where files are checked when no path argument is given.
const DEFAULT_PATH: &str = "src/routes";

/// The asset basename `build` copies the runtime to. An island route may not use
/// it: the copy would overwrite the route's own module and the page would load and
/// mount nothing.
///
/// This repeats `build.rs`'s `RUNTIME_ASSET` because that constant is private to
/// its module, and `check` must not be able to drift from the builder's answer. It
/// is one literal in two places by necessity; **changing one means changing the
/// other**, and `tests/check.rs` pins the refusal in both commands.
const RUNTIME_ASSET: &str = "runtime";

/// Run `runai check …` with the arguments that follow `check`.
///
/// Returns the process exit code: `0` clean, `1` errors found, `2` usage error,
/// missing path, or any path that could not be read. Every `2` writes to stderr
/// only.
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

    // A directory that could not be walked is not an abort: `walk` reports it and
    // keeps going, so the files it did reach are still analyzed below. Only a
    // path that does not exist or is not a route comes back as `Err`.
    let mut files = Vec::new();
    let mut unreadable = false;
    if let Err(message) = collect(path, root, &mut files, &mut unreadable) {
        eprintln!("error: {message}");
        return 2;
    }

    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    // The working directory is the same path for every file in this run, so it is
    // canonicalized once here rather than once per file: `realpath` is a syscall
    // walk and `check` is called on whole trees. `None` keeps the previous
    // behaviour for an uncanonicalizable cwd — the path is then used as given.
    let cwd_canonical = cwd.canonicalize().ok();
    // One entry per file, because `render_human` needs that file's source and
    // the JSON form needs every diagnostic across all files at once.
    let mut checked: Vec<(String, Vec<Diagnostic>)> = Vec::new();
    let mut failed = false;

    for file in files {
        // An unreadable file marks the run incomplete but does not stop it: the
        // remaining files still have diagnostics worth reporting.
        let source = match std::fs::read_to_string(&file) {
            Ok(source) => source,
            Err(error) => {
                eprintln!("error: cannot read {}: {error}", file.display());
                unreadable = true;
                continue;
            }
        };
        let relative = relative_path(&file, cwd_canonical.as_deref());
        let mut diagnostics = analyze(&source, &relative);
        // The route-set refusals `build` makes about a page's *path*, which
        // `analyze` cannot see: it is given the file's text and its displayed
        // path, not the routes root. Appended after the analysis so the
        // diagnostics read as one list per file, and located the same way, so the
        // human and JSON forms agree.
        diagnostics.extend(route_set_refusals(root, &file, &relative, &source));
        if diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == Severity::Error)
        {
            failed = true;
        }
        checked.push((source, diagnostics));
    }

    // Some entry never got analyzed, so there is no verdict to report. The
    // diagnostics found in the readable files cannot go to stdout (that would be
    // a partial array next to a failure), so they go to stderr: they are still
    // real findings, and the alternative is discarding a broken route behind an
    // unrelated permission error.
    if unreadable {
        eprintln!("error: the check did not complete: some files or directories could not be read");
        for (source, diagnostics) in &checked {
            for diagnostic in diagnostics {
                eprint!("{}", diagnostic.render_human(source));
            }
        }
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

/// The route-set refusals `build` makes about a page's **path**, which `analyze`
/// cannot see: it is given a file's text and the path to display, not the routes
/// root those segments are relative to.
///
/// Two rules live here, both already refused by `build`:
///
/// - **any dot-prefixed segment** (`build.rs`'s `route_name`): the output path
///   would be `dist/.hidden/index.html`, which many static servers 404. `build`
///   scans **every** segment, not just the first, because `a/.b/c.page.tsx` shipped
///   `dist/a/.b/c/index.html` for the same reason;
/// - **an island route whose asset path is `assets/runtime.js`** (`build.rs`'s
///   `assemble_into`): the runtime copy would overwrite the route's module and the
///   page would load and mount nothing.
///
/// Both are reported as `RA014`, the same code and the same zero-width `(0, 0)`
/// span the `[param]` refusal uses: the defect is in the file's **name**, so there
/// is no byte of the file to underline, and `analyze` already reports its own
/// `RA014` for a bracketed segment the same way.
///
/// Page-only, like the bracketed-segment rule: `build`'s walk never hands a
/// component module (`_shell.tsx`) to a route rule, so a component under
/// `.hidden` is not a route either command refuses.
fn route_set_refusals(root: &Path, file: &Path, display: &str, source: &str) -> Vec<Diagnostic> {
    let relative = route_relative(root, file);
    let relative = relative.to_string_lossy().replace('\\', "/");

    if !is_page(&relative) {
        return Vec::new();
    }

    // A `.page.tsx` suffix belongs to the file, not to the segment's shape:
    // `[slug].page.tsx` is the `[slug]` segment for the rule next to this one, and
    // `.hidden.page.tsx` is the `.hidden` segment here.
    let without = relative
        .strip_suffix(".page.tsx")
        .unwrap_or(relative.as_str());

    let segments: Vec<&str> = without.split('/').collect();

    // Any segment, not just the first, for the reason `build` documents at its
    // own scan: the refusal is about the whole output path.
    //
    // This includes a dot-prefixed **file** stem (`.modules.page.tsx`), which
    // `build`'s `walk` filters out before its rule can see it — so on a tree that
    // also holds a legal route, `build` succeeds while `check` refuses. That is
    // deliberate and it is the safe direction: the file is a route the builder
    // silently drops, which is the "successful-looking build of the wrong site"
    // this project exists to prevent, and a CI that goes red on it is telling the
    // truth. `check` is never *less* strict than `build`.
    if let Some(index) = segments.iter().position(|segment| segment.starts_with('.')) {
        let name = segments[index];
        // The last segment came from the file name, so the repair renames a file,
        // not a directory. `build` distinguishes these for the same reason: telling
        // an author to rename the directory that holds `blog/.modules.page.tsx`
        // names a directory that is not the problem.
        let noun = if index + 1 == segments.len() {
            "file"
        } else {
            "directory"
        };
        return vec![located(
            Diagnostic::new(
                Code::RA014,
                (0, 0),
                format!(
                    "`{display}` has the dot-prefixed {noun} `{name}`, whose output would \
                     contain a dot-prefixed path — many static servers refuse to serve one. \
                     Rename the {noun}."
                ),
            ),
            source,
            display,
        )];
    }

    // `build`'s `asset_name`: the route name is the path with a trailing `/index`
    // dropped, and the root route is `index`. Only a route whose whole name is the
    // runtime asset collides — `blog/runtime` writes `assets/blog/runtime.js`.
    let name = match without.strip_suffix("/index") {
        Some(prefix) => prefix,
        None if without == "index" => "",
        None => without,
    };
    let asset = if name.is_empty() { "index" } else { name };

    // Case-insensitive, exactly as `build` compares it: on macOS and Windows
    // `assets/Runtime.js` IS `assets/runtime.js`, so a byte-exact guard would pass
    // on Linux and silently overwrite the module where it matters.
    if !asset.eq_ignore_ascii_case(RUNTIME_ASSET) {
        return Vec::new();
    }

    // Only an island emits an asset at all: a static route ships no JavaScript, so
    // a static `runtime` route is harmless and `build` builds it. The mode is
    // `lower_route`'s answer, not a text scan of the directive — the same detector
    // the emitter uses, so the two cannot disagree. A file it refuses already
    // carries the analysis errors that stop `build` before this point.
    let Ok(route) = lower_route(source) else {
        return Vec::new();
    };
    if route.mode != Mode::Island {
        return Vec::new();
    }

    vec![located(
        Diagnostic::new(
            Code::RA014,
            (0, 0),
            format!(
                "`{display}` is an island route whose asset path is `assets/{asset}.js` — \
                 reserved for the runtime copy. That copy would overwrite this route's \
                 module, so the page would load and mount nothing. Rename the route."
            ),
        ),
        source,
        display,
    )]
}

/// Whether `path` names a page file: a `.page.tsx` whose name does not start with
/// `_`.
///
/// A component module (`_shell.tsx`) is a real file with real components, but it is
/// never a route, so the route-set rules above do not apply to it. This mirrors
/// `runai_compiler`'s `is_page`, which is `pub(crate)` and therefore not callable
/// from the CLI.
fn is_page(path: &str) -> bool {
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    !name.starts_with('_') && name.ends_with(".page.tsx")
}

/// `diagnostic` with the file and position filled in, as `analyze` locates its own.
fn located(mut diagnostic: Diagnostic, source: &str, path: &str) -> Diagnostic {
    diagnostic.locate(source, path);
    diagnostic
}

/// The part of `file` that names its route, relative to the routes root `build`
/// builds — `src/routes`, relative to the working directory.
///
/// The checked root is whatever argument the user gave (`check .`, `check src`,
/// `check src/routes`, or the default), so stripping only the argument would hand
/// `build`'s rules a prefix it does not expect: `check .` would make
/// `RUNTIME.page.tsx`'s route name `src/routes/RUNTIME`, which is not the asset
/// `build` derives, and the two commands would disagree again — the exact defect
/// this function exists to close.
fn route_relative(root: &Path, file: &Path) -> PathBuf {
    // The `src/routes` pair is found on the path as given, before the argument
    // strip: it is the root `build` walks, so it is the only anchor that makes the
    // two commands derive the same route name. `check .` needs it because the
    // argument leaves the prefix in place, and a single-file argument needs it
    // because the argument strip would consume the whole path.
    if let Some(relative) = after_routes_root(file) {
        return relative;
    }
    // No `src/routes` pair: the path is not under the root `build` walks, so there
    // is no build rule to mirror. Use it relative to the argument, which is what a
    // `check src/a` run needs; a path equal to the argument has no route name, so
    // its file name stands in.
    let stripped = file.strip_prefix(root).unwrap_or(file);
    if stripped.components().next().is_none() {
        return file
            .file_name()
            .map(PathBuf::from)
            .unwrap_or_else(|| stripped.to_path_buf());
    }
    stripped.to_path_buf()
}

/// Everything after the **last** `src/routes` component pair in `path`, if any.
///
/// The pair is matched as two adjacent whole components, never as a substring: a
/// directory named `src/routes-old` is not the routes root, and a route literally
/// named `src` is only the anchor when followed by one named `routes`. The last
/// pair wins so a checkout nested inside a path that happens to contain the pair
/// still anchors on the routes root nearest the file.
fn after_routes_root(path: &Path) -> Option<PathBuf> {
    let components: Vec<&std::ffi::OsStr> = path.iter().collect();
    let index = components
        .windows(2)
        .rposition(|pair| pair[0] == "src" && pair[1] == "routes")?;
    // `index + 2` is at most `len`: `windows(2)` stops at `len - 2`, so the
    // largest `index` leaves `index + 2 == len`, which collects to an empty path.
    // An empty path names the root route, which has no segments to refuse.
    Some(path.iter().skip(index + 2).collect())
}

/// Every `*.tsx` file under `path`, sorted by relative path.
///
/// `path` may be a directory (walked recursively) or a single `*.tsx` file, in
/// which case that one file is checked. A file with any other extension is a
/// usage error rather than a file to analyze: `analyze` treats its input as TSX,
/// so `package.json` would come back as a syntax diagnostic and a non-route
/// would look like a broken route. A path that does not exist is the exit-`2`
/// message, which names the path the user asked for so the repair is obvious.
///
/// Those are the only `Err` cases: an unreadable directory *inside* the tree does
/// not abort the walk. It is reported on stderr, `incomplete` is set, and the
/// walk moves to the next entry, so the files it did reach are still analyzed.
fn collect(
    path: &str,
    root: &Path,
    files: &mut Vec<PathBuf>,
    incomplete: &mut bool,
) -> Result<(), String> {
    let metadata =
        std::fs::metadata(root).map_err(|_| format!("no such directory or file: `{path}`"))?;

    if metadata.is_file() {
        if root.extension().is_some_and(|ext| ext == "tsx") {
            files.push(root.to_path_buf());
            return Ok(());
        }
        return Err(format!(
            "not a route file (expected a `*.tsx` file or a directory): `{path}`"
        ));
    }

    walk(root, files, incomplete);
    files.sort_by_key(|path| separators(path));
    Ok(())
}

/// Depth-first walk collecting `*.tsx` files.
///
/// An entry that cannot be read is reported and skipped: one unreadable
/// subdirectory must not throw away every diagnostic the readable files produced.
fn walk(dir: &Path, files: &mut Vec<PathBuf>, incomplete: &mut bool) {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) => {
            eprintln!("error: cannot read {}: {error}", dir.display());
            *incomplete = true;
            return;
        }
    };

    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                eprintln!("error: cannot read {}: {error}", dir.display());
                *incomplete = true;
                continue;
            }
        };
        let path = entry.path();
        // `file_type` does not follow symlinks, so a link loop cannot recurse.
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(error) => {
                eprintln!("error: cannot read {}: {error}", path.display());
                *incomplete = true;
                continue;
            }
        };

        if file_type.is_dir() {
            walk(&path, files, incomplete);
        } else if file_type.is_file() && path.extension().is_some_and(|ext| ext == "tsx") {
            files.push(path);
        }
    }
}

/// `path` relative to `cwd_canonical`, with `/` separators.
///
/// `cwd_canonical` is the working directory already put through
/// `Path::canonicalize` by the caller, which is why the loop computes it once:
/// it is one path for the whole run. `None` means it could not be canonicalized,
/// and then the path is used as given — still with `/` separators, so the output
/// never depends on the platform.
///
/// The file side is still canonicalized per call: it is a different path each
/// time, and it is what turns an absolute argument under the working directory
/// into a relative diagnostic.
fn relative_path(path: &Path, cwd_canonical: Option<&Path>) -> String {
    let absolute = path.canonicalize();

    if let (Ok(absolute), Some(base)) = (absolute, cwd_canonical)
        && let Ok(stripped) = absolute.strip_prefix(base)
    {
        return separators(stripped);
    }

    separators(path)
}

/// A path rendered with `/` separators.
fn separators(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}
