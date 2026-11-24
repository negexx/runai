//! `runai build`: turn the route tree into a servable `dist/`.
//!
//! This is the first command that **executes** an artifact instead of analyzing
//! text. For each `*.page.tsx` it asks the checked emitter for a module — the same
//! gate `runai check` passes, so a build never emits something the check refused —
//! and then, for a static route, runs that module under a JavaScript runtime to get
//! the HTML. An island route is not executed at build time: its container starts
//! empty and its module mounts in the browser.
//!
//! # The shape of the output
//!
//! - static route `<route>`: `dist/<route>/index.html`, **no `<script>` at all**;
//! - island route `<route>`: the same path with an empty `<div id="app"></div>` and
//!   exactly one `<script type="module" src="/assets/<route>.js">`, the emitted
//!   module at `dist/assets/<route>.js`, and the runtime at `dist/assets/runtime.js`.
//!
//! `index` is the root: `index.page.tsx` → `dist/index.html` and (island)
//! `dist/assets/index.js`. A file or directory whose name begins with `_` is a
//! component module, not a route, and is skipped; a *file* whose name begins with `.`
//! is hidden and also not a route, while a hidden *directory* is descended into so
//! it can be refused rather than silently dropped. A `[param]` segment is `RA014`
//! because this milestone has no router: silently emitting `dist/[slug]/index.html`
//! would serve a 404 and look like a successful build. A route any of whose segments
//! begins with `.` is refused for the same class of reason: many static servers 404 a
//! dot-prefixed path, so `dist/.cache/index.html` would build cleanly and never be
//! served. `.modules` and `.runai-build` are specific instances of that rule and keep
//! their own messages because the repair differs.
//!
//! # Route names are unique, and `runtime` is reserved for island routes
//!
//! Two files can derive the same route name — `about.page.tsx` and
//! `about/index.page.tsx` both give `about`, and `index.page.tsx` and `.page.tsx`
//! both (before the hidden-file skip) gave the root. The build used to keep the last
//! one silently, so the loser never shipped. [`prepare`] accumulates name → path and
//! refuses a duplicate, naming both files. Names are compared
//! `eq_ignore_ascii_case`: on macOS and Windows `About/index.html` and
//! `about/index.html` are one file, so a byte-exact guard would pass on Linux and
//! silently overwrite on the platforms where it matters.
//!
//! An *island* route named `runtime` is refused for the same class of reason: its
//! asset is `assets/runtime.js`, the exact path the runtime copy is written to, so
//! the copy would overwrite the page's module. The check lives in [`assemble_into`],
//! where the emitted mode is known: a static route emits no asset at all, so nothing
//! is overwritten and a static `runtime` route builds normally.
//!
//! # `<out>` is only replaced when it is a previous build
//!
//! The name of a directory is not proof that this tool created it. `publish` writes a
//! stamp (`.runai-build`) into every tree it publishes and replaces `<out>` only when
//! it carries that stamp; an unstamped `<out>` is refused with exit `2`, nothing
//! written and nothing deleted. Without that gate `runai build --out src` moved the
//! source tree aside and deleted it. `--out` itself is also checked at parse time:
//! empty, `.`, `..`, any directory holding a `*.tsx` the build did not write, and a
//! path whose nearest existing ancestor is the route source tree or inside it
//! (`--out src/dist` does not exist yet, so scanning `<out>` alone would miss it).
//!
//! `<out>.old` is a derived name too, not proof of ownership, so it is removed only
//! when it carries the completeness stamp [`STAMP`].
//!
//! # Scratch is unique per build, and publication is single-writer
//!
//! Every build assembles into `<out>.tmp.<pid>.<n>`, a name no other build can be
//! using, so two concurrent builds never touch each other's tree and "who owns this
//! directory" stops being a question. All three earlier schemes reused one fixed
//! `<out>.tmp` and tried to infer ownership from its contents; that cannot work,
//! because an empty directory, one with this tool's marker, and one with neither are
//! indistinguishable from a second process's point of view. The observed failures were
//! deleting a user's `dist.tmp`, wedging forever after one `Ctrl-C`, and two concurrent
//! builds deleting each other's scratch mid-run.
//!
//! At the start of every build, leftover scratch directories are swept **only** when
//! they carry [`OWNER`] *and* nobody holds its lock: the marker is an OS file lock, so
//! "marked" means "ours" and "unlocked" means "its build is gone". Any other directory
//! is ignored — a name this tool generates is not evidence that its contents belong to
//! this tool, and refusing the build over one was what created both wedges. The previous
//! release's fixed `<out>.tmp` is additionally swept when empty, because an empty
//! directory has nothing to lose and a failed marker write leaves exactly that.
//!
//! The rename dance that publishes `<out>` is not atomic step-by-step, so the whole run
//! holds one OS file lock keyed by the resolved `--out` ([`lock_build`]). Concurrent
//! builds queue instead of interleaving; the kernel drops the lock when a process dies,
//! so a killed build cannot wedge the next one. The scratch names stay unique so two
//! builds can assemble at the same time without sharing a directory.
//!
//! Each build's own scratch is removed by [`Scratch`]'s `Drop` on every failure path,
//! so a normal failure leaves nothing behind; a killed build leaves a marked directory
//! that the next build sweeps.
//!
//! # The exit status of the child is not a failure signal
//!
//! Measured on this machine (Bun 1.4.1, Node v24.20.0): a module with a syntax
//! error, a module that throws while importing, and a module whose `render` is
//! missing each print to stderr and still exit **0**. Trusting `status.success()`
//! would write the runtime's error banner into `index.html` and report success —
//! the silent failure this project exists to prevent. So a render is accepted only
//! when **all three** hold: the status succeeded, stderr is empty, and stdout is
//! non-empty. A clean module writes exactly the HTML to stdout and nothing to
//! stderr. The child's working directory is the directory holding the module,
//! because both runtimes are invoked with a relative specifier.
//!
//! # All-or-nothing, and the previous build survives a failure
//!
//! Everything is written to `<out>.tmp` first. Only a complete build is published,
//! and publishing is a rename dance rather than a single rename: on Linux `rename`
//! over an existing **non-empty** directory fails, so `<out>` is first renamed to
//! `<out>.old`, `<out>.tmp` is renamed into place, and `<out>.old` is removed. If
//! the second rename fails, `<out>.old` is renamed back, so a failed rebuild leaves
//! the previous `dist/` intact instead of destroying it. A build that fails before
//! publishing removes `<out>.tmp` and never touches `<out>`.
//!
//! A crash between those two renames would leave `<out>` absent and the good tree in
//! `<out>.old`, so [`recover`] runs before anything else: if `<out>` is absent and
//! `<out>.old` carries the stamp, it is renamed back. If both exist, the stale `.old`
//! is dropped.
//!
//! # `runai.json` is required only when there is an island
//!
//! A tree of static routes ships no runtime, so it does not need a config: the
//! absence of `runai.json` is not an error, and `static_route_html_has_no_script_tag`
//! is the test that pins this. The moment one island exists, `runai.json` and the
//! `runtime` it names are required; the path is resolved relative to the
//! `runai.json` that named it, and a missing or unreadable runtime is a build
//! failure whose message names the **resolved** path. Copying a nonexistent runtime
//! would ship an island whose `<script src="/assets/runtime.js">` 404s: the page
//! loads, does nothing, and looks fine.
//!
//! `runai.json` is `{ "runtime": "<path>" }` and the CLI has no JSON dependency, so
//! [`runtime_value`] reads that one string with a small escape-aware scanner rather
//! than pulling a parser into the binary.

use std::collections::HashMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use runai_compiler::analyze::analyze;
use runai_compiler::codegen::{emit_client_checked, emit_prerender_checked};
use runai_compiler::diagnostic::{Code, Diagnostic, Severity};

use crate::USAGE;

/// Where routes are read from.
const ROUTES: &str = "src/routes";

/// Where the build goes when no `--out` is given.
const DEFAULT_OUT: &str = "dist";

/// The config that names the runtime, relative to the working directory.
const CONFIG: &str = "runai.json";

/// Where the island module imports its runtime from, and where [`publish`]'s tree
/// carries it. An absolute URL path, because the browser fetches it, not the build.
const RUNTIME_URL: &str = "/assets/runtime.js";

/// The asset basename the runtime is copied to. An island route may not use it:
/// `asset_name(name) == RUNTIME_ASSET` would have the runtime copy overwrite the
/// page's own module, and the page would load successfully and mount nothing.
const RUNTIME_ASSET: &str = "runtime";

/// The completeness stamp. Written **last** into `<out>.tmp`, and required in `<out>`
/// before [`publish`] replaces it: it means "this tree is a complete build". `--out`
/// is a name, not a proof of ownership: without a stamp, `runai build --out src`
/// renamed the source tree aside and deleted it.
const STAMP: &str = ".runai-build";

/// The ownership marker **and** the liveness lock, one file doing both jobs.
///
/// Created and exclusively locked **immediately after the scratch directory is created
/// and before any route content**, so:
///
/// - its presence means "this scratch directory is ours" and licenses removing a
///   leftover one;
/// - holding the lock means "a build is using this directory right now", so a concurrent
///   build's sweep leaves it alone. The OS releases the lock when the process dies, so a
///   killed build's tree is immediately sweepable while a live peer's is not.
///
/// The second half is why this is a lock and not just a marker: a concurrent build's
/// scratch carries the same marker, so a marker alone let round 3's `build & build` turn
/// into one build deleting the other's live scratch.
///
/// It is deliberately not the same file as [`STAMP`]: the stamp is written last and
/// licenses *publishing over* `<out>`, while this is written first and licenses
/// *deleting* scratch. It is removed from the published tree, so `<out>` never carries it.
const OWNER: &str = ".runai-scratch";

/// The suffix appended to `<out>` for a scratch directory's final component. Scratch
/// is `<out>.tmp.<pid>.<n>`: unique per build, so two concurrent builds never share
/// it, and no other process can be writing to the same tree.
const SCRATCH_SUFFIX: &str = "tmp";

/// The scratch directory inside the temporary tree. It is removed before publish,
/// so a route whose first segment is this name would be written and then deleted:
/// [`route_name`] refuses it rather than shipping a 404 that built cleanly.
const SCRATCH: &str = ".modules";

/// The document shell. `{body}` is the prerendered HTML or an empty string.
const SHELL_HEAD: &str = "<!doctype html><html><head><meta charset=\"utf-8\"><title>runai</title></head><body><div id=\"app\">";

/// One route, read and analyzed, ready to emit.
struct Prepared {
    /// The route path with `/` separators: what diagnostics report and `is_page`
    /// sees, e.g. `src/routes/blog/post.page.tsx`.
    display: String,
    /// The route name, e.g. `""` for `index`, `about`, `blog/post`.
    name: String,
    source: String,
}

/// A build failure and the exit status it owes the process.
enum Failure {
    /// A build that got far enough to be about the app: diagnostics, a render that
    /// threw, a missing runtime. `1`.
    Build(String),
    /// A refused publish: `<out>` exists but is not a previous build. `2`, because
    /// the invocation must change — but without the usage line, since the argument
    /// was well formed and the directory was simply not ours.
    Refused(String),
}

#[derive(Debug)]
enum Refusal {
    /// Everything found in one pass: diagnostics (each paired with the source that
    /// locates it) and complete sentences about route-set defects. They are reported
    /// together because a repair loop that fixes one problem per run is expensive —
    /// returning early on the first reserved name used to hide every diagnostic
    /// already collected.
    Collected {
        diagnostics: Vec<(String, Diagnostic)>,
        messages: Vec<String>,
    },
    /// One message, already a complete sentence about a system-level failure.
    Message(String),
}

/// Run `runai build …` with the arguments that follow `build`.
///
/// Returns the process exit code: `0` built, `1` the build failed (diagnostics,
/// a runtime that would not render, a missing runtime, an unreadable file), `2`
/// the command was misused. Every `1` and `2` leaves `<out>` as it was.
pub fn run(args: &[String]) -> i32 {
    let mut out: Option<&str> = None;
    let mut index = 0;

    while index < args.len() {
        let arg = args[index].as_str();
        index += 1;
        match arg {
            // `--out dist` and `--out=dist` are the same flag; both are supported
            // because a script that builds a path often writes the second form.
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

    let out = PathBuf::from(out.unwrap_or(DEFAULT_OUT));

    // `--out` is a name, so it must name a directory the build may own. `""`, `.`
    // and `..` are usage errors, and so is a directory that already holds source
    // files the build did not write: `runai build --out src` used to publish over
    // the sources and delete them. See [`refuse_out`].
    if let Err(message) = refuse_out(&out) {
        eprintln!("error: {message}");
        eprintln!("{USAGE}");
        return 2;
    }

    // Publishing is a sequence of renames on names derived from `<out>`, so exactly one
    // build may run at a time per `<out>`. The lock is an OS file lock: the kernel
    // releases it when the process dies, so a killed build cannot wedge the next one.
    // Concurrent builds therefore queue instead of interleaving, and each publishes a
    // complete tree; the scratch *names* are still unique per build, which is what lets
    // two builds assemble at the same time without sharing a directory.
    let _lock = match lock_build(&out) {
        Ok(lock) => lock,
        Err(message) => {
            eprintln!("error: {message}");
            return 1;
        }
    };

    // Before anything else: a crash between `publish`'s two renames leaves the good
    // tree in `<out>.old` and `<out>` absent, so put it back. See [`recover`].
    if let Err(message) = recover(&out) {
        eprintln!("error: {message}");
        return 1;
    }

    // Scratch is unique per build (`<out>.tmp.<pid>.<n>`), so nothing can be in this
    // build's way. Leftovers are swept here rather than refused: only those carrying
    // the ownership marker are ours, and an unrecognised directory is ignored — a name
    // this tool generates is not proof that its contents belong to this tool.
    if let Err(message) = sweep_scratch(&out) {
        eprintln!("error: {message}");
        return 1;
    }

    // Phase one: find, read, and analyze every route. Nothing is written yet, so a
    // refusal here cannot leave a partial tree.
    let prepared = match prepare() {
        Ok(prepared) => prepared,
        Err(Refusal::Collected {
            diagnostics,
            messages,
        }) => {
            for message in messages {
                eprintln!("error: {message}");
            }
            for (source, diagnostic) in diagnostics {
                eprint!("{}", diagnostic.render_human(&source));
            }
            return 1;
        }
        Err(Refusal::Message(message)) => {
            eprintln!("error: {message}");
            return 1;
        }
    };

    // Phase two: emit into `<out>.tmp`, then publish. Any failure cleans the
    // temporary tree and leaves `<out>` exactly as it was.
    match assemble(&out, &prepared) {
        Ok(()) => 0,
        Err(Failure::Build(message)) => {
            eprintln!("error: {message}");
            1
        }
        Err(Failure::Refused(message)) => {
            eprintln!("error: {message}");
            2
        }
    }
}

/// Refuse an `--out` value that must not be published over.
///
/// `""`, `.` and `..` are refused by name. A path inside the source tree is refused
/// whether or not it exists yet, because `--out src/dist` is a build that writes into
/// `src/`. An existing directory is refused when it holds any `*.tsx` file the build
/// did not write: that is the project's own source, and `runai build --out src` used to
/// rename it aside and delete it. Everything else is left to [`publish`]'s stamp gate,
/// which refuses to replace a non-empty directory that is not a previous build without
/// deleting anything. A directory that does not exist yet, or is empty, is otherwise
/// acceptable.
fn refuse_out(out: &Path) -> Result<(), String> {
    let text = out.to_string_lossy();
    if text.is_empty() {
        return Err("`--out` needs a directory: it was empty".to_string());
    }
    // Checked on the raw text before any resolution: `.` and `..` resolve to
    // directories that exist, but the message a user needs is about the argument.
    let last = out
        .components()
        .next_back()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .unwrap_or_default();
    if last == "." || last == ".." || text == "." || text == ".." {
        return Err(format!(
            "`--out {text}` is not a build directory: it is the working tree or its parent"
        ));
    }

    if let Some(source_root) = inside_sources(out) {
        return Err(format!(
            "`--out {text}` is inside the source tree `{}`: the build would write into the \
             project's own sources. Pick a directory beside them.",
            source_root.display()
        ));
    }
    if !out.exists() {
        return Ok(());
    }
    if !out.is_dir() {
        return Err(format!(
            "`--out {text}` already exists and is not a directory"
        ));
    }

    if let Some(source) = first_tsx(out)? {
        return Err(format!(
            "`--out {text}` holds the source file `{}`, which this build did not write. \
             Refusing to publish over the project's own sources; pick a new directory.",
            source.display()
        ));
    }

    Ok(())
}

/// The canonical source tree, when `out` would land in it.
///
/// `--out src/dist` does not exist yet, so no scan of `<out>` can see the danger; its
/// nearest existing ancestor can. This is deliberately keyed on the *source tree*
/// (`src/`) rather than on "an ancestor holding a `.tsx`": that broader rule would
/// refuse a normal `--out dist` in any project whose root has a stray `.tsx` file.
fn inside_sources(out: &Path) -> Option<PathBuf> {
    let source_root = Path::new(ROUTES)
        .parent()
        .unwrap_or(Path::new(""))
        .canonicalize()
        .ok()?;
    let ancestor = nearest_existing(out)?.canonicalize().ok()?;
    if ancestor.starts_with(&source_root) {
        Some(source_root)
    } else {
        None
    }
}

/// `path` itself when it exists, else its nearest existing ancestor.
///
/// `--out src/dist` has no entry to scan, but `src` does, and that is enough to tell
/// that the build would write into the sources.
fn nearest_existing(path: &Path) -> Option<PathBuf> {
    let mut candidate = path;
    loop {
        if candidate.exists() {
            return Some(candidate.to_path_buf());
        }
        candidate = candidate.parent()?;
    }
}

/// Sweep leftover scratch directories beside `<out>`.
///
/// A scratch directory is named `<out>.tmp.<pid>.<n>`, unique per build, so a leftover
/// can never be in the way of this one — which is why an unrecognised one is
/// **ignored** rather than refused. Whether it may be deleted is a separate question,
/// and contents are the only evidence available:
///
/// - named like our scratch, carrying [`OWNER`], and **not locked** — a killed build of
///   ours: remove;
/// - named like our scratch, carrying [`OWNER`], and **locked** — a build running right
///   now (possibly a concurrent one in this very process): leave it alone. Sweeping a
///   peer's live scratch is precisely how round 3 made `build & build` delete each
///   other's tree;
/// - named like our scratch with no marker — nothing says it is ours: leave it. A
///   directory killed between `mkdir` and the marker write is indistinguishable from
///   somebody else's, and refusing the build over a name this tool generates was what
///   wedged every build after one interrupted one, first in round 2 and again in round
///   3. It is harmless: it cannot block a build;
/// - the previous release's fixed `<out>.tmp` — remove when it is empty (nothing to
///   lose; a failed marker write leaves exactly that) or marked and unlocked; otherwise
///   leave it;
/// - anything else — not named like scratch at all: untouched.
///
/// The only failure is an unreadable directory, because a later build would then be
/// building beside garbage while claiming success.
fn sweep_scratch(out: &Path) -> Result<(), String> {
    let parent = match out.parent().filter(|p| !p.as_os_str().is_empty()) {
        Some(parent) => parent,
        None => Path::new("."),
    };
    let prefix = scratch_prefix(out);

    let entries = match std::fs::read_dir(parent) {
        Ok(entries) => entries,
        // A fresh nested `--out` (`build/dist`) has no parent yet, so there is nothing
        // to sweep; `Scratch::create` creates the directory.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("cannot read {}: {error}", parent.display())),
    };
    for entry in entries {
        let entry = entry.map_err(|error| format!("cannot read {}: {error}", parent.display()))?;
        let Some(kind) = scratch_kind(&entry.file_name().to_string_lossy(), &prefix) else {
            continue;
        };
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
        if !file_type.is_dir() {
            continue;
        }

        // Only a marked directory whose owner is gone is certainly ours. The previous
        // scheme's fixed name is additionally safe to remove when empty, because an
        // empty directory holds nothing that could belong to someone else — and a
        // failed marker write leaves exactly that.
        let removable = match kind {
            ScratchKind::Legacy => is_empty(&path)? || (is_owned(&path) && is_abandoned(&path)?),
            ScratchKind::Unique => is_owned(&path) && is_abandoned(&path)?,
        };
        if !removable {
            continue;
        }
        match std::fs::remove_dir_all(&path) {
            Ok(()) => {}
            // A peer removed it between the check and the removal.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!(
                    "cannot clear the leftover scratch directory {}: {error}",
                    path.display()
                ));
            }
        }
    }

    Ok(())
}

/// The final-component prefix of a scratch directory beside `out`: `dist` → `dist.tmp`,
/// so a scratch tree is `dist.tmp.<pid>.<n>` and the previous release's was `dist.tmp`.
fn scratch_prefix(out: &Path) -> String {
    sibling(out, SCRATCH_SUFFIX)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| format!("out.{SCRATCH_SUFFIX}"))
}

/// Which scheme a sibling directory's name belongs to, or `None` when it is not a
/// scratch name at all.
///
/// `<out>.tmp` is the previous release's fixed scratch name. `<out>.tmp.<numbers>` is
/// this release's. A name with any other suffix (`dist.tmp.keep`) is not ours: naming a
/// directory after scratch is not evidence that it is scratch.
fn scratch_kind(name: &str, prefix: &str) -> Option<ScratchKind> {
    if name == prefix {
        return Some(ScratchKind::Legacy);
    }
    let suffix = name.strip_prefix(prefix)?.strip_prefix('.')?;
    if suffix.is_empty() || !suffix.split('.').all(|part| part.parse::<u64>().is_ok()) {
        return None;
    }
    Some(ScratchKind::Unique)
}

/// The two scratch naming schemes, which differ in when an unmarked directory may be
/// removed. See [`sweep_scratch`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ScratchKind {
    /// `<out>.tmp`: one shared name, used by every build before this release.
    Legacy,
    /// `<out>.tmp.<pid>.<n>`: unique per build.
    Unique,
}

/// Whether `dir` carries the ownership marker — the sign that this build created it.
///
/// A **non-empty** [`OWNER`] is the marker: the build locks the file first and only then
/// writes into it, so an empty file is a directory still being created, not one that may
/// be swept. Without that distinction a concurrent sweep could see a freshly created
/// directory in the instant before its lock was taken and delete a live build's tree. An
/// empty marker left by a build killed in that instant is ignored forever rather than
/// guessed at — harmless, because scratch names are unique and it cannot block a build.
fn is_owned(dir: &Path) -> bool {
    dir.join(OWNER)
        .metadata()
        .map(|metadata| metadata.is_file() && metadata.len() > 0)
        .unwrap_or(false)
}

/// Whether the scratch tree at `dir` is abandoned: no build holds its [`OWNER`] lock.
///
/// A build holds the lock for its whole lifetime, so "the lock is free" means the build
/// that created the tree is gone and the directory may be swept. A tree from before the
/// lock existed has no marker file at all, which [`is_owned`] already excludes.
///
/// `try_lock` is granted to exactly one handle system-wide, including a second handle in
/// the same process, which is what lets two concurrent builds in one process see each
/// other. The probe handle is dropped immediately; the lock belongs to the owning build.
fn is_abandoned(dir: &Path) -> Result<bool, String> {
    let marker = dir.join(OWNER);
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&marker)
        .map_err(|error| format!("cannot read {}: {error}", marker.display()))?;
    match file.try_lock() {
        // We got it, so nobody else holds it: the tree is abandoned. Dropping `file`
        // releases it again, which is required — the sweep is about to delete it.
        Ok(()) => Ok(true),
        Err(_) => Ok(false),
    }
}

/// Whether `dir` carries the build stamp.
fn is_stamped(dir: &Path) -> bool {
    dir.join(STAMP).is_file()
}

/// Whether `dir` has no entries.
///
/// An unreadable directory is a failure rather than a guess: assuming it is empty
/// would delete something nobody could inspect.
fn is_empty(dir: &Path) -> Result<bool, String> {
    let mut entries = std::fs::read_dir(dir)
        .map_err(|error| format!("cannot read {}: {error}", dir.display()))?;
    Ok(entries.next().is_none())
}

/// The first `*.tsx` file under `dir`, recursively, or `None`.
///
/// Used by [`refuse_out`] for an existing `<out>`. The name is derived from an
/// argument, and a name is not proof that the directory belongs to this tool —
/// `runai build --out src` deleted the sources. A directory holding TypeScript is
/// somebody's project, so it is never removed without asking. The sibling paths
/// `<out>.tmp` and `<out>.old` use the stronger [`is_stamped`] test instead: both are
/// written by this build when it owns them, so the stamp is present on exactly the
/// directories it may delete.
fn first_tsx(dir: &Path) -> Result<Option<PathBuf>, String> {
    let entries = std::fs::read_dir(dir)
        .map_err(|error| format!("cannot read {}: {error}", dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|error| format!("cannot read {}: {error}", dir.display()))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
        if file_type.is_dir() {
            if let Some(found) = first_tsx(&path)? {
                return Ok(Some(found));
            }
        } else if path.to_string_lossy().ends_with(".tsx") {
            return Ok(Some(path));
        }
    }
    Ok(None)
}

/// Put a stranded previous build back before anything else runs.
///
/// [`publish`] renames `<out>` to `<out>.old` and then `<out>.tmp` to `<out>`. A
/// crash between the two leaves a complete, working tree in `<out>.old` and no
/// `<out>` at all. Without this, the next build (which may itself fail) would leave
/// that tree stranded forever. If both exist, `.old` is stale — a tree this tool
/// published carries [`STAMP`] — so it is dropped.
///
/// `.old` is a name derived from `--out`, not a proof of ownership either. An
/// unstamped `.old` is left exactly as it is: it is somebody else's directory.
fn recover(out: &Path) -> Result<(), String> {
    let backup = sibling(out, "old");
    if !backup.exists() {
        return Ok(());
    }

    if backup.join(STAMP).is_file() {
        if out.exists() {
            // Both present: `<out>` is the live tree, the backup is stale.
            std::fs::remove_dir_all(&backup)
                .map_err(|error| format!("cannot clear {}: {error}", backup.display()))?;
        } else {
            std::fs::rename(&backup, out).map_err(|error| {
                format!(
                    "cannot restore the previous build at {}: {error}",
                    backup.display()
                )
            })?;
        }
    }

    Ok(())
}

/// The error that a directory is not a previous build, for [`publish`].
fn not_a_build(out: &Path) -> String {
    format!(
        "`{}` does not look like a previous build (no `{STAMP}`): refusing to replace it. \
         Nothing was written or deleted.",
        out.display()
    )
}

/// The routes under [`ROUTES`], read, analyzed, and refused for `[param]` segments.
///
/// Every problem across every route is collected before returning — the diagnostics
/// from `analyze`, the `[param]` refusals, the reserved-name refusals and the
/// duplicate-name collisions — so one build prints the whole list rather than the
/// first file's problems. A repair loop that fixes one file at a time is expensive,
/// and `route_name(..)?` returning early used to discard diagnostics already found.
fn prepare() -> Result<Vec<Prepared>, Refusal> {
    let root = Path::new(ROUTES);
    if !root.is_dir() {
        return Err(Refusal::Message(format!(
            "no `{ROUTES}` directory: a runai app keeps its routes there"
        )));
    }

    let mut files = Vec::new();
    walk(root, &mut files).map_err(Refusal::Message)?;
    files.sort_by_key(|path| path.to_string_lossy().replace('\\', "/"));

    if files.is_empty() {
        return Err(Refusal::Message(format!(
            "no `*.page.tsx` routes under `{ROUTES}`: there is nothing to build"
        )));
    }

    let mut refused: Vec<(String, Diagnostic)> = Vec::new();
    let mut messages: Vec<String> = Vec::new();
    let mut prepared = Vec::new();
    // name → the source path that claimed it first, lowercased for the lookup. Two
    // files can derive the same route name (`about.page.tsx` and
    // `about/index.page.tsx` both give `about`), and the build used to keep the last
    // one silently, so the loser never shipped. The comparison is case-insensitive
    // because on macOS and Windows `About/index.html` and `about/index.html` are one
    // file: a byte-exact guard would pass here and silently overwrite there.
    //
    // Known boundary: `to_ascii_lowercase` folds ASCII only, so on a case-insensitive
    // filesystem `Über.page.tsx` and `über/index.page.tsx` can still collide into one
    // output (APFS is case-insensitive on macOS; NTFS on Windows is too). Closing that
    // needs Unicode case folding or normalization, and a half-right rule is worse than
    // a documented boundary, so non-ASCII names are not guarded here.
    let mut claimed: HashMap<String, (String, String)> = HashMap::new();

    for file in files {
        let display = file.to_string_lossy().replace('\\', "/");
        let source = std::fs::read_to_string(&file)
            .map_err(|error| Refusal::Message(format!("cannot read {display}: {error}")))?;

        // `analyze` is the same rule pass `runai check` runs, so a build and a check
        // cannot disagree about whether a route is legal.
        let diagnostics = analyze(&source, &display);
        refused.extend(
            diagnostics
                .into_iter()
                .filter(|diagnostic| diagnostic.severity == Severity::Error)
                .map(|diagnostic| (source.clone(), diagnostic)),
        );

        // A bad route-set name is a message, not a diagnostic, but it lands in the
        // same collected list: returning early here used to drop the diagnostics
        // already found in other routes, so a repair loop fixed one problem per run.
        let name = match route_name(&file) {
            Ok(name) => name,
            Err(message) => {
                messages.push(message);
                continue;
            }
        };

        // This milestone has no router, so a `[param]` segment has no served path:
        // writing `dist/[slug]/index.html` would be a 404 that looks built.
        if name.split('/').any(|segment| segment.contains(['[', ']'])) {
            let mut diagnostic = Diagnostic::new(
                Code::RA014,
                (0, 0),
                "dynamic route segments are not supported yet",
            );
            diagnostic.locate(&source, &display);
            refused.push((source.clone(), diagnostic));
        }

        // Two routes, one output path: whichever is written last wins and the other
        // silently does not ship. Name both so the author can decide which is right.
        if let Some((first, first_name)) = claimed.get(&name.to_ascii_lowercase()) {
            let shown = if *first_name == name {
                format!("the route `{name}`")
            } else {
                format!("`{first_name}`, which is the same route on a case-insensitive filesystem")
            };
            messages.push(format!(
                "`{first}` and `{display}` both map to {shown}: rename one, because only \
                 one can be served and the other would silently not ship"
            ));
            continue;
        }
        claimed.insert(name.to_ascii_lowercase(), (display.clone(), name.clone()));

        prepared.push(Prepared {
            display,
            name,
            source,
        });
    }

    if !refused.is_empty() || !messages.is_empty() {
        return Err(Refusal::Collected {
            diagnostics: refused,
            messages,
        });
    }
    Ok(prepared)
}

/// Every route file under `dir`, recursively.
///
/// A name beginning with `_` is a component module (`_shell.tsx`) or a support
/// directory, never a route, so it is skipped whole. A name beginning with `.` is
/// hidden, never a route: without that skip `.page.tsx` became the root page and
/// shipped its contents. Only a `*.page.tsx` file is a route; anything else is not
/// built.
///
/// A hidden *directory* is descended into rather than ignored, so `.modules` and
/// `.cache` reach [`route_name`]'s refusal instead of vanishing: a route directory
/// that is silently not built is the failure this project exists to prevent. The
/// dot-*file* skip is different — it happens here, before [`route_name`] ever sees the
/// path, so the refusal tests below deliberately assert on the directory case: a test
/// on `.page.tsx` would exercise this filter, not a rule the build enforces.
///
/// An unreadable entry is an error rather than a skip: a build that quietly omits a
/// route it could not read is a successful-looking build of the wrong site.
fn walk(dir: &Path, files: &mut Vec<PathBuf>) -> Result<(), String> {
    let entries = std::fs::read_dir(dir)
        .map_err(|error| format!("cannot read {}: {error}", dir.display()))?;

    for entry in entries {
        let entry = entry.map_err(|error| format!("cannot read {}: {error}", dir.display()))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|error| format!("cannot read {}: {error}", path.display()))?;

        if file_type.is_dir() {
            if !starts_with(&path, '_') {
                walk(&path, files)?;
            }
        } else if file_type.is_file()
            && !starts_with(&path, '_')
            && !starts_with(&path, '.')
            && path.to_string_lossy().ends_with(".page.tsx")
        {
            files.push(path);
        }
    }

    Ok(())
}

/// What a route path segment actually is on disk, so a refusal can say "file" or
/// "directory" instead of guessing.
///
/// The old message always said "hidden directory", which is false for
/// `blog/.modules.page.tsx`: that segment is a file stem, and telling an author to
/// rename a directory that does not exist sends a repair loop in circles.
#[derive(Clone, Copy)]
enum Segment<'a> {
    /// The last segment of a route path: it came from the `*.page.tsx` file name.
    File(&'a str),
    /// Any earlier segment: a directory containing the route.
    Directory(&'a str),
}

impl<'a> Segment<'a> {
    /// The segment at `index` in a `/`-split route path, where the last segment is the
    /// file's own stem and every earlier one is a directory.
    fn at(segments: &[&'a str], index: usize) -> Segment<'a> {
        let name = segments[index];
        if index + 1 == segments.len() {
            Segment::File(name)
        } else {
            Segment::Directory(name)
        }
    }

    /// The noun to use for the repair.
    fn noun(self) -> &'static str {
        match self {
            Segment::File(_) => "file",
            Segment::Directory(_) => "directory",
        }
    }
}

impl std::fmt::Display for Segment<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Segment::File(name) => write!(formatter, "the dot-prefixed file `{name}`"),
            Segment::Directory(name) => write!(formatter, "the dot-prefixed directory `{name}`"),
        }
    }
}

/// Whether the final component of `path` begins with `prefix`.
fn starts_with(path: &Path, prefix: char) -> bool {
    path.file_name()
        .is_some_and(|name| name.to_string_lossy().starts_with(prefix))
}

/// The route name for a file under [`ROUTES`].
///
/// `index.page.tsx` is `""` (the root), `blog/index.page.tsx` is `blog`, and
/// `blog/post.page.tsx` is `blog/post`. A segment that is `.` or `..`, or is empty,
/// is refused: it could only come from a hand-built path, but it is the one shape
/// that would walk out of the output tree, so it is checked here rather than trusted.
///
/// The returned `Err` is a complete sentence, not a diagnostic, because it describes a
/// route-set defect rather than a problem inside a file. [`prepare`] pushes it onto
/// the same collected list as the diagnostics: returning early threw away problems
/// already found in other routes.
fn route_name(file: &Path) -> Result<String, String> {
    let relative = file.strip_prefix(ROUTES).unwrap_or(file);
    let relative = relative.to_string_lossy().replace('\\', "/");
    let without = relative
        .strip_suffix(".page.tsx")
        .unwrap_or(relative.as_str());

    // Checked before the `index` strip, on the segments that will become the
    // output path. `.`/`..` are the only shapes that would walk out of it.
    if let Some(segment) = without
        .split('/')
        .find(|segment| *segment == "." || *segment == "..")
    {
        return Err(format!(
            "`{}` has a relative route segment (`{segment}`): it would escape the output \
             directory, so it is not buildable",
            file.display()
        ));
    }

    // `SCRATCH` and `STAMP` are names this build owns inside its output tree. A
    // route under one would either be deleted with the scratch tree or collide with
    // the stamp, so each keeps its own message — the repair is different from the
    // general hidden-directory rule below. Only the *first* segment names something
    // in the output root: `blog/.modules` is just a dot-prefixed path, and the
    // general rule below refuses it with the reason that actually applies.
    let segments: Vec<&str> = without.split('/').collect();
    if let Some(first) = segments
        .first()
        .filter(|seg| **seg == SCRATCH || **seg == STAMP)
    {
        return Err(format!(
            "`{}` has `{first}`, a name `runai build` reserves inside its output \
             directory: the route could not be served, so it is not buildable. Rename \
             the {}.",
            file.display(),
            Segment::at(&segments, 0).noun()
        ));
    }

    // Any dot-prefixed segment is refused, not just the first. `walk` skips
    // dot-prefixed *files* but descends into dot-prefixed *directories*, so
    // `src/routes/.cache/x` reached the build and shipped `dist/.cache/x/index.html` —
    // a path many static servers 404. `a/.b/c.page.tsx` shipped `dist/a/.b/c/index.html`
    // for the same reason: the refusal is about **any** segment of the output path, so
    // checking only the first left the identical defect one level deeper.
    if let Some(index) = segments.iter().position(|segment| segment.starts_with('.')) {
        let found = Segment::at(&segments, index);
        return Err(format!(
            "`{}` has {found}, whose output would contain a dot-prefixed path — many \
             static servers refuse to serve one. Rename the {}.",
            file.display(),
            found.noun()
        ));
    }

    let name = match without.strip_suffix("/index") {
        Some(prefix) => prefix,
        None if without == "index" => "",
        None => without,
    };

    Ok(name.to_string())
}

/// The output path for a route name: `index` at the root, a directory otherwise.
fn output_path(root: &Path, name: &str) -> PathBuf {
    if name.is_empty() {
        root.join("index.html")
    } else {
        root.join(name).join("index.html")
    }
}

/// The asset basename for a route name: `index` for the root, the route path
/// otherwise (`blog/post` → `assets/blog/post.js`).
fn asset_name(name: &str) -> &str {
    if name.is_empty() { "index" } else { name }
}

/// Which runtime executes a prerender module, in preference order.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Runner {
    Bun,
    Node,
}

impl Runner {
    /// The runtime the build will use: `bun` when it is on `PATH`, else `node`.
    ///
    /// Returns `None` when neither runs, which is a build failure and not a reason
    /// to ship an empty page.
    fn detect() -> Option<Runner> {
        if available("bun") {
            Some(Runner::Bun)
        } else if available("node") {
            Some(Runner::Node)
        } else {
            None
        }
    }

    /// Run `module` in `dir`, returning the HTML it wrote to stdout.
    ///
    /// The three-part check is the point: see the module docs. `stderr` is printed
    /// by the caller, so a failing route's real error reaches the agent instead of
    /// being written into `index.html`.
    fn render(self, dir: &Path, module: &str) -> Result<String, String> {
        let script = format!(
            "import('./{module}').then(m => process.stdout.write(m.render()))",
            module = module
        );
        let mut command = Command::new(match self {
            Runner::Bun => "bun",
            Runner::Node => "node",
        });
        if self == Runner::Node {
            command.args(["--input-type=module", "-e", &script]);
        } else {
            command.args(["-e", &script]);
        }

        let output = command
            .current_dir(dir)
            .output()
            .map_err(|error| format!("cannot run {}: {error}", self.name()))?;

        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        // A failing module still exits 0 on both runtimes, so the status alone says
        // nothing: an empty stderr and a non-empty stdout are the real signals.
        if !output.status.success() || !stderr.is_empty() {
            return Err(format!(
                "{} could not render the route:\n{}",
                self.name(),
                stderr.trim_end()
            ));
        }
        if output.stdout.is_empty() {
            return Err(format!(
                "{} printed no HTML for the route: `render()` must write the page",
                self.name()
            ));
        }

        String::from_utf8(output.stdout)
            .map_err(|error| format!("{} printed invalid UTF-8: {error}", self.name()))
    }

    fn name(self) -> &'static str {
        match self {
            Runner::Bun => "bun",
            Runner::Node => "node",
        }
    }
}

/// Whether `binary --version` runs successfully.
fn available(binary: &str) -> bool {
    Command::new(binary)
        .arg("--version")
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

/// The emitted module's kind.
enum Emitted {
    Static(String),
    Island(String),
}

/// The module for one route, whichever emitter its directive calls for.
///
/// The checked emitters are the gate, and exactly one of them accepts a page whose
/// analysis passed: the other returns the mode refusal. So this asks the prerender
/// emitter first and treats its `Err` as "not static", which means the directive
/// rule is never re-derived here. If the client emitter also refuses, the prerender
/// error is returned — the analysis already passed, so that is the informative one.
fn emit(prepared: &Prepared) -> Result<Emitted, Vec<Diagnostic>> {
    match emit_prerender_checked(&prepared.source, &prepared.display) {
        Ok(module) => Ok(Emitted::Static(module)),
        Err(static_error) => {
            match emit_client_checked(&prepared.source, &prepared.display, RUNTIME_URL) {
                Ok(module) => Ok(Emitted::Island(module)),
                Err(_) => Err(static_error),
            }
        }
    }
}

/// Write every route into a scratch directory and publish it as `<out>`.
///
/// The scratch tree is removed on any failure, so `dist/` never exists in
/// half-written form. `<out>` itself is only touched by [`publish`], which runs after
/// every route has been written and every render has succeeded.
///
/// The scratch directory is [`Scratch`]: `<out>.tmp.<pid>.<n>`, unique per build, so
/// two concurrent builds never share it. Its `Drop` removes the tree on every early
/// return, so a normal failure leaves nothing behind.
fn assemble(out: &Path, prepared: &[Prepared]) -> Result<(), Failure> {
    let scratch = Scratch::create(out).map_err(Failure::Build)?;
    let tmp = scratch.path();
    assemble_into(tmp, prepared).map_err(Failure::Build)?;
    publish(scratch, out)
}

/// A build's own scratch directory. Removed on `Drop` unless [`publish`] consumed it.
///
/// The name embeds the pid and a per-process counter, so two builds — concurrent
/// threads, nested invocations, or an old leftover — never resolve to the same path.
/// That is the whole reason ownership stops needing to be inferred from contents: no
/// other process can even be writing here.
struct Scratch {
    path: PathBuf,
    /// Held for the build's lifetime; dropped with the guard, which is what tells a
    /// concurrent sweep that this tree is live.
    lock: Option<std::fs::File>,
    /// Whether `Drop` should remove `path`. Cleared by [`Scratch::consumed`], which
    /// happens once the tree has been renamed into place.
    armed: bool,
}

impl Scratch {
    /// Create a fresh, empty scratch directory beside `out`, marked and locked.
    ///
    /// [`OWNER`] is written and locked before any route content, so a build killed at
    /// any later point leaves a directory the next build's [`sweep_scratch`] can
    /// identify as ours and as abandoned.
    fn create(out: &Path) -> Result<Scratch, String> {
        let parent = match out.parent().filter(|p| !p.as_os_str().is_empty()) {
            Some(parent) => parent.to_path_buf(),
            None => PathBuf::from("."),
        };
        std::fs::create_dir_all(&parent)
            .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;

        let prefix = scratch_prefix(out);

        // A counter rather than trusting the pid alone: two threads in one process
        // share the pid, and concurrency in one process is exactly the case that broke.
        for attempt in 0..1024u32 {
            let name = format!("{prefix}.{}.{attempt}", std::process::id());
            let path = parent.join(name);
            match std::fs::create_dir(&path) {
                Ok(()) => {
                    // Lock first, then write the marker: a sweep treats a non-empty
                    // marker as owned, so publishing that sign only after the lock is
                    // held means no peer can see "marked and unlocked" on a live tree.
                    let marker = path.join(OWNER);
                    let owned = std::fs::OpenOptions::new()
                        .read(true)
                        .write(true)
                        .create(true)
                        .truncate(false)
                        .open(&marker)
                        .and_then(|mut file| {
                            file.lock()?;
                            file.write_all(
                                format!("runai {} build\n", runai_compiler::VERSION).as_bytes(),
                            )?;
                            file.flush()?;
                            Ok(file)
                        });
                    let lock = match owned {
                        Ok(lock) => lock,
                        Err(error) => {
                            let _ = std::fs::remove_dir_all(&path);
                            return Err(format!(
                                "cannot mark the scratch directory {}: {error}",
                                path.display()
                            ));
                        }
                    };
                    return Ok(Scratch {
                        path,
                        lock: Some(lock),
                        armed: true,
                    });
                }
                // Another build already took this name: try the next counter.
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => {
                    return Err(format!(
                        "cannot create the scratch directory {}: {error}",
                        path.display()
                    ));
                }
            }
        }

        Err(format!(
            "cannot find a free scratch name for {}: tried {prefix}.<pid>.0..1023",
            out.display()
        ))
    }

    fn path(&self) -> &Path {
        &self.path
    }

    /// Disarm the guard: the tree has been renamed into place, so there is nothing to
    /// remove. The lock file is scratch bookkeeping and is cleared before publishing —
    /// see [`assemble_into`] — so it never reaches `<out>`.
    fn consumed(mut self) {
        self.armed = false;
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        // Release the lock before removing: on Linux removing a directory does not
        // require the open handle to be closed, but a peer's `is_abandoned` probe must
        // see the tree as deletable rather than as a live build.
        self.lock.take();
        // Best effort: this runs on a failure path that is already reporting an error,
        // and a leftover directory carrying `OWNER` is swept by the next build.
        if self.path.exists() {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

/// The body of [`assemble`], with `tmp` already created.
fn assemble_into(tmp: &Path, prepared: &[Prepared]) -> Result<(), String> {
    // Modules live in a scratch directory inside `tmp` so one `remove_dir_all`
    // cleans everything, and so a crash cannot leave a `.mjs` in the published
    // tree. It is removed before publishing; nothing here is a route.
    let modules = tmp.join(SCRATCH);
    let mut rendered: Vec<(usize, Emitted)> = Vec::new();

    for (index, route) in prepared.iter().enumerate() {
        let module = emit(route).map_err(|diagnostics| {
            let mut message = String::new();
            for diagnostic in diagnostics {
                message.push_str(&diagnostic.render_human(&route.source));
            }
            message.trim_end().to_string()
        })?;
        // The asset path `assets/runtime.js` is where the runtime is copied, so an
        // island route that derives it would have its own module overwritten and the
        // page would load and mount nothing. This check is here, not in `prepare`,
        // because only here is the mode known: a static route emits no asset at all,
        // so a static `runtime` route is harmless and must build.
        //
        // Compared case-insensitively: on macOS and Windows `assets/Runtime.js` IS
        // `assets/runtime.js`, so a byte-exact guard would pass on Linux and silently
        // overwrite the module on the platforms where it matters.
        if matches!(module, Emitted::Island(_))
            && asset_name(&route.name).eq_ignore_ascii_case(RUNTIME_ASSET)
        {
            return Err(format!(
                "`{}` is an island route whose asset path is `assets/{}.js` — reserved \
                 for the runtime copy. That copy would overwrite this route's module, so \
                 the page would load and mount nothing. Rename the route.",
                route.display,
                asset_name(&route.name)
            ));
        }
        rendered.push((index, module));
    }

    let mut runner: Option<Runner> = None;
    let mut has_island = false;

    for (index, module) in &rendered {
        let route = &prepared[*index];
        match module {
            Emitted::Static(module) => {
                let runtime = match runner {
                    Some(runner) => runner,
                    None => {
                        let detected = Runner::detect().ok_or_else(|| {
                            "neither `bun` nor `node` is on PATH: a static route is prerendered by \
                             executing its module, so the build cannot continue"
                                .to_string()
                        })?;
                        runner = Some(detected);
                        detected
                    }
                };

                std::fs::create_dir_all(&modules)
                    .map_err(|error| format!("cannot create {}: {error}", modules.display()))?;
                let filename = format!("route{index}.mjs");
                std::fs::write(modules.join(&filename), module)
                    .map_err(|error| format!("cannot write {filename}: {error}"))?;
                let html = runtime.render(&modules, &filename)?;

                let file = output_path(tmp, &route.name);
                verify_under(tmp, &file)?;
                write_file(&file, &format!("{SHELL_HEAD}{html}</div></body></html>\n"))?;
            }
            Emitted::Island(module) => {
                let file = output_path(tmp, &route.name);
                verify_under(tmp, &file)?;
                let asset = asset_name(&route.name);
                let html = format!(
                    "{SHELL_HEAD}</div><script type=\"module\" src=\"/assets/{asset}.js\"></script></body></html>\n"
                );
                write_file(&file, &html)?;

                let path = tmp.join("assets").join(format!("{asset}.js"));
                verify_under(tmp, &path)?;
                write_file(&path, module)?;
                has_island = true;
            }
        }
    }

    if has_island {
        // One runtime copy serves every island; the resolved path in any error is
        // the one the config named, so the repair is obvious.
        let root = std::env::current_dir()
            .map_err(|error| format!("cannot read the working directory: {error}"))?;
        let runtime = runtime_path(&root)?;
        let bytes = std::fs::read(&runtime).map_err(|error| {
            format!(
                "cannot read the runtime named by `{CONFIG}` at {}: {error}",
                runtime.display()
            )
        })?;
        let path = tmp.join("assets").join("runtime.js");
        verify_under(tmp, &path)?;
        std::fs::write(&path, bytes)
            .map_err(|error| format!("cannot write {}: {error}", path.display()))?;
    }

    // The scratch modules are not part of the site.
    if modules.exists() {
        std::fs::remove_dir_all(&modules)
            .map_err(|error| format!("cannot clear {}: {error}", modules.display()))?;
    }

    // The stamp must be the last thing written into the temporary tree: its presence
    // is the only claim that this directory is a complete build, and [`publish`]
    // refuses to replace any directory that lacks it.
    let stamp = tmp.join(STAMP);
    write_file(
        &stamp,
        &format!("runai {} build\n", runai_compiler::VERSION),
    )?;

    // The ownership marker has done its job and is not part of the site: the published
    // tree carries the completeness stamp only. It is unlinked *after* the stamp is
    // written, so a crash in this window leaves a tree that is still recognisably ours
    // either way; removed before it would reopen the unclaimed-leftover window. The
    // lock handle outlives the unlink (it is held until `Scratch` drops), and an open
    // but unlinked marker is invisible to [`sweep_scratch`], which only looks at names
    // on disk.
    let owner = tmp.join(OWNER);
    if owner.exists() {
        std::fs::remove_file(&owner)
            .map_err(|error| format!("cannot clear {}: {error}", owner.display()))?;
    }

    Ok(())
}

/// `file`'s parent must be inside `root`, checked by canonical path.
///
/// A string-prefix test would accept `dist-evil` for root `dist`, and would not
/// notice a symlink. Canonicalizing both sides and using `starts_with` on paths
/// compares real components. The parent must already exist, which it does whenever
/// this is called: the output directory is created before its file is written.
fn verify_under(root: &Path, file: &Path) -> Result<(), String> {
    if !file.is_absolute() && file.components().any(|part| part.as_os_str() == "..") {
        return Err(format!(
            "`{}` would escape the output directory",
            file.display()
        ));
    }

    let root = root
        .canonicalize()
        .map_err(|error| format!("cannot resolve {}: {error}", root.display()))?;
    let parent = file
        .parent()
        .ok_or_else(|| format!("`{}` has no parent directory", file.display()))?;
    std::fs::create_dir_all(parent)
        .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
    let parent = parent
        .canonicalize()
        .map_err(|error| format!("cannot resolve {}: {error}", parent.display()))?;

    if parent.starts_with(&root) {
        Ok(())
    } else {
        Err(format!(
            "`{}` resolves outside the output directory `{}`",
            file.display(),
            root.display()
        ))
    }
}

/// Write `contents` to `file`, creating the parent directories first.
fn write_file(file: &Path, contents: &str) -> Result<(), String> {
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
    }
    std::fs::write(file, contents)
        .map_err(|error| format!("cannot write {}: {error}", file.display()))
}

/// The resolved runtime path from `<cwd>/runai.json`.
fn runtime_path(root: &Path) -> Result<PathBuf, String> {
    let config = root.join(CONFIG);
    let text = std::fs::read_to_string(&config).map_err(|error| {
        format!(
            "this app has an island route, so it needs `{CONFIG}` naming its runtime at {}: {error}",
            config.display()
        )
    })?;
    let value = runtime_value(&text).map_err(|error| {
        format!(
            "`{CONFIG}` at {} must contain {{\"runtime\":\"<path>\"}}: {error}",
            config.display()
        )
    })?;
    Ok(root.join(value))
}

/// The `runtime` string in a `runai.json` document.
///
/// The CLI deliberately has no JSON dependency, and this file's shape is fixed by
/// the plan, so this reads exactly the one string and rejects anything it cannot
/// read unambiguously. It handles the standard JSON escapes in a string literal.
fn runtime_value(text: &str) -> Result<String, String> {
    let key = text
        .find("\"runtime\"")
        .ok_or_else(|| "no `runtime` key".to_string())?;
    let rest = &text[key + "\"runtime\"".len()..];
    let colon = rest
        .find(':')
        .ok_or_else(|| "`runtime` has no value".to_string())?;
    let rest = rest[colon + 1..].trim_start();
    json_string(rest).ok_or_else(|| "`runtime` is not a string".to_string())
}

/// The JSON string literal at the start of `text`, decoded, or `None`.
///
/// A `\uXXXX` escape that is a UTF-16 surrogate is combined with a following low
/// surrogate, because a path may legitimately contain an emoji and
/// `char::from_u32` refuses a lone surrogate. An unpaired surrogate is not a
/// character at all, so it is rejected rather than replaced.
fn json_string(text: &str) -> Option<String> {
    let mut chars = text.chars();
    if chars.next()? != '"' {
        return None;
    }

    let mut value = String::new();
    while let Some(c) = chars.next() {
        match c {
            '"' => return Some(value),
            '\\' => match chars.next()? {
                '"' => value.push('"'),
                '\\' => value.push('\\'),
                '/' => value.push('/'),
                'b' => value.push('\u{8}'),
                'f' => value.push('\u{c}'),
                'n' => value.push('\n'),
                'r' => value.push('\r'),
                't' => value.push('\t'),
                'u' => {
                    let code = hex4(&mut chars)?;
                    if (0xD800..=0xDBFF).contains(&code) {
                        // A high surrogate is only half a character; the low half
                        // must follow or the escape is not a character.
                        if chars.next()? != '\\' || chars.next()? != 'u' {
                            return None;
                        }
                        let low = hex4(&mut chars)?;
                        if !(0xDC00..=0xDFFF).contains(&low) {
                            return None;
                        }
                        let combined = 0x10000 + ((code - 0xD800) << 10) + (low - 0xDC00);
                        value.push(char::from_u32(combined)?);
                    } else {
                        value.push(char::from_u32(code)?);
                    }
                }
                _ => return None,
            },
            // A raw control character is invalid JSON and could not be a path here.
            c if c.is_control() => return None,
            c => value.push(c),
        }
    }
    None
}

/// The next four characters as a hex `u16`, or `None` when they are not hex.
fn hex4(chars: &mut std::str::Chars<'_>) -> Option<u32> {
    let hex: String = chars.by_ref().take(4).collect();
    if hex.len() != 4 {
        return None;
    }
    u32::from_str_radix(&hex, 16).ok()
}

/// The build lock's path for `out`: a file in the system temp directory keyed by the
/// resolved output path.
///
/// It is deliberately **not** beside `<out>`: a lock file next to the artifact would be
/// written into the user's project on every build, and deleting it is not an option —
/// unlinking a lock file another process is blocked on splits the lock into two. Keying
/// by the resolved output path keeps two builds of the same `--out` in one lock while a
/// build of a different `--out` proceeds independently, and leaves the project clean.
///
/// The key is `DefaultHasher` over the resolved path. Two builds of this binary agree on
/// it, which is all locking requires; a hash change between releases would only mean
/// two different releases do not queue with each other.
fn build_lock_path(out: &Path) -> PathBuf {
    use std::hash::{Hash, Hasher};

    // Key on the resolved *parent* plus the final name, so `dist` and `./dist` are one
    // lock (`dist` need not exist yet) while `dist` and `other` are two. Canonicalizing
    // `<out>` itself would fail before the first build and key every output of a fresh
    // project to the same lock, serializing builds that share nothing.
    let name = out
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| out.to_string_lossy().into_owned());
    let parent = match out.parent().filter(|p| !p.as_os_str().is_empty()) {
        Some(parent) => nearest_existing(parent)
            .and_then(|existing| existing.canonicalize().ok())
            .unwrap_or_else(|| parent.to_path_buf()),
        None => std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
    };

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    parent.to_string_lossy().hash(&mut hasher);
    name.hash(&mut hasher);
    std::env::temp_dir().join(format!("runai-build-{:016x}.lock", hasher.finish()))
}

/// Hold the build lock for the lifetime of the returned guard.
///
/// Blocks until any other build of the same `<out>` finishes. The kernel releases the
/// lock when the process ends, so a killed build does not wedge the next one; there is
/// no stale-lock/heal logic because there is nothing to go stale.
fn lock_build(out: &Path) -> Result<std::fs::File, String> {
    let path = build_lock_path(out);
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .map_err(|error| format!("cannot open the build lock {}: {error}", path.display()))?;
    file.lock()
        .map_err(|error| format!("cannot lock {}: {error}", path.display()))?;
    Ok(file)
}

/// Publish a complete `scratch` tree as `out`, keeping the previous `out` on failure.
///
/// On Linux `rename(tmp, out)` fails when `out` is a non-empty directory, so `out`
/// is moved aside first. If the second rename fails the old tree is moved back, so
/// the failure mode is "the previous build is still there", never "no dist at all".
///
/// Two gates run before anything is removed. First, the tree about to be installed
/// must itself carry [`STAMP`]: that is a structural check of the artifact, not trust
/// that [`assemble_into`] happened to write it last, so a future code path that
/// returns early cannot publish an incomplete tree. Second, `out` is replaced only
/// when it carries [`STAMP`] too — the name of a directory is not proof that this tool
/// built it: without this gate `runai build --out src` moved the source tree aside and
/// deleted it. An unstamped `out` is refused with nothing written and nothing deleted.
///
/// `scratch` is consumed: on success its tree has been renamed into place, and on any
/// failure its `Drop` removes it.
///
/// Every step here assumes it is the only build touching these names, which
/// [`lock_build`] guarantees: the whole run holds the per-`<out>` lock. Without it, two
/// builds could interleave the renames and one could see a `.old` that the other had
/// just created but not yet stamped.
fn publish(scratch: Scratch, out: &Path) -> Result<(), Failure> {
    let tmp = scratch.path().to_path_buf();
    let backup = sibling(out, "old");

    // The tree being installed must be a complete build. This is the same fact that
    // licenses replacing `<out>`, checked on the incoming side: a build that skipped
    // the stamp would otherwise install a tree the next build refuses to replace.
    if !is_stamped(&tmp) {
        return Err(Failure::Build(format!(
            "the assembled tree at {} does not carry `{STAMP}`, so it is not a complete \
             build: refusing to publish it. Nothing was written or deleted.",
            tmp.display()
        )));
    }

    // The stamp gate comes first so a refusal happens before anything is removed.
    // An *empty* unstamped directory is allowed: there is no content to destroy, and
    // `mkdir dist && runai build` is a normal CI shape. Anything with content and no
    // stamp is not a build and is refused.
    let had_previous = out.exists();
    if had_previous && !out.join(STAMP).is_file() && !is_empty(out).map_err(Failure::Build)? {
        return Err(Failure::Refused(not_a_build(out)));
    }

    // `.old` is another derived name, so it is removed only when it is a tree this
    // tool published. Anything else is left alone and the build stops rather than
    // destroying a directory it cannot identify.
    if backup.exists() {
        if !backup.join(STAMP).is_file() {
            return Err(Failure::Build(format!(
                "{} exists but is not a previous build (no `{STAMP}`), so it will not be \
                 removed. Rename it and build again.",
                backup.display()
            )));
        }
        std::fs::remove_dir_all(&backup).map_err(|error| {
            Failure::Build(format!("cannot clear {}: {error}", backup.display()))
        })?;
    }

    if had_previous {
        std::fs::rename(out, &backup).map_err(|error| {
            Failure::Build(format!(
                "cannot move the previous `{}` aside ({error}); it was left untouched",
                out.display()
            ))
        })?;
    }

    if let Err(error) = std::fs::rename(&tmp, out) {
        if had_previous {
            // Best effort: the error below is the one worth reporting, but the old
            // tree must go back if it can.
            let _ = std::fs::rename(&backup, out);
        }
        return Err(Failure::Build(format!(
            "cannot publish {}: {error}",
            out.display()
        )));
    }

    // The tree is live under its final name; nothing is left for `Drop` to do.
    scratch.consumed();

    if had_previous {
        // The new tree is live; a stale backup is untidy, not a reason to fail a
        // build that already succeeded.
        let _ = std::fs::remove_dir_all(&backup);
    }

    Ok(())
}

/// `path` with `suffix` appended to its final component, as a sibling.
fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path
        .file_name()
        .map(|name| name.to_os_string())
        .unwrap_or_default();
    name.push(format!(".{suffix}"));
    path.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    // The route → output path contract. `index` is the root, anything else is a
    // directory, and a nested route keeps its nesting — the shape a static server
    // serves as `/`, `/about`, `/blog/post`.
    #[test]
    fn route_names_map_to_the_documented_output_paths() {
        let root = Path::new("dist");
        assert_eq!(output_path(root, ""), Path::new("dist/index.html"));
        assert_eq!(
            output_path(root, "about"),
            Path::new("dist/about/index.html")
        );
        assert_eq!(
            output_path(root, "blog/post"),
            Path::new("dist/blog/post/index.html")
        );
        assert_eq!(asset_name(""), "index");
        assert_eq!(asset_name("blog/post"), "blog/post");
    }

    // `blog/index.page.tsx` is the route `blog`, not `blog/index`, so its output is
    // `dist/blog/index.html`. The strip is what makes that so; a regression would
    // build `dist/blog/index/index.html` and 404 the real path.
    #[test]
    fn a_nested_index_collapses_to_its_directory() {
        let name = route_name(Path::new("src/routes/blog/index.page.tsx")).unwrap();
        assert_eq!(name, "blog");
        let root_name = route_name(Path::new("src/routes/index.page.tsx")).unwrap();
        assert_eq!(root_name, "");
    }

    // A name that walks up is refused before any write: the segments are checked,
    // not the canonical result, so the refusal does not depend on what exists.
    #[test]
    fn a_relative_route_segment_is_refused() {
        let error = route_name(Path::new("src/routes/../escape.page.tsx"));
        assert!(
            matches!(&error, Err(message) if message.contains("escape")),
            "{error:?}"
        );
    }

    // The scratch directory is deleted before publishing, so a route under it would
    // vanish. Checked on the first segment, before the `index` strip, so both
    // `.modules.page.tsx` and `.modules/x.page.tsx` are caught.
    #[test]
    fn a_route_under_a_reserved_name_is_refused() {
        for path in [
            "src/routes/.modules.page.tsx",
            "src/routes/.modules/x.page.tsx",
            "src/routes/.runai-build.page.tsx",
        ] {
            let error = route_name(Path::new(path));
            assert!(
                matches!(&error, Err(message) if message.contains("reserves")),
                "{path}: {error:?}"
            );
        }
        // Any other hidden first segment is refused by the general rule, so a typo
        // in a scratch name does not ship a dot-prefixed path.
        for path in [
            "src/routes/.cache/x.page.tsx",
            "src/routes/.hidden.page.tsx",
        ] {
            let error = route_name(Path::new(path));
            assert!(
                matches!(&error, Err(message) if message.contains("dot-prefixed")),
                "{path}: {error:?}"
            );
        }
        // A later segment with the same name is not the scratch directory, but it IS a
        // dot-prefixed output component, so the general rule refuses it: it used to
        // build `dist/blog/.modules/index.html`, which many static servers 404.
        let error = route_name(Path::new("src/routes/blog/.modules.page.tsx"));
        assert!(
            matches!(&error, Err(message) if message.contains("dot-prefixed")),
            "{error:?}"
        );
    }

    // D: the message must name what the offending segment actually is. `blog/.modules`
    // is a *file stem*, so the old "rename the directory" advice pointed at something
    // that does not exist and sent the repair loop in circles.
    #[test]
    fn a_refusal_names_a_file_stem_or_a_directory_as_found() {
        let stem = route_name(Path::new("src/routes/blog/.modules.page.tsx")).unwrap_err();
        assert!(stem.contains("dot-prefixed file `.modules`"), "{stem}");
        assert!(stem.ends_with("Rename the file."), "{stem}");

        let dir = route_name(Path::new("src/routes/a/.b/c.page.tsx")).unwrap_err();
        assert!(dir.contains("dot-prefixed directory `.b`"), "{dir}");
        assert!(dir.ends_with("Rename the directory."), "{dir}");

        // The reserved-name refusal follows the same rule: `.modules/x` is a directory,
        // `.runai-build` alone is the file stem the route would be named after.
        let reserved_dir = route_name(Path::new("src/routes/.modules/x.page.tsx")).unwrap_err();
        assert!(
            reserved_dir.ends_with("Rename the directory."),
            "{reserved_dir}"
        );
    }

    // The dot rule scans every segment of the *relative route path*, not just the
    // first: `a/.b/c.page.tsx` shipped `dist/a/.b/c/index.html` while the first-segment
    // check passed, because the defect and the reason are about the output path.
    #[test]
    fn a_dot_segment_below_the_first_is_refused() {
        let error = route_name(Path::new("src/routes/a/.b/c.page.tsx"));
        assert!(
            matches!(&error, Err(message) if message.contains(".b") && message.contains("dot-prefixed")),
            "{error:?}"
        );
        // A normal nested route is unaffected: no segment begins with a dot.
        assert_eq!(
            route_name(Path::new("src/routes/blog/x.page.tsx")).unwrap(),
            "blog/x"
        );
    }

    // `.` and `..` are refused on any segment, including one that is not the first.
    #[test]
    fn a_dot_segment_anywhere_is_refused() {
        assert!(route_name(Path::new("src/routes/blog/../x.page.tsx")).is_err());
    }

    // E: `publish` checks the incoming tree, not the fact that `assemble_into` was the
    // one who wrote it. A scratch tree without the completeness stamp is refused, so a
    // future early return in the assembler cannot install a half-built site.
    #[test]
    fn publish_refuses_a_tree_that_does_not_carry_the_stamp() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("dist");
        let scratch = Scratch::create(&out).unwrap();
        std::fs::write(scratch.path().join("index.html"), "half a site\n").unwrap();

        let error = match publish(scratch, &out) {
            Ok(()) => panic!("an unstamped tree must not be published"),
            Err(error) => error,
        };
        let Failure::Build(message) = error else {
            panic!("an unstamped tree is a build failure, not a refusal");
        };
        assert!(message.contains(STAMP), "{message}");
        assert!(!out.exists(), "nothing was published");

        // The guard cleaned up after itself, and the (empty) `<out>` was not created.
        let left: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert!(left.is_empty(), "leftovers: {left:?}");
    }

    // `<out>.tmp.<pid>.<n>` must differ per build even within one process: the pid is
    // shared by concurrent threads, which is exactly the case that broke.
    #[test]
    fn scratch_names_are_unique_within_one_process() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("dist");
        let first = Scratch::create(&out).unwrap();
        let second = Scratch::create(&out).unwrap();
        assert_ne!(first.path(), second.path());
        assert!(first.path().join(OWNER).is_file());
        assert!(second.path().join(OWNER).is_file());
    }

    // The marker is written only after the lock is held, so a non-empty marker means a
    // lock is in place. An EMPTY marker is a directory mid-creation, and a sweep must not
    // treat it as ours: that instant is a live build. It is ignored instead — never
    // refused, because it cannot block a build.
    #[test]
    fn an_empty_marker_is_not_ownership() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("dist");
        let creating = dir.path().join("dist.tmp.1.0");
        std::fs::create_dir_all(&creating).unwrap();
        std::fs::write(creating.join(OWNER), "").unwrap();
        std::fs::write(creating.join("index.html"), "half\n").unwrap();

        sweep_scratch(&out).unwrap();

        assert!(
            creating.join("index.html").is_file(),
            "a directory whose marker is still empty is being created, not abandoned"
        );
    }

    // A leftover carrying `OWNER` but left unlocked is abandoned: its build is gone,
    // so it is swept. An unmarked one is ignored, never refused, because refusing over
    // a name this tool generates is the wedge. The locked case is
    // `a_live_builds_scratch_is_not_swept` in the integration tests, where a real
    // process-level lock is the point.
    #[test]
    fn sweep_removes_an_abandoned_marked_scratch_and_ignores_an_unmarked_one() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("dist");
        let marked = dir.path().join("dist.tmp.1.0");
        let unmarked = dir.path().join("dist.tmp.2.0");
        std::fs::create_dir_all(&marked).unwrap();
        std::fs::write(marked.join(OWNER), "runai 0.1.0 build\n").unwrap();
        std::fs::create_dir_all(&unmarked).unwrap();
        std::fs::write(unmarked.join("keep.txt"), "mine\n").unwrap();

        sweep_scratch(&out).unwrap();

        assert!(
            !marked.exists(),
            "a marked, unlocked leftover is abandoned and is removed"
        );
        assert!(
            unmarked.join("keep.txt").is_file(),
            "an unmarked leftover is not ours and is not"
        );
    }

    // The previous release used one fixed `dist.tmp`; a failed marker write left it
    // EMPTY, and round 3 wedged forever on exactly that. Empty is removable.
    #[test]
    fn sweep_removes_an_empty_legacy_scratch_and_keeps_a_full_one() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("dist");
        let empty = dir.path().join("dist.tmp");
        std::fs::create_dir(&empty).unwrap();
        sweep_scratch(&out).unwrap();
        assert!(!empty.exists());

        let full = dir.path().join("dist.tmp");
        std::fs::create_dir_all(full.join("keep")).unwrap();
        std::fs::write(full.join("keep/notes.md"), "mine\n").unwrap();
        sweep_scratch(&out).unwrap();
        assert!(full.join("keep/notes.md").is_file());
    }

    // The lock is what makes the publish sequence single-writer. Two handles on the
    // same output must not both hold it, and it lives outside the tree being published.
    #[test]
    fn the_build_lock_admits_one_holder_and_lives_outside_the_output() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("dist");

        let first = lock_build(&out).unwrap();
        let path = build_lock_path(&out);
        assert!(
            !path.starts_with(dir.path()),
            "the lock must not be written into the project: {}",
            path.display()
        );
        assert_ne!(
            build_lock_path(&dir.path().join("other")),
            path,
            "different outputs must not share a lock"
        );

        // A second handle must not acquire it while the first is held.
        let probe = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        assert!(
            probe.try_lock().is_err(),
            "a second handle must not hold the lock"
        );

        drop(first);
        assert!(
            probe.try_lock().is_ok(),
            "the lock is released when its holder is dropped"
        );
    }

    // A killed build holds the lock no longer than the process lives — the kernel
    // releases it — so the next build must not wedge. A pid file would have failed
    // here: a stale pid file is indistinguishable from a live build.
    #[test]
    fn a_released_lock_leaves_the_next_build_free_to_publish() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("dist");
        drop(lock_build(&out).unwrap());
        assert!(
            lock_build(&out).is_ok(),
            "a released lock must be reacquirable"
        );
    }

    // `dist.tmp.keep` is somebody's directory that merely resembles ours, and
    // `dist.tmpfile` is not a scratch name.
    #[test]
    fn only_generated_scratch_names_are_candidates() {
        assert_eq!(
            scratch_kind("dist.tmp", "dist.tmp"),
            Some(ScratchKind::Legacy)
        );
        assert_eq!(
            scratch_kind("dist.tmp.123.0", "dist.tmp"),
            Some(ScratchKind::Unique)
        );
        assert_eq!(
            scratch_kind("dist.tmp.4294967294.3", "dist.tmp"),
            Some(ScratchKind::Unique)
        );
        for name in [
            "dist.tmp.keep",
            "dist.tmpfile",
            "dist.old",
            "distx.tmp.1.0",
            "dist",
        ] {
            assert_eq!(scratch_kind(name, "dist.tmp"), None, "{name}");
        }
    }

    // The degenerate `--out` values are usage errors before any resolution, because
    // resolving `.` would land on a directory that does exist.
    #[test]
    fn degenerate_out_values_are_refused_by_name() {
        for out in ["", ".", ".."] {
            assert!(refuse_out(Path::new(out)).is_err(), "{out:?}");
        }
        // A path whose last component is `..` is the same shape. `dist/.` is not:
        // `Path` normalizes it to `dist`, which is a legitimate target.
        assert!(refuse_out(Path::new("dist/..")).is_err());
        assert_eq!(Path::new("dist/."), Path::new("dist"));
    }

    // A non-existent directory and a stamped one are both acceptable; the stamp is
    // what separates "a previous build" from "the project".
    #[test]
    fn a_missing_or_stamped_out_is_accepted() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("dist");
        assert!(refuse_out(&out).is_ok(), "a fresh path must be accepted");
        std::fs::create_dir(&out).unwrap();
        std::fs::write(out.join(STAMP), "runai 0.1.0 build\n").unwrap();
        assert!(refuse_out(&out).is_ok(), "a stamped tree must be accepted");
    }

    // `the_stamp_is_a_dotfile_naming_the_version` was deleted here: it rebuilt the
    // expected `format!` and asserted it. The integration test
    // `a_first_build_writes_the_stamp_and_a_second_build_succeeds` reads a real stamp.

    #[test]
    fn the_runtime_string_is_read_with_its_escapes_decoded() {
        assert_eq!(
            runtime_value(r#"{"runtime":"packages/runai/runtime.js"}"#).unwrap(),
            "packages/runai/runtime.js"
        );
        // A quoted path (`"a\"b"`) and a Windows-style one both survive decoding.
        assert_eq!(
            runtime_value(r#"{"runtime":"a\"b.js"}"#).unwrap(),
            "a\"b.js"
        );
        assert_eq!(
            runtime_value(r#"{"runtime":"C:\\apps\\runtime.js"}"#).unwrap(),
            "C:\\apps\\runtime.js"
        );
        // A surrogate pair is one character, not two replacement chars.
        assert_eq!(
            runtime_value(r#"{"runtime":"\ud83d\ude80.js"}"#).unwrap(),
            "\u{1F680}.js"
        );
        // `runtime` may be the second key; the value is still found.
        assert_eq!(
            runtime_value(r#"{"port":3000,"runtime":"r.js"}"#).unwrap(),
            "r.js"
        );
    }

    #[test]
    fn a_config_without_a_runtime_string_is_refused() {
        assert!(runtime_value("{}").is_err());
        assert!(runtime_value(r#"{"runtime":42}"#).is_err());
        assert!(runtime_value(r#"{"runtime":"unterminated"#).is_err());
        // A lone high surrogate is not a character, so the value is not readable.
        assert!(runtime_value(r#"{"runtime":"\ud83d.js"}"#).is_err());
    }
}
