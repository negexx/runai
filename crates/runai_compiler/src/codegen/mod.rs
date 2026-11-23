//! Code generation: from a lowered route to the module a build writes.
//!
//! Two emitters exist: [`prerender`] for a `"use static"` route, and [`client`]
//! for a `"use island"` route. The prerender module is self-contained with no
//! imports; the island module imports the runtime from a URL and mounts into
//! `#app`, addressing its bindings by marker ordinal rather than by an index path
//! (see [`client`]'s module docs for why).
//!
//! # The checked wrappers are the public API, and that is a deliberate ruling
//!
//! [`crate::ir::lower_route`] is **not** a gate: it assumes every rule already
//! passed, and its spans index the type-stripped source rather than the agent's
//! file. So a caller must run [`crate::analyze::entry::analyze`] first, and the
//! only public way to reach an artifact is the `*_checked` function — it calls
//! `analyze`, refuses on any error, and only then lowers and emits. The unchecked
//! emitters are `pub(crate)`, which makes "emit without checking" unreachable at
//! compile time from outside this crate. The naming is the invariant: a `pub`
//! emitter name that does not end in `_checked` would be a path from a source
//! file to an artifact with no rule pass in between.

pub(crate) mod client;
pub(crate) mod prerender;

use oxc_allocator::Allocator;

use crate::analyze::entry::{analyze, is_page};

use crate::codegen::client::emit_client;
use crate::codegen::prerender::emit_prerender;
use crate::diagnostic::{Code, Diagnostic, Severity};
use crate::ir::lower_route;
use crate::source::{Mode, parse_tsx};

pub use client::template_html;
pub use prerender::template_literal_body;

/// The prerender module for one route file, or the diagnostics that stop it.
///
/// This is the public entry point for a `"use static"` page. It refuses a path
/// that is not a page, runs [`analyze`] next — the same list `runai check`
/// reports, so the build and the check cannot disagree — and returns it unchanged
/// when any diagnostic is an error. A file whose parse failed gets `RA007` and
/// nothing else, because `analyze` short-circuits there; a file with a signal, a
/// handler, a refused JSX construct, or an unreachable name gets that diagnostic
/// rather than an artifact.
///
/// # Why the path is checked first
///
/// [`analyze`] applies the page-only rules — `RA010`, the reserved-name rule, and
/// the reachability rule — only when [`crate::analyze::entry::is_page`] says the
/// path names a route file. That is correct for `runai check`: a `_shell.tsx` is
/// a component module and owes no mode. But this function **emits a route**, so a
/// non-page path would fold every page-only rule away and write an artifact from a
/// file the analysis deliberately treated as a component module — a `const render
/// = 1` beside `export function render()`, or a static `signal(0)`, both of which
/// the checks would have refused on a `.page.tsx` path. Refusing the path here
/// keeps the page boundary as the one definition of "route file" while leaving
/// `analyze`'s own contract untouched.
///
/// # Why the mode is checked here too
///
/// A `"use island"` page is `emit_client_checked`'s, and a prerender module for it
/// would carry a signal into a file with no runtime: `render()` returns one
/// string, so there is no mount point for it to write into. `analyze` would also
/// accept the island file (nothing about it is an error in island mode), so the
/// mode is the one refusal `analyze` cannot make. `RA014` is the right code: the
/// file parses, and this is the compiler declining a construct — here, the wrong
/// emitter for the declared mode — not a syntax error.
///
/// # Errors
///
/// The full diagnostic list from `analyze`, plus a path refusal when the file is
/// not a page and a `Mode` refusal when the route is an island. Both are located
/// like every other diagnostic. `lower_route`'s own diagnostics can only appear
/// for a file the analysis accepted, so they are an internal invariant rather
/// than the agent's first signal.
pub fn emit_prerender_checked(source: &str, path: &str) -> Result<String, Vec<Diagnostic>> {
    if !crate::analyze::entry::is_page(path) {
        return Err(vec![located(
            Diagnostic::new(
                Code::RA014,
                (0, 0),
                format!(
                    "`{path}` is not a page file: the prerender emitter writes route files only, \
                     and the rules a route owes (a mode, no signal or handler, no reserved name) \
                     apply to a page. Give the file a `.page.tsx` name that does not start with \
                     `_`, or check it as a component module with `runai check`."
                ),
            ),
            source,
            path,
        )]);
    }

    let diagnostics = analyze(source, path);
    if diagnostics.iter().any(|d| d.severity == Severity::Error) {
        return Err(diagnostics);
    }
    let route = lower_route(source)?;
    if route.mode != Mode::Static {
        return Err(vec![located(
            Diagnostic::new(
                Code::RA014,
                (0, 0),
                format!(
                    "`{path}` is a \"use island\" route: the prerender emitter writes static routes only. \
                     Change line 1 to \"use static\", or emit it with `emit_client_checked`."
                ),
            ),
            source,
            path,
        )]);
    }
    Ok(emit_prerender(&route))
}

/// The import list the island module for `source` needs: every `"runai"` name the
/// file declares, merged with the helpers the artifact calls, sorted and deduped.
///
/// This is the one place the list is computed, and it is deliberately *not* inside
/// [`emit_client`]: the emitter sees only the [`RouteIr`](crate::ir::RouteIr), and
/// the IR cannot see an import that **only a module statement** uses. A route like
///
/// ```tsx
/// import { signal, effect } from "runai"
/// const start = signal(5); effect(() => { console.log(start()) })
/// ```
///
/// names `effect` in a module statement and nowhere in the markup, so a
/// markup-driven list would drop it and the module would throw `ReferenceError` at
/// load — Task 9's cross-task risk coming due. So the list is merged from
/// [`runai_imports`] on the **stripped** program — every named specifier the file
/// declares, whether or not the markup uses it — plus the helpers the artifact
/// needs (`mount`, `template`, and the used subset of `_setText`/`_setAttr`/`_on`).
///
/// # Errors
///
/// The full diagnostic list from [`analyze`], located, when the file has errors — the
/// same list `runai check` reports, so a caller cannot compute imports for a file it
/// would not emit.
pub fn client_imports(source: &str) -> Result<Vec<String>, Vec<Diagnostic>> {
    let stripped = crate::strip::strip_types(source)?;
    // `analyze` is the gate; a file it refuses gets its diagnostics, not an import
    // list. It runs on the original source so its spans are the agent's own.
    // `client_imports` has no path argument (the brief's signature), so the page
    // gate is the checked wrapper's; here the rules are the file-level ones.
    let diagnostics = analyze_source(source);
    if diagnostics.iter().any(|d| d.severity == Severity::Error) {
        return Err(diagnostics);
    }

    let alloc = Allocator::default();
    let program = parse_tsx(&alloc, &stripped)?;
    let mut names = import_specifiers(program);
    if let Ok(route) = lower_route(source) {
        extend_helpers(&mut names, &route.tree);
    }
    Ok(names)
}

/// Every `"runai"` import specifier the module writes, as **specifier text**
/// (`owner as who`, or `signal`), plus the always-present `mount` and `template`.
///
/// Sorted and deduped. This is the text of the one `import { … }` statement, not
/// the set of names in scope: an alias's *local* name is what a declaration
/// collides with, and [`island_reserved_names`] uses that instead. Both read the
/// same specifier list, so the import statement and the reserved set cannot
/// disagree about which aliases exist.
pub(crate) fn import_specifiers(program: &oxc_ast::ast::Program<'_>) -> Vec<String> {
    let mut names: Vec<String> = crate::analyze::signals::runai_specifiers(program)
        .into_iter()
        .filter(|(_, named)| *named)
        .map(|(specifier, _)| {
            let (imported, local) = crate::analyze::signals::named_specifier_names(specifier);
            if imported == local {
                imported
            } else {
                format!("{imported} as {local}")
            }
        })
        .collect();
    names.push("mount".to_string());
    names.push("template".to_string());
    names.sort();
    names.dedup();
    names
}

/// The names an island module binds that a route file's own declaration would
/// collide with, for [`crate::analyze::emitted_names`].
///
/// [`crate::ir::ISLAND_LOCALS`] (the emitter's own `_c`, `_a`, `_el`, `_t0`) plus
/// every **local** name the module imports — `who` for `import { owner as who }`,
/// because that is the binding in scope, not `owner` — plus the tree-dependent
/// helpers the artifact calls. Sorted and deduped.
///
/// It does **not** run [`analyze`]; [`crate::analyze::entry`] calls it from inside
/// the analysis pass, so a nested gate would recurse forever. The parse is of the
/// typed `program` the caller already holds, and the helper extension lowers the
/// source once — the price of asking "which helpers does the artifact actually
/// use" rather than reserving a superset.
pub(crate) fn island_reserved_names(
    source: &str,
    program: &oxc_ast::ast::Program<'_>,
) -> Vec<String> {
    let mut names: Vec<String> = crate::ir::ISLAND_LOCALS
        .iter()
        .map(|name| (*name).to_string())
        .collect();
    for (specifier, named) in crate::analyze::signals::runai_specifiers(program) {
        if named {
            let (_, local) = crate::analyze::signals::named_specifier_names(specifier);
            names.push(local);
        }
    }
    names.push("mount".to_string());
    names.push("template".to_string());
    if let Ok(route) = lower_route(source) {
        for helper in client::used_helpers(&route.tree) {
            names.push(helper.to_string());
        }
    }
    names.sort();
    names.dedup();
    names
}

/// Append the tree-dependent helpers ([`client::used_helpers`]) to `names`, keeping
/// it sorted and deduped.
pub(crate) fn extend_helpers(names: &mut Vec<String>, tree: &crate::ir::IrNode) {
    for helper in client::used_helpers(tree) {
        names.push(helper.to_string());
    }
    names.sort();
    names.dedup();
}

/// The island module for one route file, or the diagnostics that stop it.
///
/// The public entry point for a `"use island"` page, mirroring
/// [`emit_prerender_checked`]: it refuses a path that is not a page, runs
/// [`analyze`] next (the same list `runai check` reports), refuses a mode that is
/// not [`Mode::Island`], and only then computes the import list and emits.
///
/// # Why the path is checked first
///
/// [`analyze`] applies the page-only rules — `RA010`, the reserved-name rule, and
/// the reachability rule — only when [`is_page`] says the path names a route file.
/// Folded over a non-page path, this emitter would write an artifact from a file the
/// analysis deliberately treated as a component module, with none of those gates.
/// Refusing the path keeps the page boundary as the one definition of "route file".
///
/// # Why the mode is checked here
///
/// A `"use static"` page is [`emit_prerender_checked`]'s. This module would import
/// the runtime and mount into `#app` — a JavaScript island for a route that must
/// ship zero bytes of it. `analyze` accepts a static file (nothing about it is an
/// error in static mode), so the mode is the one refusal `analyze` cannot make.
///
/// # Errors
///
/// The full diagnostic list from [`analyze`], plus a path refusal when the file is
/// not a page and a `Mode` refusal when the route is static. Both are located.
pub fn emit_client_checked(
    source: &str,
    path: &str,
    runtime_url: &str,
) -> Result<String, Vec<Diagnostic>> {
    if !is_page(path) {
        return Err(vec![located(
            Diagnostic::new(
                Code::RA014,
                (0, 0),
                format!(
                    "`{path}` is not a page file: the island emitter writes route files only, \
                     and the rules a route owes (a mode, the reserved names, the reachable \
                     values) apply to a page. Give the file a `.page.tsx` name that does not \
                     start with `_`, or check it as a component module with `runai check`."
                ),
            ),
            source,
            path,
        )]);
    }

    let diagnostics = analyze(source, path);
    if diagnostics.iter().any(|d| d.severity == Severity::Error) {
        return Err(diagnostics);
    }
    let route = lower_route(source)?;
    if route.mode != Mode::Island {
        return Err(vec![located(
            Diagnostic::new(
                Code::RA014,
                (0, 0),
                format!(
                    "`{path}` is a \"use static\" route: the island emitter writes island routes \
                     only, and this module would import the runtime and mount JavaScript for a \
                     route that must ship none. Change line 1 to \"use island\", or emit it with \
                     `emit_prerender_checked`."
                ),
            ),
            source,
            path,
        )]);
    }

    let imports = client_imports(source)?;
    Ok(emit_client(&route, runtime_url, &imports))
}

/// [`analyze`] without a path.
///
/// `client_imports` takes no path (the brief's signature), and the page-only rules
/// are gated on [`is_page`]. Called with no path, [`analyze`] would fold those away,
/// which is correct here: `client_imports` is not the gate — [`emit_client_checked`]
/// is, and it passes the real path to [`analyze`] itself. This helper exists only so
/// `client_imports` still refuses a file whose *file-level* rules (parse, JSX,
/// imports, signals) fail, rather than computing imports for a file that will not be
/// emitted.
fn analyze_source(source: &str) -> Vec<Diagnostic> {
    // A page-shaped path so the page rules also apply when the caller reached us
    // directly; a real path only ever *adds* refusals, never removes one, and this
    // function is not the gate.
    analyze(source, "route.page.tsx")
}

/// `diagnostic` located against `source` and `path`.
///
/// Every diagnostic a caller surfaces to an agent must carry a real
/// `file:line:col`; `analyze` locates what it returns, and the wrapper's own
/// refusals are located here so all of them share that property.
fn located(mut diagnostic: Diagnostic, source: &str, path: &str) -> Diagnostic {
    diagnostic.locate(source, path);
    diagnostic
}

#[cfg(test)]
mod tests {
    use super::*;

    const P: &str = "export default function P(props: Record<never, never>)";

    /// The whole point of the checked wrapper: a file the analysis refuses never
    /// becomes an artifact. A signal in a static route is `RA010`, and the
    /// returned list carries it *located*, exactly as `runai check` would print
    /// it — not a bare error string.
    #[test]
    fn a_refused_file_returns_the_diagnostics_instead_of_a_module() {
        let src = format!(
            "\"use static\"\nimport {{ signal }} from \"runai\"\n{P} {{ const n = signal(0); return <p>{{n()}}</p> }}\n"
        );
        let diagnostics = emit_prerender_checked(&src, "src/routes/index.page.tsx")
            .expect_err("a static route with a signal must not emit");
        assert!(
            diagnostics.iter().any(|d| d.code == Code::RA010),
            "{diagnostics:?}"
        );
        let first = &diagnostics[0];
        assert_eq!(first.file, "src/routes/index.page.tsx");
        assert!(first.line >= 1, "located: {first:?}");
    }

    /// A parse failure is `RA007` and nothing else, so the emitter never sees a
    /// tree the parser did not produce.
    #[test]
    fn a_syntax_error_is_ra007_and_never_emits() {
        let src = format!("\"use static\"\n{P} {{ return <div> }}\n");
        let diagnostics =
            emit_prerender_checked(&src, "src/routes/bad.page.tsx").expect_err("no artifact");
        assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
        assert_eq!(diagnostics[0].code, Code::RA007);
    }

    /// An island page is the client emitter's; a prerender module for it would
    /// carry a signal into a file with no runtime. The refusal is the mode, which
    /// is the one thing `analyze` cannot report for an island file.
    #[test]
    fn an_island_route_is_refused_and_the_repair_names_the_directive() {
        let src = format!("\"use island\"\n{P} {{ return <p>hi</p> }}\n");
        let diagnostics =
            emit_prerender_checked(&src, "src/routes/index.page.tsx").expect_err("island refused");
        assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
        assert_eq!(diagnostics[0].code, Code::RA014);
        assert!(
            diagnostics[0].message.contains("emit_client_checked"),
            "the repair names the emitter that owns it: {:?}",
            diagnostics[0].message
        );
        assert!(
            diagnostics[0].message.contains("\"use static\""),
            "the repair names the directive: {:?}",
            diagnostics[0].message
        );
    }

    /// The conforming content-model shapes from the spec emit — not just pass the
    /// analyzer. A `div` wrapper in a `<dl>`, a script-supporting child in a list,
    /// and a `<menu>` list are all legal HTML, so a static route holding them must
    /// produce an artifact with the markup written verbatim.
    #[test]
    fn the_conforming_list_shapes_emit() {
        for body in [
            "<dl><div><dt>a</dt><dd>b</dd></div></dl>",
            "<dl><div>x</div></dl>",
            "<ul><script src=\"/a.js\"></script><li>x</li></ul>",
            "<menu><li>x</li></menu>",
        ] {
            let src = format!("\"use static\"\n{P} {{ return {body} }}\n");
            let js = emit_prerender_checked(&src, "src/routes/about.page.tsx")
                .unwrap_or_else(|d| panic!("{body}: a conforming shape must emit: {d:?}"));
            assert!(
                js.contains(body),
                "{body}: the markup is written verbatim: {js}"
            );
        }
    }

    /// A clean static route emits, and the artifact is what the emitter's own
    /// tests pin.
    #[test]
    fn a_clean_static_route_emits_a_module_with_no_imports() {
        let src = format!("\"use static\"\n{P} {{ return <p>hi</p> }}\n");
        let js = emit_prerender_checked(&src, "src/routes/about.page.tsx")
            .expect("a clean static route emits");
        assert!(!js.contains("import"), "{js}");
        assert!(js.contains("export function render()"), "{js}");
        assert!(js.contains("return `<p>hi</p>`"), "{js}");
    }

    /// The island-mode refusal is a diagnostic like any other, so it carries
    /// `file`/`line`/`col`. Without `locate` it reported `file: ""`,
    /// `line: 0`, `col: 0` — a position no editor or agent can follow.
    #[test]
    fn the_island_refusal_is_located() {
        let src = format!("\"use island\"\n{P} {{ return <p>hi</p> }}\n");
        let diagnostics =
            emit_prerender_checked(&src, "src/routes/index.page.tsx").expect_err("island refused");
        assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
        let row = &diagnostics[0];
        assert_eq!(row.file, "src/routes/index.page.tsx", "{row:?}");
        assert_eq!(row.line, 1, "the directive is line 1: {row:?}");
        assert!(row.col >= 1, "unlocated: {row:?}");
    }

    /// The wrapper emits a **route**, and every page-only rule in `analyze` —
    /// `RA010`, the reserved-name rule, the reachability rule — is gated on
    /// [`crate::analyze::entry::is_page`]. Folded over a non-page path it skipped
    /// every gate: this returned `Ok` with both `const render = 1` and
    /// `export function render()` in one module, which is a duplicate declaration
    /// and a `SyntaxError` on load.
    #[test]
    fn a_non_page_path_is_refused_by_the_emitter() {
        let src = format!("\"use static\"\nconst render = 1\n{P} {{ return <p>hi</p> }}\n");
        let diagnostics = emit_prerender_checked(&src, "about.tsx")
            .expect_err("a non-page path is not a route file");
        assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
        assert_eq!(diagnostics[0].code, Code::RA014);
        assert!(
            diagnostics[0].message.contains("about.tsx"),
            "the message names the path: {:?}",
            diagnostics[0].message
        );
        assert_eq!(
            diagnostics[0].line, 1,
            "the refusal is located: {diagnostics:?}"
        );
    }

    /// The control for the test above: the *same source* through a `.page.tsx`
    /// path must still reach the real gate and report the reserved name. If the
    /// page check merely rejected everything, this would pass with the refusals
    /// masked.
    #[test]
    fn a_page_path_still_reaches_the_reserved_name_gate() {
        let src = format!("\"use static\"\nconst render = 1\n{P} {{ return <p>hi</p> }}\n");
        let diagnostics = emit_prerender_checked(&src, "src/routes/about.page.tsx")
            .expect_err("a reserved name is refused");
        assert!(
            diagnostics.iter().any(|row| row.code == Code::RA014
                && row.message.contains("render")
                && row.message.contains("declares for itself")),
            "the page path must reach the reserved-name rule, not the path rule: {diagnostics:?}"
        );
    }

    /// A `signal` in a static route, through a non-page path, must not slip past
    /// `RA010` either — the same gate the reserved-name test above covers, on the
    /// other page-only rule. Asserting only `is_err()` would pass on the path
    /// refusal alone, so this asserts the *path* refusal is what answered: the
    /// signal never reached `analyze`, because the wrapper stopped first.
    #[test]
    fn a_signal_does_not_emit_through_a_non_page_path() {
        let src = format!(
            "\"use static\"\nimport {{ signal }} from \"runai\"\n{P} {{ const n = signal(0); return <p>{{n()}}</p> }}\n"
        );
        let diagnostics =
            emit_prerender_checked(&src, "about.tsx").expect_err("a non-page path must not emit");
        assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
        let row = &diagnostics[0];
        assert_eq!(row.code, Code::RA014);
        assert!(
            row.message.contains("not a page file") && row.message.contains("about.tsx"),
            "the path gate answers, naming the path: {:?}",
            row.message
        );
        assert!(
            row.message.contains(".page.tsx"),
            "the repair names the page suffix: {:?}",
            row.message
        );
    }

    /// The reparse **shape** rules are mode-independent: a static route's template
    /// is re-parsed as HTML too, so the parser reshapes it exactly as it reshapes
    /// an island's. Only the *marker*-hostile rule is island-gated; the tree-shape
    /// rules must fire in a `"use static"` route as well. Nothing asserted that
    /// before, so a future "island-only" gate on the whole `RA014` block would
    /// silently stop refusing a static page whose DOM the browser rewrites.
    ///
    /// RED for a wrongly gated rule: `emit_prerender_checked` returns `Ok`.
    #[test]
    fn a_static_route_refuses_a_shape_the_parser_reshapes() {
        // `<div><td>` — parse5 drops the cell and keeps its content in the div
        // (`<div><td>x</td></div>` → `(div "x")`), so the page is not the markup.
        let src = format!("\"use static\"\n{P} {{ return <div><td>x</td></div> }}\n");
        let diagnostics = emit_prerender_checked(&src, "src/routes/about.page.tsx")
            .expect_err("a shaped static route must not emit");
        assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
        assert_eq!(diagnostics[0].code, Code::RA014);
        assert!(
            diagnostics[0].message.contains("`<td>`") && diagnostics[0].message.contains("`<tr>`"),
            "the shape rule names the tag and its required parent: {}",
            diagnostics[0].message
        );
    }

    /// The control: a **legal** static route that also contains a binding still
    /// emits. It proves the refusal above is the shape rule and not "static routes
    /// with markup are refused", and that a legal static template interpolates its
    /// binding as a string (the mode independence cuts both ways).
    #[test]
    fn a_legal_static_route_with_a_binding_still_emits() {
        let src = format!(
            "\"use static\"\n{P} {{ const c = \"on\"; return <div class={{c}}><span>x</span></div> }}\n"
        );
        let js = emit_prerender_checked(&src, "src/routes/about.page.tsx")
            .expect("a legal static route emits");
        assert!(js.contains("export function render()"), "{js}");
        assert!(
            js.contains("_attr(\"class\", c)"),
            "the static binding is interpolated as a string, not a marker: {js}"
        );
        assert!(
            js.contains("<span>x</span>"),
            "the legal nesting is written verbatim: {js}"
        );
        assert!(
            !js.contains("<!>"),
            "a static template has no markers: {js}"
        );
    }
}
