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
//! component module, not a route, and is skipped. A `[param]` segment is `RA014`
//! because this milestone has no router: silently emitting `dist/[slug]/index.html`
//! would serve a 404 and look like a successful build.
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

/// A route with every reason it cannot be built, before anything is written.
#[derive(Debug)]
enum Refusal {
    /// Diagnostics, each already paired with the source that locates it.
    Diagnostics(Vec<(String, Diagnostic)>),
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

    // Phase one: find, read, and analyze every route. Nothing is written yet, so a
    // refusal here cannot leave a partial tree.
    let prepared = match prepare() {
        Ok(prepared) => prepared,
        Err(Refusal::Diagnostics(diagnostics)) => {
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
        Err(message) => {
            eprintln!("error: {message}");
            1
        }
    }
}

/// The routes under [`ROUTES`], read, analyzed, and refused for `[param]` segments.
///
/// Every diagnostic across every route is collected before returning, so one build
/// prints the whole list rather than the first file's problems — a repair loop that
/// fixes one file at a time is expensive.
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
    let mut prepared = Vec::new();

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

        let name = route_name(&file)?;
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

        prepared.push(Prepared {
            display,
            name,
            source,
        });
    }

    if refused.is_empty() {
        Ok(prepared)
    } else {
        Err(Refusal::Diagnostics(refused))
    }
}

/// Every route file under `dir`, recursively.
///
/// A name beginning with `_` is a component module (`_shell.tsx`) or a support
/// directory, never a route, so it is skipped whole. Only a `*.page.tsx` file is a
/// route; anything else is not built.
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
            if !starts_with_underscore(&path) {
                walk(&path, files)?;
            }
        } else if file_type.is_file()
            && !starts_with_underscore(&path)
            && path.to_string_lossy().ends_with(".page.tsx")
        {
            files.push(path);
        }
    }

    Ok(())
}

/// Whether the final component of `path` begins with `_`.
fn starts_with_underscore(path: &Path) -> bool {
    path.file_name()
        .is_some_and(|name| name.to_string_lossy().starts_with('_'))
}

/// The route name for a file under [`ROUTES`].
///
/// `index.page.tsx` is `""` (the root), `blog/index.page.tsx` is `blog`, and
/// `blog/post.page.tsx` is `blog/post`. A segment that is `.` or `..`, or is empty,
/// is refused: it could only come from a hand-built path, but it is the one shape
/// that would walk out of the output tree, so it is checked here rather than trusted.
fn route_name(file: &Path) -> Result<String, Refusal> {
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
        return Err(Refusal::Message(format!(
            "`{}` has a relative route segment (`{segment}`): it would escape the output \
             directory, so it is not buildable",
            file.display()
        )));
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

/// Write every route into `<out>.tmp` and publish it as `<out>`.
///
/// The temporary tree is removed on any failure, so `dist/` never exists in
/// half-written form. `<out>` itself is only touched by [`publish`], which runs
/// after every route has been written and every render has succeeded.
fn assemble(out: &Path, prepared: &[Prepared]) -> Result<(), String> {
    let tmp = sibling(out, "tmp");
    if tmp.exists() {
        std::fs::remove_dir_all(&tmp)
            .map_err(|error| format!("cannot clear {}: {error}", tmp.display()))?;
    }
    if let Some(parent) = tmp.parent().filter(|parent| !parent.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
    }
    std::fs::create_dir_all(&tmp)
        .map_err(|error| format!("cannot create {}: {error}", tmp.display()))?;

    match assemble_into(&tmp, prepared) {
        Ok(()) => publish(&tmp, out),
        Err(message) => {
            let _ = std::fs::remove_dir_all(&tmp);
            Err(message)
        }
    }
}

/// The body of [`assemble`], with `tmp` already created.
fn assemble_into(tmp: &Path, prepared: &[Prepared]) -> Result<(), String> {
    // Modules live in a scratch directory inside `tmp` so one `remove_dir_all`
    // cleans everything, and so a crash cannot leave a `.mjs` in the published
    // tree. It is removed before publishing; nothing here is a route.
    let modules = tmp.join(".modules");
    let mut rendered: Vec<(usize, Emitted)> = Vec::new();

    for (index, route) in prepared.iter().enumerate() {
        let module = emit(route).map_err(|diagnostics| {
            let mut message = String::new();
            for diagnostic in diagnostics {
                message.push_str(&diagnostic.render_human(&route.source));
            }
            message.trim_end().to_string()
        })?;
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

/// Publish a complete `tmp` tree as `out`, keeping the previous `out` on failure.
///
/// On Linux `rename(tmp, out)` fails when `out` is a non-empty directory, so `out`
/// is moved aside first. If the second rename fails the old tree is moved back, so
/// the failure mode is "the previous build is still there", never "no dist at all".
fn publish(tmp: &Path, out: &Path) -> Result<(), String> {
    let backup = sibling(out, "old");

    if backup.exists() {
        std::fs::remove_dir_all(&backup)
            .map_err(|error| format!("cannot clear {}: {error}", backup.display()))?;
    }

    let had_previous = out.exists();
    if had_previous {
        std::fs::rename(out, &backup).map_err(|error| {
            let _ = std::fs::remove_dir_all(tmp);
            format!(
                "cannot move the previous `{}` aside ({error}); it was left untouched",
                out.display()
            )
        })?;
    }

    if let Err(error) = std::fs::rename(tmp, out) {
        if had_previous {
            // Best effort: the error below is the one worth reporting, but the old
            // tree must go back if it can.
            let _ = std::fs::rename(&backup, out);
        }
        let _ = std::fs::remove_dir_all(tmp);
        return Err(format!("cannot publish {}: {error}", out.display()));
    }

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
        assert!(matches!(error, Err(Refusal::Message(message)) if message.contains("escape")));
    }

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
