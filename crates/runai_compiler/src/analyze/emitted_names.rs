//! `RA014` for a route file whose own names collide with the names the emitted
//! module declares.
//!
//! The prerender module is three names — `_esc`, `_attr`, `render` — and
//! [`crate::ir::PRERENDER_NAMES`] is the one list of them. A route file that
//! declares one of those names, exports one, or references one would shadow the
//! helper the template calls, or export `render` twice:
//!
//! ```tsx
//! const render = 1                          // the emitted module exports `render`
//! export { x as render }                    // the emitted module exports `render` twice
//! function P() { const _esc = …; … }        // every `${_esc(…)}` calls the local
//! function P() { return <p>{_esc("x")}</p> } // an undeclared `_esc` resolves to ours
//! ```
//!
//! The last one is the sharpest: an undeclared `_esc` in the template resolves to
//! the compiler's own helper and renders its **source text**, silently. That is
//! why a reference is refused, not only a declaration.
//!
//! # Three shapes, in three regions
//!
//! 1. a **declaration** in an emitted module statement
//!    ([`crate::ir::emitted_module_parts`]) or an emitted body statement
//!    ([`crate::ir::emitted_body_statements`]) whose bound name is reserved;
//! 2. an **export specifier** (`export { x as render }`) whose *exported* name is
//!    reserved — the emitted module already exports `render`. `export const
//!    render = …` is shape 1's declaration and 2's name in one statement, and one
//!    row is enough;
//! 3. a **reference** to a reserved name, in the markup, the body, or the module
//!    level — but only when the reserved name is **in scope at the emitted call
//!    site**: unresolved, declared at module scope, declared in the route
//!    component's own function scope (the `var` case, including a `var` in a
//!    nested block, a `for` init, or a `try`/`catch`), or the route component
//!    itself. A reference to any deeper scope (`items.map((render) => …)`, a
//!    nested function's local, a block `const`) is legal JavaScript shadowing the
//!    emitted module preserves.
//!
//! # What is deliberately not refused
//!
//! - **a type alias** named `render` — `strip_types` erases it, so nothing is
//!   emitted under that name. Refusing it would be the false positive Task 9's
//!   review round 2 proved;
//! - **a member property** (`o.render`) or an **object key** (`{ render: 1 }`) —
//!   neither is a binding, and refusing them would reject an ordinary page;
//! - **a deeper-scope local** named a reserved name — an arrow parameter, a
//!   nested function's local, a block `const`. Its nearest enclosing function is
//!   not `render()`, so the template's call site cannot reach it, and the calls
//!   still reach the module-level helper;
//! - **a reserved name in a component module** (`_shell.tsx`, or a `.tsx` with no
//!   route) — nothing is emitted from such a file, so there is no helper to
//!   shadow. The same boundary [`crate::analyze::reachable`] draws.
//!
//! The reference walk is [`crate::analyze::reachable`]'s, shared rather than
//! reimplemented, so "a type-only reference is not a value" has one definition in
//! the crate.

use oxc_ast::ast::{Declaration, ModuleExportName, Program, Statement};
use oxc_ast_visit::Visit;
use oxc_semantic::SemanticBuilder;
use oxc_span::GetSpan;

use crate::analyze::component::find_route_component;
use crate::analyze::jsx::route_render;
use crate::analyze::reachable::references;
use crate::diagnostic::{Code, Diagnostic};
use crate::ir::{emitted_body_statements, emitted_module_parts};
use crate::source::Mode;

/// `RA014` for every declaration, export, or reference that collides with a name
/// the emitted module declares for itself.
///
/// `mode` picks the reserved list: a `"use static"` route emits
/// [`crate::ir::PRERENDER_NAMES`], and Task 11 adds the island module's names to
/// the same constant. `program` is the typed tree `analyze` parsed, so the spans
/// are the agent's own — a name `strip_types` would erase is a type declaration in
/// this tree and is recognized by node kind rather than guessed at.
///
/// Returns an empty list for a file with no route component: nothing is emitted
/// from such a file, so no name can collide. This mirrors
/// [`crate::analyze::reachable`], which draws the same boundary.
pub fn check_emitted_names(mode: Mode, program: &Program<'_>) -> Vec<Diagnostic> {
    let Some(route) = find_route_component(program) else {
        return Vec::new();
    };
    let Some(render) = route_render(program) else {
        return Vec::new();
    };
    let reserved = reserved_names(mode);

    // One semantic build for the reference walk, which judges a name by its
    // binding. A type-only reference is filtered there, exactly as
    // `check_reachable_expressions` filters it.
    let semantic = SemanticBuilder::new().build(program).semantic;
    let scoping = semantic.scoping();

    // The one scope the emitted `render()` copy shares with the source: the route
    // component's own function scope. A reference whose binding lives there is in
    // scope at every emitted call site, because the emitter copies the body into
    // `render()` and the template's calls sit at the end of that same function.
    // This is why `var` is the sharp case: it hoists to this scope from anywhere
    // in the body — a nested block, a `for` init, a `try`/`catch` — while the
    // block-scoped `const` beside it stays in its block and does not reach the
    // call site at all.
    let route_scope = render.scope_id.get();

    let mut diagnostics = Vec::new();

    // (1) Declarations in the two emitted scopes: an emitted module statement
    // (including the declarators a multi-declarator route statement leaves
    // behind), and an emitted body statement.
    for part in emitted_module_parts(program, &route) {
        check_part(&part, reserved, &mut diagnostics);
    }
    if let (Some(body), Some(return_span)) = (render.body, render.return_span) {
        for statement in emitted_body_statements(body, return_span) {
            check_declaration(statement, reserved, &mut diagnostics);
        }
    }

    // (3) References in the three emitted regions.
    //
    // A reference is refused when the reserved name is **in scope at the emitted
    // call site**, which is the template's `${_esc(…)}` / `${_attr(…)}` and the
    // module-level `render`:
    //
    // - **unresolved** (`symbol` is `None`): the name is declared nowhere, so in
    //   the template it resolves to the compiler's own helper or to `render()`.
    //   This is the silent case the rule exists for.
    // - **module scope** (the symbol's declaration scope is the root): a module
    //   binding named `render` collides with `export function render()`, and one
    //   named `_esc` shadows the helper for every statement and for the template.
    // - **the route component's own function scope**: the emitter copies the body
    //   into `render()`, so a binding in that scope is in scope at the call site.
    //   This is the `var` case — a `var` hoists here from a nested block, a `for`
    //   init, or a `try`/`catch`. It is *not* "declared at root": round 1's
    //   predicate, which missed this case and emitted a module that rendered
    //   `<p>hijacked</p>`.
    // - **the route component itself** when its name is reserved: the emitters
    //   rebuild it, so the name is not a binding in the artifact. The row for that
    //   case is `analyze::reachable`'s; this walk lands on the module-scope arm
    //   above and `analyze` keeps one row per span.
    //
    // A reference that resolves to any **other** scope — an arrow parameter, a
    // local of a nested function, a block's `const` — is ordinary JavaScript
    // shadowing of a legal local, and the emitted module preserves it: the
    // template's own `${_esc(…)}` still reaches the module-level helper, because
    // the shadow's nearest enclosing function is not `render()`'s. Refusing it
    // would reject `<p>{items.map((render) => render.name)}</p>`, which is why
    // [`reference_collision`] asks "is it in scope at the call site" rather than
    // "is it declared at root".
    for reference in references(scoping, |visitor| visitor.visit_expression(render.root)) {
        if let Some(row) = reference_collision(scoping, &reference, &route, route_scope, reserved) {
            diagnostics.push(row);
        }
    }
    if let (Some(body), Some(return_span)) = (render.body, render.return_span) {
        for statement in emitted_body_statements(body, return_span) {
            for reference in references(scoping, |visitor| visitor.visit_statement(statement)) {
                if let Some(row) =
                    reference_collision(scoping, &reference, &route, route_scope, reserved)
                {
                    diagnostics.push(row);
                }
            }
        }
    }
    for reference in emitted_reference_roots(program, &route, scoping) {
        if let Some(row) = reference_collision(scoping, &reference, &route, route_scope, reserved) {
            diagnostics.push(row);
        }
    }
    diagnostics.sort_by_key(|diagnostic| diagnostic.span.0);
    diagnostics
}

/// `RA014` for `reference` if it names something in scope at the emitted call
/// site, or `None` when it is an ordinary shadow the emitted module preserves.
///
/// The predicate is "is the reserved name in scope where `render()` calls its
/// helpers", not "is it declared at root":
///
/// - an unresolved name resolves to the emitter's own helper — refused;
/// - a binding in the module scope is one the emitter also writes — refused;
/// - a binding in the route component's own function scope is in scope at the
///   call site, because the emitter copies the body there — refused. This is the
///   `var` arm: `var` hoists to this scope from nested blocks, `for` inits, and
///   `try`/`catch`;
/// - a binding in any deeper scope (an arrow parameter, a nested function's
///   local, a block `const`) is not in scope at the call site — allowed.
///
/// A reserved-named route component is declared at module scope, so it lands in
/// the module-scope arm here; the row an agent sees for it is
/// [`crate::analyze::reachable`]'s, which owns the "the component is not a
/// binding in the artifact" wording, and `analyze` dedupes one problem to one row.
fn reference_collision(
    scoping: &oxc_semantic::Scoping,
    reference: &crate::analyze::reachable::Reference,
    route: &crate::analyze::ComponentInfo<'_>,
    route_scope: Option<oxc_semantic::ScopeId>,
    reserved: &[&str],
) -> Option<Diagnostic> {
    if !reserved.contains(&reference.name.as_str()) {
        return None;
    }

    let Some(symbol) = reference.symbol else {
        // Undeclared: in the emitted template it resolves to the compiler's own
        // helper or to `render()`, which is the silent case this rule exists for.
        return Some(reference_refusal(
            reference.span,
            &reference.name,
            ShadowKind::Undeclared,
            route,
        ));
    };

    let declaration_scope = scoping.symbol_scope_id(symbol);

    // Module scope: a binding there is one the emitter would also declare
    // (`render`) or one that shadows a helper for the whole module (`_esc`). A
    // route component named a reserved name is also declared here; its row is
    // `analyze::reachable`'s, which owns the "the component is not a binding in
    // the artifact" wording, and `analyze` dedupes one problem to one row.
    if declaration_scope == scoping.root_scope_id() {
        return Some(reference_refusal(
            reference.span,
            &reference.name,
            ShadowKind::ModuleScope,
            route,
        ));
    }

    // The route component's own function scope: the emitter copies the body into
    // `render()`, so a binding here is in scope at the template's call site. A
    // `var` lands here from any depth, which is exactly the defect this arm
    // closes.
    if Some(declaration_scope) == route_scope {
        return Some(reference_refusal(
            reference.span,
            &reference.name,
            ShadowKind::BodyScope,
            route,
        ));
    }

    // Any other nested-scope binding is an ordinary local the emitted function
    // keeps: its nearest enclosing function is not `render()`, so the template's
    // calls still reach the module-level helper.
    None
}

/// The names the module for `mode` declares for itself.
///
/// Today only the prerender module exists, so only `Static` has a list. The
/// island module is Task 11's, and returning an empty list is the honest answer
/// until that emitter is written: refusing `render` in an island page would be a
/// false positive, because the island module does not necessarily declare it. Task
/// 11 fills this arm in and, per the plan, adds the island set to the same
/// constant — so this stays one function rather than a second rule.
fn reserved_names(mode: Mode) -> &'static [&'static str] {
    match mode {
        Mode::Static => crate::ir::PRERENDER_NAMES,
        Mode::Island => &[],
    }
}

/// One emitted module part: a whole statement, or the declarators a route
/// statement leaves behind.
///
/// [`emitted_module_parts`] is the one definition of "emitted" at module scope, so
/// this walk cannot check a statement the module omits, nor skip one it carries.
fn check_part(
    part: &crate::ir::EmittedModulePart<'_, '_>,
    reserved: &[&str],
    out: &mut Vec<Diagnostic>,
) {
    match part {
        crate::ir::EmittedModulePart::Statement(statement) => {
            check_declaration(statement, reserved, out);
            // (2) Export names on the same statement. `export { x as render }`
            // exports a reserved name without declaring one; `export const
            // render` is caught by the declaration walk, so the specifier walk
            // reports only the shapes that declare nothing reserved.
            check_export_names(statement, reserved, out);
        }
        crate::ir::EmittedModulePart::Declarators { declarators, .. } => {
            for declarator in declarators {
                for identifier in declarator.id.get_binding_identifiers() {
                    refuse_if_reserved(identifier.name.as_str(), identifier.span(), reserved, out);
                }
            }
        }
    }
}

/// The module-level statements the emitters write, in source order.
///
/// [`emitted_module_parts`] is the one definition of "emitted" at module scope,
/// but it can also hand back the declarators a route statement leaves behind, so
/// the reference walk needs those declarators as roots too. A statement is walked
/// whole; a declarator alone is walked declarator by declarator, which is exactly
/// what [`crate::ir::EmittedModulePart::visit`] does for the same span.
fn emitted_reference_roots<'a>(
    program: &Program<'a>,
    route: &crate::analyze::ComponentInfo<'a>,
    scoping: &oxc_semantic::Scoping,
) -> Vec<crate::analyze::reachable::Reference> {
    let mut found = Vec::new();
    for part in emitted_module_parts(program, route) {
        found.extend(references(scoping, |visitor| part.visit(visitor)));
    }
    found
}

/// Every name `statement` **declares**, refusing a reserved one.
///
/// Only declarations, not every statement: a reference is shape 3's question, and
/// walking both here would report a name twice when it is declared and read. A
/// `type` declaration, an `interface`, and an `enum` are not value declarations
/// in the way this rule means — but `enum` and `namespace` **do** emit a runtime
/// binding, so they are declarations here. A type alias and an interface are
/// erased by `strip_types` and are skipped, which is the false positive this rule
/// must never make.
fn check_declaration(statement: &Statement<'_>, reserved: &[&str], out: &mut Vec<Diagnostic>) {
    match statement {
        Statement::VariableDeclaration(declaration) => {
            for declarator in &declaration.declarations {
                for identifier in declarator.id.get_binding_identifiers() {
                    refuse_if_reserved(identifier.name.as_str(), identifier.span(), reserved, out);
                }
            }
        }
        Statement::FunctionDeclaration(function) => {
            if let Some(identifier) = &function.id {
                refuse_if_reserved(identifier.name.as_str(), identifier.span(), reserved, out);
            }
        }
        Statement::ClassDeclaration(class) => {
            if let Some(identifier) = &class.id {
                refuse_if_reserved(identifier.name.as_str(), identifier.span(), reserved, out);
            }
        }
        Statement::TSEnumDeclaration(enumeration) => {
            refuse_if_reserved(
                enumeration.id.name.as_str(),
                enumeration.id.span(),
                reserved,
                out,
            );
        }
        Statement::TSNamespaceDeclaration(namespace) => {
            refuse_if_reserved(
                namespace.id.name.as_str(),
                namespace.id.span(),
                reserved,
                out,
            );
        }
        Statement::ExportDeclaration(exported) => {
            match &exported.declaration {
                Declaration::VariableDeclaration(declaration) => {
                    for declarator in &declaration.declarations {
                        for identifier in declarator.id.get_binding_identifiers() {
                            refuse_if_reserved(
                                identifier.name.as_str(),
                                identifier.span(),
                                reserved,
                                out,
                            );
                        }
                    }
                }
                Declaration::FunctionDeclaration(function) => {
                    if let Some(identifier) = &function.id {
                        refuse_if_reserved(
                            identifier.name.as_str(),
                            identifier.span(),
                            reserved,
                            out,
                        );
                    }
                }
                Declaration::ClassDeclaration(class) => {
                    if let Some(identifier) = &class.id {
                        refuse_if_reserved(
                            identifier.name.as_str(),
                            identifier.span(),
                            reserved,
                            out,
                        );
                    }
                }
                Declaration::TSEnumDeclaration(enumeration) => {
                    refuse_if_reserved(
                        enumeration.id.name.as_str(),
                        enumeration.id.span(),
                        reserved,
                        out,
                    );
                }
                // A `type` alias and an `interface` are erased before emission, so
                // nothing exists under that name in the artifact.
                Declaration::TSTypeAliasDeclaration(_) | Declaration::TSInterfaceDeclaration(_) => {
                }
                _ => {}
            }
        }
        // A type alias, an interface, and a bodyless overload signature are erased
        // by `strip_types`; they declare nothing in the emitted module.
        Statement::TSTypeAliasDeclaration(_) | Statement::TSInterfaceDeclaration(_) => {}
        _ => {}
    }
}

/// Refuse `name` at `span` when it is reserved.
fn refuse_if_reserved(
    name: &str,
    span: oxc_span::Span,
    reserved: &[&str],
    out: &mut Vec<Diagnostic>,
) {
    if reserved.contains(&name) {
        out.push(declaration_refusal((span.start, span.end), name));
    }
}

/// Refuse every **exported** name in `statement` that is reserved.
///
/// Only the exported name matters: `const x = 1; export { x as render }` exports
/// `render` without binding it, and the emitted module's own `export function
/// render()` would then be a duplicate export — a syntax error. A local name is
/// shape 1's question, so a specifier whose local and exported names are both
/// ordinary is ignored.
///
/// `export const render = 1` is reported by [`check_declaration`] as a
/// declaration, and this function reports its local name too — the same span, so
/// `analyze`'s dedup keeps one row.
fn check_export_names(statement: &Statement<'_>, reserved: &[&str], out: &mut Vec<Diagnostic>) {
    let Statement::ExportNamedDeclaration(exported) = statement else {
        return;
    };
    for specifier in &exported.specifiers {
        let exported_name: &str = match &specifier.exported {
            ModuleExportName::IdentifierName(identifier) => identifier.name.as_str(),
            ModuleExportName::IdentifierReference(identifier) => identifier.name.as_str(),
            ModuleExportName::StringLiteral(literal) => literal.value.as_str().unwrap_or(""),
        };
        if reserved.contains(&exported_name) {
            out.push(Diagnostic::new(
                Code::RA014,
                (specifier.span.start, specifier.span.end),
                format!(
                    "the generated module already exports `{exported_name}` itself, so exporting \
                     `{exported_name}` here is a duplicate export and the module cannot load. \
                     Export it under a different name."
                ),
            ));
        }
    }
}

/// `RA014` for a declaration that binds a reserved name.
fn declaration_refusal(span: (u32, u32), name: &str) -> Diagnostic {
    Diagnostic::new(
        Code::RA014,
        span,
        format!(
            "`{name}` is one of the names the generated module declares for itself (`{names}`), \
             so a declaration here would shadow it and the template's call would reach the \
             wrong binding. Rename `{name}`.",
            names = crate::ir::PRERENDER_NAMES.join("`, `"),
        ),
    )
}

/// `RA014` for a reference to a reserved name.
///
/// The message distinguishes the kinds, because they are different defects with
/// different repairs, and the repair must be legal when applied verbatim:
///
/// - [`ShadowKind::Undeclared`]: the name is declared nowhere, so in the emitted
///   template it resolves to the compiler's own helper and renders its source
///   text. This is the silent case the rule exists for.
/// - [`ShadowKind::ModuleScope`] and [`ShadowKind::BodyScope`]: the name is
///   declared here, so the declaration row already reports the rename; this row
///   says the *use* names the shadow, so the agent updates it too rather than only
///   the declaration.
fn reference_refusal(
    span: (u32, u32),
    name: &str,
    kind: ShadowKind,
    route: &crate::analyze::ComponentInfo<'_>,
) -> Diagnostic {
    let names = crate::ir::PRERENDER_NAMES.join("`, `");
    let message = match kind {
        ShadowKind::Undeclared => format!(
            "`{name}` is one of the names the generated module declares for itself (`{names}`), \
             and nothing in `{route}` declares it, so in the emitted template it resolves to the \
             compiler's own helper or to `render()` and renders their source text. Declare the \
             value under a different name.",
            route = route.display_name(),
        ),
        ShadowKind::ModuleScope => format!(
            "`{name}` is one of the names the generated module declares for itself (`{names}`). \
             This use names a module-level binding the file declares, which collides with the \
             emitted module's own `{name}`. Rename it here and at its declaration.",
        ),
        ShadowKind::BodyScope => format!(
            "`{name}` is one of the names the generated module declares for itself (`{names}`). \
             This use names a binding the route body declares, and the body is copied into \
             `render()`, so `{name}` is in scope at the generated call site and shadows the \
             module's own `{name}` there. Rename the body binding here and at its declaration.",
        ),
    };
    Diagnostic::new(Code::RA014, span, message)
}

/// How a reserved-name reference collides with the emitted module, which decides
/// the reason [`reference_refusal`] gives.
///
/// A reference that resolves to an ordinary nested-scope local is none of these
/// and is not refused; see [`reference_collision`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShadowKind {
    /// Declared nowhere: it resolves to the compiler's own helper or `render()`.
    Undeclared,
    /// Declared at the file's module scope, where the emitter also declares
    /// `render` and its helpers.
    ModuleScope,
    /// Declared in the route component's own function scope — the scope the
    /// emitter copies into `render()`. A `var` hoists here from a nested block, a
    /// `for` init, or a `try`/`catch`, so the emitted call site reaches it.
    BodyScope,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_module_declaration_named_render_is_ra014() {
        let src = "\"use static\"\nconst render = 1\nexport default function P(props: Record<never, never>) { return <p>hi</p> }\n";
        let d = crate::analyze::analyze(src, "src/routes/about.page.tsx");
        assert_eq!(d[0].code, Code::RA014);
        assert!(d[0].message.contains("render"), "{}", d[0].message);
    }

    /// The module would export `render` twice: once by its own `export function
    /// render()`, once by this alias. The row is at the specifier, which is the
    /// statement to edit — the local `x` is fine and needs no rename.
    #[test]
    fn an_export_alias_named_render_is_ra014() {
        let src = "\"use static\"\nconst x = 1\nexport { x as render }\nexport default function P(props: Record<never, never>) { return <p>hi</p> }\n";
        let d = crate::analyze::analyze(src, "src/routes/about.page.tsx");
        assert!(
            d.iter().any(|diagnostic| diagnostic.code == Code::RA014
                && diagnostic.message.contains("already exports")),
            "{d:?}"
        );
        let row = d
            .iter()
            .find(|diagnostic| diagnostic.message.contains("already exports"))
            .unwrap();
        assert_eq!(
            &src[row.span.0 as usize..row.span.1 as usize],
            "x as render",
            "the span is the specifier to rewrite: {row:?}"
        );
    }

    /// The body binding would shadow the escaper the template calls: every
    /// `${_esc(…)}` in the emitted `render()` would reach this local.
    #[test]
    fn a_body_binding_named_esc_is_ra014() {
        let src = "\"use static\"\nexport default function P(props: Record<never, never>) { const _esc = (v: string) => \"no\"; const name = \"<Ada>\"; return <p>{name}</p> }\n";
        let d = crate::analyze::analyze(src, "src/routes/about.page.tsx");
        assert_eq!(d.len(), 1, "{d:?}");
        assert_eq!(d[0].code, Code::RA014);
        assert!(d[0].message.contains("_esc"), "{}", d[0].message);
        assert_eq!(
            &src[d[0].span.0 as usize..d[0].span.1 as usize],
            "_esc",
            "the span is the binding to rename: {d:?}"
        );
    }

    /// The sharpest case: `_esc` is not declared anywhere, so in the emitted
    /// template it resolves to the compiler's own helper and renders its source
    /// text. Refusing the reference is the only way to stop that being silent.
    #[test]
    fn a_reference_to_a_reserved_name_is_ra014() {
        let src = "\"use static\"\nexport default function P(props: Record<never, never>) { return <p>{_esc(\"x\")}</p> }\n";
        let d = crate::analyze::analyze(src, "src/routes/about.page.tsx");
        assert_eq!(d.len(), 1, "{d:?}");
        assert_eq!(d[0].code, Code::RA014);
        assert!(
            d[0].message.contains("_esc"),
            "the message names the colliding name: {}",
            d[0].message
        );
    }

    /// The false positive this rule must never make: `strip_types` erases a type
    /// alias, so nothing is emitted under that name.
    #[test]
    fn a_type_alias_named_render_is_allowed() {
        let src = "\"use static\"\ntype render = 1\nexport default function P(props: Record<never, never>) { return <p>hi</p> }\n";
        assert!(crate::analyze::analyze(src, "src/routes/about.page.tsx").is_empty());
    }

    /// A property name is not a binding. Refusing it would reject an ordinary page.
    #[test]
    fn a_member_property_named_render_is_allowed() {
        let src = "\"use static\"\nconst o = { render: \"x\" }\nexport default function P(props: Record<never, never>) { return <p>{o.render}</p> }\n";
        assert!(crate::analyze::analyze(src, "src/routes/about.page.tsx").is_empty());
    }

    /// A nested arrow parameter named `render` shadows nothing the emitter
    /// declares: it is an ordinary local of a callback in the markup, and every
    /// `${_esc(…)}` in the template still reaches the module-level helper. The
    /// reference resolves to that parameter, which the file itself declares, so it
    /// is a legitimate local, not a collision. Refusing it was a false positive on
    /// a legal page.
    #[test]
    fn a_nested_arrow_parameter_named_render_is_allowed() {
        let src = "\"use static\"\nexport default function P(props: Record<never, never>) { return <p>{items.map((render) => render.render).join(\",\")}</p> }\n";
        let d = crate::analyze::analyze(src, "src/routes/about.page.tsx");
        assert!(
            d.is_empty(),
            "an arrow parameter is an ordinary shadow: {d:?}"
        );
    }

    /// The control for the test above: an **undeclared** `render` is still
    /// refused, because it resolves to the compiler's own `export function
    /// render()` and the template would call it. Loosening the rule for a real
    /// shadow must not loosen it for the silent case.
    #[test]
    fn an_undeclared_render_reference_is_still_refused() {
        let src = "\"use static\"\nexport default function P(props: Record<never, never>) { return <p>{render}</p> }\n";
        let d = crate::analyze::analyze(src, "src/routes/about.page.tsx");
        assert!(
            d.iter().any(|row| row.code == Code::RA014),
            "an undeclared `render` resolves to the emitted `render()`: {d:?}"
        );
    }

    /// A `var` in a **nested block** of the route body is hoisted to the
    /// component's own function scope — the scope the emitter copies into
    /// `render()` — so the template's `_esc(…)` reaches it. Round 1 judged a
    /// reference by "is its symbol declared at module scope", which is the wrong
    /// question for `var`: this emitted a module that rendered `<p>hijacked</p>`
    /// with no diagnostic, because the nested block is not module scope.
    ///
    /// The predicate is now "is the reserved name in scope at the emitted call
    /// site", i.e. does the reference resolve to a binding whose nearest
    /// enclosing function is the route component itself.
    #[test]
    fn a_var_in_a_nested_block_that_reaches_the_call_site_is_ra014() {
        let src = "\"use static\"\nexport default function P(props: Record<never, never>) {\n  const on = true; if (on) { var _esc = (v: unknown) => \"hijacked\"; }\n  return <p>{_esc(\"x\")}</p>\n}\n";
        let d = crate::analyze::analyze(src, "src/routes/about.page.tsx");
        let row = d
            .iter()
            .find(|row| row.code == Code::RA014 && row.message.contains("_esc"))
            .unwrap_or_else(|| panic!("a hoisted `var` shadows the escaper: {d:?}"));
        assert!(
            row.message.contains("body declares"),
            "the message names the body binding: {}",
            row.message
        );
        assert_eq!(
            &src[row.span.0 as usize..row.span.1 as usize],
            "_esc",
            "the span is the hijacking call: {row:?}"
        );
    }

    /// A `for (var …)` initialiser hoists to the function scope just as a nested
    /// block does. This shape emitted a module that threw at build time
    /// (`TypeError: _esc is not a function`) while every test in the crate passed.
    #[test]
    fn a_var_in_a_for_initialiser_that_reaches_the_call_site_is_ra014() {
        let src = "\"use static\"\nexport default function P(props: Record<never, never>) {\n  for (var _esc = 1; false;) {}\n  return <p>{_esc(\"x\")}</p>\n}\n";
        let d = crate::analyze::analyze(src, "src/routes/about.page.tsx");
        assert!(
            d.iter()
                .any(|row| row.code == Code::RA014 && row.message.contains("body declares")),
            "a `var` in a `for` init reaches the call site: {d:?}"
        );
    }

    /// A block-scoped `const` in a nested block does **not** reach the call site:
    /// in the emitted template the reference resolves to nothing, which is the
    /// silent-helper case the round-1 message already covers. Refusing it stays
    /// correct, and the reason is the reference, not the declaration.
    #[test]
    fn a_nested_block_const_that_does_not_reach_the_call_site_is_still_refused() {
        let src = "\"use static\"\nexport default function P(props: Record<never, never>) {\n  { const _esc = 1; }\n  return <p>{_esc(\"x\")}</p>\n}\n";
        let d = crate::analyze::analyze(src, "src/routes/about.page.tsx");
        assert!(
            d.iter()
                .any(|row| row.code == Code::RA014
                    && row.message.contains("nothing in `P` declares it")),
            "the reference resolves to nothing at the call site: {d:?}"
        );
    }

    /// A `var` inside a **nested function** never reaches the emitted call site:
    /// its nearest enclosing function is that nested function, so the emitted
    /// module's `_esc` is untouched. Nothing in the markup names it, so the file
    /// is clean. This is the guard that keeps the hoisting rule from refusing
    /// every helper that happens to use the name.
    #[test]
    fn a_var_in_a_nested_function_that_nothing_references_is_clean() {
        let src = "\"use static\"\nexport default function P(props: Record<never, never>) {\n  function f() { var _esc = 1 }\n  return <p>hi</p>\n}\n";
        let d = crate::analyze::analyze(src, "src/routes/about.page.tsx");
        assert!(
            d.is_empty(),
            "a nested function's local never reaches render()'s call site: {d:?}"
        );
    }

    /// The route component always names something in the emitted module: the
    /// emitters rebuild it as `export function render()`, so a component named
    /// `render` is legal. The row an agent sees for a reserved-named component is
    /// `reachable`'s, which owns the "the component is not a binding in the
    /// artifact" wording; `emitted_names` must not add a second row for the same
    /// span.
    #[test]
    fn a_reserved_named_route_component_is_named_by_the_reachable_rule() {
        let src = "\"use static\"\nexport default function render(props: Record<never, never>) { return <p>{render.name}</p> }\n";
        assert!(crate::analyze::analyze(src, "src/routes/about.page.tsx")
            .iter()
            .any(|row| row.code == Code::RA014 && row.message.contains("route's own component")));
    }

    /// A component module is not emitted by this milestone, so there is no helper
    /// to shadow. Without this control, dropping the route guard would refuse
    /// every `_shell.tsx` that happens to name `render`.
    #[test]
    fn a_component_module_may_name_a_reserved_name() {
        let src = "const render = 1\nexport function Header(props: Record<never, never>) { return <h1>{render}</h1> }\n";
        assert!(
            crate::analyze::analyze(src, "src/routes/_shell.tsx").is_empty(),
            "nothing is emitted from a component module"
        );
    }
}
