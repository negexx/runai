//! `RA014` for a value the generated module genuinely cannot see.
//!
//! `analyze` refuses a static route's *constructs* (a signal, a handler) with
//! `RA010`, and `check_jsx` refuses JSX the build cannot turn into JavaScript.
//! This module covers the remaining way the markup can name something the
//! emitted module does not have.
//!
//! # What is reachable, and why the list is short
//!
//! The emitters write [`RouteIr::module_statements`] above the component and
//! [`RouteIr::statements`] inside it, so a name in the returned markup resolves
//! if it is declared in either place. Design §7.6 expects module-level state —
//! "module-scope signals are per-island too, since each island is its own module
//! instance" — so a module-level `const title = "About"` referenced by
//! `<h1>{title}</h1>` is **legal**, and round 3 of this task removed the earlier
//! refusal that called it unreachable.
//!
//! Two scopes are walked, because the emitters write two: the returned markup
//! and the file's **module level**. A module statement is emitted above the
//! component, so a name it references has to exist in the emitted module too —
//! and the prerender module has no imports and does not declare the component at
//! all.
//!
//! Exactly four things stay refused:
//!
//! 1. **a `"runai"` name in a `"use static"` route**, whether the markup or a
//!    module-level statement names it. The prerender module contains no imports
//!    at all, so the name is not evaluable there. The repair is the directive,
//!    `"use island"` — the same escape hatch `RA010` points at, because the page
//!    genuinely needs a runtime. A reference that *is* a primitive call is left
//!    to `RA010`, so one expression never gets two diagnostics.
//! 2. **the route's own parameter in a `"use static"` route.** `render()` takes
//!    no arguments, so `{props}` would be `undefined` at build time. The repair
//!    is not "move it": there is no props value to move.
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
//! mode-dependent half of this rule.
//!
//! Everything else at module scope is emitted by [`RouteIr::module_statements`]
//! and is therefore reachable, which is the round-3 ruling this module follows.
//!
//! [`RouteIr::module_statements`]: crate::ir::RouteIr::module_statements
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
/// returned markup, and a second of the file's module level.
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
    };

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

    // (2) The module level. Every statement `RouteIr::module_statements` will
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
}

impl EmittedScope<'_, '_> {
    /// Append `RA014` when `reference` names something the emitted module does
    /// not have.
    ///
    /// `inside_emitted_function` answers "is this declaration copied into the
    /// emitted component?" — the markup walk passes the route body's span, the
    /// module walk answers `false` because no module statement declares it.
    fn judge(
        &self,
        mode: Mode,
        reference: &Reference,
        inside_emitted_function: impl Fn((u32, u32)) -> bool,
        diagnostics: &mut Vec<Diagnostic>,
    ) {
        // Unresolved: a language built-in, which every module has.
        let Some(symbol) = reference.symbol else {
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
            diagnostics.push(route_parameter(reference.span, &reference.name));
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
}

/// One identifier reference in the returned markup or a module-level statement.
struct Reference {
    name: String,
    span: (u32, u32),
    /// `None` for an unresolved name, i.e. a built-in such as `Math`.
    symbol: Option<SymbolId>,
}

/// Collects every **value** identifier reference under whatever node the caller
/// visits: the returned root for the markup walk, or one module-level statement
/// for the module-level walk.
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
struct References<'s> {
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

/// `RA014`: a static page names a `"runai"` import, in its markup or at module
/// scope.
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

/// `RA014`: a static page's markup names the route's own parameter.
///
/// The repair is not "move it into the body": a route is given no props, so
/// there is no value to move. Whatever the markup needs has to be written there.
fn route_parameter(span: (u32, u32), name: &str) -> Diagnostic {
    Diagnostic::new(
        Code::RA014,
        span,
        format!(
            "`{name}` is the route's parameter, but a route is given no props and the \
             prerendered module takes no arguments, so `{name}` would be undefined at build \
             time. Remove the reference, and write the value in the component body."
        ),
    )
}

/// `RA014`: a static page names its own component.
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
    /// props` there resolves to nothing at all, not to the route's parameter. That
    /// is a different defect from this rule's, and refusing it would mean
    /// refusing every unresolved module-level name — including `Math` and DOM
    /// globals — so it is deliberately out of scope. Asserted so the boundary is
    /// written down rather than inferred.
    #[test]
    fn the_route_parameter_is_not_in_scope_at_module_scope() {
        let src = "\"use static\"\nconst x = props\nexport default function P(props: Record<never, never>) { return <p>{x}</p> }\n";
        for mode in [Mode::Static, Mode::Island] {
            assert!(
                codes(src, mode).is_empty(),
                "{mode:?}: module level cannot see the parameter, so it is not this rule's read"
            );
        }
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
