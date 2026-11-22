//! `RA014` for a value the generated module genuinely cannot see.
//!
//! `analyze` refuses a static route's *constructs* (a signal, a handler) with
//! `RA010`, and `check_jsx` refuses JSX the build cannot turn into JavaScript.
//! This module covers the remaining way the markup can name something the
//! emitted module does not have.
//!
//! # What is reachable, and why the list is short
//!
//! The emitters write [`RouteIr::module_statements`] above the component,
//! [`RouteIr::statements`] inside it, and the returned markup as its template, so
//! a name resolves if it is declared in any of those. Design §7.6 expects
//! module-level state — "module-scope signals are per-island too, since each
//! island is its own module instance" — so a module-level `const title = "About"`
//! referenced by `<h1>{title}</h1>` is **legal**, and round 3 of this task removed
//! the earlier refusal that called it unreachable.
//!
//! **Three** scopes are walked, because the emitters write three: the returned
//! markup, the component body's statements (the range before the JSX `return`,
//! which both emitters copy verbatim), and the file's **module level**. A module
//! statement is emitted above the component and a body statement inside it, so a
//! name either one references has to exist in the emitted module too — and the
//! prerender module has no imports and does not declare the component at all.
//!
//! Exactly four things stay refused:
//!
//! 1. **a `"runai"` name in a `"use static"` route**, whether the markup, a
//!    body statement, or a module-level statement names it. The prerender module
//!    contains no imports at all, so the name is not evaluable there. The repair
//!    is the directive, `"use island"` — the same escape hatch `RA010` points at,
//!    because the page genuinely needs a runtime. A reference that *is* a
//!    primitive call is left to `RA010`, so one expression never gets two
//!    diagnostics.
//! 2. **the route's own parameter, wherever the emitted module cannot declare
//!    it.** In the markup or a component-body statement of a `"use static"`
//!    route, `render()` takes no arguments, so the value is `undefined` at build
//!    time. At **module scope it is refused in both modes**, because no emitter
//!    declares the parameter there at all — the emitted component declares it and
//!    the module does not — so `const x = props` is a `ReferenceError`. The repair
//!    is a literal value, not "move it": a body statement is already in the body,
//!    and a module statement has no `props` to move in the first place.
//! 3. **the route component's own name in a `"use static"` route.** The
//!    prerender module contains `render()` and nothing else — the component is
//!    not declared there — so `const x = Page` or `{Page}` is a `ReferenceError`
//!    at build time. The repair is the directive again: an island module declares
//!    the component.
//! 4. **a type-only reference.** Never reported, in fact: `strip_types` erases
//!    it before anything is emitted, so `("a" as Kind)` where `Kind` is a type
//!    alias is legal TypeScript that compiles and runs today. Oxc's visitor
//!    funnels `as`/type-annotation/generic-argument references into the same
//!    `visit_identifier_reference` as value references, so the walk has to ask
//!    the semantic pass which ones are types. See [`References`].
//!
//! An island module imports its `"runai"` names and declares `function
//! <Name>(props)`, so all of those are reachable there — that is the
//! mode-dependent half of this rule. The module scope is the exception: no
//! island declares `props` above the component either, so a parameter name there
//! is refused in both modes.
//!
//! Everything else at module scope is emitted by [`RouteIr::module_statements`]
//! and is therefore reachable, which is the round-3 ruling this module follows.
//!
//! [`RouteIr::module_statements`]: crate::ir::RouteIr::module_statements
//!
//! # The fourth region: the parameter list
//!
//! The parameter list is a region the emitters **rebuild** rather than copy:
//! both write the component as `function <Name>(props)`. A default, a destructured
//! pattern, a rest parameter, or a different name is therefore dropped, and
//! everything it binds does not exist in the artifact. That is a **shape** rule,
//! not a reachability one, so it fires in both modes: `props: P = owner` in an
//! island is a real name (the island's import writes it) but a dropped default,
//! so a reachability walk would be silent while the emitted component differs
//! from the source. [`emitted_parameter_list`] owns it.
//!
//! # The third region: the component body
//!
//! The body is a third emitted region with a reachability question of its own.
//! Both emitters copy the statements *before* the JSX `return` into the generated
//! component — the range [`crate::ir::emitted_body_statements`] returns, and the
//! same range [`RouteIr::statements`] slices to text — so a name those statements
//! read has to exist in the emitted module exactly as a name the markup reads
//! does. Before this walk existed the body was covered by neither other walk: the
//! markup walk starts at the returned root, and the module walk skips the route
//! component's own declaration.
//!
//! A body walk judges a reference with body locals **allowed**, using the same
//! `contains(body, …)` test the markup walk uses. A local declared in the body is
//! copied into the emitted component, so it is reachable; a reference to it from
//! *outside* the body region is not, because a module statement or the markup
//! cannot see the component's locals. Ordering and temporal-dead-zone inside the
//! emitted function are JavaScript's rules, not this rule's: this walk asks only
//! "does the name exist in the emitted module", which is the same question it asks
//! at the other two scopes.
//!
//! That arm is currently **silence** either way (see [`EmittedScope::judge`]): the
//! fall-through is permissive, so a body local produces no diagnostic whether the
//! arm fires or not. It is kept as the statement of the rule, not as tested
//! behaviour, and the tests that look like they pin it are recording a false
//! positive that would appear if the fall-through ever stopped being permissive.
//!
//! # Scope: only a page, and only one that has a mode
//!
//! The rule is about a **page's** emitted module. A component module
//! (`_shell.tsx`, or any `.tsx` with no default export) is not emitted by this
//! milestone, so nothing there is unreachable — the same boundary `check_jsx`
//! draws for module-level JSX. A page with no directive is not checked either:
//! `RA012` already stops the build, and guessing the mode would make this
//! rule's own claim uncertain.
//!
//! # Identity, not names
//!
//! Resolution is `oxc_semantic`'s, so a reference is judged by the binding it
//! resolves to. `{title}` in a block that redeclares `title` is the block's
//! binding, not the module-level one, and a name comparison could not tell.

use oxc_ast::ast::{IdentifierReference, Program};
use oxc_ast_visit::Visit;
use oxc_semantic::{Scoping, SemanticBuilder, SymbolId};
use oxc_span::GetSpan;

use crate::analyze::component::{ComponentInfo, find_route_component};
use crate::analyze::jsx::route_render;
use crate::analyze::signals::{RunaiImport, SignalBindings, runai_imports};
use crate::diagnostic::{Code, Diagnostic};
use crate::ir::emitted_module_parts;
use crate::source::Mode;

/// `RA014` for every value the generated module cannot name: one walk of the
/// returned markup, one of the component body's emitted statements, and one of
/// the file's module level.
///
/// `bindings` is [`crate::analyze::signals::find_signal_bindings`]'s answer for
/// the same program; only its [`SignalBindings::primitive_calls`] spans are read
/// here, so a primitive call is left to `RA010` rather than reported twice.
///
/// Returns an empty list for a file with no route component: nothing is emitted
/// from such a file. See the module docs for the rule and why it lives in
/// `analyze` rather than in the emitters.
pub fn check_reachable_expressions(
    mode: Mode,
    program: &Program<'_>,
    bindings: &SignalBindings,
) -> Vec<Diagnostic> {
    let Some(render) = route_render(program) else {
        return Vec::new();
    };
    let Some(route) = find_route_component(program) else {
        // `route_render` found a render, so there is a route, but a defensive
        // early return is cheaper than an `unwrap` in a diagnostic pass.
        return Vec::new();
    };

    // A second semantic build, for a page only: the reference resolution this
    // rule needs is not part of `SignalBindings`. Files without a route never
    // reach this line.
    let semantic = SemanticBuilder::new().build(program).semantic;
    let scoping = semantic.scoping();

    // Every value binding a `"runai"` import declares. `named` is what the
    // client emitter can write; a default or namespace binding is refused by the
    // import rule in `entry.rs` in *every* mode, so this rule stays silent about
    // it — one root cause is one row.
    let runai = runai_imports(program);

    let mut diagnostics = Vec::new();
    let emitter = EmittedScope {
        scoping,
        route: &route,
        runai: &runai,
        bindings,
        // The names the route's own parameter list binds, so a reference to one
        // of them is refused wherever it appears — including at module scope,
        // where the semantic pass cannot resolve it to the parameter (a module
        // statement is outside the component's scope in JavaScript).
        parameter_bindings: parameter_bindings(render.params),
    };

    // (0) The parameter list is a fourth region, and the only one the emitters
    // **rebuild** rather than copy: both write `function <Name>(props)`. A
    // default, a destructuring pattern, a rest parameter, or a different name is
    // therefore dropped, and so is everything it binds. This is a shape rule
    // rather than a reachability one — see `emitted_parameter_list`.
    if let Some(diagnostic) = emitted_parameter_list(render.params) {
        diagnostics.push(diagnostic);
    }

    // (1) The returned markup. Anything declared inside the component body, or
    // inside an arrow parameter in the markup, is copied into the emitted
    // function; an island also declares the route parameter and imports the
    // `"runai"` names the tree uses.
    let body = render
        .body
        .map_or((render.root.span().start, render.root.span().end), |body| {
            (body.span.start, body.span.end)
        });
    let mut markup = References {
        scoping,
        references: Vec::new(),
    };
    markup.visit_expression(render.root);
    for reference in markup.references {
        emitter.judge(
            mode,
            &reference,
            // `declaration` is the span of the identifier that declares the
            // symbol, so a body local and an arrow parameter are both "inside".
            |declaration| contains(body, declaration),
            &mut diagnostics,
        );
    }

    // (2) The component body's statements — the range before the JSX `return`,
    // which both emitters copy into the generated component. A body local is
    // therefore reachable here: the same `contains(body, …)` test the markup walk
    // uses, because the statement that declares it is inside the copied range.
    //
    // The range is [`crate::ir::emitted_body_statements`], the one function that
    // decides which body statements are emitted, so this walk cannot check a
    // statement the module omits (after the `return`) nor skip one it carries.
    if let (Some(body), Some(return_span)) = (render.body, render.return_span) {
        for statement in crate::ir::emitted_body_statements(body, return_span) {
            let mut declared = References {
                scoping,
                references: Vec::new(),
            };
            declared.visit_statement(statement);
            for reference in declared.references {
                // A later statement's `const` is still inside the body region, so
                // the test is the markup walk's: the declaration is copied into
                // the emitted function, which is what makes the name exist. Order
                // within the emitted function is JavaScript's business, not this
                // rule's.
                emitter.judge(
                    mode,
                    &reference,
                    |declaration| contains((body.span.start, body.span.end), declaration),
                    &mut diagnostics,
                );
            }
        }
    }

    // (3) The module level. Every statement `RouteIr::module_statements` will
    // carry is evaluated in the emitted module, so a name it uses has the same
    // reachability question as the markup's.
    //
    // The pieces walked are exactly the pieces the emitters slice:
    // [`crate::ir::emitted_module_parts`] is the one function that decides
    // "emitted", so this walk cannot check a statement the module omits, nor
    // skip one it carries.
    for part in emitted_module_parts(program, &route) {
        let mut module = References {
            scoping,
            references: Vec::new(),
        };
        part.visit(&mut module);
        for reference in module.references {
            // A module statement cannot declare the route body's locals, so
            // nothing here is one; the declarations that *are* reachable are the
            // other emitted module statements, which is the fall-through.
            emitter.judge(mode, &reference, |_| false, &mut diagnostics);
        }
    }

    diagnostics.sort_by_key(|diagnostic| diagnostic.span.0);
    diagnostics
}

/// The context both walks judge a reference against: resolution, the route's own
/// spans, the `"runai"` bindings, and the primitive calls already reported.
struct EmittedScope<'s, 'a> {
    scoping: &'s Scoping,
    route: &'s ComponentInfo<'a>,
    runai: &'s [RunaiImport],
    bindings: &'s SignalBindings,
    /// Every `(name, declaration span)` the route's parameter list binds.
    ///
    /// Needed because the semantic pass does **not** resolve a module-level
    /// reference to the component's parameter: JavaScript scoping makes those
    /// different regions, and `oxc_semantic` is right to say so. The rule is not
    /// JavaScript's scoping rule, though: the parameter is not in scope at module
    /// level *because the emitted module never has it*, so the reference is a
    /// `ReferenceError` and is refused. Matching on the name is what lets one
    /// judge answer for all three regions, because an unresolved reference has no
    /// symbol to compare.
    parameter_bindings: Vec<String>,
}

impl EmittedScope<'_, '_> {
    /// Append `RA014` when `reference` names something the emitted module does
    /// not have.
    ///
    /// `inside_emitted_function` answers "is this declaration copied into the
    /// emitted component?" — the markup walk and the body walk pass the route
    /// body's span, the module walk answers `false` because no module statement
    /// declares it.
    ///
    /// # This predicate is not currently load-bearing, and that is recorded
    ///
    /// The last arm is **silence**, so a reference to a body local ends exactly
    /// the same whether this predicate returns `true` or the code falls through:
    /// it is not a `"runai"` name, not the parameter, and not the component, so
    /// nothing is pushed. Deleting the arm (the `contains(body, …)` call) leaves
    /// every test green — verified by mutation, twice, on both walks.
    ///
    /// It is kept because it states the rule the walk implements ("a body local
    /// is emitted; a module value is emitted; a `"runai"` name is not"), and
    /// because the arm becomes load-bearing the moment the fall-through stops
    /// being permissive. The day a later task refuses something at this
    /// fall-through — say a name that is neither a module statement nor a body
    /// local — the arm is what keeps `const title = …; return <h1>{title}</h1>`
    /// from becoming a false positive. Recorded rather than deleted so the next
    /// reader does not re-derive it, and so nobody mistakes the tests for proof
    /// that the arm fires.
    fn judge(
        &self,
        mode: Mode,
        reference: &Reference,
        inside_emitted_function: impl Fn((u32, u32)) -> bool,
        diagnostics: &mut Vec<Diagnostic>,
    ) {
        // Unresolved: a language built-in, which every module has — unless the
        // name is one the route's own parameter list binds. That case is why this
        // arm cannot simply return: a module-level `const x = props` resolves to
        // nothing at all (JavaScript scoping), but the emitted module has no
        // `props` either, so the statement is a `ReferenceError` at load.
        let Some(symbol) = reference.symbol else {
            if self.names_a_parameter(&reference.name) {
                diagnostics.push(route_parameter(
                    reference.span,
                    &reference.name,
                    ParameterSite::ModuleScope,
                ));
            }
            return;
        };
        let declaration = declaration_of(self.scoping, symbol);

        // (1) A body local: its statement is copied into the component.
        if inside_emitted_function(declaration) {
            return;
        }

        // (2) An island module imports these names and declares the parameter.
        // It also declares the component, so the route's own name is reachable.
        if mode == Mode::Island
            && (is_named_runai(self.runai, symbol)
                || contains(self.route.params_span, declaration)
                || self.route.name_span == declaration)
        {
            return;
        }

        // A primitive call in a static route is already `RA010` at this span; a
        // second diagnostic at the callee would describe one expression twice.
        if is_primitive_callee(reference.span, self.bindings) {
            return;
        }

        if is_runai(self.runai, symbol) {
            // A **named** `"runai"` binding on a static route is unreachable:
            // the prerender module has no imports at all, so the repair is the
            // directive.
            //
            // A default or namespace binding is refused by `entry.rs`'s import
            // rule instead, in *every* mode, because neither emitter writes that
            // form. Reporting it at each use here would put one root cause on the
            // page several times — the same duplication the module-JSX const used
            // to produce.
            if mode == Mode::Static && is_named_runai(self.runai, symbol) {
                diagnostics.push(runtime_import(reference.span, &reference.name));
            }
            return;
        }
        if contains(self.route.params_span, declaration) {
            diagnostics.push(route_parameter(
                reference.span,
                &reference.name,
                ParameterSite::Component,
            ));
            return;
        }
        if self.route.name_span == declaration {
            // The prerender module declares `render()` and nothing else, so the
            // component is not a name there. An island module declares it.
            if mode == Mode::Static {
                diagnostics.push(route_component(reference.span, &reference.name));
            }
        }

        // Everything else — a module-level value, a class, an enum, a function —
        // is emitted by `RouteIr::module_statements`, so it is reachable.
    }

    /// Whether `name` is one the route's own parameter list binds.
    ///
    /// Only used for a reference the semantic pass could **not** resolve: a name
    /// that resolves to a binding is judged by symbol, and a module-level name
    /// that shadows a parameter (`const props = …`) resolves to its own binding
    /// and is therefore never refused. That is the control in
    /// `a_parameter_name_at_module_scope_is_refused_in_both_modes`: matching by
    /// name alone would refuse the file's own `props`.
    fn names_a_parameter(&self, name: &str) -> bool {
        self.parameter_bindings.iter().any(|bound| bound == name)
    }
}

/// One identifier reference in the returned markup, a component-body statement,
/// or a module-level statement.
pub(crate) struct Reference {
    pub(crate) name: String,
    pub(crate) span: (u32, u32),
    /// `None` for an unresolved name, i.e. a built-in such as `Math`.
    pub(crate) symbol: Option<SymbolId>,
}

/// Every value identifier reference `walk` reaches, with type-only references
/// filtered out.
///
/// The one reference walk in the crate: [`check_reachable_expressions`] judges
/// these against the emitted module's bindings, and
/// [`crate::analyze::emitted_names`] judges a subset of them against the names
/// the emitter declares. Sharing it means "a type-only reference is not a value"
/// has one definition, so the two rules cannot disagree about whether `("a" as
/// Kind)` mentions `Kind`.
///
/// `walk` decides the region — the returned root, one body statement, one module
/// statement — so a caller cannot accidentally walk a region the emitters omit.
pub(crate) fn references(
    scoping: &Scoping,
    walk: impl FnOnce(&mut References<'_>),
) -> Vec<Reference> {
    let mut visitor = References {
        scoping,
        references: Vec::new(),
    };
    walk(&mut visitor);
    visitor.references
}

/// Collects every **value** identifier reference under whatever node the caller
/// visits: the returned root for the markup walk, one body statement for the body
/// walk, or one module-level statement for the module-level walk.
///
/// # Type positions are not references
///
/// `oxc_ast_visit` sends `walk_ts_as_expression`, `walk_ts_type_reference`, and
/// `walk_ts_type_name` through `walk_identifier_reference`, so this visitor sees
/// `Kind` in `("a" as Kind)`, `Entry` in `(v: Entry) => v.name`, and a type
/// argument in `foo<Kind>(1)`. None of them survives `strip_types`, and none is
/// emitted, so treating one as a missing value would refuse a file that compiles
/// and runs today — the false positive this task's review proved by execution.
///
/// The predicate is [`ReferenceFlags::is_type_only`], i.e. "flagged `Type` and
/// nothing else". That is deliberately **not** `is_type()`: a reference flagged
/// `Type | Value` (a `typeof` query that also reads the value) is a real use and
/// must still be judged. `oxc_semantic`'s own
/// `Scoping::delete_typescript_bindings` deletes exactly this set, so "what the
/// semantic pass considers a type" has one definition in the crate.
///
/// [`ReferenceFlags::is_type_only`]: oxc_syntax::reference::ReferenceFlags::is_type_only
pub(crate) struct References<'s> {
    scoping: &'s Scoping,
    references: Vec<Reference>,
}

impl<'a> Visit<'a> for References<'_> {
    fn visit_identifier_reference(&mut self, reference: &IdentifierReference<'a>) {
        // A reference the semantic pass could not resolve has no id, and is
        // handled below as an unresolved name — never as a type.
        let reference_id = reference.reference_id.get();
        if reference_id.is_some_and(|id| self.scoping.get_reference(id).flags().is_type_only()) {
            return;
        }
        self.references.push(Reference {
            name: reference.name.as_str().to_string(),
            span: (reference.span.start, reference.span.end),
            symbol: reference_id.and_then(|id| self.scoping.get_reference(id).symbol_id()),
        });
        oxc_ast_visit::walk::walk_identifier_reference(self, reference);
    }
}

/// Whether `symbol` is any value binding of `"runai"`, named or not.
fn is_runai(imports: &[RunaiImport], symbol: SymbolId) -> bool {
    imports.iter().any(|import| import.symbol == symbol)
}

/// Whether `symbol` is a **named** `"runai"` binding — the form the client
/// emitter writes, and therefore the only one reachable in an island module.
fn is_named_runai(imports: &[RunaiImport], symbol: SymbolId) -> bool {
    imports
        .iter()
        .any(|import| import.symbol == symbol && import.named)
}

/// The `(start, end)` of the identifier that declares `symbol`.
fn declaration_of(scoping: &Scoping, symbol: SymbolId) -> (u32, u32) {
    let span = scoping.symbol_span(symbol);
    (span.start, span.end)
}

/// Whether `outer` encloses `inner`.
fn contains(outer: (u32, u32), inner: (u32, u32)) -> bool {
    outer.0 <= inner.0 && inner.1 <= outer.1
}

/// Whether `span` is the callee of a recorded primitive call.
///
/// [`SignalBindings::primitive_calls`] holds whole call spans, which start at
/// their callee, so containment with a shared start identifies the callee. The
/// spans come from a different semantic build than [`Reference::symbol`], which
/// is why this compares positions rather than symbol ids.
fn is_primitive_callee(span: (u32, u32), bindings: &SignalBindings) -> bool {
    bindings
        .primitive_calls
        .iter()
        .any(|&(start, end)| span.0 == start && span.1 <= end)
}

/// The names the route's parameter list binds, in source order.
///
/// A destructured parameter (`{ id }`, `[a, b]`) binds several names, and a
/// pattern's default (`{ a = 1 }`) binds only its left side. `props` and a rest
/// parameter's binding (`...props`) are the same question with a different shape,
/// so all four go through [`BindingPattern::get_binding_identifiers`] rather than
/// being special-cased here.
fn parameter_bindings(params: &oxc_ast::ast::FormalParameters<'_>) -> Vec<String> {
    let mut names = Vec::new();
    for parameter in &params.items {
        for identifier in parameter.pattern.get_binding_identifiers() {
            names.push(identifier.name.as_str().to_string());
        }
    }
    if let Some(rest) = &params.rest {
        for identifier in rest.rest.argument.get_binding_identifiers() {
            names.push(identifier.name.as_str().to_string());
        }
    }
    names
}

/// `RA014` for a route signature both emitters would **drop**, or `None` when
/// they can rebuild it.
///
/// The emitters do not copy the source's parameter list. Both write the
/// component from [`RouteIr::name`] and [`RouteIr::statements`], so the emitted
/// signature is always `function <Name>(props)` — one identifier, named `props`.
/// Anything else in the source is therefore not emitted:
///
/// | Written | Emitted | Why it is refused |
/// |---|---|---|
/// | `props: P` | `props` | identical at runtime, so legal |
/// | `ctx: P` | `props` | the body reads `ctx`, which the artifact does not have |
/// | `{ id }: P` | `props` | the destructuring is gone, so `id` has no binding |
/// | `...props: P[]` | `props` | the rest collection is gone |
/// | `props: P = <expr>` | `props` | the default is gone, and `<expr>` is never evaluated |
///
/// **This is a shape rule, not a reachability rule**, and that is why it fires in
/// both modes. The case that proved it — R2 — was `props: P = owner` in an
/// *island*: `owner` is a real name there (the island emitter's own import
/// writes it), so a reachability walk would say the reference is fine. It is the
/// *parameter* that disappears, so the defect is the dropped signature rather
/// than the reference, and no mode makes a default evaluable: nothing calls the
/// emitted component with arguments.
///
/// **Zero parameters and two or more are not this rule's.** `RA006` asks the
/// first one for a parameter and `RA011` refuses the second as a route shape, so
/// the same file would get two rows for one problem. Only the exactly-one
/// parameter case — where `RA006` and `RA011` are both satisfied and the emitter
/// still rewrites the signature — reaches the checks below.
fn emitted_parameter_list(params: &oxc_ast::ast::FormalParameters<'_>) -> Option<Diagnostic> {
    // `RA006` demands a type annotation on the single parameter, and its repair
    // replaces the whole parameter list — which removes a default, a
    // destructuring pattern, and a rename in the same edit. So an unannotated
    // parameter is that rule's row, and refusing its shape here would put two
    // codes on one fix.
    let annotated = params
        .items
        .iter()
        .any(|parameter| parameter.type_annotation.is_some())
        || params
            .rest
            .as_ref()
            .is_some_and(|rest| rest.type_annotation.is_some());
    if !annotated {
        return None;
    }

    let only = match (params.items.as_slice(), &params.rest) {
        // Exactly one parameter and no rest: the only shape the emitter rewrites
        // without `RA006` or `RA011` already owning the file.
        ([only], None) => only,
        // A rest parameter with no items is one parameter by `RA006`'s count and
        // `RA011`'s, so it is this rule's: the rest collection is dropped.
        ([], Some(_)) => {
            return Some(signature_refusal(
                params,
                "a rest parameter is dropped, because the emitted component takes one `props` value",
            ));
        }
        // Zero parameters (`RA006`) and two or more (`RA011`) are already
        // reported; a second row would describe one problem twice.
        _ => return None,
    };

    // A destructuring pattern binds names the emitted `props` parameter does not
    // declare, whatever those names are. Checked before the name test because
    // `{ props }` would otherwise look like the plain identifier.
    if matches!(
        only.pattern,
        oxc_ast::ast::BindingPattern::ObjectPattern(_)
            | oxc_ast::ast::BindingPattern::ArrayPattern(_)
    ) {
        return Some(signature_refusal(
            params,
            "a destructuring pattern is dropped, and the names it binds are not declared in the \
             emitted component",
        ));
    }

    match only.pattern.get_identifier_name() {
        Some(name) if name == "props" => {}
        Some(_) => {
            return Some(signature_refusal(
                params,
                "the emitted component names its parameter `props`, so a differently named \
                 parameter is not declared in it",
            ));
        }
        None => return None,
    }

    if only.initializer.is_some() {
        return Some(signature_refusal(
            params,
            "the default value is dropped, because nothing calls the emitted component with \
             arguments",
        ));
    }
    None
}

/// `RA014` for a route signature the emitters cannot rebuild, pointing at the
/// parameter list and naming the signature to write.
///
/// The repair is a literal signature rather than "move it into the body", because
/// the parameter *is* the thing to rewrite: applying this text verbatim produces
/// the one shape both emitters write.
///
/// `reason` is a complete clause, so the message composes without a conjunction
/// that would double when the clause already carries one.
fn signature_refusal(params: &oxc_ast::ast::FormalParameters<'_>, reason: &str) -> Diagnostic {
    Diagnostic::new(
        Code::RA014,
        (params.span.start, params.span.end),
        format!(
            "this route signature is not emitted: the generated component is always written as \
             `function <Name>(props)`, so {reason}. Write the parameter as `(props: <Type>)` and \
             read the fields from `props` in the body."
        ),
    )
}

/// `RA014`: a static page names a `"runai"` import, in its markup, in a body
/// statement, or at module scope.
///
/// The prerender module contains no imports, so the repair cannot be "move it":
/// taking the value into the body does not import it. The page needs the
/// runtime, and the escape hatch is the directive `RA010` also points at.
fn runtime_import(span: (u32, u32), name: &str) -> Diagnostic {
    Diagnostic::new(
        Code::RA014,
        span,
        format!(
            "`{name}` is imported from `\"runai\"`, but a `\"use static\"` route is evaluated in \
             a module with no imports. Change line 1 to \"use island\" to ship the runtime this \
             value comes from, or remove the reference."
        ),
    )
}

/// `RA014`: a page names the route's own parameter where the emitted module has
/// no such binding.
///
/// Two regions, two reasons, one repair. In the markup or a body statement the
/// emitted component's own parameter is the one that applies: a static route's
/// `render()` takes no arguments, so the value is `undefined`; an island declares
/// `function <Name>(props)`, so the same reference is legal there. At module scope
/// no emitter declares the parameter in **either** mode, so the statement is a
/// `ReferenceError` at load.
///
/// The repair is a literal value, not "write the value in the component body":
/// the reference may already *be* in the body, so that sentence is circular and
/// would leave an agent nowhere to go. Only the static markup/body case gets the
/// directive as an alternative, because that is the one place it is the genuine
/// escape hatch.
fn route_parameter(span: (u32, u32), name: &str, site: ParameterSite) -> Diagnostic {
    let message = match site {
        ParameterSite::Component => format!(
            "`{name}` is the route's parameter, but a route is given no props and the \
             prerendered module takes no arguments, so `{name}` would be undefined at build \
             time. Replace the reference with a literal value, or change line 1 to \
             \"use island\" to declare the parameter."
        ),
        ParameterSite::ModuleScope => format!(
            "`{name}` is the route's parameter, and a module-level statement is evaluated \
             outside the component, where no `{name}` exists — the emitted component declares \
             it and the module does not. Replace the reference with a literal value or a \
             binding this file declares at module scope."
        ),
    };
    Diagnostic::new(Code::RA014, span, message)
}

/// Where a reference to the route's parameter was found, which decides the reason
/// [`route_parameter`] gives.
///
/// The rule is one code and one repair; only the explanation differs, because the
/// two regions fail for different reasons and a single sentence would be false in
/// one of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ParameterSite {
    /// The returned markup or a component-body statement: the emitted component's
    /// own parameter is the missing binding, and only a static route is refused.
    Component,
    /// A module-level statement: no emitter declares the parameter at module
    /// scope, so both modes are a `ReferenceError`.
    ModuleScope,
}

/// `RA014`: a static page names its own component, in its markup, in a body
/// statement, or at module scope.
///
/// The prerender module exports `render()` and declares nothing else — the
/// component is rebuilt inside that function — so `Page` is not a binding there.
/// An island module does declare `function <Name>(props)`, so the repair is the
/// directive, exactly as for a `"runai"` name.
fn route_component(span: (u32, u32), name: &str) -> Diagnostic {
    Diagnostic::new(
        Code::RA014,
        span,
        format!(
            "`{name}` is the route's own component, but a `\"use static\"` route is emitted as \
             a `render()` function and the component is not declared in it, so `{name}` would be \
             undefined at build time. Change line 1 to \"use island\" to keep the component, or \
             remove the reference."
        ),
    )
}

#[cfg(test)]
mod tests {
    use oxc_allocator::Allocator;

    use super::*;
    use crate::source::parse_tsx;

    fn diagnostics(src: &str, mode: Mode) -> Vec<Diagnostic> {
        let alloc = Allocator::default();
        let program = parse_tsx(&alloc, src).unwrap();
        let bindings = crate::analyze::signals::find_signal_bindings(program);
        check_reachable_expressions(mode, program, &bindings)
    }

    fn codes(src: &str, mode: Mode) -> Vec<Code> {
        diagnostics(src, mode)
            .iter()
            .map(|diagnostic| diagnostic.code)
            .collect()
    }

    /// The codes of an already-computed list, so a test that needs the messages
    /// as well does not run the analysis twice.
    fn codes_of(diagnostics: &[Diagnostic]) -> Vec<Code> {
        diagnostics
            .iter()
            .map(|diagnostic| diagnostic.code)
            .collect()
    }

    /// A module-level value is **reachable** now: `RouteIr::module_statements`
    /// emits it above the component, so the markup can name it. This is the
    /// reversal of the round-2 rule, and both modes are asserted because the
    /// old refusal fired in both.
    #[test]
    fn a_module_level_value_is_reachable_in_both_modes() {
        let src = "\"use static\"\nconst title = \"About\"\nexport default function Page(props: Record<never, never>) { return <h1>{title}</h1> }\n";
        for mode in [Mode::Static, Mode::Island] {
            assert!(codes(src, mode).is_empty(), "{mode:?}");
        }
    }

    /// A route parameter and a `"runai"` name are the only things a page's
    /// markup may not name, and only on the static side.
    #[test]
    fn a_body_local_and_a_builtin_are_reachable() {
        for src in [
            "\"use static\"\nexport default function P(props: Record<never, never>) { const title = \"Hi\"; return <h1>{title}</h1> }\n",
            "\"use static\"\nexport default function P(props: Record<never, never>) { return <p>{Math.max(1, 2)}</p> }\n",
            "\"use static\"\nexport default function P(props: Record<never, never>) { const n = 1; return <p title={String(n)}>x</p> }\n",
            // A parameter of an arrow nested in the markup is not the route's own.
            "\"use static\"\nexport default function P(props: Record<never, never>) { return <p>{[1].map((n) => n + 1)}</p> }\n",
        ] {
            assert!(codes(src, Mode::Static).is_empty(), "{src}");
        }
    }

    /// References resolve by symbol, so a block that redeclares the name is a
    /// different binding from the module-level one.
    #[test]
    fn shadowing_is_resolved_by_symbol() {
        let src = "\"use static\"\nconst title = \"A\"\nexport default function P(props: Record<never, never>) { const title = \"B\"; return <h1>{title}</h1> }\n";
        assert!(codes(src, Mode::Static).is_empty(), "{src}");
    }

    /// A static page's route parameter is undefined in the prerender module.
    #[test]
    fn a_static_page_cannot_reference_its_route_parameter() {
        let src = "\"use static\"\nexport default function P(props: Record<never, never>) { return <p>{props}</p> }\n";
        let diagnostics = diagnostics(src, Mode::Static);
        assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
        assert!(
            diagnostics[0].message.contains("route's parameter"),
            "{diagnostics:?}"
        );
    }

    /// An island module declares the parameter, so the same reference is legal
    /// there. This is one of the two mode-dependent branches, and it must not
    /// drift into the static side.
    #[test]
    fn an_island_page_may_reference_its_route_parameter() {
        let src = "\"use island\"\nexport default function P(props: Record<never, never>) { return <p>{props}</p> }\n";
        assert!(codes(src, Mode::Island).is_empty(), "{src}");
    }

    /// A named `"runai"` import is reachable in an island module (the emitter
    /// maps it onto the runtime import) but not in a prerender module, which has
    /// no imports at all.
    #[test]
    fn a_runai_import_is_reachable_only_in_an_island_module() {
        let src = "\"use island\"\nimport { owner } from \"runai\"\nexport default function P(props: Record<never, never>) { return <p>{owner}</p> }\n";
        assert!(codes(src, Mode::Island).is_empty(), "{src}");

        let diagnostics = diagnostics(src, Mode::Static);
        assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
        assert!(
            diagnostics[0]
                .message
                .contains("Change line 1 to \"use island\""),
            "the static repair is the directive: {diagnostics:?}"
        );
    }

    /// A default or namespace `"runai"` binding is refused by the *import rule*
    /// (`entry.rs`), once, at the declaration — not by this rule at every use.
    /// This rule staying silent here is the invariant: one root cause is one row.
    /// The end-to-end shape is pinned in
    /// `entry::tests::an_unwritable_runai_import_form_is_one_ra014_in_both_modes`.
    #[test]
    fn an_unwritable_runai_import_form_is_left_to_the_import_rule() {
        for declaration in [
            "import * as runai from \"runai\"",
            "import runai from \"runai\"",
        ] {
            for mode in [Mode::Static, Mode::Island] {
                let directive = match mode {
                    Mode::Static => "\"use static\"",
                    Mode::Island => "\"use island\"",
                };
                let src = format!(
                    "{directive}\n{declaration}\nexport default function P(props: Record<never, never>) {{ return <p>{{String(runai.owner)}}</p> }}\n"
                );
                assert!(
                    codes(&src, mode).is_empty(),
                    "{declaration} ({mode:?}): the import rule owns this declaration"
                );
            }
        }
    }

    /// A primitive call in a static route is `RA010`'s; this rule must not add a
    /// second diagnostic at its callee.
    #[test]
    fn a_primitive_call_is_left_to_ra010() {
        let src = "\"use static\"\nimport { signal } from \"runai\"\nexport default function P(props: Record<never, never>) { return <p>{signal(0)}</p> }\n";
        assert!(codes(src, Mode::Static).is_empty(), "{src}");
    }

    /// No route component means nothing is emitted from this file, so nothing
    /// here is this rule's business — the same boundary `check_jsx` draws for
    /// module-level JSX.
    #[test]
    fn a_component_module_is_not_checked() {
        let src = "const title = \"A\"\nexport function Header(props: Record<never, never>) { return <h1>{title}</h1> }\n";
        assert!(codes(src, Mode::Island).is_empty(), "{src}");
    }

    /// A concise arrow body has no statements, so locals declared inside the
    /// returned expression are the only body region that exists.
    #[test]
    fn a_concise_arrow_route_is_checked_too() {
        let local = "\"use static\"\nexport default (props: Record<never, never>) => <p>{[1].map((n) => n)}</p>\n";
        assert!(codes(local, Mode::Static).is_empty(), "{local}");

        let param =
            "\"use static\"\nexport default (props: Record<never, never>) => <p>{props}</p>\n";
        assert_eq!(codes(param, Mode::Static), vec![Code::RA014]);
    }

    /// The round-2 test that covered the concise shape was replaced with a
    /// props fixture, so the `<p>{title}</p>` half — a concise arrow naming a
    /// module-level value — had no fixture of its own left. It is the shape the
    /// module-level walk must *not* refuse, and the return value is the body when
    /// there are no statements, so the two scopes are easy to confuse.
    #[test]
    fn a_concise_arrow_route_may_name_a_module_level_value() {
        let src = "\"use static\"\nconst title = \"About\"\nexport default (props: Record<never, never>) => <p>{title}</p>\n";
        for mode in [Mode::Static, Mode::Island] {
            assert!(
                codes(src, mode).is_empty(),
                "{mode:?}: a module-level value is emitted, so the concise body may name it"
            );
        }

        // The control: the same concise shape naming the route's parameter *is*
        // refused, so the fixture above is not passing because nothing is walked.
        let param = "\"use static\"\nconst title = \"A\"\nexport default (props: Record<never, never>) => <p>{props}</p>\n";
        assert_eq!(
            codes(param, Mode::Static),
            vec![Code::RA014],
            "the concise body is walked"
        );
    }

    // --- Fix round 3: the module level is walked too -------------------------

    /// A `"runai"` value named at **module scope** in a static route. The
    /// prerender module has no imports, so the emitted statement is a
    /// `ReferenceError` at load — the same defect as the markup case, one scope
    /// out, which is why the rule has to walk both.
    #[test]
    fn a_runai_value_at_module_scope_is_refused_when_static() {
        let src = "\"use static\"\nimport { owner } from \"runai\"\nconst o = owner\nexport default function P(props: Record<never, never>) { return <p>hi</p> }\n";
        let diagnostics = diagnostics(src, Mode::Static);
        assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
        assert!(
            diagnostics[0]
                .message
                .contains("Change line 1 to \"use island\""),
            "the repair is the directive: {diagnostics:?}"
        );

        // The island module writes the import, so the same statement is fine.
        assert!(
            codes(src, Mode::Island).is_empty(),
            "an island imports the name the module statement reads"
        );
    }

    /// The route component's own name at module scope, in a static route: the
    /// prerender module has `render()` and no component, so `const x = Page` is
    /// a `ReferenceError`. An island declares `function <Name>(props)`.
    #[test]
    fn the_route_component_at_module_scope_is_refused_when_static() {
        let src = "\"use static\"\nconst x = P\nexport default function P(props: Record<never, never>) { return <p>{x}</p> }\n";
        let diagnostics = diagnostics(src, Mode::Static);
        assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
        assert!(
            diagnostics[0].message.contains("route's own component"),
            "the message must name the component: {diagnostics:?}"
        );

        assert!(
            codes(src, Mode::Island).is_empty(),
            "an island declares the component"
        );
    }

    /// The route's parameter is **not** in scope at module level: `const x =
    /// props` there resolves to nothing at all, not to the route's parameter.
    ///
    /// **This test is the one P2 deliberately reverses.** Round 3 concluded from
    /// that fact that the reference was "not this rule's read" and stayed
    /// silent; the emitted module then threw at load, because the statement *is*
    /// emitted and `props` has no module binding anywhere. The truth is the
    /// opposite of the old conclusion: the parameter is not in scope at module
    /// level in **either** mode, so the reference is a `ReferenceError` in both
    /// and is refused in both. The old reasoning — "refusing it would mean
    /// refusing every unresolved module-level name, including `Math`" — is
    /// answered by name: only the names the route's own parameter list binds are
    /// refused, which `Math` is not.
    #[test]
    fn a_parameter_name_at_module_scope_is_refused_in_both_modes() {
        let src = |directive: &str| {
            format!(
                "{directive}\nconst x = props\nexport default function Page(props: Record<never, never>) {{ return <p>{{x}}</p> }}\n"
            )
        };
        for (directive, mode) in [
            ("\"use static\"", Mode::Static),
            ("\"use island\"", Mode::Island),
        ] {
            let diagnostics = diagnostics(&src(directive), mode);
            assert_eq!(
                codes_of(&diagnostics),
                vec![Code::RA014],
                "{directive}: the emitted module statement names a parameter it cannot see: {diagnostics:?}"
            );
            assert!(
                diagnostics[0].message.contains("module-level statement"),
                "{directive}: the reason is the region, so the message must name it: {diagnostics:?}"
            );
        }

        // The control: a module-level binding of the same name is a *different*
        // binding, so the module can see it and the statement is emitted. Refusing
        // by name alone would fail here.
        let own = "\"use static\"\nconst props = { a: 1 }\nconst x = props\nexport default function Page(props: Record<never, never>) { return <p>{x.a}</p> }\n";
        for mode in [Mode::Static, Mode::Island] {
            assert!(
                codes(own, mode).is_empty(),
                "{mode:?}: the module declares its own `props`, which resolves"
            );
        }
    }

    // --- Fix round 5: the parameter list is a fourth region --------------------

    /// The emitters rebuild the route signature rather than copying it: the
    /// emitted component is `function <Name>(props)`. So a parameter list the
    /// rebuild cannot reproduce is not emitted as written, and everything it
    /// binds is absent from the artifact.
    ///
    /// This is R2. `props: P = owner` is the case that motivated it: `owner` is
    /// not a primitive call, so `RA010` never sees it, and the default was
    /// dropped with no diagnostic. Refusing the *shape* is the honest rule,
    /// because the defect is the dropped signature, not an unreachable name —
    /// which is why the row appears in **both** modes. A reachability walk would
    /// be silent in an island, where the island's own import makes `owner` a
    /// real name.
    #[test]
    fn a_route_signature_the_emitter_cannot_rebuild_is_refused() {
        for (label, params) in [
            ("a default naming a runai value", "props: P = owner"),
            (
                "a default naming an ordinary call",
                "props: P = ((n: number) => n)(1)",
            ),
            ("a destructured props object", "{ id }: P"),
            ("a rest parameter", "...props: P[]"),
            ("a renamed parameter", "ctx: P"),
        ] {
            for (directive, mode) in [
                ("\"use static\"", Mode::Static),
                ("\"use island\"", Mode::Island),
            ] {
                let src = format!(
                    "{directive}\nimport {{ owner }} from \"runai\"\ntype P = Record<never, never>\nexport default function Page({params}) {{ return <p>hi</p> }}\n"
                );
                let diagnostics = diagnostics(&src, mode);
                assert_eq!(
                    codes_of(&diagnostics),
                    vec![Code::RA014],
                    "{label} ({directive}): one row, and not RA010's accident: {diagnostics:?}"
                );
                assert!(
                    diagnostics[0].message.contains("(props:"),
                    "{label} ({directive}): the repair must name the signature to write: {diagnostics:?}"
                );
            }
        }

        // The shape both emitters write stays clean in an island, and the static
        // half is still `RA014` only for reading it — so this test cannot pass by
        // refusing every parameter list.
        let conforming = "\"use island\"\ntype P = Record<never, never>\nexport default function Page(props: P) { return <p>{props}</p> }\n";
        assert!(
            codes(conforming, Mode::Island).is_empty(),
            "the signature both emitters write is legal: {conforming}"
        );
        let read = "\"use static\"\ntype P = Record<never, never>\nexport default function Page(props: P) { return <p>{props}</p> }\n";
        assert_eq!(codes(read, Mode::Static), vec![Code::RA014]);
    }

    /// Every other module-level value stays reachable: the ruling, restated at
    /// the scope the round-3 finding moved the walk into. A module statement that
    /// names a *body* local is not on this list — that local is declared inside
    /// the component, so the module cannot see it, which is JavaScript's rule
    /// rather than this one's.
    #[test]
    fn ordinary_module_level_values_are_reachable_in_both_modes() {
        for src in [
            // A module-level const read by another module statement.
            "\"use static\"\nconst a = 1\nconst b = a\nexport default function P(props: Record<never, never>) { return <p>{b}</p> }\n",
            // A class, and a recursive function that names itself.
            "\"use static\"\nclass Store {}\nconst s = Store\nexport default function P(props: Record<never, never>) { return <p>{s.name}</p> }\n",
            "\"use static\"\nconst fact = (n: number): number => (n < 1 ? 1 : n * fact(n - 1))\nexport default function P(props: Record<never, never>) { return <p>{fact(3)}</p> }\n",
            // A language built-in, which every module has.
            "\"use static\"\nconst n = Math.max(1, 2)\nexport default function P(props: Record<never, never>) { return <p>{n}</p> }\n",
            // A nested function's own locals are inside the emitted statement.
            "\"use static\"\nconst f = () => { const x = 1; return x }\nexport default function P(props: Record<never, never>) { return <p>{f()}</p> }\n",
        ] {
            for mode in [Mode::Static, Mode::Island] {
                assert!(codes(src, mode).is_empty(), "{mode:?}: {src}");
            }
        }
    }

    /// The multi-declarator shape the emitters rebuild: the route is dropped and
    /// `title` is emitted, so the module-level walk must see `title` and must
    /// *not* treat the route half as an emitted statement.
    #[test]
    fn a_value_declared_beside_the_route_is_reachable() {
        let src = "\"use static\"\nconst title = \"A\", Page = (props: Record<never, never>) => <h1>{title}</h1>\nexport default Page\n";
        for mode in [Mode::Static, Mode::Island] {
            assert!(codes(src, mode).is_empty(), "{mode:?}: {src}");
        }
    }

    /// A type-only reference at module scope is not a read: `type T = typeof
    /// owner` and a type-only `"runai"` specifier both vanish before anything
    /// is emitted, so neither is a `ReferenceError`.
    #[test]
    fn a_type_position_at_module_scope_is_not_a_read() {
        for src in [
            "\"use static\"\nimport { owner } from \"runai\"\ntype T = typeof owner\nexport default function P(props: Record<never, never>) { return <p>hi</p> }\n",
            "\"use static\"\nimport { type Accessor } from \"runai\"\nconst x = 1 as Accessor<number>\nexport default function P(props: Record<never, never>) { return <p>{x}</p> }\n",
        ] {
            for mode in [Mode::Static, Mode::Island] {
                assert!(codes(src, mode).is_empty(), "{mode:?}: {src}");
            }
        }
    }

    /// A primitive call at module scope is `RA010`'s in a static route; this
    /// rule must not add a second diagnostic at the callee, at module scope any
    /// more than in the markup.
    #[test]
    fn a_module_level_primitive_call_is_left_to_ra010() {
        let src = "\"use static\"\nimport { signal } from \"runai\"\nconst n = signal(0)\nexport default function P(props: Record<never, never>) { return <p>{n()}</p> }\n";
        assert!(codes(src, Mode::Static).is_empty(), "{src}");
    }

    /// The module walk must not refuse a statement the emitted module does not
    /// carry. An import of a foreign module is `check_imports`' row; reporting
    /// it here too would put one root cause on the page twice, and the statement
    /// is never emitted.
    #[test]
    fn an_import_is_left_to_the_import_rule() {
        let src = "\"use static\"\nimport { team } from \"./data\"\nconst x = team\nexport default function P(props: Record<never, never>) { return <p>{x}</p> }\n";
        assert!(
            codes(src, Mode::Static).is_empty(),
            "the import rule owns the import, and the statement is not emitted"
        );
    }

    /// A re-export is not an emitted statement either: `check_imports` refuses
    /// it, and `module_statements` excludes it, so this walk must be silent.
    #[test]
    fn a_reexport_is_left_to_the_import_rule() {
        for src in [
            "\"use static\"\nexport * from \"./data\"\nexport default function P(props: Record<never, never>) { return <p>hi</p> }\n",
            "\"use static\"\nexport { team } from \"./data\"\nexport default function P(props: Record<never, never>) { return <p>hi</p> }\n",
        ] {
            assert!(codes(src, Mode::Static).is_empty(), "{src}");
        }
    }

    // --- Fix round 4: the component body is the third emitted region ----------

    /// The body statements are copied into the generated component, so a name
    /// they read has the same reachability question as one the markup reads. A
    /// `"runai"` value in a **static** route's body is a `ReferenceError` in the
    /// prerender module, which has no imports at all — the same defect as the
    /// markup case, one region over, and it was walked by neither other walk
    /// (the markup walk starts at the root, and the module walk skips the route
    /// declaration).
    #[test]
    fn a_runai_value_in_the_body_is_refused_when_static() {
        let src = "\"use static\"\nimport { owner } from \"runai\"\nexport default function P(props: Record<never, never>) { const o = owner; return <p>hi</p> }\n";
        let diagnostics = diagnostics(src, Mode::Static);
        assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
        assert!(
            diagnostics[0]
                .message
                .contains("Change line 1 to \"use island\""),
            "the repair is the directive: {diagnostics:?}"
        );
        let span = diagnostics[0].span;
        assert_eq!(
            &src[span.0 as usize..span.1 as usize],
            "owner",
            "the refusal names the reference, not the statement: {diagnostics:?}"
        );

        // The island module writes the import, so the very same statement is
        // fine there: the client emitter's import makes `owner` a name.
        assert!(
            codes(src, Mode::Island).is_empty(),
            "an island imports the name the body statement reads"
        );

        // A statement *after* the JSX `return` is unreachable code and is never
        // emitted, so a reference there is not a `ReferenceError`. This is the
        // boundary between "walk the body" and "walk the emitted statements": the
        // range stops at the return, exactly as `RouteIr::statements` does.
        let after_return = "\"use static\"\nimport { owner } from \"runai\"\nexport default function P(props: Record<never, never>) { return <p>hi</p>; const later = owner }\n";
        assert!(
            codes(after_return, Mode::Static).is_empty(),
            "a statement after the return is not emitted, so it names nothing"
        );
    }

    /// `const x = props` in a **static** body is a distinct sub-case of the
    /// parameter rule: `props` *is* resolved (to the route parameter), so the
    /// "unresolved names are built-ins" boundary does not cover it. `render()`
    /// takes no arguments, so the value would be `undefined` at build time.
    #[test]
    fn the_route_parameter_in_the_body_is_refused_when_static() {
        let src = "\"use static\"\nexport default function P(props: Record<never, never>) { const x = props; return <p>hi</p> }\n";
        let diagnostics = diagnostics(src, Mode::Static);
        assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
        assert!(
            diagnostics[0].message.contains("route's parameter"),
            "the parameter rule owns this, not the unresolved boundary: {diagnostics:?}"
        );

        assert!(
            codes(src, Mode::Island).is_empty(),
            "an island declares `function P(props)`"
        );
    }

    /// The body walk judges a body local by the same `contains(body, …)` test the
    /// markup walk uses, so `const title = …; return <h1>{title}</h1>` stays clean
    /// — and a body statement reading an **earlier** body local is clean too. Both
    /// directions are asserted, because the cheap wrong fix (refusing every body
    /// reference that is not the module level) passes the refusal tests and fails
    /// these.
    #[test]
    fn a_body_local_is_reachable_from_the_body_and_from_the_markup() {
        for src in [
            // The reviewer's false-positive case: a body local read by the markup.
            "\"use static\"\nexport default function P(props: Record<never, never>) { const title = \"About\"; return <h1>{title}</h1> }\n",
            // A body statement reading an earlier body local.
            "\"use static\"\nexport default function P(props: Record<never, never>) { const a = 1; const b = a + 1; return <p>{b}</p> }\n",
            // A body-local function with its own parameter, called from the body.
            "\"use static\"\nexport default function P(props: Record<never, never>) { const twice = (n: number) => n * 2; const v = twice(2); return <p>{v}</p> }\n",
        ] {
            for mode in [Mode::Static, Mode::Island] {
                assert!(codes(src, mode).is_empty(), "{mode:?}: {src}");
            }
        }
    }

    /// The third walk must not refuse the body of a **concise arrow** route: there
    /// are no body statements, only the returned expression, which the markup walk
    /// already owns. The control is the same concise shape naming the parameter,
    /// which is refused — so this is not passing because nothing is walked.
    #[test]
    fn a_concise_arrow_body_has_no_statements_for_the_third_walk() {
        let clean = "\"use static\"\nimport { owner } from \"runai\"\nexport default (props: Record<never, never>) => <p>{String(1)}</p>\n";
        assert!(codes(clean, Mode::Static).is_empty(), "{clean}");

        let refused =
            "\"use static\"\nexport default (props: Record<never, never>) => <p>{props}</p>\n";
        assert_eq!(codes(refused, Mode::Static), vec![Code::RA014]);
    }

    /// A **module-level function that is never called** and names a `"runai"`
    /// value is still refused: the statement is emitted into the module, so the
    /// reference is a `ReferenceError` at load whether or not anything calls it.
    /// This pins the indirect case, which held by construction before it had a
    /// fixture.
    #[test]
    fn an_uncalled_module_function_that_names_a_runai_value_is_refused() {
        let src = "\"use static\"\nimport { owner } from \"runai\"\nfunction helper() { return owner }\nexport default function P(props: Record<never, never>) { return <p>hi</p> }\n";
        let diagnostics = diagnostics(src, Mode::Static);
        assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
        assert!(
            diagnostics[0]
                .message
                .contains("Change line 1 to \"use island\""),
            "the module statement is emitted, so its body is in the module: {diagnostics:?}"
        );

        assert!(
            codes(src, Mode::Island).is_empty(),
            "an island imports `owner`, so the helper resolves"
        );
    }

    /// A `"runai"` re-export is silent here: `entry.rs`'s import rule owns the
    /// statement, in every mode, and `module_statements` excludes it, so one root
    /// cause is one row and this walk reports nothing at a statement the emitted
    /// module never carries.
    #[test]
    fn a_runai_reexport_is_left_to_the_import_rule() {
        for src in [
            "\"use static\"\nexport { owner } from \"runai\"\nexport default function P(props: Record<never, never>) { return <p>hi</p> }\n",
            "\"use static\"\nexport * from \"runai\"\nexport default function P(props: Record<never, never>) { return <p>hi</p> }\n",
        ] {
            assert!(codes(src, Mode::Static).is_empty(), "{src}");
        }
    }

    /// Every refusal is its own diagnostic, at its own span: each one is an
    /// edit. **Both** spans are asserted, so a walk that collapsed two
    /// references into one — or reported the second at the first's span — fails
    /// here rather than merely not-fatal.
    #[test]
    fn each_refusal_is_reported_at_its_own_span() {
        let src = "\"use static\"\nexport default function P(props: Record<never, never>) { return <p>{props}{props}</p> }\n";
        let diagnostics = diagnostics(src, Mode::Static);
        assert_eq!(diagnostics.len(), 2, "{diagnostics:?}");
        assert!(
            diagnostics[0].span.0 < diagnostics[1].span.0,
            "{diagnostics:?}"
        );
        for diagnostic in &diagnostics {
            assert_eq!(
                &src[diagnostic.span.0 as usize..diagnostic.span.1 as usize],
                "props",
                "each refusal names its own occurrence: {diagnostics:?}"
            );
        }
    }

    // --- N1: type positions are never references -----------------------------

    /// The three fixtures the review proved were false positives. Each is a
    /// type position inside the body, and each compiles and runs today:
    /// `strip_types` erases the reference before anything is emitted.
    ///
    /// Every fixture here names a **type alias**, which is now reachable as a
    /// module-level declaration anyway — so these alone would not prove the skip
    /// is load-bearing. `a_type_position_naming_a_refused_value_is_still_skipped`
    /// is the control that does.
    #[test]
    fn a_type_position_is_not_a_reference() {
        for src in [
            // A type alias used in a type assertion.
            "\"use static\"\ntype Kind = \"a\" | \"b\"\nexport default function P(props: Record<never, never>) { return <p>{\"a\" as Kind}</p> }\n",
            // A type alias annotating an arrow parameter inside the markup.
            "\"use static\"\ntype Entry = { name: string }\nexport default function P(props: Record<never, never>) { return <p>{[{ name: \"x\" }].map((v: Entry) => v.name)}</p> }\n",
            // A named generic argument in a call.
            "\"use static\"\ntype Kind = \"a\"\nexport default function P(props: Record<never, never>) { return <p>{[1].map<Kind>(() => \"a\" as Kind)}</p> }\n",
            // A `import type` from `"runai"` used as a type.
            "\"use island\"\nimport { type Accessor } from \"runai\"\nexport default function P(props: Record<never, never>) { return <p>{1 as Accessor<number>}</p> }\n",
        ] {
            for mode in [Mode::Static, Mode::Island] {
                assert!(codes(src, mode).is_empty(), "{mode:?}: {src}");
            }
        }
    }

    /// The skip is load-bearing **only** where the type position names something
    /// the rule still refuses. Both remaining refusals have such a fixture, and
    /// without the skip each becomes the false positive the review proved:
    ///
    /// 1. `typeof props` as a *type annotation* names the route parameter, which a
    ///    static route may not read — but a type annotation is erased, so it is
    ///    not a read;
    /// 2. a `{ Accessor }` **value** import from `"runai"` used only as a type
    ///    names a `"runai"` import, which a static route may not evaluate — but
    ///    the use is a type, so nothing is evaluated.
    ///
    /// Deleting the `is_type_only` early return makes exactly these two fixtures
    /// fail. That is why the assertion is on the exact empty list, not on a
    /// length.
    #[test]
    fn a_type_position_naming_a_refused_value_is_still_skipped() {
        // (1) the route parameter, referenced from a type annotation in markup.
        let parameter_as_type = "\"use static\"\nexport default function P(props: Record<never, never>) { return <p>{[1].map((v: typeof props) => v)}</p> }\n";
        for mode in [Mode::Static, Mode::Island] {
            assert!(
                codes(parameter_as_type, mode).is_empty(),
                "{mode:?}: a type annotation is erased, so it is not a read of `props`"
            );
        }

        // (2) a value import from `"runai"` used only as a type.
        let runai_as_type = "\"use static\"\nimport { Accessor } from \"runai\"\nexport default function P(props: Record<never, never>) { return <p>{1 as Accessor<number>}</p> }\n";
        for mode in [Mode::Static, Mode::Island] {
            assert!(
                codes(runai_as_type, mode).is_empty(),
                "{mode:?}: a type position evaluates nothing, so the runtime import is unused"
            );
        }
    }

    /// The predicate is `is_type_only`, not `is_type`: a reference flagged
    /// `Type | Value` is a real use and must still be judged. `typeof props` in a
    /// JSX expression container is the JavaScript operator, not a TypeScript type
    /// query, so it reads the value — and the value is the route parameter.
    ///
    /// The type-query spellings that *are* type positions are covered by
    /// `a_type_position_is_not_a_reference`; this is the control that proves the
    /// skip did not become "ignore everything with a `Type` bit".
    #[test]
    fn a_value_read_still_counts_even_when_it_is_a_typeof_operator() {
        let src = "\"use static\"\nexport default function P(props: Record<never, never>) { return <p>{typeof props}</p> }\n";
        assert_eq!(codes(src, Mode::Static), vec![Code::RA014]);
    }
}
