//! `RA014` for a value the generated module cannot see.
//!
//! `analyze` refuses a static route's *constructs* (a signal, a handler) with
//! `RA010`, and `check_jsx` refuses JSX the build cannot turn into JavaScript.
//! This module closes the third hole of the same kind, and it is the one that
//! used to pass every gate: a **plain expression** whose identifier names a
//! binding the generated module never declares.
//!
//! ```tsx
//! "use static"
//! const title = "About"
//! export default function Page(props: Record<never, never>) {
//!   return <h1>{title}</h1>
//! }
//! ```
//!
//! Every gate above accepts that file: `{title}` is a plain expression, which
//! `RA010` deliberately allows. But [`RouteIr::statements`] is **body-only**, so
//! the prerender module is
//!
//! ```js
//! const _esc = (v) => …
//! export function render() {
//!   return `<h1>${_esc(title)}</h1>`
//! }
//! ```
//!
//! — a reference to a symbol that module never declares. That is accept-then-
//! reject, the worst direction: `runai check` reports nothing and `runai build`
//! fails. The analysis must refuse it, because the emitter cannot repair it.
//!
//! [`RouteIr::statements`]: crate::ir::RouteIr::statements
//!
//! # The exact rule, derived from what the emitters declare
//!
//! A reference in the route's returned markup is **reachable** when it resolves
//! to one of exactly four things — the list is what Task 10's `emit_prerender`
//! and Task 11's `emit_client` actually put in scope, not a guess:
//!
//! 1. a **binding declared inside the component body** (`const title = …` in the
//!    body). Both emitters copy [`RouteIr::statements`] into the module, so the
//!    statement and therefore the binding is there.
//! 2. **no symbol at all** — an unresolved name is a language built-in (`Math`,
//!    `String`, `undefined`), which every module has.
//! 3. for an **island** page, a **named import from `"runai"`**. The client
//!    emitter writes exactly one import from the runtime URL, merging the
//!    route's own `"runai"` specifiers.
//! 4. for an **island** page, the **route's own parameter**. The client emitter
//!    declares `function <Name>(props)`.
//!
//! Everything else is `RA014`. The cases worth knowing:
//!
//! - a **module-level declaration** is never copied by either emitter — the
//!   repair is the one the message carries: move it into the body, or inline it;
//! - a **`"runai"` import used in a static page's markup** is refused: the
//!   prerender module has **no imports at all** (pinned by Task 10), so the name
//!   is not evaluable there. The repair is the directive, `"use island"` — the
//!   same escape hatch `RA010` points at, because the page genuinely needs a
//!   runtime. A reference that *is* a primitive call is left to `RA010`, so one
//!   expression never gets two diagnostics;
//! - a **static page's route parameter** is refused: `render()` takes no
//!   arguments, so the value would be `undefined` at build time.
//!
//! # Scope: only a page, and only one that has a mode
//!
//! The rule is about a **page's** emitted module. A component module
//! (`_shell.tsx`, or any `.tsx` with no default export) is not emitted by this
//! milestone, so a module-level value there is legal and is not reported — the
//! same boundary `check_jsx` draws for module-level JSX. A page with no
//! directive is not checked either: `RA012` already stops the build, and
//! guessing the mode would make this rule's own claim uncertain.
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

use crate::analyze::component::find_route_component;
use crate::analyze::jsx::route_render;
use crate::analyze::signals::{SignalBindings, runai_import_symbols};
use crate::diagnostic::{Code, Diagnostic};
use crate::source::Mode;

/// `RA014` for every value in the route's markup the generated module cannot
/// name.
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

    // The one region whose declarations are copied into the generated module: the
    // route body's own statements. A concise arrow body (`(props) => <p/>`) has no
    // statements, so the returned expression itself is that region — a parameter
    // of an arrow nested in the markup is declared inside the emitted template.
    let root = {
        let span = render.root.span();
        (span.start, span.end)
    };
    let body = render
        .body
        .map_or(root, |body| (body.span.start, body.span.end));
    let params = find_route_component(program).map(|route| route.params_span);

    // A second semantic build, for a page only: the reference resolution this
    // rule needs is not part of `SignalBindings`. Files without a route never
    // reach this line.
    let semantic = SemanticBuilder::new().build(program).semantic;
    let scoping = semantic.scoping();

    let mut walk = TreeReferences {
        scoping,
        references: Vec::new(),
    };
    walk.visit_expression(render.root);

    // Named imports of `"runai"` are the specifiers the client emitter can map
    // onto its runtime import; a default or namespace import is not one it
    // writes, so it is not in this list and is refused like anything else.
    let runai = runai_import_symbols(program);

    let mut diagnostics = Vec::new();
    for reference in walk.references {
        // Unresolved: a language built-in, which every module has.
        let Some(symbol) = reference.symbol else {
            continue;
        };
        let declaration = declaration_of(scoping, symbol);

        // (1) A body-local: its statement is copied into the module.
        if contains(body, declaration) {
            continue;
        }

        // (3) and (4), island only: the client module imports these names and
        // declares the parameter.
        if mode == Mode::Island
            && (runai.contains(&symbol) || params.is_some_and(|p| contains(p, declaration)))
        {
            continue;
        }

        // A primitive call in a static route is already `RA010` at this span; a
        // second diagnostic at the callee would describe one expression twice.
        if is_primitive_callee(reference.span, bindings) {
            continue;
        }

        let diagnostic = if runai.contains(&symbol) {
            runtime_import(reference.span, &reference.name)
        } else if params.is_some_and(|p| contains(p, declaration)) {
            route_parameter(reference.span, &reference.name)
        } else {
            module_level_value(reference.span, &reference.name)
        };
        diagnostics.push(diagnostic);
    }

    diagnostics.sort_by_key(|diagnostic| diagnostic.span.0);
    diagnostics
}

/// One identifier reference in the returned markup.
struct Reference {
    name: String,
    span: (u32, u32),
    /// `None` for an unresolved name, i.e. a built-in such as `Math`.
    symbol: Option<SymbolId>,
}

/// Collect every identifier reference in the route's returned markup.
///
/// The walk starts at the returned root, not the program, so a module-level
/// helper that happens to name the same binding is not part of this question:
/// only what the emitted template evaluates matters.
struct TreeReferences<'s> {
    scoping: &'s Scoping,
    references: Vec<Reference>,
}

impl<'a> Visit<'a> for TreeReferences<'_> {
    fn visit_identifier_reference(&mut self, reference: &IdentifierReference<'a>) {
        self.references.push(Reference {
            name: reference.name.as_str().to_string(),
            span: (reference.span.start, reference.span.end),
            symbol: reference
                .reference_id
                .get()
                .and_then(|id| self.scoping.get_reference(id).symbol_id()),
        });
        oxc_ast_visit::walk::walk_identifier_reference(self, reference);
    }
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

/// `RA014`: the markup names a module-level value the emitters do not copy.
fn module_level_value(span: (u32, u32), name: &str) -> Diagnostic {
    Diagnostic::new(
        Code::RA014,
        span,
        format!(
            "`{name}` is not available in the generated module: only statements inside the \
             component body are emitted, so a module-level declaration never reaches it. Move \
             `{name}` into the component body, or inline its value."
        ),
    )
}

/// `RA014`: a static page's markup names a `"runai"` import.
///
/// The prerender module contains no imports, so the repair cannot be "move it":
/// taking the value into the body does not import it. The page needs the
/// runtime, and the escape hatch is the directive `RA010` also points at.
fn runtime_import(span: (u32, u32), name: &str) -> Diagnostic {
    Diagnostic::new(
        Code::RA014,
        span,
        format!(
            "`{name}` is imported from `\"runai\"`, but a `\"use static\"` route's markup is \
             evaluated in a module with no imports. Change line 1 to \"use island\" to ship the \
             runtime this value comes from, or remove the reference."
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

    /// The canonical defect: a module-level constant the build never emits. It is
    /// a plain expression, so every other gate accepts it.
    #[test]
    fn a_module_level_constant_referenced_by_the_tree_is_refused() {
        let src = "\"use static\"\nconst title = \"About\"\nexport default function Page(props: Record<never, never>) { return <h1>{title}</h1> }\n";
        for mode in [Mode::Static, Mode::Island] {
            assert_eq!(codes(src, mode), vec![Code::RA014], "{mode:?}");
        }

        let diagnostics = diagnostics(src, Mode::Static);
        assert!(
            diagnostics[0].message.contains("`title`"),
            "{diagnostics:?}"
        );
        assert!(
            diagnostics[0]
                .message
                .contains("Move `title` into the component body, or inline its value"),
            "{diagnostics:?}"
        );
        // The span is the reference, not the declaration: that is the site to edit.
        assert_eq!(
            &src[diagnostics[0].span.0 as usize..diagnostics[0].span.1 as usize],
            "title"
        );
    }

    /// The case the brief requires to stay legal, plus the resolver's own answer
    /// for a built-in.
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

    /// A primitive call in a static route is `RA010`'s; this rule must not add a
    /// second diagnostic at its callee.
    #[test]
    fn a_primitive_call_is_left_to_ra010() {
        let src = "\"use static\"\nimport { signal } from \"runai\"\nexport default function P(props: Record<never, never>) { return <p>{signal(0)}</p> }\n";
        assert!(codes(src, Mode::Static).is_empty(), "{src}");
    }

    /// No route component means nothing is emitted from this file, so a
    /// module-level value is not this rule's business — the same boundary
    /// `check_jsx` draws for module-level JSX.
    #[test]
    fn a_component_module_is_not_checked() {
        let src = "const title = \"A\"\nexport function Header(props: Record<never, never>) { return <h1>{title}</h1> }\n";
        assert!(codes(src, Mode::Island).is_empty(), "{src}");
    }

    /// A concise arrow body has no statements, so locals declared inside the
    /// returned expression are the only region that exists. A module-level value
    /// is still refused, because nothing declares it there.
    #[test]
    fn a_concise_arrow_route_is_checked_too() {
        let local = "\"use static\"\nexport default (props: Record<never, never>) => <p>{[1].map((n) => n)}</p>\n";
        assert!(codes(local, Mode::Static).is_empty(), "{local}");

        let module_level = "\"use static\"\nconst title = \"A\"\nexport default (props: Record<never, never>) => <p>{title}</p>\n";
        assert_eq!(codes(module_level, Mode::Static), vec![Code::RA014]);
    }

    /// Every reference is its own diagnostic, at its own span: each one is an
    /// edit.
    #[test]
    fn each_reference_is_reported_at_its_own_span() {
        let src = "\"use static\"\nconst a = \"A\"\nconst b = \"B\"\nexport default function P(props: Record<never, never>) { return <p>{a}{b}</p> }\n";
        let diagnostics = diagnostics(src, Mode::Static);
        assert_eq!(diagnostics.len(), 2, "{diagnostics:?}");
        assert!(
            diagnostics[0].span.0 < diagnostics[1].span.0,
            "{diagnostics:?}"
        );
        assert_eq!(
            &src[diagnostics[0].span.0 as usize..diagnostics[0].span.1 as usize],
            "a"
        );
        assert_eq!(
            &src[diagnostics[1].span.0 as usize..diagnostics[1].span.1 as usize],
            "b"
        );
    }
}
