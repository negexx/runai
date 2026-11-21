//! `analyze`: every rule, in one pass, for one file.
//!
//! This is the single entry point the CLI (`runai check`) and the checked
//! emitters use, so "what does the compiler report about this file?" has exactly
//! one answer. It runs:
//!
//! 1. [`parse_tsx`] — a rejection short-circuits and returns only `RA007`;
//! 2. for a page file: [`detect_mode`] and [`validate_route_component`];
//! 3. for every `.tsx` file: `check_props` on every component, [`check_jsx`],
//!    the import rule, and [`check_signal_rules`];
//! 4. for a `"use static"` page: [`check_static_mode`].
//!
//! A file whose name starts with `_` is **never** a page. `_shell.tsx` is a
//! component module: it owes the props, JSX, import, and signal checks, but no
//! mode directive and no route validation, because there is no route to validate.
//!
//! # Every diagnostic comes back located
//!
//! [`Diagnostic::locate`](crate::diagnostic::Diagnostic::locate) is called here,
//! on every diagnostic this function returns, and nowhere else — the rules are
//! given source text, not a path. Until it is called, `file`, `line`, and `col`
//! are empty, so a caller that skipped it would report every error at `:0:0`.
//!
//! # One line per problem
//!
//! The list is sorted by span, then by code, and identical `(code, span)` pairs
//! are dropped: several rules can land on one construct, and an agent reading
//! `runai check` should see each problem once.

use oxc_allocator::Allocator;
use oxc_ast::ast::{ImportOrExportKind, Program, Statement};

use crate::analyze::component::{find_components, find_route_component, validate_route_component};
use crate::analyze::jsx::check_jsx;
use crate::analyze::props::check_props;
use crate::analyze::rules::{check_signal_rules, check_static_mode};
use crate::analyze::signals::{PRIMITIVE_SOURCE, find_signal_bindings};
use crate::diagnostic::{Code, Diagnostic};
use crate::source::{Mode, detect_mode, parse_tsx, raw_text};

/// Every diagnostic for one file, located and in source order.
///
/// The single entry point: `runai check` and the checked emitters both call it,
/// so a file's verdict cannot differ by caller. See the module docs for the
/// pipeline, and for why the returned list is located and deduplicated.
pub fn analyze(source: &str, path: &str) -> Vec<Diagnostic> {
    // The AST borrows the allocator, so both live for the whole call and are
    // dropped together; only owned `Diagnostic`s escape.
    let alloc = Allocator::default();

    let mut diagnostics = match parse_tsx(&alloc, source) {
        Ok(program) => check(program, source, path),
        // Nothing downstream can run on a tree the parser did not produce, so a
        // rejection is the whole answer.
        Err(parse_failures) => parse_failures,
    };

    diagnostics.sort_by(|left, right| {
        (left.span.0, left.code.as_str()).cmp(&(right.span.0, right.code.as_str()))
    });
    // Two rules can name the same construct — `check_jsx` and a later pass are
    // separate walks. One problem is one row. A seen-set rather than
    // `dedup_by`: identical keys are not guaranteed adjacent under this sort.
    let mut seen: Vec<(Code, (u32, u32))> = Vec::with_capacity(diagnostics.len());
    diagnostics.retain(|diagnostic| {
        let key = (diagnostic.code, diagnostic.span);
        if seen.contains(&key) {
            return false;
        }
        seen.push(key);
        true
    });
    for diagnostic in &mut diagnostics {
        diagnostic.locate(source, path);
    }
    diagnostics
}

/// The rules for a file the parser accepted, in pipeline order.
fn check(program: &Program<'_>, source: &str, path: &str) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();

    // (2) A page owes a mode and a route. A component module — a `_`-prefixed
    // file, or any `.tsx` that is not a page — owes neither: there is no route to
    // render, so demanding a directive would be advice with no effect.
    let mut mode = None;
    if is_page(path) {
        match detect_mode(program, source) {
            Ok(detected) => mode = Some(detected),
            Err(missing) => diagnostics.push(missing),
        }
        // Independent of the mode: a page with no directive still needs a route
        // component, and reporting both saves a round.
        diagnostics.extend(validate_route_component(
            find_route_component(program).as_ref(),
        ));
    }

    // (3) Every `.tsx` file, page or not. Props only via `find_components` — it
    // already includes the default export, so validating the route separately
    // would report its `RA006` twice.
    for component in find_components(program) {
        diagnostics.extend(check_props(&component, source));
    }
    // `check_jsx` must run here, not only in lowering: `runai check` is the gate
    // an agent sees first, and a refusal it cannot reach is a false negative.
    diagnostics.extend(check_jsx(program, source));
    diagnostics.extend(check_imports(program, source));

    // One semantic build for both the signal rules and the static rules. Building
    // it twice per file would pay twice for the expensive half of analysis.
    let bindings = find_signal_bindings(program);
    diagnostics.extend(check_signal_rules(program, &bindings));

    // (4) Only a static page: an island ships the runtime the constructs need.
    if mode == Some(Mode::Static) {
        diagnostics.extend(check_static_mode(Mode::Static, program, &bindings));
    }

    diagnostics
}

/// Whether `path` names a page file.
///
/// A page is a `.page.tsx` file whose name does not start with `_`. The
/// underscore marks a component module (`_shell.tsx`) — a real file with real
/// components, but never a route, so it is checked for props and JSX and never
/// asked for a directive.
fn is_page(path: &str) -> bool {
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    !name.starts_with('_') && name.ends_with(".page.tsx")
}

/// `RA014` for every import of a module other than `"runai"`.
///
/// This milestone has no bundler: it emits one module per route from the route
/// file's own text, so a second module cannot be resolved or inlined. Reporting
/// the import is the only honest answer — emitting the file would produce a
/// module that cannot load.
///
/// `import type` is exempt, because types are stripped before anything is
/// emitted: there is no runtime import left to resolve. Every other form
/// (`import x from`, `import "./x"`, `import { x } from`, `import * as x from`)
/// is a value import and is reported.
fn check_imports(program: &Program<'_>, source: &str) -> Vec<Diagnostic> {
    program
        .body
        .iter()
        .filter_map(|statement| {
            let Statement::ImportDeclaration(declaration) = statement else {
                return None;
            };
            if declaration.import_kind == ImportOrExportKind::Type {
                return None;
            }
            let module = raw_text(source, declaration.source.span)
                .unwrap_or_else(|| declaration.source.value.as_str().unwrap_or(""));
            if declaration.source.value.as_str() == Some(PRIMITIVE_SOURCE) {
                return None;
            }
            Some(Diagnostic::new(
                Code::RA014,
                (declaration.source.span.start, declaration.source.span.end),
                format!(
                    "cannot import {module} yet: this milestone has no bundler to resolve a \
                     module other than `\"{PRIMITIVE_SOURCE}\"`, and a module-level import is not \
                     emitted. Inline the value here, or import it from `\"{PRIMITIVE_SOURCE}\"`."
                ),
            ))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::diagnostic::Code;

    const P: &str = "export default function P(props: Record<never, never>)";

    #[test]
    fn static_route_with_handler_is_ra010() {
        let src =
            format!("\"use static\"\n{P} {{ return <button onClick={{() => {{}}}}>x</button> }}\n");
        let d = analyze(&src, "src/routes/index.page.tsx");
        assert_eq!(d.len(), 1, "{d:?}");
        assert_eq!(d[0].code, Code::RA010);
        assert!(d[0].fix.contains("Change line 1 to \"use island\""));
        assert_eq!(d[0].line, 2);
    }

    #[test]
    fn static_route_with_signal_is_ra010() {
        let src = format!(
            "\"use static\"\nimport {{ signal }} from \"runai\"\n{P} {{ const n = signal(0); return <p>{{n()}}</p> }}\n"
        );
        let d = analyze(&src, "src/routes/index.page.tsx");
        assert!(d.iter().any(|x| x.code == Code::RA010), "{d:?}");
    }

    #[test]
    fn static_route_pure_html_is_clean() {
        let src = format!("\"use static\"\n{P} {{ return <div class=\"a\">hi</div> }}\n");
        assert!(analyze(&src, "src/routes/about.page.tsx").is_empty());
    }

    #[test]
    fn static_route_with_a_plain_expression_is_clean() {
        let src =
            format!("\"use static\"\n{P} {{ const title = \"Hi\"; return <h1>{{title}}</h1> }}\n");
        assert!(analyze(&src, "src/routes/about.page.tsx").is_empty());
    }

    #[test]
    fn shell_file_needs_no_directive() {
        let src = "export default function Shell(props: { children: JSX.Element }) { return <main>{props.children}</main> }\n";
        assert!(analyze(src, "src/routes/_shell.tsx").is_empty());
    }

    #[test]
    fn foreign_import_is_ra014() {
        let src = format!("\"use static\"\nimport x from \"./data\"\n{P} {{ return <p>hi</p> }}\n");
        let d = analyze(&src, "src/routes/about.page.tsx");
        assert_eq!(d[0].code, Code::RA014, "{d:?}");
    }

    #[test]
    fn syntax_error_short_circuits() {
        let src = format!("\"use static\"\n{P} {{ return <div> }}\n");
        let d = analyze(&src, "src/routes/bad.page.tsx");
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].code, Code::RA007);
    }

    // --- The rest is what the brief names but does not pin ---------------------

    /// `_shell.tsx` is not a page, so it owes no directive — but it is still a
    /// component module, so its components owe a written props type. Both halves
    /// matter: reporting `RA012` here would demand a directive from a file with no
    /// route, and reporting nothing would let an untyped shell through.
    #[test]
    fn a_shell_file_is_checked_for_props_but_not_for_a_mode() {
        let src = "export default function Shell(props) { return <main>x</main> }\n";
        let d = analyze(src, "src/routes/_shell.tsx");
        assert_eq!(d.len(), 1, "{d:?}");
        assert_eq!(d[0].code, Code::RA006);
        assert!(
            !d.iter().any(|x| x.code == Code::RA012),
            "a file with no route owes no directive: {d:?}"
        );
    }

    /// The import rule, both halves: a foreign module is `RA014`, and a type-only
    /// import is exempt because types are stripped before anything is emitted.
    #[test]
    fn a_type_only_import_is_not_ra014() {
        let src = format!(
            "\"use static\"\nimport type {{ Data }} from \"./data\"\n{P} {{ return <p>hi</p> }}\n"
        );
        let d = analyze(&src, "src/routes/about.page.tsx");
        assert!(d.is_empty(), "{d:?}");
    }

    /// `import type` is exempt, but a *value* import is not, whichever its shape.
    #[test]
    fn every_value_import_from_a_foreign_module_is_ra014() {
        for declaration in [
            "import x from \"./data\"",
            "import \"./data\"",
            "import { x } from \"./data\"",
            "import * as x from \"./data\"",
        ] {
            let src = format!("\"use static\"\n{declaration}\n{P} {{ return <p>hi</p> }}\n");
            let d = analyze(&src, "src/routes/about.page.tsx");
            assert_eq!(d.len(), 1, "{declaration}: {d:?}");
            assert_eq!(d[0].code, Code::RA014, "{declaration}");
            assert!(
                d[0].message.contains("\"./data\""),
                "{declaration}: the message must name the module: {d:?}"
            );
        }
    }

    /// A primitive is resolved by import binding, never by name, which is exactly
    /// the reason `RA010` reads `SignalBindings::primitive_calls` instead of
    /// looking for a `signal(` in the text: a local function with that name is an
    /// ordinary function, and a static route may call it.
    #[test]
    fn a_local_function_named_signal_is_not_ra010() {
        let src = format!(
            "\"use static\"\n{P} {{ const signal = (v: number) => () => v; const n = signal(0); return <p>hi</p> }}\n"
        );
        let d = analyze(&src, "src/routes/about.page.tsx");
        assert!(d.is_empty(), "{d:?}");
    }

    /// A write to an accessor is its own `RA010`, at the write's span. The
    /// primitive call that declared the accessor is a second one, at its own span:
    /// both are real, and they are not the same site.
    #[test]
    fn an_accessor_write_is_ra010_at_its_own_span() {
        let src = format!(
            "\"use static\"\nimport {{ signal }} from \"runai\"\n{P} {{ const n = signal(0); n(1); return <p>x</p> }}\n"
        );
        let d = analyze(&src, "src/routes/about.page.tsx");
        let writes: Vec<_> = d
            .iter()
            .filter(|x| {
                x.code == Code::RA010 && &src[x.span.0 as usize..x.span.1 as usize] == "n(1)"
            })
            .collect();
        assert_eq!(writes.len(), 1, "{d:?}");
    }

    /// The rule is about a *static* route: the same handler is the point of an
    /// island, so it must not be flagged there.
    #[test]
    fn an_island_route_with_a_handler_is_clean() {
        let src =
            format!("\"use island\"\n{P} {{ return <button onClick={{() => {{}}}}>x</button> }}\n");
        assert!(analyze(&src, "src/routes/index.page.tsx").is_empty());
    }

    /// A `.tsx` file that is not a page and does not start with `_` is a component
    /// module too: checked for props and JSX, never asked for a directive.
    #[test]
    fn a_non_page_component_module_needs_no_directive() {
        let src = "export const Badge = (props) => <b>x</b>\n";
        let d = analyze(src, "src/routes/badge.tsx");
        assert_eq!(d.len(), 1, "{d:?}");
        assert_eq!(d[0].code, Code::RA006);
        assert!(!d.iter().any(|x| x.code == Code::RA012), "{d:?}");
    }

    /// The `locate` debt: every returned diagnostic names the file and a real
    /// one-based position, including the ones a parse failure short-circuits with.
    #[test]
    fn every_diagnostic_is_located() {
        for (src, path) in [
            // `RA006` and `RA010` on the same page, both on line 2.
            (
                "\"use static\"\nexport default function P(props) { return <button onClick={() => {}}>x</button> }\n",
                "src/routes/index.page.tsx",
            ),
            // A parse failure, which short-circuits.
            (
                "\"use static\"\nexport default function P(props: Record<never, never>) { return <div> }\n",
                "src/routes/bad.page.tsx",
            ),
            // A component module: props only, no directive check.
            (
                "export const Badge = (props) => <b>x</b>\n",
                "src/routes/badge.tsx",
            ),
        ] {
            let diagnostics = analyze(src, path);
            assert!(!diagnostics.is_empty(), "{path}");
            for diagnostic in &diagnostics {
                assert_eq!(diagnostic.file, path, "{diagnostic:?}");
                assert!(diagnostic.line >= 1, "unlocated: {diagnostic:?}");
                assert!(diagnostic.col >= 1, "unlocated: {diagnostic:?}");
            }
        }
    }

    /// `check` must not be a false-negative gate for `build`. Lowering may assume
    /// its input passed every rule, and `lower_route` assumes `check_jsx` refused
    /// what it cannot emit, so every `check_jsx` refusal must be reachable through
    /// `analyze`. Asserted as a superset over `(code, span)`, construct by
    /// construct: the three families are body JSX, module JSX, and the tag rule.
    #[test]
    fn analyze_surfaces_every_check_jsx_refusal() {
        let fixtures = [
            // The tag rule: a capitalized tag, a runtime primitive, a namespaced tag.
            "\"use static\"\nexport default function P(props: Record<never, never>) { return <div><Greeting /></div> }\n",
            "\"use static\"\nexport default function P(props: Record<never, never>) { return <div><For each={x} /></div> }\n",
            "\"use static\"\nexport default function P(props: Record<never, never>) { return <div><svg:rect /></div> }\n",
            // A spread attribute and a spread child.
            "\"use static\"\nexport default function P(props: Record<never, never>) { return <div {...rest} /> }\n",
            "\"use static\"\nexport default function P(props: Record<never, never>) { return <div>{...items}</div> }\n",
            // JSX inside a `{…}` expression.
            "\"use static\"\nexport default function P(props: Record<never, never>) { return <div>{ok ? <b>y</b> : null}</div> }\n",
            // A fragment root, and a second JSX return.
            "\"use static\"\nexport default function P(props: Record<never, never>) { return <><b/><i/></> }\n",
            "\"use static\"\nexport default function P(props: Record<never, never>) { if (a) return <b/>; return <i/> }\n",
            // Body JSX: a JSX-valued local and a nested renderer.
            "\"use static\"\nexport default function P(props: Record<never, never>) { const header = <h1>x</h1>; return <p>{header}</p> }\n",
            "\"use static\"\nexport default function P(props: Record<never, never>) { function Item() { return <li/> } return <ul/> }\n",
            // Module JSX in a route file.
            "\"use static\"\nconst header = <h1>x</h1>\nexport default function P(props: Record<never, never>) { return <p>{header}</p> }\n",
            // The whitespace ambiguity (`RA007`, not `RA014`), so that family is
            // covered too: a line break between two inline siblings.
            "\"use static\"\nexport default function P(props: Record<never, never>) { return <p><b>a</b>\n<i>b</i></p> }\n",
            // A component module: same walk, no route.
            "\"use static\"\nexport function Badge(props: Record<never, never>) { return <div><Greeting /></div> }\n",
        ];

        for src in fixtures {
            let alloc = oxc_allocator::Allocator::default();
            let program = parse_tsx(&alloc, src).expect("every fixture must parse");
            let expected: Vec<_> = check_jsx(program, src)
                .iter()
                .map(|diagnostic| (diagnostic.code, diagnostic.span))
                .collect();
            assert!(!expected.is_empty(), "the fixture must be refused: {src}");

            let actual = analyze(src, "src/routes/index.page.tsx");
            for (code, span) in expected {
                assert!(
                    actual
                        .iter()
                        .any(|diagnostic| diagnostic.code == code && diagnostic.span == span),
                    "{code:?} at {span:?} is refused by check_jsx but never reached analyze: {actual:?}"
                );
            }
        }
    }

    /// `check_static_mode` is `analyze`'s fourth stage on its own, so it can be
    /// read (and tested) without the rest of the pipeline.
    #[test]
    fn check_static_mode_flags_only_a_static_route() {
        let alloc = oxc_allocator::Allocator::default();
        let src = "\"use static\"\nexport default function P(props: Record<never, never>) { return <button onClick={() => {}}>x</button> }\n";
        let program = parse_tsx(&alloc, src).unwrap();
        let bindings = find_signal_bindings(program);
        assert_eq!(check_static_mode(Mode::Static, program, &bindings).len(), 1);
        assert!(check_static_mode(Mode::Island, program, &bindings).is_empty());
    }

    /// `on` + uppercase is the handler convention; a lowercase name is not one.
    /// `once` is the near miss that a naive `starts_with("on")` flags.
    #[test]
    fn only_on_plus_uppercase_is_a_handler() {
        for attribute in ["once=\"x\"", "onclick=\"x\"", "data-onClick=\"x\""] {
            let src = format!(
                "\"use static\"\nexport default function P(props: Record<never, never>) {{ return <div {attribute}>x</div> }}\n"
            );
            assert!(
                analyze(&src, "src/routes/index.page.tsx").is_empty(),
                "{attribute} is not a handler"
            );
        }

        for attribute in ["onClick={() => {}}", "onMount={() => {}}"] {
            let src = format!(
                "\"use static\"\nexport default function P(props: Record<never, never>) {{ return <div {attribute}>x</div> }}\n"
            );
            let d = analyze(&src, "src/routes/index.page.tsx");
            assert_eq!(d.len(), 1, "{attribute}: {d:?}");
            assert_eq!(d[0].code, Code::RA010, "{attribute}");
        }
    }

    /// The returned list reads top-to-bottom in the file, and no `(code, span)`
    /// appears twice. Two rules do land on one construct (a write in a static
    /// route is `RA010` from this rule and would be `RA009`/`RA002` from the other);
    /// an agent should see each problem once, in source order.
    #[test]
    fn diagnostics_are_sorted_and_deduplicated() {
        let src = "\"use static\"\nimport { signal } from \"runai\"\nexport default function P(props) { const n = signal(0); n(1); return <button onClick={() => {}}>x</button> }\n";
        let d = analyze(src, "src/routes/index.page.tsx");
        assert!(d.len() > 2, "the fixture must produce several: {d:?}");

        let keys: Vec<(Code, (u32, u32))> = d.iter().map(|x| (x.code, x.span)).collect();
        let mut unique: Vec<(String, (u32, u32))> = keys
            .iter()
            .map(|(code, span)| (code.as_str().to_string(), *span))
            .collect();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), keys.len(), "duplicate (code, span): {keys:?}");

        let starts: Vec<u32> = d.iter().map(|x| x.span.0).collect();
        let mut sorted = starts.clone();
        sorted.sort_unstable();
        assert_eq!(starts, sorted, "not in source order: {d:?}");
    }
}
