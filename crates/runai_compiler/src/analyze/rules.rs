//! `RA001`, `RA002`, `RA009`, and `RA010`: where a signal is read or written.
//!
//! The first three answer one question about one call: *what owns this read or
//! write?*
//!
//! - `RA001_UNTRACKED_READ`: a read whose nearest function is a component body and
//!   which is not inside a tracking scope. Nothing re-runs it, so the value on
//!   screen is the value at first render — the silently frozen value.
//! - `RA002_EFFECT_WRITE`: a write whose nearest function is the function passed to
//!   an `effect`. The effect re-runs for the signals it reads, and the write runs
//!   as part of that run; when the effect reads the signal it writes, the write
//!   queues the effect that is already running, and `execute`'s guard **throws**
//!   `effect cycle` rather than looping.
//! - `RA009_IMPLICIT_EFFECT`: a write whose nearest function is a component body.
//! - `RA010_STATIC_VIOLATION`: any of the above in a `"use static"` route, where
//!   there is no runtime at all — see [`check_static_mode`].
//!
#![doc = crate::diagnostic::ra009_consequence!()]
//!
//! That sentence is shared with `render_write`'s message, `Code::explain`,
//! `Code::fix_template`, and the `RA009` skill body through `ra009_consequence!` in
//! [`crate::diagnostic`], so no site can reword it alone.
//!
//! # The two exemptions a naive ancestor check gets wrong
//!
//! **An event handler, and any other nested callback, is not the component.** A read
//! inside `onClick={() => count()}` has the handler as its nearest function, not
//! the component, so it is not `RA001`: it reads when the click happens, which is
//! exactly right. The same holds for `setTimeout(() => count(), 10)`, for an
//! `onMount`-style callback, and for every other function passed to an unknown API.
//! This is the plan's Review Focus #2: the rule is *nearest* function, not "inside
//! the component anywhere". A nested callback that reads a signal sees the current
//! value; nothing is frozen. `RA009` reads the same answer in the other direction:
//! `onClick={() => count(count() + 1)}` writes, and it is not flagged, because the
//! write happens on the click rather than while markup is produced.
//!
//! The one callback that *is* a tracking scope is the function handed to a primitive:
//! `derived(fn)`, `effect(fn)`, and `resource(fn)` run it under tracking, so a read
//! there is owned and is never `RA001`.
//!
//! # Known hole: a write inside `derived` or `resource` is not flagged
//!
//! [`Primitive::Effect`] is the only compute function `RA002` covers, because that is
//! the only one the plan gives a code to. But `derived(fn)` and `resource(fn)` are
//! driven by the signals read inside `fn` (§7.1), so a write there has the same
//! self-dependency shape `RA002` describes. It is **deliberately unpinned by a code**
//! here: inventing one would claim a rule the plan does not have.
//!
//! Of the runtime's two cycle guards, only one covers this shape: `execute`'s
//! `e.running` check (`runtime.js:167`), which throws `effect cycle` when it is asked
//! to run an effect that is already running. (`recompute`'s `d.busy` guard,
//! `runtime.js:127-131`, is `derived cycle` and fires only for a derived that reads
//! itself.) Both `execute` (`runtime.js:173`) and `recompute` (`runtime.js:133`)
//! untrack at entry, so a running compute is a subscriber of a signal only if it has
//! read that signal in the same run.
//!
//! - `resource`: the compute is the body of an effect. Writing a signal it has read
//!   queues that same effect, and `flush` then executes it again while it is still
//!   running, so the guard fires inside the compute's run. `flush` rethrows the error
//!   from the setter (`runtime.js:217`), and `resource`'s own `try`/`catch` around
//!   `fn(...)` turns it into `out({ status: "error", error })` (`runtime.js:596-599`).
//!   Creating such a resource therefore does not throw: it answers `status: "error"`
//!   carrying the `effect cycle` message. A later write from outside re-runs the
//!   compute; when the compute writes that signal on every run, each run queues the
//!   next, and `flush` throws `effects keep invalidating each other` when it exceeds
//!   its wave limit to the external caller (`runtime.js:194-200`).
//! - `derived`: `recompute` has no check for this shape — its `derived cycle` guard is
//!   only for a derived that reads itself (`runtime.js:127-131`). The write marks the
//!   derived dirty and, through it, queues the readers that had read it, so they run
//!   again: measured, a derived precomputed and then subscribed computes twice and
//!   throws nothing. The `effect cycle: an effect wrote a signal it reads` message
//!   reaches the author only through `execute`'s guard, i.e. when the subscriber the
//!   write queues is itself still running — which is what happens when the derived's
//!   first compute runs inside the `effect(...)` that reads it (measured).
//!
//! A write to a signal the compute has **not** read in that run is accepted silently.
//! The compute is not a subscriber of that signal (`untrack` cleared its deps at
//! entry), so `propagate` walks other subscribers and never queues it — the compute is
//! neither re-run nor told. Measured with a second effect watching the written signal:
//! it sees the new value, and the resource neither errors nor re-runs.
//!
//! `a_write_inside_a_derived_is_not_ra002` and
//! `a_write_inside_a_resource_is_not_flagged` pin the hole so a future reader sees a
//! decision rather than an oversight. Closing it is a plan change, not a code change.
//!
//! [`Primitive::Effect`]: crate::analyze::signals::Primitive::Effect
//!
//! # Identity comes from [`SignalBindings`]
//!
//! This module compares recorded symbol ids and spans; it never looks at a name.
//! [`find_signal_bindings`] does the resolution, once, and records for every accessor
//! call its nearest function and whether a JSX expression container encloses it. See
//! [`crate::analyze::signals`] for why that must be symbol identity.
//!
//! [`SignalBindings`]: crate::analyze::signals::SignalBindings
//! [`find_signal_bindings`]: crate::analyze::signals::find_signal_bindings

use crate::analyze::component::{ComponentInfo, find_components};
use crate::analyze::signals::{AccessorCall, SignalBindings};
use crate::diagnostic::{Code, Diagnostic};
use crate::source::Mode;
use oxc_ast_visit::Visit;

/// Emit `RA001`, `RA002`, and `RA009` for `program`.
///
/// `bindings` is [`crate::analyze::signals::find_signal_bindings`]'s answer for the
/// same `program`; it carries the resolved calls. The program is read for the one
/// fact that is not about calls: which body spans are components.
///
/// Diagnostics come back in ascending source order, so one run reads top to bottom
/// in the file an agent is editing.
pub fn check_signal_rules(
    program: &oxc_ast::ast::Program<'_>,
    bindings: &SignalBindings,
) -> Vec<Diagnostic> {
    let components = find_components(program);

    let mut diagnostics: Vec<Diagnostic> = bindings
        .accessor_calls
        .iter()
        .filter_map(|call| diagnose(call, &components, bindings))
        .collect();

    diagnostics.sort_by(|left, right| {
        (left.span.0, left.code.as_str()).cmp(&(right.span.0, right.code.as_str()))
    });
    diagnostics
}

/// Emit `RA010` for every construct a `"use static"` route cannot run.
///
/// A static route is prerendered to HTML at build time and ships **zero bytes** of
/// JavaScript, so there is no runtime on the page: a signal, a derived value, an
/// effect, a resource, a write, or a JSX event handler would silently do nothing.
/// This is the compiler-enforced form of that guarantee, not an observation about
/// build output.
///
/// Three sources, all resolved, never matched by name:
///
/// - [`SignalBindings::primitive_calls`] — `signal(…)`, `derived(…)`, `effect(…)`,
///   `resource(…)` through the `"runai"` import. A local function that happens to
///   be called `signal` is an ordinary function and is not here, which is why this
///   reads the bindings rather than the text;
/// - [`AccessorCall::write`] — `count(next)`. The declaration it writes was already
///   reported as its own primitive call, at its own span;
/// - a JSX attribute named `on` followed by an uppercase letter — `onClick`,
///   `onMount`. A lowercase `on…` name (`onclick`, or an ordinary `once`) is not
///   the handler naming convention and is not flagged.
///
/// **Plain expressions are allowed.** `{title}` where `title` is a local constant
/// needs no runtime: it is evaluated once, at build time, and becomes text in the
/// prerendered HTML. Only constructs that require code to run are refused.
///
/// "Local" is load-bearing, and it is [`crate::analyze::reachable`]'s question
/// rather than this rule's: `{title}` is allowed here whatever `title` binds to,
/// because no runtime construct is involved either way. Whether the prerender
/// module can *see* that binding — a body local or a module-level `const` yes, a
/// `"runai"` import or the route's own component no — is that rule's question,
/// not a separate refusal here.
///
/// [`SignalBindings::primitive_calls`]: crate::analyze::signals::SignalBindings::primitive_calls
pub fn check_static_mode(
    mode: Mode,
    program: &oxc_ast::ast::Program<'_>,
    bindings: &SignalBindings,
) -> Vec<Diagnostic> {
    if mode != Mode::Static {
        return Vec::new();
    }

    let mut diagnostics: Vec<Diagnostic> = bindings
        .primitive_calls
        .iter()
        .map(|&span| static_primitive(span))
        .collect();

    diagnostics.extend(
        bindings
            .accessor_calls
            .iter()
            .filter(|call| call.write)
            .map(static_write),
    );

    let mut handlers = HandlerCollector::default();
    handlers.visit_program(program);
    diagnostics.extend(handlers.spans.iter().map(|&span| static_handler(span)));

    diagnostics.sort_by(|left, right| {
        (left.span.0, left.code.as_str()).cmp(&(right.span.0, right.code.as_str()))
    });
    diagnostics
}

/// `RA010_STATIC_VIOLATION`: a runtime primitive in a static route.
fn static_primitive(span: (u32, u32)) -> Diagnostic {
    Diagnostic::new(
        Code::RA010,
        span,
        "a \"use static\" route calls a runtime primitive; it ships no JavaScript, so \
         this would run nothing",
    )
}

/// `RA010_STATIC_VIOLATION`: a write to an accessor in a static route.
fn static_write(call: &AccessorCall) -> Diagnostic {
    Diagnostic::new(
        Code::RA010,
        call.span,
        format!(
            "`{name}` is written in a \"use static\" route; it ships no JavaScript, so the \
             write would never happen",
            name = call.name,
        ),
    )
}

/// `RA010_STATIC_VIOLATION`: an event handler in a static route.
fn static_handler(span: (u32, u32)) -> Diagnostic {
    Diagnostic::new(
        Code::RA010,
        span,
        "a \"use static\" route has no runtime, so this event handler would never be \
         attached or fire",
    )
}

/// Every `on` + uppercase JSX attribute in the file.
///
/// A walk rather than a name list: the convention is "`on` followed by an
/// uppercase letter", so `onClick`, `onMount`, and `onKeyDown` are all handlers
/// without this rule having to enumerate DOM events, and `once` or `onclick` are
/// not. A namespaced attribute (`foo:onClick`) is not this shape either.
#[derive(Default)]
struct HandlerCollector {
    spans: Vec<(u32, u32)>,
}

impl<'a> Visit<'a> for HandlerCollector {
    fn visit_jsx_attribute(&mut self, attribute: &oxc_ast::ast::JSXAttribute<'a>) {
        if let oxc_ast::ast::JSXAttributeName::Identifier(name) = &attribute.name
            && is_handler_name(name.name.as_str())
        {
            self.spans.push((name.span.start, name.span.end));
        }
        oxc_ast_visit::walk::walk_jsx_attribute(self, attribute);
    }
}

/// Whether `name` is `on` followed by an uppercase letter.
fn is_handler_name(name: &str) -> bool {
    name.strip_prefix("on")
        .and_then(|rest| rest.chars().next())
        .is_some_and(char::is_uppercase)
}

/// The diagnostic, if any, for one accessor call.
fn diagnose(
    call: &AccessorCall,
    components: &[ComponentInfo<'_>],
    bindings: &SignalBindings,
) -> Option<Diagnostic> {
    let nearest = call.nearest_function?;

    // The component whose body is the nearest function. `None` for a read or write
    // in a helper function, a module-level callback, or an event handler.
    let component = components
        .iter()
        .find(|component| component.body_span == nearest);

    if call.write {
        // `RA002` first: an effect body is never a component body, so the two cannot
        // both apply, and ordering here only decides which message names the scope.
        if bindings.effect_scopes.contains(&nearest) {
            return Some(effect_write(call));
        }
        return component.map(|component| render_write(call, component));
    }

    // A read in a JSX slot is owned by the slot; a read in a callback is owned by
    // whoever runs the callback. Only a read whose nearest function is the component
    // body itself, with no tracking scope in between, is `RA001`.
    if call.in_jsx_container {
        return None;
    }
    component.map(|component| untracked_read(call, component))
}

/// `RA001_UNTRACKED_READ`: a read in the component body, outside every scope.
fn untracked_read(call: &AccessorCall, component: &ComponentInfo<'_>) -> Diagnostic {
    Diagnostic::new(
        Code::RA001,
        call.span,
        format!(
            "`{name}()` is read in the body of component `{component}`, outside any tracking \
             scope; the read runs once and nothing re-runs it",
            name = call.name,
            component = component.display_name(),
        ),
    )
}

/// `RA002_EFFECT_WRITE`: a write inside an `effect(...)` argument.
fn effect_write(call: &AccessorCall) -> Diagnostic {
    Diagnostic::new(Code::RA002, call.span, effect_write_message(&call.name))
}

/// The `RA002` message for a signal named `name`, exposed so the RA002 text test
/// in [`crate::diagnostic`] can read the exact string this rule composes rather
/// than a copy of it.
#[cfg(test)]
pub(crate) fn ra002_message_for_test(name: &str) -> String {
    effect_write_message(name)
}

/// The one place the `RA002` message is written.
///
/// It states only what is unconditionally true: the effect re-runs for the
/// signals it reads, and the write executes as part of that run. It deliberately
/// makes **no** claim about a loop — the runtime throws instead — because
/// `rules.rs` flags a write whether or not the effect reads that signal, so a
/// self-referential description is false for `effect(() => { n(1) })`.
fn effect_write_message(name: &str) -> String {
    format!(
        "`{name}` is written inside an `effect(...)` argument; an effect re-runs for the \
         signals it reads, so this write runs as part of an effect run instead of deriving \
         a value"
    )
}

/// `RA009_IMPLICIT_EFFECT`: a write in the component body, including JSX slots.
fn render_write(call: &AccessorCall, component: &ComponentInfo<'_>) -> Diagnostic {
    Diagnostic::new(
        Code::RA009,
        call.span,
        format!(
            "`{name}` is written while component `{component}` renders; {consequence}",
            name = call.name,
            component = component.display_name(),
            consequence = crate::diagnostic::RA009_CONSEQUENCE,
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyze::signals::find_signal_bindings;
    use crate::source::parse_tsx;

    // Every fixture gets the runai import, because primitives are resolved by
    // import binding: a bare `signal(0)` is a plain function call to this rule.
    fn codes(body: &str) -> Vec<String> {
        let src = format!("import {{ signal, derived, effect }} from \"runai\"\n{body}");
        let alloc = oxc_allocator::Allocator::default();
        let program = parse_tsx(&alloc, &src).unwrap();
        let b = find_signal_bindings(program);
        check_signal_rules(program, &b)
            .iter()
            .map(|d| d.code.as_str().to_string())
            .collect()
    }

    #[test]
    fn write_in_event_handler_is_not_flagged() {
        let src = "export default function P() { const n = signal(0); return <button onClick={() => n(n() + 1)}>x</button> }\n";
        assert!(codes(src).is_empty(), "the counter's increment is correct");
    }

    #[test]
    fn read_in_deferred_callback_is_not_flagged() {
        let src = "export default function P() { const n = signal(0); setTimeout(() => console.log(n()), 10); return <p/> }\n";
        assert!(
            codes(src).is_empty(),
            "reads at call time, nothing is frozen"
        );
    }

    #[test]
    fn a_local_function_named_signal_is_not_a_primitive() {
        let src = "function signal2(v) { return () => v }\nexport default function P() { const signal = signal2; const n = signal(0); const x = n(); return <p/> }\n";
        assert!(
            codes(src).is_empty(),
            "shadowed name must not match the import"
        );
    }

    #[test]
    fn body_read_is_ra001() {
        let src =
            "export default function P() { const n = signal(0); const x = n(); return <p/> }\n";
        assert_eq!(codes(src), vec!["RA001_UNTRACKED_READ"]);
    }

    #[test]
    fn read_in_event_handler_is_not_flagged() {
        let src = "export default function P() { const n = signal(0); return <button onClick={() => n()}>x</button> }\n";
        assert!(codes(src).is_empty(), "event handler reads are correct");
    }

    #[test]
    fn read_in_jsx_slot_is_not_flagged() {
        let src = "export default function P() { const n = signal(0); return <p>{n()}</p> }\n";
        assert!(codes(src).is_empty());
    }

    #[test]
    fn write_inside_effect_is_ra002() {
        let src = "export default function P() { const n = signal(0); effect(() => { n(1) }); return <p/> }\n";
        assert_eq!(codes(src), vec!["RA002_EFFECT_WRITE"]);
    }

    #[test]
    fn write_during_render_is_ra009() {
        let src = "export default function P() { const n = signal(0); n(1); return <p/> }\n";
        assert_eq!(codes(src), vec!["RA009_IMPLICIT_EFFECT"]);
    }

    #[test]
    fn read_inside_effect_is_not_ra001() {
        let src = "export default function P() { const n = signal(0); effect(() => { n() }); return <p/> }\n";
        assert!(codes(src).is_empty());
    }

    // --- Shapes the brief names but does not pin ------------------------------

    /// An aliased import is still a primitive, so its `const` is still an accessor
    /// and a body read is still `RA001`.
    #[test]
    fn an_aliased_primitive_is_read_like_any_other() {
        let src = "import { signal as createSignal } from \"runai\"\nexport default function P() { const n = createSignal(0); const x = n(); return <p/> }\n";
        let alloc = oxc_allocator::Allocator::default();
        let program = parse_tsx(&alloc, src).unwrap();
        let b = find_signal_bindings(program);
        let codes: Vec<_> = check_signal_rules(program, &b)
            .iter()
            .map(|d| d.code.as_str().to_string())
            .collect();
        assert_eq!(codes, vec!["RA001_UNTRACKED_READ"]);
    }

    /// A read in an arrow that outlives the component is not frozen, so it is not
    /// `RA001`. `setTimeout` is the shape an agent writes most often.
    #[test]
    fn a_read_in_a_delayed_callback_is_not_frozen() {
        let src = "export default function P() { const n = signal(0); window.setTimeout(() => { const x = n(); }, 10); return <p/> }\n";
        assert!(
            codes(src).is_empty(),
            "the read happens when the timer fires"
        );
    }

    /// A write in an event handler runs on the event, which is the correct place for
    /// it, so it is neither `RA009` nor `RA002`.
    #[test]
    fn a_write_in_an_event_handler_is_not_flagged() {
        let src = "export default function P() { const n = signal(0); return <button onClick={() => { n(n() + 1) }}>x</button> }\n";
        assert!(codes(src).is_empty(), "{:?}", codes(src));
    }

    /// A read in a `derived` is a tracking scope, so it is owned by the derived.
    #[test]
    fn a_read_inside_a_derived_is_owned_by_it() {
        let src = "export default function P() { const n = signal(0); const d = derived(() => n() * 2); return <p>{d()}</p> }\n";
        assert!(codes(src).is_empty(), "{:?}", codes(src));
    }

    /// A write in an effect written as a `function` expression rather than an arrow is
    /// still `RA002`. This is the half of `function_body`'s `FunctionExpression` arm the
    /// suite did not exercise: if its span computation ever disagreed with the function
    /// visitor's, `RA002` would silently stop firing while every other test stayed green.
    #[test]
    fn a_write_in_a_function_expression_effect_is_ra002() {
        let src = "export default function P() { const n = signal(0); effect(function () { n(1) }); return <p/> }\n";
        assert_eq!(codes(src), vec!["RA002_EFFECT_WRITE"]);
    }

    /// The effect is resolved by symbol, so an aliased import is still the `RA002`
    /// scope. Otherwise `effect_scopes` would only be proven for the unaliased name.
    #[test]
    fn an_aliased_effect_is_still_an_effect_scope() {
        let src = "import { signal, effect as eff } from \"runai\"\nexport default function P() { const n = signal(0); eff(() => { n(1) }); return <p/> }\n";
        let alloc = oxc_allocator::Allocator::default();
        let program = parse_tsx(&alloc, src).unwrap();
        let b = find_signal_bindings(program);
        let found: Vec<_> = check_signal_rules(program, &b)
            .iter()
            .map(|d| d.code.as_str().to_string())
            .collect();
        assert_eq!(found, vec!["RA002_EFFECT_WRITE"]);
    }

    /// The mirror of `a_write_inside_a_derived_is_not_ra002`: a `resource` compute
    /// function re-runs when a signal it reads changes, so a write there has the same
    /// self-dependency shape — and is likewise not flagged. Pinned so the hole reads as
    /// a decision, not an oversight. See the module docs.
    #[test]
    fn a_write_inside_a_resource_is_not_flagged() {
        let src = "import { signal, resource } from \"runai\"\n\
                   export default function P() { const n = signal(0); const r = resource(() => { n(1); return 1 }); return <p/> }\n";
        let alloc = oxc_allocator::Allocator::default();
        let program = parse_tsx(&alloc, src).unwrap();
        let b = find_signal_bindings(program);
        let diagnostics = check_signal_rules(program, &b);
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
    }

    /// A write in a JSX slot is still a write during render: the slot runs while the
    /// markup is produced.
    #[test]
    fn a_write_in_a_jsx_slot_is_ra009() {
        let src = "export default function P() { const n = signal(0); return <p>{n(1)}</p> }\n";
        assert_eq!(codes(src), vec!["RA009_IMPLICIT_EFFECT"]);
    }

    /// A read and a write in the same body are two diagnostics, each at its own
    /// site, in source order.
    #[test]
    fn a_read_and_a_write_are_reported_in_source_order() {
        let src = "export default function P() { const n = signal(0); const x = n(); n(2); return <p/> }\n";
        assert_eq!(
            codes(src),
            vec!["RA001_UNTRACKED_READ", "RA009_IMPLICIT_EFFECT"]
        );
    }

    /// A write in a concise-arrow effect is still `RA002`: `effect(() => n(1))` has
    /// no block, so the write *is* the effect body.
    #[test]
    fn a_write_in_a_concise_effect_is_ra002() {
        let src = "export default function P() { const n = signal(0); effect(() => n(1)); return <p/> }\n";
        assert_eq!(codes(src), vec!["RA002_EFFECT_WRITE"]);
    }

    /// A parenthesized multi-line effect is still the effect scope.
    #[test]
    fn a_write_in_a_parenthesized_effect_is_ra002() {
        let src = "export default function P() { const n = signal(0); effect((() => { n(1) })); return <p/> }\n";
        assert_eq!(codes(src), vec!["RA002_EFFECT_WRITE"]);
    }

    /// The message must state only what is unconditionally true. `effect(() => { n(1) })`
    /// reads nothing, so it cannot re-run: a message claiming the write "makes its own
    /// run a dependency" is false here, while the code still fires (the rule flags
    /// every write in an effect body). Pin the message to the reading that holds for
    /// every site: it runs as part of a run, not as a derived value.
    #[test]
    fn the_ra002_message_does_not_claim_a_loop_for_an_effect_that_reads_nothing() {
        let src = "import { signal, effect } from \"runai\"\nexport default function P() { const n = signal(0); effect(() => { n(1) }); return <p/> }\n";
        let alloc = oxc_allocator::Allocator::default();
        let program = parse_tsx(&alloc, src).unwrap();
        let b = find_signal_bindings(program);
        let d = check_signal_rules(program, &b);
        assert_eq!(d.len(), 1, "{d:?}");
        let message = &d[0].message;
        for false_claim in [
            "makes its own run",
            "dependency of the run",
            "own run",
            "a loop",
        ] {
            assert!(
                !message.contains(false_claim),
                "RA002's message claims `{false_claim}` for an effect that reads nothing: {message}"
            );
        }
        assert!(
            message.contains("runs as part of an effect run"),
            "the message must state the unconditional truth: {message}"
        );
    }

    /// A write inside a `derived` is not one of these three codes: the plan assigns
    /// `RA002` to an effect only, and this rule must not claim a code the plan gave to
    /// another rule. The runtime does not reject this shape either — the compute writes
    /// `n` without having read it, so nothing joins it to this run (see the module docs)
    /// — it is simply undiagnosed here.
    #[test]
    fn a_write_inside_a_derived_is_not_ra002() {
        let src = "export default function P() { const n = signal(0); const d = derived(() => { n(1); return 1 }); return <p>{d()}</p> }\n";
        assert!(codes(src).is_empty(), "{:?}", codes(src));
    }

    /// A write in a function the component only *declares* is not a write during
    /// render: the function runs later, whoever calls it.
    #[test]
    fn a_write_in_a_declared_nested_function_is_not_ra009() {
        let src = "export default function P() { const n = signal(0); const bump = () => { n(1) }; return <p/> }\n";
        assert!(codes(src).is_empty(), "{:?}", codes(src));
    }

    /// The component need not be the default export: `RA001` is about any component
    /// body, and `find_components` covers every JSX-returning function in the file.
    #[test]
    fn a_read_in_a_named_component_is_ra001() {
        let src = "export function Badge() { const n = signal(0); const x = n(); return <b/> }\n\
                   export default function P() { return <p/> }\n";
        assert_eq!(codes(src), vec!["RA001_UNTRACKED_READ"]);
    }

    /// An arrow component's body span is its function body, not the whole arrow, and
    /// the read inside it is still `RA001`.
    #[test]
    fn a_read_in_an_arrow_component_body_is_ra001() {
        let src = "export default (props: Record<never, never>) => { const n = signal(0); const x = n(); return <p/> }\n";
        assert_eq!(codes(src), vec!["RA001_UNTRACKED_READ"]);
    }

    /// A read outside every component is not this rule's business: a module-level
    /// read is a different mistake the plan does not give a signal code to.
    #[test]
    fn a_module_level_read_is_not_ra001() {
        let src =
            "const n = signal(0)\nconst x = n()\nexport default function P() { return <p/> }\n";
        assert!(codes(src).is_empty(), "{:?}", codes(src));
    }

    /// Two components in one file: each read is attributed to its own component, and
    /// the message names the right one.
    #[test]
    fn reads_are_attributed_to_their_own_component() {
        let src = "export function Badge() { const n = signal(0); const x = n(); return <b/> }\n\
                   export default function P() { const m = signal(0); const y = m(); return <p/> }\n";
        let full = format!("import {{ signal }} from \"runai\"\n{src}");
        let alloc = oxc_allocator::Allocator::default();
        let program = parse_tsx(&alloc, &full).unwrap();
        let b = find_signal_bindings(program);
        let diagnostics = check_signal_rules(program, &b);
        assert_eq!(diagnostics.len(), 2, "{diagnostics:?}");
        assert!(
            diagnostics[0].message.contains("`Badge`"),
            "{diagnostics:?}"
        );
        assert!(diagnostics[1].message.contains("`P`"), "{diagnostics:?}");
    }

    /// A read in a `resource` compute function is a tracking scope too, so it is not
    /// `RA001`.
    #[test]
    fn a_read_inside_a_resource_is_owned_by_it() {
        let src = "import { signal, resource } from \"runai\"\n\
                   export default function P() { const n = signal(0); const r = resource(() => n()); return <p/> }\n";
        let alloc = oxc_allocator::Allocator::default();
        let program = parse_tsx(&alloc, src).unwrap();
        let b = find_signal_bindings(program);
        let diagnostics = check_signal_rules(program, &b);
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
    }

    /// A concise-arrow component's body span *is* the returned JSX, so its slots are
    /// still the render body: the write is `RA009`.
    #[test]
    fn a_write_in_a_concise_arrow_component_slot_is_ra009() {
        let src =
            "const n = signal(0)\nexport default (props: Record<never, never>) => <p>{n(1)}</p>\n";
        assert_eq!(codes(src), vec!["RA009_IMPLICIT_EFFECT"]);
    }

    /// A read and a write nested in a block inside the component body still have the
    /// component as their nearest function: a block is not a function.
    #[test]
    fn a_block_inside_the_component_body_is_still_the_component() {
        let src = "export default function P() { if (true) { const n = signal(0); const x = n() } return <p/> }\n";
        assert_eq!(codes(src), vec!["RA001_UNTRACKED_READ"]);
    }

    /// A write inside a callback nested in an effect is not `RA002`: the write runs
    /// when that callback runs, not with the effect's run. The same nearest-function
    /// rule that exempts an event handler exempts it.
    #[test]
    fn a_write_in_a_callback_nested_in_an_effect_is_not_ra002() {
        let src = "export default function P() { const n = signal(0); effect(() => { setTimeout(() => { n(1) }, 10) }); return <p/> }\n";
        assert!(codes(src).is_empty(), "{:?}", codes(src));
    }

    /// A write in a JSX *attribute* slot is still a write during render: the slot runs
    /// while the markup is produced.
    #[test]
    fn an_attribute_slot_is_checked_like_a_child_slot() {
        let reads =
            "export default function P() { const n = signal(0); return <p class={n()}>x</p> }\n";
        assert!(codes(reads).is_empty(), "{:?}", codes(reads));

        let writes =
            "export default function P() { const n = signal(0); return <p class={n(1)}>x</p> }\n";
        assert_eq!(codes(writes), vec!["RA009_IMPLICIT_EFFECT"]);
    }

    /// The RA009 message must not restate the false claim the skill and explain were
    /// corrected for: the runtime flushes writes synchronously, so a live subscriber
    /// *does* see the new value. The message may only say the component body runs once,
    /// so the write cannot re-run the component.
    #[test]
    fn the_write_during_render_message_states_only_the_true_consequence() {
        let src = "import { signal } from \"runai\"\nexport default function P() { const n = signal(0); n(1); return <p/> }\n";
        let alloc = oxc_allocator::Allocator::default();
        let program = parse_tsx(&alloc, src).unwrap();
        let b = find_signal_bindings(program);
        let diagnostics = check_signal_rules(program, &b);
        assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
        let message = &diagnostics[0].message;
        // The exact claim this message carried before: `flush` is synchronous, so a live
        // effect or `_setText` node already subscribed to `n` *does* re-run with the new
        // value.
        for false_claim in crate::diagnostic::RA009_FALSE_CLAIMS {
            assert!(
                !message.to_ascii_lowercase().contains(false_claim),
                "false claim `{false_claim}` in the message: {message}"
            );
        }
        // The exact shared sentence, not a fragment of it. `runs once` alone is passed by
        // "…runs once, so the write is never seen by a live reader", which is the false
        // claim this whole round is about.
        assert!(
            message.contains(crate::diagnostic::RA009_CONSEQUENCE),
            "the shared consequence is missing: {message}"
        );
    }

    /// The module docs are the one `RA009` site no other test reaches — re-inserting a
    /// denied claim there fails no other assertion. Three halves, because one alone is not
    /// enough:
    ///
    /// 1. the docs take the shared macro. `include_str!` sees source, not expansion, so
    ///    the assertion is on the attribute that pulls it. The needle is assembled with
    ///    `concat!` so this test's own source does not satisfy it;
    /// 2. the sentence is not written out literally in the file, which is what a hand-copy
    ///    of the shared sentence would do;
    /// 3. no entry of [`crate::diagnostic::RA009_FALSE_CLAIMS`] appears anywhere in the
    ///    file (including this comment), which is the half that fails when the round-1
    ///    reader claim is pasted back into the module docs.
    #[test]
    fn the_module_docs_state_only_the_true_consequence() {
        const SRC: &str = include_str!("rules.rs");
        let attribute = concat!("#![doc = crate::diagnostic::", "ra009_consequence!()]");
        assert!(
            SRC.contains(attribute),
            "the module docs must take the shared sentence, not a literal copy of it"
        );
        assert!(
            !SRC.contains(crate::diagnostic::RA009_CONSEQUENCE),
            "the module docs contain a hand-written copy of the shared sentence"
        );
        for false_claim in crate::diagnostic::RA009_FALSE_CLAIMS {
            assert!(
                !SRC.to_ascii_lowercase().contains(false_claim),
                "false claim `{false_claim}` in the module docs"
            );
        }
    }

    /// The message names the accessor and the component so the diagnostic is
    /// actionable without reading the whole file.
    #[test]
    fn the_message_names_the_accessor_and_the_component() {
        let src = "import { signal } from \"runai\"\nexport default function Counter() { const count = signal(0); const x = count(); return <p/> }\n";
        let alloc = oxc_allocator::Allocator::default();
        let program = parse_tsx(&alloc, src).unwrap();
        let b = find_signal_bindings(program);
        let diagnostics = check_signal_rules(program, &b);
        assert_eq!(diagnostics.len(), 1);
        let message = &diagnostics[0].message;
        assert!(message.contains("`count()`"), "{message}");
        assert!(message.contains("`Counter`"), "{message}");
    }

    /// The span is the whole call, `n()` and not just the callee `n`, so the agent's
    /// editor selects the read and the message points at it; the declaration is
    /// elsewhere and unchanged.
    #[test]
    fn the_span_points_at_the_read() {
        let src = "import { signal } from \"runai\"\n\
                   export default function P() { const n = signal(0); const x = n(); return <p/> }\n";
        let alloc = oxc_allocator::Allocator::default();
        let program = parse_tsx(&alloc, src).unwrap();
        let b = find_signal_bindings(program);
        let diagnostics = check_signal_rules(program, &b);
        let span = diagnostics[0].span;
        assert_eq!(&src[span.0 as usize..span.1 as usize], "n()");
    }
}
