//! `RA001`, `RA002`, and `RA009`: where a signal is read or written.
//!
//! All three answer one question about one call: *what owns this read or write?*
//!
//! - `RA001_UNTRACKED_READ`: a read whose nearest function is a component body and
//!   which is not inside a tracking scope. Nothing re-runs it, so the value on
//!   screen is the value at first render — the silently frozen value.
//! - `RA002_EFFECT_WRITE`: a write whose nearest function is the function passed to
//!   an `effect`. The effect re-runs when the signals it reads change and it reads
//!   the signal it writes, so the write feeds its own run.
//! - `RA009_IMPLICIT_EFFECT`: a write whose nearest function is a component body.
//!   Rendering must not have side effects: the write either runs once and leaves
//!   readers stale, or re-triggers the render that performed it.
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
    Diagnostic::new(
        Code::RA002,
        call.span,
        format!(
            "`{name}` is written inside an `effect(...)` argument; the effect re-runs when the \
             signals it reads change, so this write makes its own run a dependency",
            name = call.name,
        ),
    )
}

/// `RA009_IMPLICIT_EFFECT`: a write in the component body, including JSX slots.
fn render_write(call: &AccessorCall, component: &ComponentInfo<'_>) -> Diagnostic {
    Diagnostic::new(
        Code::RA009,
        call.span,
        format!(
            "`{name}` is written while component `{component}` renders; rendering must not have \
             side effects, so a write here either runs once or re-triggers the render that did it",
            name = call.name,
            component = component.display_name(),
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

    /// A write inside a `derived` is not one of these three codes: the plan assigns
    /// `RA002` to an effect only. The runtime rejects it, and this rule must not
    /// claim a code the plan gave to another rule.
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

    /// The span is the callee identifier, so the agent edits the read and not the
    /// declaration.
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
