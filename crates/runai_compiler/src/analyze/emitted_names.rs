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
//!    site**: unresolved, or declared at module scope. A route component named a
//!    reserved name is declared at module scope too, so it lands in that same arm;
//!    the row an agent reads for it is [`crate::analyze::reachable`]'s, which owns
//!    the "the component is not a binding in the artifact" wording. A reference
//!    to any deeper scope (`items.map((render) => …)`, a nested function's local,
//!    a block `const`) is legal JavaScript shadowing the emitted module preserves.
//!
//! # The declaration rule, and why the reference walk is not it
//!
//! Shapes 1 and 3 both see declarations, but neither is the rule this module is
//! really enforcing: **a reserved name bound in the route component's own scope
//! is in scope at the emitted call site**, because the emitter copies the body
//! into `render()`. Shape 1 walks only the *top level* of the emitted statements,
//! and shape 3 needs a reference to exist at all — so an unreferenced `var` in a
//! nested block slipped through both and emitted a module that threw
//! `TypeError: _esc is not a function` at build time while `check` was clean.
//!
//! [`check_scope_bindings`] is the direct statement of that rule: it reads the
//! route component's own scope out of the semantic pass and refuses every
//! reserved name bound there. The three shapes above stay, because they own rows
//! this one cannot produce:
//!
//! - **shape 1** names a declaration the agent must rename where it is *written*,
//!   in a module statement or the body's top level;
//! - **shape 3** is the only row for a name the file never declares (an undeclared
//!   `_esc` in the markup silently resolving to the compiler's helper), and it is
//!   the row that points at the offending *use*.
//!
//! So a file that declares and reads a reserved name gets two rows for one
//! problem — one at each site to edit — which is the same shape the module-scope
//! arm already had. Deleting either walk would lose a site, not a duplicate.
//!
//! # What is deliberately not refused
//!
//! - **a type alias** named `render` — `strip_types` erases it, so nothing is
//!   emitted under that name. Refusing it would be the false positive Task 9's
//!   review round 2 proved. The test is "type-only", not "a type": a `type _esc`
//!   beside a `var _esc` is *one* symbol whose unioned flags report a type, and
//!   the value declaration in it is refused — see [`check_scope_bindings`];
//! - **a member property** (`o.render`) or an **object key** (`{ render: 1 }`) —
//!   neither is a binding, and refusing them would reject an ordinary page;
//! - **a deeper-scope local** named a reserved name — an arrow parameter, a
//!   nested function's local, a block `const`. Its nearest enclosing function is
//!   not `render()`, so the template's call site cannot reach it, and the calls
//!   still reach the module-level helper;
//! - **a reserved name in a component module** (`_shell.tsx`, or a `.tsx` with no
//!   route) — nothing is emitted from such a file, so there is no helper to
//!   shadow. The same boundary [`crate::analyze::reachable`] draws;
//! - **a component's own name in a named function expression** — one shape binds
//!   it in the component's own scope (`export default (function render(props)
//!   {…})`), another binds a *declarator* beside it (`const Page = function
//!   render(props) {…}`). In both the emitter rebuilds the component from
//!   `RouteIr` and never writes the expression, so the name is not in the
//!   artifact; `SymbolFlags::FunctionExpression` identifies them without a skip by
//!   name (a genuine body `var render` keeps refusing);
//! - **a binding after the route's `return`** — unreachable code the emitter never
//!   writes, because it slices exactly [`crate::ir::emitted_body_statements`], which
//!   stops at `return_span`.
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
pub fn check_emitted_names(mode: Mode, source: &str, program: &Program<'_>) -> Vec<Diagnostic> {
    let Some(route) = find_route_component(program) else {
        return Vec::new();
    };
    let Some(render) = route_render(program) else {
        return Vec::new();
    };
    let reserved = reserved_names(mode, source, program);
    let reserved: &[String] = &reserved;

    // Every symbol a `"runai"` import binds. A reference that resolves to one of
    // these is the **legitimate** import the module writes, not a collision — the
    // island reserved set includes those names precisely so a *declaration* of one
    // is refused, but the import's own use must stay legal. `check_declaration` and
    // `check_scope_bindings` judge the declarations; only this reference walk needs
    // the exemption.
    let runai: Vec<oxc_semantic::SymbolId> = crate::analyze::signals::runai_imports(program)
        .into_iter()
        .map(|import| import.symbol)
        .collect();
    let runai: &[oxc_semantic::SymbolId] = &runai;

    // One semantic build for the reference walk, which judges a name by its
    // binding. A type-only reference is filtered there, exactly as
    // `check_reachable_expressions` filters it.
    let semantic = SemanticBuilder::new().build(program).semantic;
    let scoping = semantic.scoping();

    // The one scope the emitted `render()` copy shares with the source: the route
    // component's own function scope. A binding here is in scope at every emitted
    // call site, because the emitter copies the body into `render()` and the
    // template's calls sit at the end of that same function. This is why `var` is
    // the sharp case: it hoists to this scope from anywhere in the body — a nested
    // block, a `for` init, a `try`/`catch` — while the block-scoped `const` beside
    // it stays in its block and does not reach the call site at all.
    //
    // The cell fails **closed**. `None` means the semantic build never filled the
    // cell that this rule was added to read, which is a compiler defect, not a
    // clean file: treating it as "no scope, nothing in scope" would silently
    // disable the declaration rule below for a whole route shape. Naming the
    // limitation is the loud failure.
    let Some(route_scope) = render.scope_id.get() else {
        debug_assert!(
            false,
            "the semantic build must fill the route component's scope cell"
        );
        return vec![Diagnostic::new(
            Code::RA014,
            route.body_span,
            "the compiler could not resolve the route component's own scope, so it cannot tell \
             whether the body declares `_esc`, `_attr`, or `render` — the names the generated \
             module declares for itself. This is a limitation of the compiler, not of this file: \
             rerun the build, and report it if it persists.",
        )];
    };

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

    // (1b) **Every** name bound in the route component's own scope, whether or
    // not the file references it. This is the declaration rule proper, and the
    // reference walk below was only ever a proxy for it: a `var` hoists into this
    // scope from a nested block, a `for` init, or a `try`/`catch`, so it is in
    // scope at the template's call site even when the top level of the body never
    // mentions it. `check_declaration` above cannot see it — it walks the top
    // level of the emitted statements — and a reference walk cannot see a name
    // nothing references. Under `f2b2dea` this exact shape emitted
    // `return \`<p>${_esc(1 + 1)}</p>\`` under a `var _esc = 1`, and the module
    // threw `TypeError: _esc is not a function` at build time with `check` clean.
    //
    // `get_binding`, not `find_binding`: a binding is in this map only when it is
    // **declared in this scope**, which is exactly "hoisted into the emitted
    // function's scope". `find_binding` walks ancestors and would refuse every
    // module-level name too.
    check_scope_bindings(
        scoping,
        route_scope,
        &route,
        &render,
        reserved,
        &mut diagnostics,
    );

    // (3) References in the three emitted regions.
    //
    // This row is **not** redundant with (1b): a declaration is refused wherever
    // it is written, but only a reference tells an agent which *use* must change,
    // and it is the only row for a name the file never declares (an undeclared
    // `_esc` in the markup, which silently resolves to the compiler's own
    // helper). (1b) owns "a binding hoisted into `render()`'s scope"; (3) owns
    // "a use that reaches a shadow at the emitted call site".
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
        if let Some(row) =
            reference_collision(scoping, &reference, &route, route_scope, reserved, runai)
        {
            diagnostics.push(row);
        }
    }
    if let (Some(body), Some(return_span)) = (render.body, render.return_span) {
        for statement in emitted_body_statements(body, return_span) {
            for reference in references(scoping, |visitor| visitor.visit_statement(statement)) {
                if let Some(row) =
                    reference_collision(scoping, &reference, &route, route_scope, reserved, runai)
                {
                    diagnostics.push(row);
                }
            }
        }
    }
    for reference in emitted_reference_roots(program, &route, scoping) {
        if let Some(row) =
            reference_collision(scoping, &reference, &route, route_scope, reserved, runai)
        {
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
    route_scope: oxc_semantic::ScopeId,
    reserved: &[String],
    runai: &[oxc_semantic::SymbolId],
) -> Option<Diagnostic> {
    if !reserved.iter().any(|name| name == &reference.name) {
        return None;
    }

    // A reference that resolves to a `"runai"` import is the legitimate binding the
    // module writes, not a shadow of it. This arm is island-only in practice: the
    // island reserved set includes `signal`, `mount`, and the helpers because a
    // *declaration* of one must be refused, but the import's own use must stay
    // legal. `check_declaration`/`check_scope_bindings` judge the declarations.
    if reference
        .symbol
        .is_some_and(|symbol| runai.contains(&symbol))
    {
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
            reserved,
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
            reserved,
        ));
    }

    // The route component's own function scope: the emitter copies the body into
    // `render()`, so a binding here is in scope at the template's call site. A
    // `var` lands here from any depth, which is exactly the defect this arm
    // closes.
    if declaration_scope == route_scope {
        return Some(reference_refusal(
            reference.span,
            &reference.name,
            ShadowKind::BodyScope,
            route,
            reserved,
        ));
    }

    // Any other nested-scope binding is an ordinary local the emitted function
    // keeps: its nearest enclosing function is not `render()`, so the template's
    // calls still reach the module-level helper.
    None
}

/// Refuse every reserved name **bound in the route component's own scope**.
///
/// This is the declaration rule the reference walk was a proxy for. A name in
/// this scope's binding table is one the emitted `render()` declares at its own
/// top level, because the emitter copies the component body into that function —
/// so it shadows `_esc`, `_attr`, or `render` at the call site inside the
/// template, whether or not anything in the file references it.
///
/// `get_binding` is the right lookup, not `find_binding`: the map holds only the
/// names **declared in this scope**, which is exactly "hoisted into the emitted
/// function's scope". `find_binding` walks ancestors and would also refuse every
/// module-level name — a different rule, and a different row.
///
/// What lands here: a `var` from anywhere in the body (a nested block, a `for`
/// init, a `try`/`catch`, a destructuring pattern), and a top-level `const`,
/// `let`, `class`, or `function` of the body. What does **not**, because the
/// binder puts it in a deeper scope: a nested-block `const`/`let`, a block-level
/// `function` declaration (ESM is strict), a nested function's or arrow's own
/// locals and parameters, and a `catch` parameter.
///
/// Two bindings in this map are not this rule's: the route parameter, which
/// [`crate::analyze::reachable`] already refuses when it is named something other
/// than `props` (refusing it here too would be two rows for one problem), and a
/// named function expression's own name in the one shape that binds it here —
/// there is nothing to shadow, because the emitter never writes the expression.
///
/// Three things about this walk are narrower than the first version's, and each
/// was a defect a review round found:
///
/// - **each declaration's own flags**, not the symbol's union: Oxc's binder merges
///   declarations that share a name in one scope into one symbol and unions their
///   flags, so `type _esc = 1; if (false) { var _esc = 1 }` is one symbol that
///   reports a type. Judging the union let the `var` hide behind the alias, and
///   the artifact carried `var _esc = 1` above `${_esc(1 + 1)}` — `TypeError:
///   _esc is not a function` at build time. [`declarations_of`] reads each site.
/// - **the returned range**, not the whole body: the emitter writes
///   [`crate::ir::emitted_body_statements`] only, which stops at `return_span`.
///   A `var _esc` after the `return` is unreachable code nothing writes, so
///   refusing it would be a false positive. This filter reads the same bound the
///   emitter does rather than reimplementing it.
/// - **`SymbolFlags::FunctionExpression`**, not the name: a function expression's
///   own name is bound in the function's own scope, which for a route is this one,
///   and the emitter rebuilds the component from `RouteIr` instead of writing the
///   expression. `route.name_span` is the *declarator* (`const Page = function
///   render(…)` binds `render` at a different span), so the flag is what
///   identifies the shape. It is deliberately not a skip by name: a genuine
///   `var render` in the body keeps refusing.
fn check_scope_bindings(
    scoping: &oxc_semantic::Scoping,
    route_scope: oxc_semantic::ScopeId,
    route: &crate::analyze::ComponentInfo<'_>,
    render: &crate::analyze::jsx::RouteRender<'_>,
    reserved: &[String],
    out: &mut Vec<Diagnostic>,
) {
    let params = render.params.span;
    // The bound the emitter writes up to: `render()` carries only the statements
    // before the JSX `return`, so a binding declared at or after it is not in the
    // artifact. A concise body (`(props) => <p/>`) has no statements at all, so
    // nothing is emitted and nothing can be shadowed.
    let Some(return_span) = render.return_span else {
        return;
    };
    for name in reserved {
        let Some(symbol) =
            scoping.get_binding(route_scope, oxc_ast::ast::Ident::from(name.as_str()))
        else {
            continue;
        };
        // The declarations of this **name**, each with its own flags. Oxc's
        // binder merges declarations that share a name in one scope into one
        // symbol and unions their flags onto it, so the symbol's flags cannot
        // answer "is there a value here, and where is it written": a `type _esc`
        // and a `var _esc` in the same scope are one symbol whose union reports a
        // type, and whose span is whichever declaration came first. Reading the
        // redeclaration list keeps each site's own span and meaning.
        for (span, flags) in declarations_of(scoping, symbol) {
            // Only a **value** declaration can shadow the helper. A type alias or
            // interface is erased by `strip_types`, so nothing is emitted under
            // the name. `is_type()` alone is not the test: the union above reports
            // a type for the `var` too, which is how the `var` hid behind the
            // alias and emitted `TypeError: _esc is not a function`.
            if !flags.is_value() {
                continue;
            }
            // A function expression's own name is bound in the function's own
            // scope, which for a route is this one, and the emitter rebuilds the
            // component from `RouteIr` instead of writing the expression — so the
            // name is not in the artifact. The flag, not the name, identifies the
            // shape: `route.name_span` is the **declarator**
            // (`const Page = function render(…)` binds `render` elsewhere), while
            // a genuine `var render` in the body must keep refusing.
            if flags.contains(oxc_syntax::symbol::SymbolFlags::FunctionExpression) {
                continue;
            }
            // Unreachable code after the route's `return` is not emitted. This
            // bound mirrors `crate::ir::emitted_body_statements`, the one
            // definition of "emitted" the slicer filters with.
            if span.start >= return_span.start {
                continue;
            }
            // The route parameter: `analyze::reachable` already refuses a
            // differently-named one, so refusing it here would be a second row.
            if span.start >= params.start && span.end <= params.end {
                continue;
            }
            // The component's own name in `export default (function render(…))`:
            // its span *is* the name span, and the emitter never writes it.
            if (span.start, span.end) == route.name_span {
                continue;
            }
            out.push(scope_binding_refusal(
                (span.start, span.end),
                name,
                reserved,
            ));
        }
    }
}

/// Every declaration of `symbol`, each with that declaration's own flags.
///
/// [`oxc_semantic::Scoping::symbol_redeclarations`] is empty for a name declared
/// once; when it is not, it already holds the first declaration followed by each
/// redeclaration, each with the flags it was declared with. Reading that list
/// instead of the symbol's unioned flags is what lets a `type` alias and a `var`
/// of one name be judged separately — the union cannot say which site is which.
fn declarations_of(
    scoping: &oxc_semantic::Scoping,
    symbol: oxc_syntax::symbol::SymbolId,
) -> Vec<(oxc_span::Span, oxc_syntax::symbol::SymbolFlags)> {
    let redeclarations = scoping.symbol_redeclarations(symbol);
    if redeclarations.is_empty() {
        return vec![(scoping.symbol_span(symbol), scoping.symbol_flags(symbol))];
    }
    let mut declarations: Vec<_> = redeclarations
        .iter()
        .map(|redeclaration| (redeclaration.span, redeclaration.flags))
        .collect();
    declarations.sort_by_key(|(span, _)| span.start);
    declarations
}

/// `RA014` for a reserved name bound in the route component's own scope.
///
/// The repair is the rename the declaration row already gives, and the reason is
/// the **hoist**: a `var` reaches this scope from a nested block even when the
/// file never mentions it, so "nothing references it" is not a defence.
fn scope_binding_refusal(span: (u32, u32), name: &str, reserved: &[String]) -> Diagnostic {
    Diagnostic::new(
        Code::RA014,
        span,
        format!(
            "`{name}` is one of the names the generated module declares for itself (`{names}`). \
             This binding is declared in the route component's own scope, which the emitter copies \
             into the generated component, so the template's `{name}(…)` call reaches it instead \
             of the generated helper. A `var` reaches this scope from any nested block, `for` \
             init, or `try`/`catch`, even when nothing references it. Rename `{name}`.",
            names = reserved.join("`, `"),
        ),
    )
}

/// The names the module for `mode` declares for itself.
///
/// For `Static` it is the fixed [`crate::ir::PRERENDER_NAMES`]. For `Island` it is
/// [`crate::ir::ISLAND_LOCALS`] (`_c`, `_a`, `_el`, `_t0`) plus every import
/// specifier the module **will actually write**, which is data rather than a
/// constant: it depends on the route, so the merged import list is passed in.
/// [`crate::codegen::island_reserved_names`] computes it from this same program, so
/// "the module imports a name" and "a declaration of that name is refused" read one
/// specifier list. The "is the name in scope at the emitted call site" predicate
/// below is unchanged; only the name set grows.
///
/// `source` is only read by the island arm, for the tree-dependent helpers.
fn reserved_names(mode: Mode, source: &str, program: &Program<'_>) -> Vec<String> {
    match mode {
        Mode::Static => crate::ir::PRERENDER_NAMES
            .iter()
            .map(|name| (*name).to_string())
            .collect(),
        Mode::Island => crate::codegen::island_reserved_names(source, program),
    }
}

/// One emitted module part: a whole statement, or the declarators a route
/// statement leaves behind.
///
/// [`emitted_module_parts`] is the one definition of "emitted" at module scope, so
/// this walk cannot check a statement the module omits, nor skip one it carries.
fn check_part(
    part: &crate::ir::EmittedModulePart<'_, '_>,
    reserved: &[String],
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
fn check_declaration(statement: &Statement<'_>, reserved: &[String], out: &mut Vec<Diagnostic>) {
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
    reserved: &[String],
    out: &mut Vec<Diagnostic>,
) {
    if reserved.iter().any(|reserved_name| reserved_name == name) {
        out.push(declaration_refusal((span.start, span.end), name, reserved));
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
fn check_export_names(statement: &Statement<'_>, reserved: &[String], out: &mut Vec<Diagnostic>) {
    let Statement::ExportNamedDeclaration(exported) = statement else {
        return;
    };
    for specifier in &exported.specifiers {
        let exported_name: &str = match &specifier.exported {
            ModuleExportName::IdentifierName(identifier) => identifier.name.as_str(),
            ModuleExportName::IdentifierReference(identifier) => identifier.name.as_str(),
            ModuleExportName::StringLiteral(literal) => literal.value.as_str().unwrap_or(""),
        };
        if reserved.iter().any(|name| name == exported_name) {
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
fn declaration_refusal(span: (u32, u32), name: &str, reserved: &[String]) -> Diagnostic {
    Diagnostic::new(
        Code::RA014,
        span,
        format!(
            "`{name}` is one of the names the generated module declares for itself (`{names}`), \
             so a declaration here would shadow it and the template's call would reach the \
             wrong binding. Rename `{name}`.",
            names = reserved.join("`, `"),
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
    reserved: &[String],
) -> Diagnostic {
    let names = reserved.join("`, `");
    let message = match kind {
        ShadowKind::Undeclared => format!(
            "`{name}` is one of the names the generated module declares for itself (`{names}`), \
             and nothing in `{route}` declares it, so in the emitted template it resolves to the \
             compiler's own helper and renders its source text. Declare the value under a \
             different name.",
            route = route.display_name(),
        ),
        ShadowKind::ModuleScope => format!(
            "`{name}` is one of the names the generated module declares for itself (`{names}`). \
             This use names a module-level binding the file declares, which collides with the \
             emitted module's own `{name}`. Rename it here and at its declaration.",
        ),
        ShadowKind::BodyScope => format!(
            "`{name}` is one of the names the generated module declares for itself (`{names}`). \
             This use names a binding the route body declares, and the body is copied into the \
             generated component, so `{name}` is in scope at the generated call site and shadows \
             the module's own `{name}` there. Rename the body binding here and at its declaration.",
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
        // Two rows for one problem, the same shape the module-scope arm has: the
        // declaration is where the rename happens, the use is where the agent reads
        // why. The declaration row is `check_scope_bindings`'s; the other is the
        // reference walk's. Both rows are pinned, not "at least one": each names a
        // distinct edit, and dropping either would hide a site.
        assert_eq!(d.len(), 2, "a declaration row and a reference row: {d:?}");

        let declaration = d
            .iter()
            .find(|row| {
                row.message
                    .contains("declared in the route component's own scope")
            })
            .unwrap_or_else(|| panic!("the declaration row is missing: {d:?}"));
        assert_eq!(declaration.code, Code::RA014);
        assert_eq!(
            &src[declaration.span.0 as usize..declaration.span.1 as usize],
            "_esc",
            "the declaration row points at the binding to rename: {declaration:?}"
        );

        let reference = d
            .iter()
            .find(|row| row.message.contains("body declares"))
            .unwrap_or_else(|| panic!("the reference row is missing: {d:?}"));
        assert_eq!(reference.code, Code::RA014);
        assert_eq!(
            &src[reference.span.0 as usize..reference.span.1 as usize],
            "_esc",
            "the span is the hijacking call: {reference:?}"
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
    /// problem. Exactly one row, not "at least one": the claim is one problem, one
    /// row.
    #[test]
    fn a_reserved_named_route_component_is_named_by_the_reachable_rule() {
        let src = "\"use static\"\nexport default function render(props: Record<never, never>) { return <p>{render.name}</p> }\n";
        let d = crate::analyze::analyze(src, "src/routes/about.page.tsx");
        assert_eq!(d.len(), 1, "one problem, one row: {d:?}");
        assert_eq!(d[0].code, Code::RA014);
        assert!(
            d[0].message.contains("route's own component"),
            "the row is `reachable`'s: {}",
            d[0].message
        );
    }

    /// The declaration rule proper: an **unreferenced** `var` in a nested block
    /// still hoists into the route component's own scope, which the emitter copies
    /// into `render()`. Nothing in the file names `_esc`, so the reference walk
    /// has nothing to say — and `check_declaration` only walks the *top level* of
    /// the emitted body, so a nested `if` swallowed it. The artifact this emitted
    /// under `f2b2dea` was `return \`<p>${_esc(1 + 1)}</p>\`` with `var _esc = 1`
    /// above it, i.e. `TypeError: _esc is not a function` at build time while
    /// `check` was clean.
    #[test]
    fn an_unreferenced_var_in_a_nested_block_is_ra014() {
        let src = "\"use static\"\nexport default function P(props: Record<never, never>) {\n  if (false) { var _esc = 1 }\n  return <p>{1 + 1}</p>\n}\n";
        let d = crate::analyze::analyze(src, "src/routes/about.page.tsx");
        assert_eq!(d.len(), 1, "a hoisted `var` is refused: {d:?}");
        assert_eq!(d[0].code, Code::RA014);
        assert!(
            d[0].message.contains("`_esc`"),
            "the message names the reserved name: {}",
            d[0].message
        );
        assert_eq!(
            &src[d[0].span.0 as usize..d[0].span.1 as usize],
            "_esc",
            "the span is the binding to rename: {d:?}"
        );
    }

    /// The same defect through the dynamic-attribute helper: an unreferenced
    /// `var _attr` is in scope where `${_attr("class", …)}` runs, so the emitted
    /// module would throw on the attribute instead of the slot.
    #[test]
    fn an_unreferenced_var_for_a_dynamic_attribute_is_ra014() {
        let src = "\"use static\"\nexport default function P(props: Record<never, never>) {\n  const cls = \"a\"\n  if (false) { var _attr = 1 }\n  return <p class={cls}>hi</p>\n}\n";
        let d = crate::analyze::analyze(src, "src/routes/about.page.tsx");
        assert_eq!(d.len(), 1, "a hoisted `var` is refused: {d:?}");
        assert_eq!(d[0].code, Code::RA014);
        assert!(
            d[0].message.contains("`_attr`"),
            "the message names the reserved name: {}",
            d[0].message
        );
    }

    /// The scope cell is populated from a different AST node for an arrow route
    /// (`ArrowFunctionExpression` rather than `Function`), so the declaration rule
    /// is exercised through that node too. `f2b2dea` judged only a *reference's*
    /// scope, so this fixture was clean and the artifact threw.
    #[test]
    fn an_unreferenced_var_in_an_arrow_route_is_ra014() {
        let src = "\"use static\"\nconst P = (props: Record<never, never>) => {\n  if (false) { var _esc = 1 }\n  return <p>{1 + 1}</p>\n}\nexport default P\n";
        let d = crate::analyze::analyze(src, "src/routes/about.page.tsx");
        assert_eq!(d.len(), 1, "a hoisted `var` is refused: {d:?}");
        assert_eq!(d[0].code, Code::RA014);
        assert!(
            d[0].message.contains("`_esc`"),
            "the message names the reserved name: {}",
            d[0].message
        );
    }

    /// ESM is strict, so a **block-level function declaration** is scoped to its
    /// block; it does not hoist out to `render()`. `oxc_semantic`'s binder agrees
    /// (the block is its own scope), so this is legal and the artifact must still
    /// be emitted.
    #[test]
    fn a_block_level_function_declaration_is_not_hoisted_out_of_its_block() {
        let src = "\"use static\"\nexport default function P(props: Record<never, never>) {\n  if (false) { function _esc() {} }\n  return <p>{1 + 1}</p>\n}\n";
        let d = crate::analyze::analyze(src, "src/routes/about.page.tsx");
        assert!(
            d.is_empty(),
            "a block-level function declaration stays in its block: {d:?}"
        );
        assert!(
            crate::codegen::emit_prerender_checked(src, "src/routes/about.page.tsx").is_ok(),
            "the artifact is still emitted"
        );
    }

    /// A block-scoped `const` is declared in the block's own scope, not the
    /// route component's, so with nothing referencing it there is no collision at
    /// all.
    #[test]
    fn an_unreferenced_nested_block_const_is_clean() {
        let src = "\"use static\"\nexport default function P(props: Record<never, never>) {\n  if (false) { const _esc = 1 }\n  return <p>hi</p>\n}\n";
        let d = crate::analyze::analyze(src, "src/routes/about.page.tsx");
        assert!(d.is_empty(), "a block `const` stays in its block: {d:?}");
    }

    /// The declaration rule reads the route component's own scope, and one shape
    /// binds a name *there* without shadowing anything: a named **function
    /// expression** route. Its name is the component's, and the emitter rebuilds
    /// the component from `RouteIr` rather than writing the expression, so the
    /// name never appears in the artifact. Refusing it would be a false positive
    /// on a legal page — the same class of mistake the arrow-parameter control
    /// guards.
    #[test]
    fn a_named_function_expression_route_is_clean() {
        let src = "\"use static\"\nexport default (function render(props: Record<never, never>) { return <p>hi</p> })\n";
        let d = crate::analyze::analyze(src, "src/routes/about.page.tsx");
        assert!(
            d.is_empty(),
            "the component's own name is not a binding in the artifact: {d:?}"
        );
        assert!(
            crate::codegen::emit_prerender_checked(src, "src/routes/about.page.tsx").is_ok(),
            "the artifact is still emitted"
        );
    }

    /// A nested function expression's own name is declared in *its* scope, not the
    /// route component's, so it cannot reach the emitted call site.
    #[test]
    fn a_named_nested_function_expression_is_clean() {
        let src = "\"use static\"\nexport default function P(props: Record<never, never>) {\n  const f = function _esc() {};\n  return <p>hi</p>\n}\n";
        let d = crate::analyze::analyze(src, "src/routes/about.page.tsx");
        assert!(
            d.is_empty(),
            "a nested function expression owns its name: {d:?}"
        );
    }

    /// A `var` declared through a destructuring pattern hoists exactly like a
    /// plain `var`: the pattern binds `_esc` in the component's scope. Nothing
    /// references it, so only the declaration rule can see it.
    #[test]
    fn an_unreferenced_destructured_var_is_ra014() {
        let src = "\"use static\"\nexport default function P(props: Record<never, never>) {\n  if (false) { var { _esc } = { _esc: 1 } }\n  return <p>hi</p>\n}\n";
        let d = crate::analyze::analyze(src, "src/routes/about.page.tsx");
        assert_eq!(d.len(), 1, "a destructured `var` hoists too: {d:?}");
        assert_eq!(d[0].code, Code::RA014);
        assert!(
            d[0].message.contains("`_esc`"),
            "the message names the reserved name: {}",
            d[0].message
        );
    }

    /// A `try`/`catch` body's `var` hoists out of the `catch` block in the same
    /// way a nested `if` does, so the same rule catches it.
    #[test]
    fn an_unreferenced_var_in_a_catch_block_is_ra014() {
        let src = "\"use static\"\nexport default function P(props: Record<never, never>) {\n  try {} catch (e) { var _esc = 1 }\n  return <p>hi</p>\n}\n";
        let d = crate::analyze::analyze(src, "src/routes/about.page.tsx");
        assert_eq!(d.len(), 1, "a `catch` block's `var` hoists: {d:?}");
        assert_eq!(d[0].code, Code::RA014);
    }

    /// A `catch` parameter is scoped to the `catch` block, not the route function,
    /// so it never reaches the emitted call site.
    #[test]
    fn a_catch_parameter_is_clean() {
        let src = "\"use static\"\nexport default function P(props: Record<never, never>) {\n  try {} catch (_esc) {}\n  return <p>hi</p>\n}\n";
        let d = crate::analyze::analyze(src, "src/routes/about.page.tsx");
        assert!(
            d.is_empty(),
            "a `catch` parameter stays in its block: {d:?}"
        );
    }

    /// A `for (let …)` head is block-scoped to the loop, so it is not the route
    /// function's binding. The control for the `for (var …)` case.
    #[test]
    fn an_unreferenced_for_let_is_clean() {
        let src = "\"use static\"\nexport default function P(props: Record<never, never>) {\n  for (let _esc = 1; false;) {}\n  return <p>hi</p>\n}\n";
        let d = crate::analyze::analyze(src, "src/routes/about.page.tsx");
        assert!(d.is_empty(), "a `let` is scoped to its loop: {d:?}");
    }

    /// A `type` alias or `interface` in the body binds a symbol in the route
    /// scope's type space, but `strip_types` erases it, so nothing is emitted
    /// under the name. The declaration rule must not refuse what the binder puts
    /// there for a type.
    #[test]
    fn an_unreferenced_type_alias_in_the_body_is_clean() {
        for declaration in ["type _esc = 1", "interface _esc { a: 1 }"] {
            let src = format!(
                "\"use static\"\nexport default function P(props: Record<never, never>) {{\n  {declaration};\n  return <p>hi</p>\n}}\n"
            );
            let d = crate::analyze::analyze(&src, "src/routes/about.page.tsx");
            assert!(
                d.is_empty(),
                "a type is erased before emission: {declaration}: {d:?}"
            );
        }
    }

    /// N2: the `scope_id` cell fails **closed**, and this pins that it is
    /// populated for both route shapes — the shape that made the old `None` arm a
    /// silent no-op. `None` would mean the walk never visited the component, which
    /// the `debug_assert!` in `check_emitted_names` turns into a refusal rather
    /// than a quiet pass.
    #[test]
    fn the_route_scope_cell_is_populated_for_both_route_shapes() {
        for src in [
            "\"use static\"\nexport default function P(props: Record<never, never>) { return <p>hi</p> }\n",
            "\"use static\"\nconst P = (props: Record<never, never>) => <p>hi</p>\nexport default P\n",
        ] {
            let alloc = oxc_allocator::Allocator::default();
            let program = crate::source::parse_tsx(&alloc, src).unwrap();
            let render = crate::analyze::jsx::route_render(program).unwrap();
            let semantic = oxc_semantic::SemanticBuilder::new().build(program).semantic;
            assert!(
                render.scope_id.get().is_some(),
                "the semantic build fills the route scope cell: {src}"
            );
            drop(semantic);
        }
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

    /// Run `module` as an ESM file under `node` and return what `render()` prints.
    ///
    /// The point of the clean-fixture controls below is the **artifact**, not the
    /// diagnostic list: a module can be clean and still throw, which is exactly how
    /// earlier rounds found their defects. `node` is the engine the build uses.
    /// `None` means `node` is not installed, in which case the caller still proves
    /// the artifact is a module (it was emitted and parses); shelling out is the
    /// extra evidence, not the only evidence.
    fn render_under_node(module: &str) -> Option<String> {
        use std::io::Write as _;
        let dir = std::env::temp_dir().join(format!(
            "runai-emitted-names-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("route.mjs");
        std::fs::File::create(&path)
            .and_then(|mut file| file.write_all(module.as_bytes()))
            .expect("write the emitted module");
        let output = std::process::Command::new("node")
            .arg("--input-type=module")
            .arg("-e")
            .arg(format!(
                "const {{ pathToFileURL }} = await import(\"node:url\"); \
                 const m = await import(pathToFileURL(\"{}\").href); \
                 process.stdout.write(m.render());",
                path.display()
            ))
            .output();
        let _ = std::fs::remove_dir_all(&dir);
        match output {
            Ok(output) if output.status.success() => {
                Some(String::from_utf8_lossy(&output.stdout).into_owned())
            }
            Ok(output) => panic!(
                "the emitted module must run under node: {module}\n{}",
                String::from_utf8_lossy(&output.stderr)
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => panic!("could not run node: {error}"),
        }
    }

    /// Assert that `module` renders `expected` — running it under `node` when it is
    /// installed, and otherwise asserting it parses as ESM, so the check never
    /// silently disappears.
    fn assert_renders(module: &str, expected: &str) {
        match render_under_node(module) {
            Some(html) => assert_eq!(html, expected, "the artifact renders: {module}"),
            None => {
                let alloc = oxc_allocator::Allocator::default();
                let ret =
                    oxc_parser::Parser::new(&alloc, module, oxc_span::SourceType::mjs()).parse();
                assert!(
                    ret.diagnostics.is_empty(),
                    "node is unavailable; the artifact must at least parse: {module}\n{:?}",
                    ret.diagnostics
                );
                eprintln!("node is not installed: asserted the artifact parses instead");
            }
        }
    }

    /// NEW-3: a body `type` alias beside a `var` of the same reserved name. Oxc's
    /// binder unions the nested `var`'s flags onto the type-alias symbol for the
    /// name, so the union reports a type — and skipping "any symbol whose flags
    /// look like a type" hid the **value** binding with it. The `var` alone is a
    /// real hoist into `render()`'s scope: the artifact carried `var _esc = 1`
    /// above `${_esc(1 + 1)}` and threw `TypeError: _esc is not a function`.
    #[test]
    fn a_var_beside_a_type_alias_of_the_same_name_is_ra014() {
        let src = "\"use static\"\nexport default function P(props: Record<never, never>) {\n  type _esc = 1; if (false) { var _esc = 1 }\n  return <p>{1 + 1}</p>\n}\n";
        let d = crate::analyze::analyze(src, "src/routes/about.page.tsx");
        assert_eq!(d.len(), 1, "the `var` is a value binding: {d:?}");
        assert_eq!(d[0].code, Code::RA014);
        assert!(
            d[0].message
                .contains("declared in the route component's own scope"),
            "the row is the declaration rule's: {}",
            d[0].message
        );
        assert_eq!(
            &src[d[0].span.0 as usize..d[0].span.1 as usize],
            "_esc",
            "the span is the binding to rename: {d:?}"
        );
        assert!(
            crate::codegen::emit_prerender_checked(src, "src/routes/about.page.tsx").is_err(),
            "the file is refused, so no module with `var _esc = 1` is written"
        );
    }

    /// The control for NEW-3: `type _esc = 1` **alone** is erased by
    /// `strip_types`, so nothing is emitted under the name. The ruling is "skip
    /// only when the symbol is type-only", not "skip when it is a type".
    #[test]
    fn a_body_type_alias_alone_is_clean() {
        let src = "\"use static\"\nexport default function P(props: Record<never, never>) {\n  type _esc = 1\n  return <p>{1 + 1}</p>\n}\n";
        let d = crate::analyze::analyze(src, "src/routes/about.page.tsx");
        assert!(d.is_empty(), "a type is erased before emission: {d:?}");
        let module = crate::codegen::emit_prerender_checked(src, "src/routes/about.page.tsx")
            .expect("a type alias alone emits");
        assert!(
            !module.contains("_esc = 1"),
            "the type alias is not written: {module}"
        );
        assert_renders(&module, "<p>2</p>");
    }

    /// The pin for the shape that already refused: a `type` alias and a top-level
    /// `var` of the same reserved name in the same scope. Oxc merges them into one
    /// symbol whose flags are both type and value; the declaration rule must see
    /// the value half. One row, because both walks land on the same identifier.
    #[test]
    fn a_top_level_var_beside_a_type_alias_of_the_same_name_is_ra014() {
        let src = "\"use static\"\nexport default function P(props: Record<never, never>) {\n  type _esc = 1; var _esc = 1\n  return <p>hi</p>\n}\n";
        let d = crate::analyze::analyze(src, "src/routes/about.page.tsx");
        assert_eq!(d.len(), 1, "one problem, one row: {d:?}");
        assert_eq!(d[0].code, Code::RA014);
        assert_eq!(
            &src[d[0].span.0 as usize..d[0].span.1 as usize],
            "_esc",
            "the span is the binding to rename: {d:?}"
        );
    }

    /// NEW-1: a **declarator-named** function-expression component. The binder
    /// puts the function expression's own name (`render`) in the function's own
    /// scope — which is the route scope — while `ComponentInfo::name_span` records
    /// the **declarator** (`Page`), so the `route.name_span` skip misses it. The
    /// emitter rebuilds the component from `RouteIr` and never writes the function
    /// expression's name, so this is a false positive on a legal page — and the
    /// artifact it refused was fine, which the `node` run below proves.
    #[test]
    fn a_declarator_named_function_expression_route_is_clean() {
        let src = "\"use static\"\nconst Page = function render(props: Record<never, never>) { return <p>hi</p> }\nexport default Page\n";
        let d = crate::analyze::analyze(src, "src/routes/about.page.tsx");
        assert!(
            d.is_empty(),
            "the function expression's own name is not a binding in the artifact: {d:?}"
        );
        let module = crate::codegen::emit_prerender_checked(src, "src/routes/about.page.tsx")
            .expect("the artifact is emitted");
        assert!(
            !module.contains("function render(props)"),
            "the function expression itself is not written: {module}"
        );
        assert_renders(&module, "<p>hi</p>");
    }

    /// The control for the ruling's "do not widen the skip by name": a function
    /// expression whose own name is an ordinary identifier stays clean.
    #[test]
    fn a_declarator_named_function_expression_with_an_ordinary_name_is_clean() {
        let src = "\"use static\"\nconst Page = function helper(props: Record<never, never>) { return <p>hi</p> }\nexport default Page\n";
        let d = crate::analyze::analyze(src, "src/routes/about.page.tsx");
        assert!(d.is_empty(), "an ordinary own name is clean: {d:?}");
    }

    /// The other half of NEW-1's ruling: `SymbolFlags::FunctionExpression` must
    /// not become a blanket skip. A genuine `var render` in a nested block of a
    /// block-body route is a real hoist into `render()`'s scope and keeps refusing.
    #[test]
    fn a_genuine_nested_var_named_render_is_still_ra014() {
        let src = "\"use static\"\nexport default function P(props: Record<never, never>) {\n  if (false) { var render = 1 }\n  return <p>hi</p>\n}\n";
        let d = crate::analyze::analyze(src, "src/routes/about.page.tsx");
        assert_eq!(d.len(), 1, "a hoisted `var render` is refused: {d:?}");
        assert_eq!(d[0].code, Code::RA014);
        assert!(
            d[0].message.contains("`render`"),
            "the message names the reserved name: {}",
            d[0].message
        );
    }

    /// NEW-2: a reserved binding **after** the JSX `return` is never emitted —
    /// [`crate::ir::emitted_body_statements`] stops at `return_span`, so the
    /// artifact has no shadow and the only defect would be a false positive.
    #[test]
    fn a_reserved_var_after_the_return_is_clean() {
        let src = "\"use static\"\nexport default function P(props: Record<never, never>) { return <p>hi</p>; var _esc = 1 }\n";
        let d = crate::analyze::analyze(src, "src/routes/about.page.tsx");
        assert!(
            d.is_empty(),
            "a binding after the return is never emitted: {d:?}"
        );
    }

    /// The same rule for a `const`, which is not hoisted anywhere the emitter
    /// writes: unreachable code after the `return` is not copied into `render()`.
    #[test]
    fn a_reserved_const_after_the_return_is_clean() {
        let src = "\"use static\"\nexport default function P(props: Record<never, never>) { return <p>hi</p>; const _attr = 1 }\n";
        let d = crate::analyze::analyze(src, "src/routes/about.page.tsx");
        assert!(
            d.is_empty(),
            "a binding after the return is never emitted: {d:?}"
        );
    }

    /// The control for NEW-2: move the same binding **before** the return and it is
    /// emitted into `render()`, so it refuses. Narrowing the rule to the emitted
    /// range must not narrow it past the range.
    #[test]
    fn a_reserved_var_before_the_return_is_ra014() {
        let src = "\"use static\"\nexport default function P(props: Record<never, never>) { var _esc = 1; return <p>hi</p> }\n";
        let d = crate::analyze::analyze(src, "src/routes/about.page.tsx");
        assert_eq!(d.len(), 1, "the emitted range includes this binding: {d:?}");
        assert_eq!(d[0].code, Code::RA014);
        assert_eq!(
            &src[d[0].span.0 as usize..d[0].span.1 as usize],
            "_esc",
            "the span is the binding to rename: {d:?}"
        );
    }

    // --- The island reserved set (Task 11, Step 4) ---------------------------

    /// An island module declares `signal`, `mount`, `template`, the helpers it
    /// uses, and its own `_el`/`_a`/`_t0`/`_c`. A route file that declares one of
    /// those names would shadow the import or the emitter local, so it is `RA014`.
    #[test]
    fn island_reserved_names_are_refused_for_each_shape() {
        for (src, name) in [
            // A module-level `const mount` collides with the import the module
            // writes (`Identifier 'mount' has already been declared`).
            (
                "\"use island\"\nconst mount = 1\nexport default function P(props: Record<never, never>) { return <p>{mount}</p> }\n",
                "mount",
            ),
            // A module-level `const signal` beside the `signal` import.
            (
                "\"use island\"\nimport { signal } from \"runai\"\nconst signal = 1\nexport default function P(props: Record<never, never>) { return <p>{signal}</p> }\n",
                "signal",
            ),
            // A body `const _a` collides with the emitter's marker list.
            (
                "\"use island\"\nexport default function P(props: Record<never, never>) { const _a = 1; return <p>{_a}</p> }\n",
                "_a",
            ),
        ] {
            let d = crate::analyze::analyze(src, "src/routes/index.page.tsx");
            assert!(
                d.iter().any(
                    |row| row.code == Code::RA014 && row.message.contains(&format!("`{name}`"))
                ),
                "{name}: the reserved name is refused and named: {d:?}"
            );
        }
    }

    /// The same names are legal in a **static** route except the prerender set:
    /// a static module has no import and no `_el`, so `mount`/`signal`/`_a` are
    /// ordinary names there.
    #[test]
    fn island_reserved_names_are_legal_in_a_static_route() {
        for src in [
            "\"use static\"\nconst mount = 1\nexport default function P(props: Record<never, never>) { return <p>{mount}</p> }\n",
            "\"use static\"\nconst signal = 1\nexport default function P(props: Record<never, never>) { return <p>{signal}</p> }\n",
            "\"use static\"\nexport default function P(props: Record<never, never>) { const _a = 1; return <p>{_a}</p> }\n",
        ] {
            assert!(
                crate::analyze::analyze(src, "src/routes/about.page.tsx").is_empty(),
                "an island name is legal in a static route: {src}"
            );
        }
    }

    /// A name the route **never imports** is not reserved — refusing `derived` on a
    /// page that does not import it would be a false positive on a legal route. The
    /// reserved set is data (the merged import list), not a fixed superset.
    #[test]
    fn a_name_the_route_does_not_import_is_not_refused() {
        // `derived` is not imported, so a body local of that name is legal.
        let src = "\"use island\"\nexport default function P(props: Record<never, never>) { const derived = 1; return <p>{derived}</p> }\n";
        assert!(
            crate::analyze::analyze(src, "src/routes/index.page.tsx").is_empty(),
            "a name the module does not import is not reserved"
        );
    }

    /// The positive control for the test above: import `derived` and a body local of
    /// the same name **is** refused, because the module now writes that import.
    #[test]
    fn an_imported_name_is_refused_even_when_only_the_import_uses_it() {
        let src = "\"use island\"\nimport { derived } from \"runai\"\nexport default function P(props: Record<never, never>) { const derived = 1; return <p>{derived}</p> }\n";
        let d = crate::analyze::analyze(src, "src/routes/index.page.tsx");
        assert_eq!(d[0].code, Code::RA014, "{d:?}");
        assert!(
            d.iter().any(|row| row.message.contains("`derived`")),
            "the message names it: {d:?}"
        );
    }
}
