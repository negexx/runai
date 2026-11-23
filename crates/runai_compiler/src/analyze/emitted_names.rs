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
//!    level — but only when it resolves to the compiler's own helper, to a
//!    module-level binding, or to the route component. A reference to an
//!    ordinary nested-scope local (`items.map((render) => …)`) is legal
//!    JavaScript shadowing the emitted module preserves.
//!
//! # What is deliberately not refused
//!
//! - **a type alias** named `render` — `strip_types` erases it, so nothing is
//!   emitted under that name. Refusing it would be the false positive Task 9's
//!   review round 2 proved;
//! - **a member property** (`o.render`) or an **object key** (`{ render: 1 }`) —
//!   neither is a binding, and refusing them would reject an ordinary page;
//! - **a nested-scope local** named a reserved name — an arrow parameter, a
//!   nested function's local, a block `const`. It shadows the helper only inside
//!   its own callback, and the template's calls still reach the module-level one;
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
    // A reference is refused only when it resolves to something the emitted module
    // cannot have as a distinct binding:
    //
    // - **unresolved** (`symbol` is `None`): the name is declared nowhere, so in
    //   the template it resolves to the compiler's own helper or to `render()`.
    //   This is the silent case the rule exists for.
    // - **module scope** (the symbol's declaration scope is the root): a module
    //   binding named `render` collides with `export function render()`, and one
    //   named `_esc` shadows the helper for every statement and for the template.
    // - **the route component or its parameter** when that name is reserved.
    //
    // A reference that resolves to a binding declared in a **nested** scope inside
    // the component — an arrow parameter, a local of a nested function, a block's
    // `const` — is ordinary JavaScript shadowing of a legal local, and the emitted
    // module preserves it: the template's own `${_esc(…)}` still reaches the
    // module-level helper, because the shadow lives only inside that callback.
    // Refusing it would reject `<p>{items.map((render) => render.name)}</p>`, which
    // is why [`reference_collision`] narrows on the scope rather than the name
    // alone.
    for reference in references(scoping, |visitor| visitor.visit_expression(render.root)) {
        if let Some(row) = reference_collision(scoping, &reference, &route, reserved) {
            diagnostics.push(row);
        }
    }
    if let (Some(body), Some(return_span)) = (render.body, render.return_span) {
        for statement in emitted_body_statements(body, return_span) {
            for reference in references(scoping, |visitor| visitor.visit_statement(statement)) {
                if let Some(row) = reference_collision(scoping, &reference, &route, reserved) {
                    diagnostics.push(row);
                }
            }
        }
    }
    for reference in emitted_reference_roots(program, &route, scoping) {
        if let Some(row) = reference_collision(scoping, &reference, &route, reserved) {
            diagnostics.push(row);
        }
    }
    diagnostics.sort_by_key(|diagnostic| diagnostic.span.0);
    diagnostics
}

/// `RA014` for `reference` if it collides with a reserved name, or `None` when it
/// is an ordinary shadow the emitted module preserves.
///
/// The three refused shapes are documented on the call site. The distinction is
/// the **scope** the reference resolves to: a binding declared in the root scope
/// is one the emitter would also declare or merge, while a binding declared in a
/// nested scope is a local the emitted function keeps.
fn reference_collision(
    scoping: &oxc_semantic::Scoping,
    reference: &crate::analyze::reachable::Reference,
    route: &crate::analyze::ComponentInfo<'_>,
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

    let declaration = scoping.symbol_span(symbol);
    let declaration_scope = scoping.symbol_scope_id(symbol);

    // Module scope: a binding there is one the emitter would also declare
    // (`render`) or one that shadows a helper for the whole module (`_esc`).
    if declaration_scope == scoping.root_scope_id() {
        return Some(reference_refusal(
            reference.span,
            &reference.name,
            ShadowKind::ModuleScope,
            route,
        ));
    }

    // The route component's own name is excluded from `module_statements` and the
    // emitters rebuild it, so a reserved-named component is still a collision with
    // the emitted `render()`.
    if declaration.start == route.name_span.0 && declaration.end == route.name_span.1 {
        return Some(reference_refusal(
            reference.span,
            &reference.name,
            ShadowKind::RouteComponent,
            route,
        ));
    }

    // Any other nested-scope binding is an ordinary local the emitted function
    // keeps: it shadows the reserved name only inside its own callback/block, and
    // the template's calls still reach the module-level helper.
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
/// - [`ShadowKind::ModuleScope`] and [`ShadowKind::RouteComponent`]: the name is
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
        ShadowKind::RouteComponent => format!(
            "`{name}` is one of the names the generated module declares for itself (`{names}`). \
             This use names the route component itself, which the emitters rebuild rather than \
             copy, so `{name}` is not a binding in the artifact. Rename the component here and at \
             its declaration.",
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
    /// Declared as the route component, which the emitters rebuild from `name`
    /// and `statements` rather than copy.
    RouteComponent,
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
