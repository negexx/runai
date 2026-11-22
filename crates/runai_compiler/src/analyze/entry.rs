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
//! 4. for a `"use static"` page: [`check_static_mode`];
//! 5. for a page whose mode is known: [`check_reachable_expressions`], so every
//!    plain expression the markup evaluates names something the generated module
//!    actually has — a `"runai"` import in a static route or the route's own
//!    parameter in a static route are the two refusals; a module-level value is
//!    emitted by `RouteIr::module_statements` and is therefore reachable.
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
use oxc_ast::ast::{ImportDeclarationSpecifier, ImportOrExportKind, Program, Statement};

use crate::analyze::component::{find_components, find_route_component, validate_route_component};
use crate::analyze::jsx::check_jsx;
use crate::analyze::props::check_props;
use crate::analyze::reachable::check_reachable_expressions;
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

    // One semantic build for the signal rules. A page with a known mode pays one
    // more later, in stage 5, and only when it has a route component to check.
    let bindings = find_signal_bindings(program);
    diagnostics.extend(check_signal_rules(program, &bindings));

    // (4) Only a static page: an island ships the runtime the constructs need.
    if mode == Some(Mode::Static) {
        diagnostics.extend(check_static_mode(Mode::Static, program, &bindings));
    }

    // (5) A page whose mode is known: every value its markup names must exist in
    // the module the build emits. Without a mode the build stops at `RA012`, and
    // which module would be emitted — and therefore what is reachable — is
    // undecided, so the rule would be guessing.
    //
    // This is the one second semantic build in the pipeline, and it is deliberate:
    // `SignalBindings` records accessor calls, not arbitrary identifier
    // references, and widening it would make the signal pass pay for a question it
    // never asks. The rule returns immediately for a file with no route component,
    // so a component module pays nothing here, and a page pays one extra build —
    // not one per rule.
    if let Some(mode) = mode {
        diagnostics.extend(check_reachable_expressions(mode, program, &bindings));
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

/// `RA014` for every module the emitted file cannot resolve or write.
///
/// This milestone has no bundler: it emits one module per route from the route
/// file's own text, so a second module can be neither resolved nor inlined.
/// Reporting the statement is the only honest answer — emitting the file would
/// produce a module that cannot load.
///
/// **Three shapes are refused**, and the repair differs because the legal rewrite
/// differs:
///
/// - **an import of a module other than `"runai"`** — nothing can resolve it
///   (the bundler family below);
/// - **a default or namespace import of `"runai"`** — `import runai from
///   "runai"` / `import * as runai from "runai"`. The island emitter writes
///   exactly one import merging the route's **named** specifiers, so there is no
///   form it can write for these. The repair is to rewrite the declaration as
///   named imports; "move it into the component body" would be a syntax error,
///   which is why this declaration is refused here rather than left to the
///   reference rule;
/// - **a re-export** — `export … from "./x"`, `export * from "./x"`, whose
///   source is not `"runai"`. The emitted module writes one import (the island's,
///   from the runtime URL) or none, so the re-export names a module nothing
///   resolves. A route file has nothing legitimate to re-export in this
///   milestone: it emits one component, and [`crate::ir::module_statements`]
///   excludes re-exports for the same reason.
///
/// **The two CommonJS spellings TypeScript allows are refused too**, because
/// [`crate::strip::strip_types`] rewrites them into JavaScript that a `.mjs`
/// cannot run: `import x = require("./x")` becomes `const x = require("./x")`,
/// and `export = x` becomes `module.exports = x`. Neither `require` nor `module`
/// exists in the emitted module. `import x = localName` is *not* refused: it
/// strips to a plain `var x = localName`, which is valid JavaScript.
///
/// **A type-only statement is exempt, in both spellings TypeScript allows.**
/// Types are stripped before anything is emitted, so there is no runtime import
/// or re-export left to resolve:
///
/// - `import type { Data } from "./data"` — the whole declaration is a type
///   import ([`ImportOrExportKind::Type`] on the declaration);
/// - `import { type Data } from "./data"` — the *module* is a value import, but
///   the only specifier in it is marked `Type` on the specifier, so nothing
///   survives stripping. `signals::import_symbols` reads the same field for the
///   same reason;
/// - `export type { Data } from "./data"` and `export { type Data } from …` are
///   the re-export forms of the same two spellings.
///
/// Every other form (`import x from`, `import "./x"`, `import {} from`,
/// `import { x } from`, `import * as x from`, and a mixed `import { type T, x }`)
/// keeps at least one runtime binding and is reported.
fn check_imports(program: &Program<'_>, source: &str) -> Vec<Diagnostic> {
    program
        .body
        .iter()
        .filter_map(|statement| match statement {
            Statement::ImportDeclaration(declaration) => check_import(declaration, source),
            Statement::ExportFromDeclaration(declaration) => {
                let kinds: Vec<_> = declaration
                    .specifiers
                    .iter()
                    .map(|specifier| specifier.export_kind)
                    .collect();
                if is_type_only_reexport(declaration.export_kind, &kinds) {
                    return None;
                }
                check_reexport(&declaration.source, source)
            }
            Statement::ExportAllDeclaration(declaration) => {
                if declaration.export_kind == ImportOrExportKind::Type {
                    return None;
                }
                check_reexport(&declaration.source, source)
            }
            Statement::TSImportEqualsDeclaration(declaration) => {
                check_import_equals(declaration, source)
            }
            Statement::TSExportAssignment(declaration) => Some(export_assignment((
                declaration.span.start,
                declaration.span.end,
            ))),
            _ => None,
        })
        .collect()
}

/// `RA014` for one `import`, or `None` when the emitted module can write it.
fn check_import(
    declaration: &oxc_ast::ast::ImportDeclaration<'_>,
    source: &str,
) -> Option<Diagnostic> {
    if is_type_only_import(declaration) {
        return None;
    }
    if declaration.source.value.as_str() == Some(PRIMITIVE_SOURCE) {
        // Named value specifiers are the one form the island emitter can write.
        // A default or namespace binding is a `"runai"` import this milestone
        // cannot emit, and the repair has to name the declaration to rewrite
        // rather than point at each use.
        return unwritable_runai_import(declaration);
    }

    let module = raw_text(source, declaration.source.span)
        .unwrap_or_else(|| declaration.source.value.as_str().unwrap_or(""));
    Some(Diagnostic::new(
        Code::RA014,
        (declaration.source.span.start, declaration.source.span.end),
        format!(
            "cannot import {module} yet: this milestone has no bundler to resolve a \
             module other than `\"{PRIMITIVE_SOURCE}\"`, and a module-level import is not \
             emitted. Inline the value here, or import it from `\"{PRIMITIVE_SOURCE}\"`."
        ),
    ))
}

/// `RA014` for a re-export of a module other than `"runai"`, or `None`.
///
/// The emitted module writes one import (the island's, from the runtime URL) or
/// none, so a re-export names a module nothing resolves — the same defect the
/// import rule exists for, one export out. The repair is to inline the value,
/// not to move the statement: nothing declares it in this file.
fn check_reexport(module: &oxc_ast::ast::StringLiteral<'_>, source: &str) -> Option<Diagnostic> {
    if module.value.as_str() == Some(PRIMITIVE_SOURCE) {
        // The brief's boundary: a `"runai"` re-export is not the bundler family.
        // It is still excluded from `module_statements` (an emitted module
        // writes none), so nothing is re-exported; a route file's exports have no
        // consumer in this milestone, so that is inert rather than a load error.
        return None;
    }
    let written =
        raw_text(source, module.span).unwrap_or_else(|| module.value.as_str().unwrap_or(""));
    Some(Diagnostic::new(
        Code::RA014,
        (module.span.start, module.span.end),
        format!(
            "cannot re-export from {written} yet: this milestone has no bundler, and a route \
             file is emitted from its own text with no re-export. Write the value in this file, \
             or import it from `\"{PRIMITIVE_SOURCE}\"` and use it here."
        ),
    ))
}

/// `RA014` for `import x = require("./x")`, or `None`.
///
/// Only the `require(…)` form is refused. `import x = localName` strips to
/// `var x = localName`, which is ordinary JavaScript; the external module
/// reference strips to a `require(…)` call, and `require` does not exist in the
/// emitted `.mjs`. A `type` import-equals binds nothing and is exempt.
fn check_import_equals(
    declaration: &oxc_ast::ast::TSImportEqualsDeclaration<'_>,
    source: &str,
) -> Option<Diagnostic> {
    if declaration.import_kind == ImportOrExportKind::Type {
        return None;
    }
    let oxc_ast::ast::TSModuleReference::ExternalModuleReference(reference) =
        &declaration.module_reference
    else {
        return None;
    };

    let module = raw_text(source, reference.expression.span)
        .unwrap_or_else(|| reference.expression.value.as_str().unwrap_or(""));
    let name = declaration.id.name.as_str();
    Some(Diagnostic::new(
        Code::RA014,
        (declaration.span.start, declaration.span.end),
        format!(
            "cannot use `import {name} = require({module})` yet: it becomes a `require(…)` call, \
             and the emitted module is an ES module where `require` does not exist. Write it as \
             an import instead — `import {{ owner }} from \"{PRIMITIVE_SOURCE}\"` for a \
             `\"{PRIMITIVE_SOURCE}\"` value — or inline the value here."
        ),
    ))
}

/// `RA014` for `export = x`.
///
/// [`crate::strip::strip_types`] rewrites it to `module.exports = x`, and a `.mjs`
/// has no `module`. The repair is the ES module export form.
fn export_assignment(span: (u32, u32)) -> Diagnostic {
    Diagnostic::new(
        Code::RA014,
        span,
        "cannot use `export =` yet: it becomes `module.exports = …`, and the emitted module is \
         an ES module where `module` does not exist. Write `export default` or a named `export` \
         instead.",
    )
}

/// `RA014` for a `"runai"` import whose binding form the client emitter cannot
/// write, or `None` for a named-only value import.
///
/// The emitted island module has exactly one import, built from the route's
/// named specifiers. `import runai from "runai"` and `import * as runai from
/// "runai"` have no named specifier to merge, and rewriting a member access
/// (`runai.owner`) into a named binding needs knowledge of the runtime's export
/// list this compiler does not carry. So the declaration is refused once, at the
/// specifier, with the rewrite that works.
fn unwritable_runai_import(
    declaration: &oxc_ast::ast::ImportDeclaration<'_>,
) -> Option<Diagnostic> {
    let specifiers = declaration.specifiers.as_ref()?;
    for specifier in specifiers {
        let (local, form) = match specifier {
            // The writable form: a value named specifier, aliases included.
            // A `type` specifier binds nothing at runtime either, so it is not
            // an unwritable *value* import.
            ImportDeclarationSpecifier::ImportSpecifier(_) => continue,
            ImportDeclarationSpecifier::ImportDefaultSpecifier(specifier) => {
                (&specifier.local, "a default import")
            }
            ImportDeclarationSpecifier::ImportNamespaceSpecifier(specifier) => {
                (&specifier.local, "a namespace import")
            }
        };
        let name = local.name.as_str();
        return Some(Diagnostic::new(
            Code::RA014,
            (local.span.start, local.span.end),
            format!(
                "`{name}` is {form} of `\"{PRIMITIVE_SOURCE}\"`, and the generated module writes \
                 only its named imports. Write the names you use as named imports instead, e.g. \
                 `import {{ owner }} from \"{PRIMITIVE_SOURCE}\"`, and refer to `owner` directly."
            ),
        ));
    }
    None
}

/// Whether a re-export binds nothing at runtime.
///
/// Two spellings, mirroring [`is_type_only_import`]: `export type { X } from
/// "./x"` marks the declaration, and `export { type X } from "./x"` marks each
/// specifier. Both are erased by [`crate::strip::strip_types`], so neither is a
/// module the emitted file would have to resolve — and reporting the inline form
/// would demand a rewrite that fixes nothing.
///
/// `specifier_kinds` is a closure rather than a slice so the same rule serves the
/// two node types without either owing the other a conversion.
fn is_type_only_reexport(
    declaration_kind: ImportOrExportKind,
    specifier_kinds: &[ImportOrExportKind],
) -> bool {
    if declaration_kind == ImportOrExportKind::Type {
        return true;
    }
    !specifier_kinds.is_empty()
        && specifier_kinds
            .iter()
            .all(|kind| *kind == ImportOrExportKind::Type)
}

/// Whether `declaration` binds nothing at runtime.
///
/// Two spellings, one fact. `import type { X } from "./x"` marks the declaration;
/// `import { type X } from "./x"` marks each specifier instead, so the declaration
/// looks like a value import to a check that reads only `import_kind`. The second
/// form is what an agent gets from an editor's auto-import, and reporting it would
/// demand a rewrite that fixes nothing.
///
/// An import with **no** specifiers (`import "./x.css"`) is not type-only: it is
/// the side-effect form, and the module would still have to load.
fn is_type_only_import(declaration: &oxc_ast::ast::ImportDeclaration<'_>) -> bool {
    if declaration.import_kind == ImportOrExportKind::Type {
        return true;
    }
    let Some(specifiers) = &declaration.specifiers else {
        return false;
    };
    !specifiers.is_empty()
        && specifiers.iter().all(|specifier| {
            matches!(
                specifier,
                ImportDeclarationSpecifier::ImportSpecifier(specifier)
                    if specifier.import_kind == ImportOrExportKind::Type
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::diagnostic::Code;
    use crate::ir::{IrNode, lower_route};

    const P: &str = "export default function P(props: Record<never, never>)";

    fn codes(d: &[Diagnostic]) -> Vec<Code> {
        d.iter().map(|diagnostic| diagnostic.code).collect()
    }

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

    /// The primitive is `RA010`; the read of the accessor it declared is **not**.
    /// An exact count and span, so a rule that also flagged reads (a deleted
    /// `.filter(|call| call.write)`) is visible rather than merely not-fatal.
    #[test]
    fn static_route_with_signal_is_ra010() {
        let src = format!(
            "\"use static\"\nimport {{ signal }} from \"runai\"\n{P} {{ const n = signal(0); return <p>{{n()}}</p> }}\n"
        );
        let d = analyze(&src, "src/routes/index.page.tsx");
        let ra010: Vec<_> = d.iter().filter(|x| x.code == Code::RA010).collect();
        assert_eq!(ra010.len(), 1, "the primitive only, never the read: {d:?}");
        assert_eq!(
            &src[ra010[0].span.0 as usize..ra010[0].span.1 as usize],
            "signal(0)",
            "the span is the primitive call: {d:?}"
        );
        assert_eq!(codes(&d), vec![Code::RA010], "nothing else either: {d:?}");
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

    /// A module-level constant is **legal** as of round 3: `RouteIr` carries
    /// `module_statements`, so the emitters write it above the component and the
    /// markup's reference resolves. This test replaces
    /// `a_module_level_constant_referenced_by_the_tree_is_ra014`, which pinned
    /// the round-2 refusal the plan ruling reversed.
    ///
    /// Both halves are asserted, because the old defect was the pair disagreeing:
    /// `analyze` is clean **and** lowering now carries the declaration the tree
    /// names.
    #[test]
    fn a_module_level_constant_is_emitted_so_the_tree_can_name_it() {
        let src = "\"use static\"\nconst title = \"About\"\nexport default function Page(props: Record<never, never>) { return <h1>{title}</h1> }\n";
        let d = analyze(src, "src/routes/about.page.tsx");
        assert!(d.is_empty(), "{d:?}");

        let route = lower_route(src).expect("a module-level constant lowers");
        assert!(
            route.module_statements.contains("const title = \"About\""),
            "the module level must carry the declaration the tree names: {:?}",
            route.module_statements
        );
        assert_eq!(
            route.statements, "",
            "the component body is still empty; the declaration is module scope"
        );
        assert!(
            matches!(&route.tree, IrNode::Element { children, .. }
                if matches!(children.as_slice(), [IrNode::Expr(expr)] if expr == "title")),
            "the tree still names `title`, which is now declared: {:?}",
            route.tree
        );
    }

    /// A module-level **signal** is per-island state (design §7.6: "module-scope
    /// signals are per-island too, since each island is its own module
    /// instance"), so an island is clean. A static route still has no runtime, so
    /// the same file is `RA010` — that is the static rule, not this task's.
    #[test]
    fn a_module_level_signal_is_island_state_and_still_ra010_when_static() {
        let src = |directive: &str| {
            format!(
                "{directive}\nimport {{ signal }} from \"runai\"\nconst n = signal(0)\nexport default function P(props: Record<never, never>) {{ return <h1>{{n()}}</h1> }}\n"
            )
        };

        assert!(
            analyze(&src("\"use island\""), "src/routes/index.page.tsx").is_empty(),
            "module-scope signals are per-island state"
        );
        assert_eq!(
            codes(&analyze(
                &src("\"use static\""),
                "src/routes/index.page.tsx"
            )),
            vec![Code::RA010],
            "a static route has no runtime, so the primitive is still refused"
        );
    }

    /// The `runtime_import` branch, end to end: a named `"runai"` import used in
    /// a static route's markup must get the **directive** repair, not "move it
    /// into the body". The same file as an island is clean.
    #[test]
    fn a_runai_value_in_a_static_route_gets_the_directive_repair() {
        let src = |directive: &str| {
            format!(
                "{directive}\nimport {{ owner }} from \"runai\"\nexport default function P(props: Record<never, never>) {{ return <p>{{owner}}</p> }}\n"
            )
        };

        let d = analyze(&src("\"use static\""), "src/routes/index.page.tsx");
        assert_eq!(codes(&d), vec![Code::RA014], "{d:?}");
        assert!(
            d[0].message.contains("Change line 1 to \"use island\""),
            "the repair is the directive: {d:?}"
        );
        assert!(
            !d[0]
                .message
                .contains("Move `owner` into the component body"),
            "moving an import into a body is not a repair: {d:?}"
        );

        assert!(
            analyze(&src("\"use island\""), "src/routes/index.page.tsx").is_empty(),
            "an island imports the name the client module writes"
        );
    }

    // --- Fix round 3: the module level, through `analyze` ---------------------

    /// The Finding-1 fixture, run through the entry point an agent actually uses.
    /// The runai value is at **module scope**, not in the markup, so the round-2
    /// rule (which walked only the returned tree) passed it. The emitted
    /// `module_statements` is `const o = owner;` in a module with no imports,
    /// which is a `ReferenceError` at load — the same accept-then-reject class
    /// the import rule exists for.
    ///
    /// The island half is the control: the client module writes the import, so
    /// the identical statement is fine and this must stay silent.
    #[test]
    fn a_runai_value_at_module_scope_is_ra014_through_analyze() {
        let src = |directive: &str| {
            format!(
                "{directive}\nimport {{ owner }} from \"runai\"\nconst o = owner\nexport default function P(props: Record<never, never>) {{ return <p>hi</p> }}\n"
            )
        };

        let static_src = src("\"use static\"");
        let d = analyze(&static_src, "src/routes/index.page.tsx");
        assert_eq!(codes(&d), vec![Code::RA014], "{d:?}");
        assert_eq!(d[0].line, 3, "the module statement is the site: {d:?}");
        assert!(
            d[0].message.contains("Change line 1 to \"use island\""),
            "the repair is the directive: {d:?}"
        );
        // `build` still succeeds — the gate is what refuses, not lowering — but
        // the statement is in the artifact it would have written, so the refusal
        // is about the emitted module rather than about lowering's contract.
        assert!(
            lower_route(&static_src)
                .expect("lowering is not the gate")
                .module_statements
                .contains("const o = owner"),
            "the statement the diagnostics names is the statement the module carries"
        );

        assert!(
            analyze(&src("\"use island\""), "src/routes/index.page.tsx").is_empty(),
            "an island writes the import the module statement reads"
        );
    }

    /// The route component's own name at module scope. `Page` is not in the
    /// prerender module at all — it is rebuilt inside `render()` — so
    /// `const x = Page` is a `ReferenceError`. An island declares
    /// `function Page(props)`, so the same file is clean there.
    #[test]
    fn the_route_component_at_module_scope_is_ra014_through_analyze() {
        let src = |directive: &str| {
            format!(
                "{directive}\nconst x = P\nexport default function P(props: Record<never, never>) {{ return <p>{{x}}</p> }}\n"
            )
        };

        let d = analyze(&src("\"use static\""), "src/routes/index.page.tsx");
        assert_eq!(codes(&d), vec![Code::RA014], "{d:?}");
        assert!(
            d[0].message.contains("route's own component"),
            "the message must name the component: {d:?}"
        );

        assert!(
            analyze(&src("\"use island\""), "src/routes/index.page.tsx").is_empty(),
            "an island declares the component"
        );
    }

    /// Finding 2, end to end: neither re-export form is refused *or* excluded
    /// before this round. Both are `RA014`, and neither appears in
    /// `module_statements` — the emitted `.mjs` re-exporting a module it cannot
    /// resolve is exactly what RA014's import family exists to prevent.
    #[test]
    fn a_reexport_of_a_foreign_module_is_ra014_and_never_emitted() {
        for statement in [
            "export * from \"./data\"",
            "export { team } from \"./data\"",
        ] {
            let src = format!(
                "\"use static\"\n{statement}\nexport default function P(props: Record<never, never>) {{ return <p>hi</p> }}\n"
            );
            let d = analyze(&src, "src/routes/index.page.tsx");
            assert_eq!(codes(&d), vec![Code::RA014], "{statement}: {d:?}");
            assert_eq!(d[0].line, 2, "{statement}: the statement is the site");
            assert!(
                d[0].message.contains("re-export"),
                "{statement}: the message must name the re-export: {d:?}"
            );

            let route = lower_route(&src).expect("the analyzer is the gate, not lowering");
            assert!(
                route.module_statements.is_empty(),
                "{statement}: a re-export is not emitted: {:?}",
                route.module_statements
            );
        }

        // The type-only spellings are stripped, so nothing is left to resolve.
        for statement in [
            "export type { Data } from \"./data\"",
            "export { type Data } from \"./data\"",
            "export type * from \"./data\"",
        ] {
            let src = format!(
                "\"use static\"\n{statement}\nexport default function P(props: Record<never, never>) {{ return <p>hi</p> }}\n"
            );
            assert!(
                analyze(&src, "src/routes/index.page.tsx").is_empty(),
                "{statement}: a type-only re-export is erased"
            );
        }

        // A re-export of a *local* binding is a different statement: it stays.
        let local = "\"use static\"\nconst helper = 1\nexport { helper }\nexport default function P(props: Record<never, never>) { return <p>{helper}</p> }\n";
        assert!(
            analyze(local, "src/routes/index.page.tsx").is_empty(),
            "`export {{ helper }}` names this file's binding"
        );
        assert!(
            lower_route(local)
                .unwrap()
                .module_statements
                .contains("export { helper }"),
            "a local re-export is emitted"
        );
    }

    /// The CommonJS spellings `strip_types` rewrites into JavaScript a `.mjs`
    /// cannot run: `import x = require("./x")` becomes `const x = require(…)`,
    /// and `export = x` becomes `module.exports = x`. Neither `require` nor
    /// `module` exists in the emitted module.
    ///
    /// `import x = localName` strips to `var x = localName`, which is valid
    /// JavaScript and stays clean — the control that this rule is about `require`
    /// rather than about `import =`.
    #[test]
    fn a_commonjs_import_or_export_is_ra014() {
        for (statement, line) in [
            ("import x = require(\"./data\")", 2),
            ("import x = require(\"runai\")", 2),
            ("export = foo", 3),
        ] {
            let src = if statement.starts_with("export") {
                format!(
                    "\"use static\"\nconst foo = 1\n{statement}\nexport default function P(props: Record<never, never>) {{ return <p>hi</p> }}\n"
                )
            } else {
                format!(
                    "\"use static\"\n{statement}\nexport default function P(props: Record<never, never>) {{ return <p>hi</p> }}\n"
                )
            };
            let d = analyze(&src, "src/routes/index.page.tsx");
            assert_eq!(codes(&d), vec![Code::RA014], "{statement}: {d:?}");
            assert_eq!(d[0].line, line, "{statement}: the statement is the site");
        }

        // The control: `import x = localName` is not a `require(…)` call.
        let local = "\"use static\"\nconst y = 1\nimport x = y\nexport default function P(props: Record<never, never>) { return <p>x{1}</p> }\n";
        assert!(
            analyze(local, "src/routes/index.page.tsx").is_empty(),
            "`import x = y` strips to `var x = y`, which is valid JavaScript"
        );

        // And a type-only import-equals binds nothing at all.
        let typed = "\"use static\"\nimport type x = require(\"./data\")\nexport default function P(props: Record<never, never>) { return <p>hi</p> }\n";
        assert!(
            analyze(typed, "src/routes/index.page.tsx").is_empty(),
            "a type-only import-equals is erased"
        );
    }

    /// The four nits from the review, as facts rather than comments: a
    /// `module_statements` that cannot name a `"runai"` value, cannot name the
    /// route, and does not carry a re-export. The existing
    /// `!contains("import")` assertion is joined by the two that would catch a
    /// re-export, because `export … from` carries neither the word `import` nor
    /// the route's name.
    #[test]
    fn module_statements_carry_no_reference_the_module_cannot_resolve() {
        for (src, emitted) in [
            (
                "\"use static\"\nconst title = \"A\"\nexport default function Page(props: Record<never, never>) { return <h1>{title}</h1> }\n",
                "const title = \"A\";",
            ),
            (
                "\"use static\"\nexport { helper } from \"./data\"\nexport default function Page(props: Record<never, never>) { return <p>hi</p> }\n",
                "",
            ),
            (
                "\"use static\"\nexport * from \"./data\"\nexport default function Page(props: Record<never, never>) { return <p>hi</p> }\n",
                "",
            ),
        ] {
            let route = lower_route(src).expect("lowered");
            assert_eq!(route.module_statements, emitted, "{src}");
            for forbidden in ["import", "export *", "export {"] {
                assert!(
                    !route.module_statements.contains(forbidden),
                    "`{forbidden}` must never be emitted: {:?}",
                    route.module_statements
                );
            }
        }
    }

    /// The `route_parameter` branch, end to end, in both modes: `render()` takes
    /// no arguments, so a static route's `{props}` is `undefined` at build time;
    /// an island declares `function <Name>(props)`.
    #[test]
    fn a_route_parameter_is_refused_when_static_and_clean_when_island() {
        let src = |directive: &str| {
            format!(
                "{directive}\nexport default function P(props: Record<never, never>) {{ return <p>{{props}}</p> }}\n"
            )
        };

        let d = analyze(&src("\"use static\""), "src/routes/index.page.tsx");
        assert_eq!(codes(&d), vec![Code::RA014], "{d:?}");
        assert!(
            d[0].message.contains("route's parameter"),
            "the message must name the parameter, not a module value: {d:?}"
        );

        assert!(
            analyze(&src("\"use island\""), "src/routes/index.page.tsx").is_empty(),
            "an island declares the parameter"
        );
    }

    /// N1, end to end. These three are **type positions**: `strip_types` erases
    /// them before anything is emitted, and the program compiles and runs today.
    /// Round 2 refused all three through `analyze` and the review proved it by
    /// execution — `check` reported `RA014` while `build` succeeded. The walk now
    /// skips type-only references, and each fixture must be clean in **both**
    /// modes.
    #[test]
    fn a_type_position_is_clean_end_to_end() {
        // The body of each fixture, without its directive: the type position is
        // mode-independent, so both modes are asserted for every one.
        for body in [
            // A type alias in a type assertion, inside the body.
            "type Kind = \"a\" | \"b\"\nexport default function P(props: Record<never, never>) { return <p>{\"a\" as Kind}</p> }\n",
            // A type alias annotating an arrow parameter inside the markup.
            "type Entry = { name: string }\nexport default function P(props: Record<never, never>) { return <p>{[{ name: \"x\" }].map((v: Entry) => v.name)}</p> }\n",
            // A named generic argument.
            "type Kind = \"a\"\nexport default function P(props: Record<never, never>) { return <p>{[1].map<Kind>(() => \"a\")}</p> }\n",
        ] {
            for directive in ["\"use static\"", "\"use island\""] {
                let src = format!("{directive}\n{body}");
                let d = analyze(&src, "src/routes/index.page.tsx");
                assert!(
                    d.is_empty(),
                    "a type position must never be refused: {d:?}\n{src}"
                );
                assert!(
                    lower_route(&src).is_ok(),
                    "the file compiles and runs today: {src}"
                );
            }
        }

        // `import { type X } from "runai"` used as a type is the same fact: the
        // specifier is marked type, so nothing survives stripping to import.
        let typed_runai = "\"use island\"\nimport { type Accessor } from \"runai\"\nexport default function P(props: Record<never, never>) { return <p>{1 as Accessor<number>}</p> }\n";
        assert!(
            analyze(typed_runai, "src/routes/index.page.tsx").is_empty(),
            "{:?}",
            analyze(typed_runai, "src/routes/index.page.tsx")
        );
    }

    /// A module-level JSX const is `check_jsx`'s construct, and its repair is the
    /// legal one (write the markup inline). This rule must not add a second row
    /// telling the agent to move it into the body, which the *body*-JSX refusal
    /// forbids — one root cause, one diagnostic.
    #[test]
    fn a_module_level_jsx_const_produces_exactly_one_ra014() {
        let src = "\"use static\"\nconst header = <h1>x</h1>\nexport default function P(props: Record<never, never>) { return <p>{header}</p> }\n";
        let d = analyze(src, "src/routes/index.page.tsx");
        assert_eq!(d.len(), 1, "one root cause, one row: {d:?}");
        assert_eq!(d[0].code, Code::RA014);
        assert!(
            d[0].message
                .contains("JSX outside the route's returned tree"),
            "check_jsx owns this construct: {d:?}"
        );
        assert!(
            !d[0]
                .message
                .contains("Move `header` into the component body"),
            "the contradictory repair must be gone: {d:?}"
        );
    }

    /// A default or namespace `"runai"` import is a form neither emitter writes,
    /// so the *import rule* refuses it — once, at the declaration — in both
    /// modes. The repair has to be one an agent can apply: rewriting the
    /// declaration as named imports, never moving an `import` into a function
    /// body, which is a syntax error.
    #[test]
    fn an_unwritable_runai_import_form_is_one_ra014_in_both_modes() {
        for declaration in [
            "import * as runai from \"runai\"",
            "import runai from \"runai\"",
        ] {
            for directive in ["\"use static\"", "\"use island\""] {
                let src = format!(
                    "{directive}\n{declaration}\nexport default function P(props: Record<never, never>) {{ return <p>{{String(runai.owner)}}</p> }}\n"
                );
                let d = analyze(&src, "src/routes/index.page.tsx");
                assert_eq!(
                    d.len(),
                    1,
                    "{declaration} ({directive}): one row at the declaration: {d:?}"
                );
                assert_eq!(d[0].code, Code::RA014);
                assert_eq!(
                    d[0].line, 2,
                    "{declaration}: the declaration is the site to edit: {d:?}"
                );
                assert!(
                    d[0].message.contains("as named imports"),
                    "{declaration}: the repair must be legal: {d:?}"
                );
                assert!(
                    !d[0]
                        .message
                        .contains("Move `runai` into the component body"),
                    "{declaration}: moving an import into a body is a syntax error: {d:?}"
                );
            }
        }
    }

    #[test]
    fn shell_file_needs_no_directive() {
        let src = "export default function Shell(props: { children: JSX.Element }) { return <main>{props.children}</main> }\n";
        assert!(analyze(src, "src/routes/_shell.tsx").is_empty());
    }

    /// The import rule's first branch, pinned by an **exact** code set. Every
    /// other `"runai"` fixture asserts `.any(RA010)` or a length floor, so an
    /// extra `RA014` — the exemption deleted — would have passed them all.
    #[test]
    fn foreign_import_is_ra014() {
        let src = format!("\"use static\"\nimport x from \"./data\"\n{P} {{ return <p>hi</p> }}\n");
        let d = analyze(&src, "src/routes/about.page.tsx");
        assert_eq!(codes(&d), vec![Code::RA014], "{d:?}");
        assert_eq!(d[0].line, 2, "the module specifier: {d:?}");
    }

    #[test]
    fn syntax_error_short_circuits() {
        let src = format!("\"use static\"\n{P} {{ return <div> }}\n");
        let d = analyze(&src, "src/routes/bad.page.tsx");
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].code, Code::RA007);
    }

    // --- The rest is what the brief names but does not pin ---------------------

    /// The `"runai"` exemption, pinned by an exact list in both modes. The island
    /// fixture tolerates **no** code at all, so any spurious `RA014` is visible;
    /// the static fixture asserts the primitive is the only code, so the import is
    /// not a second one.
    #[test]
    fn a_runai_import_is_never_ra014() {
        let island = format!(
            "\"use island\"\nimport {{ signal }} from \"runai\"\n{P} {{ const n = signal(0); return <p>{{n()}}</p> }}\n"
        );
        assert_eq!(
            codes(&analyze(&island, "src/routes/index.page.tsx")),
            Vec::<Code>::new(),
            "an island primitive is clean, and its import is not a second code"
        );

        let statik = format!(
            "\"use static\"\nimport {{ signal }} from \"runai\"\n{P} {{ const n = signal(0); return <p>{{n()}}</p> }}\n"
        );
        assert_eq!(
            codes(&analyze(&statik, "src/routes/index.page.tsx")),
            vec![Code::RA010],
            "the static route's only code is the primitive: {statik}"
        );
    }

    /// `validate_route_component`, reached through `analyze`. Every other page
    /// fixture has a valid default export, so deleting the call site would not
    /// have failed any of them.
    #[test]
    fn a_page_with_no_default_export_is_ra011_through_analyze() {
        let src = "\"use static\"\nexport function Page(props: Record<never, never>) { return <p>hi</p> }\n";
        let d = analyze(src, "src/routes/about.page.tsx");
        assert_eq!(codes(&d), vec![Code::RA011], "{d:?}");
        assert!(
            d[0].message.contains("no default-exported component"),
            "{d:?}"
        );
    }

    /// `RA012`, emitted. Both `_shell` tests assert its *absence*; without this
    /// fixture no page in the suite is a page without a directive, so the emission
    /// could be deleted silently.
    #[test]
    fn a_page_with_no_directive_is_ra012() {
        let src = "export default function P(props: Record<never, never>) { return <p>hi</p> }\n";
        let d = analyze(src, "src/routes/about.page.tsx");
        assert_eq!(codes(&d), vec![Code::RA012], "{d:?}");
        assert_eq!(
            (d[0].line, d[0].col),
            (1, 1),
            "the directive goes at the top"
        );
    }

    /// One exact line per diagnostic family, so a span that quietly regresses to
    /// `(1, 1)` is caught for every code — not only for the handler. `RA011` and
    /// `RA012` genuinely point at `1:1`; every other family points at a real
    /// construct, and the line differs from 1.
    #[test]
    fn every_diagnostic_family_is_located_on_its_own_line() {
        let fixtures: [(Code, &str, u32); 9] = [
            (
                Code::RA001,
                "\"use island\"\nimport { signal } from \"runai\"\nexport default function P(props: Record<never, never>) { const n = signal(0); const x = n(); return <p/> }\n",
                3,
            ),
            (
                Code::RA002,
                "\"use island\"\nimport { signal, effect } from \"runai\"\nexport default function P(props: Record<never, never>) { const n = signal(0); effect(() => { n(1) }); return <p/> }\n",
                3,
            ),
            (
                Code::RA006,
                "export default function Shell(props) { return <main>x</main> }\n",
                1,
            ),
            (
                Code::RA007,
                "\"use static\"\nexport default function P(props: Record<never, never>) { return <div> }\n",
                2,
            ),
            (
                Code::RA009,
                "\"use island\"\nimport { signal } from \"runai\"\nexport default function P(props: Record<never, never>) { const n = signal(0); n(1); return <p/> }\n",
                3,
            ),
            (
                Code::RA010,
                "\"use static\"\nexport default function P(props: Record<never, never>) { return <button onClick={() => {}}>x</button> }\n",
                2,
            ),
            (
                Code::RA011,
                "\"use static\"\nexport function Page(props: Record<never, never>) { return <p>hi</p> }\n",
                1,
            ),
            (
                Code::RA012,
                "export default function P(props: Record<never, never>) { return <p>hi</p> }\n",
                1,
            ),
            (
                Code::RA014,
                "\"use static\"\nimport x from \"./data\"\nexport default function P(props: Record<never, never>) { return <p>hi</p> }\n",
                2,
            ),
        ];

        for (code, src, line) in fixtures {
            let path = if code == Code::RA006 {
                "src/routes/_shell.tsx"
            } else {
                "src/routes/index.page.tsx"
            };
            let d = analyze(src, path);
            let diagnostic = d
                .iter()
                .find(|diagnostic| diagnostic.code == code)
                .unwrap_or_else(|| panic!("{} was not reported: {d:?}", code.as_str()));
            assert_eq!(
                diagnostic.line,
                line,
                "{} is on the wrong line: {diagnostic:?}",
                code.as_str()
            );
            assert_eq!(diagnostic.file, path, "{diagnostic:?}");
        }
    }

    /// `check_props` runs once per component, never a second time for the route.
    /// The route is the component most likely to be validated twice — it is both
    /// "the default export" and "a component in the file" — so an untyped one must
    /// still produce **one** `RA006`, not two.
    ///
    /// The other page fixtures use a *typed* `P`, so they cannot prove this: a
    /// duplicate `RA006` needs an untyped route to exist. This is the fixture that
    /// does, and its exact length is the assertion.
    #[test]
    fn an_untyped_route_produces_one_ra006_not_two() {
        let src = "\"use static\"\nexport default function P(props) { return <p>hi</p> }\n";
        let d = analyze(src, "src/routes/index.page.tsx");
        assert_eq!(codes(&d), vec![Code::RA006], "{d:?}");

        // The same route in a component module: still one, with no directive owed.
        let module = "export default function P(props) { return <p>hi</p> }\n";
        let d = analyze(module, "src/routes/_shell.tsx");
        assert_eq!(codes(&d), vec![Code::RA006], "{d:?}");
    }

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
    ///
    /// Both spellings TypeScript allows are exempt. `import { type Data }` is the
    /// one an editor's auto-import produces, and it leaves the declaration a
    /// *value* import while marking the specifier — so a check reading only
    /// `import_kind` would report a type it must not.
    #[test]
    fn a_type_only_import_is_not_ra014() {
        for declaration in [
            "import type { Data } from \"./data\"",
            "import { type Data } from \"./data\"",
            "import { type A, type B } from \"./data\"",
        ] {
            let src = format!("\"use static\"\n{declaration}\n{P} {{ return <p>hi</p> }}\n");
            let d = analyze(&src, "src/routes/about.page.tsx");
            assert!(d.is_empty(), "{declaration}: {d:?}");
        }

        // One value specifier beside a type one is still a runtime import.
        let mixed = format!(
            "\"use static\"\nimport {{ type Data, x }} from \"./data\"\n{P} {{ return <p>hi</p> }}\n"
        );
        assert_eq!(
            codes(&analyze(&mixed, "src/routes/about.page.tsx")),
            vec![Code::RA014],
            "{mixed}"
        );
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
            assert_eq!(codes(&d), vec![Code::RA014], "{declaration}: {d:?}");
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
