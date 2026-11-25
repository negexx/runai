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

/// Emit `RA010` for every construct a `"use static"` route cannot run or ship.
///
/// A static route is prerendered to HTML at build time and ships **zero bytes** of
/// JavaScript, so there is no runtime on the page: a signal, a derived value, an
/// effect, a resource, a write, or a JSX event handler would silently do nothing,
/// and an inline handler or a literal `<script>` would ship the code the claim
/// says it does not. This is the compiler-enforced form of that guarantee, not an
/// observation about build output.
///
/// Four sources, all resolved, never matched by name:
///
/// - [`SignalBindings::primitive_calls`] — `signal(…)`, `derived(…)`, `effect(…)`,
///   `resource(…)` through the `"runai"` import. A local function that happens to
///   be called `signal` is an ordinary function and is not here, which is why this
///   reads the bindings rather than the text;
/// - [`AccessorCall::write`] — `count(next)`. The declaration it writes was already
///   reported as its own primitive call, at its own span;
/// - an `on…` JSX attribute. A static page has no runtime, so an `on…` attribute
///   there is either dead or live inline JavaScript, and both are refused: the rule
///   is a **prefix**, matched on the lowercased name because that is the name the
///   HTML tokenizer produces (`Onclick`, `ONCLICK`, and `oNclick` all reach the page
///   as a live `onclick`). It is deliberately not an enumerated event list — such a
///   list cannot be complete and nothing fails when it drifts. The accepted cost is
///   that `once="x"` and `onwards="x"` are refused in a static route; an island
///   still accepts them. See [`handler_form`];
/// - a literal `<script>` element. A static page ships no JavaScript, so a script
///   element is refused whether it is inline or pulls a file in. `<style>` is
///   deliberately **not** refused: CSS is not a runtime, and the guarantee is
///   about JavaScript, so a prerendered page may carry styles.
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

    let functions = FunctionBindings::collect(program);
    let markup = StaticMarkupCollector {
        functions: &functions.0,
        ..StaticMarkupCollector::default()
    };
    // The walker is `Visit`, which needs `&mut`; the collector is built first so
    // `functions` outlives it.
    let mut markup = markup;
    markup.visit_program(program);
    diagnostics.extend(
        markup
            .handlers
            .iter()
            .map(|&(span, form)| static_handler(span, form)),
    );
    diagnostics.extend(markup.scripts.iter().map(|&span| static_script(span)));

    // `RA016`: an attribute value the build cannot evaluate. Runs only for a static
    // route, and only over attribute expressions, because that is the one place a
    // function value reaches the emitted HTML string.
    diagnostics.extend(
        markup
            .function_attributes
            .iter()
            .map(|&(span, kind)| static_function_attribute(span, kind)),
    );

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

/// `RA010_STATIC_VIOLATION`: an `on…` attribute in a static route.
///
/// Three sentences, one per mechanism, because each is false for the other two: an
/// attribute the HTML keeps and the browser **does** run is code the page ships, so
/// "would never run" is wrong; a dropped `onClick={fn}` has no listener to attach,
/// so "ships code" is wrong; and a name neither convention classifies (`once`,
/// `onwards`, `onclickish`, `onÖ`) is refused only because the route is static, so
/// neither of the first two is true. Each site gets the one that is true for it.
fn static_handler(span: (u32, u32), form: HandlerForm) -> Diagnostic {
    let message = match form {
        HandlerForm::ShipsCode => {
            "a \"use static\" route ships no JavaScript, so this inline handler is refused: the \
             served HTML carries it as an `on…` attribute and the browser evaluates it as \
             JavaScript"
        }
        HandlerForm::NeverAttached => {
            "a \"use static\" route has no runtime, so this event handler would never be \
             attached or fire"
        }
        HandlerForm::Unclassified => {
            "a \"use static\" route has no runtime, so an `on…` attribute here is either dead or \
             inline JavaScript; both are refused"
        }
    };
    Diagnostic::new(Code::RA010, span, message)
}

/// `RA010_STATIC_VIOLATION`: a literal `<script>` element in a static route.
fn static_script(span: (u32, u32)) -> Diagnostic {
    Diagnostic::new(
        Code::RA010,
        span,
        "a \"use static\" route ships no JavaScript, so a `<script>` element in its markup \
         would load or run code the page promised not to ship",
    )
}

/// Whether this attribute is a handler in a `"use static"` route, and how the
/// emitted markup treats it.
///
/// **The static rule is a prefix, not a list.** A static page has no runtime, so an
/// `on…` attribute there is one of two things: dead, or live inline JavaScript. The
/// first is harmless to refuse and the second must be refused, so both are refused.
/// Any identifier whose name starts with `on` and has an event after it
/// (`onclick`, `onClick`, `Onclick`, `ONCLICK`, `oNclick`, …) is a handler.
///
/// The name is matched **lowercased**, because it is the HTML tokenizer's name: it
/// lowercases an attribute, so `ONCLICK="…"` reaches the served page as a live
/// `onclick` and must be refused as one. The as-written spelling is kept only for
/// the [`HandlerForm`] split below, which mirrors what the emitter does with it.
///
/// This is deliberately **not** an enumerated event list. Such a list cannot be
/// complete — `onemptied`, `onanimationstart`, `onbeforetoggle`, `oncontextlost`,
/// `onbeforematch`, and `onpointerrawupdate` were all missing from one — and nothing
/// fails when it drifts. The accepted trade is that `once="x"` and `onwards="x"`, two
/// ordinary attributes that happen to start with `on`, are refused in a static route.
/// They are vanishingly rare next to a live-JS hole, and an island still accepts them.
///
/// A namespaced attribute (`foo:onClick`, `xml:lang`) is never this shape.
fn handler_form(attribute: &oxc_ast::ast::JSXAttribute<'_>) -> Option<HandlerForm> {
    let oxc_ast::ast::JSXAttributeName::Identifier(name) = &attribute.name else {
        return None;
    };
    let spelling = name.name.as_str();
    let lowercased = spelling.to_ascii_lowercase();
    // `on` alone names no event, so it is an ordinary attribute. Anything longer is
    // an `on…` handler however its letters are capitalised.
    if !lowercased.starts_with("on") || lowercased.len() == 2 {
        return None;
    }

    let uppercase = is_uppercase_handler_name(spelling);

    let literal = matches!(
        attribute.value.as_ref(),
        Some(oxc_ast::ast::JSXAttributeValue::StringLiteral(_))
    );
    let expression = matches!(
        attribute.value.as_ref(),
        Some(oxc_ast::ast::JSXAttributeValue::ExpressionContainer(_))
    );

    // The emitter's own split, mirrored: a string literal is a static attribute, and
    // an expression is a static attribute unless `event_name` makes it a listener —
    // which needs `on` + a capital *as written*. So an uppercase expression is the one
    // form the prerender emitter drops; a bare attribute carries no value to evaluate.
    let carried = literal || (expression && !uppercase);
    if !carried {
        return Some(HandlerForm::NeverAttached);
    }
    // The HTML carries this attribute, but "the browser evaluates it" is true only
    // when the name is one HTML defines as a handler: `onclick` yes, `once` and
    // `onwards` no. That is a fact about the *name*, used only to pick the true
    // sentence — see [`is_known_handler_event`]. It can never weaken or widen the
    // refusal, which happened above on the prefix.
    Some(if is_known_handler_event(&lowercased) {
        HandlerForm::ShipsCode
    } else {
        HandlerForm::Unclassified
    })
}

/// Whether `lowercased` (`on…`) is one of HTML's inline event handler content
/// attributes such as `onclick` or `onkeydown`.
///
/// **This list is for the message only.** [`handler_form`] already decided that
/// the attribute is refused, on the prefix; nothing here can accept one. Its job
/// is narrower and honest: the `ShipsCode` sentence claims *the browser evaluates
/// the attribute as JavaScript*, which is true of a real handler name and false of
/// `once` or `onwards`. Drift is safe in the only direction it can go — a real
/// handler this list has not heard of falls to [`HandlerForm::Unclassified`], whose
/// sentence is true for every `on…` name — so an out-of-date entry can only make the
/// message less specific, never false, and it cannot make the rule incomplete
/// because the rule does not consult it.
fn is_known_handler_event(lowercased: &str) -> bool {
    const EVENTS: &[&str] = &[
        "onabort",
        "onafterprint",
        "onanimationstart",
        "onauxclick",
        "onbeforeinput",
        "onbeforematch",
        "onbeforeprint",
        "onbeforetoggle",
        "onbeforeunload",
        "onblur",
        "oncancel",
        "oncanplay",
        "oncanplaythrough",
        "onchange",
        "onclick",
        "onclose",
        "oncontextlost",
        "oncontextmenu",
        "oncopy",
        "oncuechange",
        "oncut",
        "ondblclick",
        "ondrag",
        "ondragend",
        "ondragenter",
        "ondragleave",
        "ondragover",
        "ondragstart",
        "ondrop",
        "ondurationchange",
        "onemptied",
        "onended",
        "onerror",
        "onfocus",
        "onfocusin",
        "onfocusout",
        "onformdata",
        "onhashchange",
        "oninput",
        "oninvalid",
        "onkeydown",
        "onkeypress",
        "onkeyup",
        "onlanguagechange",
        "onload",
        "onloadeddata",
        "onloadedmetadata",
        "onloadstart",
        "onmessage",
        "onmessageerror",
        "onmousedown",
        "onmouseenter",
        "onmouseleave",
        "onmousemove",
        "onmouseout",
        "onmouseover",
        "onmouseup",
        "onoffline",
        "ononline",
        "onpagehide",
        "onpageshow",
        "onpaste",
        "onpause",
        "onplay",
        "onplaying",
        "onpointercancel",
        "onpointerdown",
        "onpointerenter",
        "onpointerleave",
        "onpointermove",
        "onpointerout",
        "onpointerover",
        "onpointerrawupdate",
        "onpointerup",
        "onpopstate",
        "onprogress",
        "onratechange",
        "onrejectionhandled",
        "onreset",
        "onresize",
        "onscroll",
        "onscrollend",
        "onsecuritypolicyviolation",
        "onseeked",
        "onseeking",
        "onselect",
        "onselectionchange",
        "onselectstart",
        "onslotchange",
        "onstalled",
        "onstorage",
        "onsubmit",
        "onsuspend",
        "ontimeupdate",
        "ontoggle",
        "ontransitionend",
        "onunhandledrejection",
        "onunload",
        "onvolumechange",
        "onwaiting",
        "onwheel",
    ];
    EVENTS.contains(&lowercased)
}

/// What the emitted markup does with a handler attribute, so the message can say
/// the true mechanism instead of one sentence that is false for some shapes.
#[derive(Clone, Copy)]
enum HandlerForm {
    /// The HTML carries the attribute and its lowercased name is one HTML defines
    /// as a handler, so the browser evaluates it as JavaScript: `onclick="alert(1)"`,
    /// `onClick="alert(1)"` (HTML lowercases the name), and `onclick={expr}` (the
    /// prerender emitter interpolates the value into the attribute) all land in the
    /// served page as live code.
    ShipsCode,
    /// The prerender emitter writes nothing for it (`Binding::Event`), so there is
    /// no listener on the page: `onClick={fn}`, and a bare `onClick` (or `onclick`)
    /// with no value to carry.
    NeverAttached,
    /// The HTML carries the attribute, but its name is not one HTML defines as a
    /// handler (`once`, `onwards`, `onclickish`, `onÖ`). The browser does not run
    /// it; it is refused only because a static route has no runtime. Neither
    /// "ships code" nor "never attached" is true here, so this form says only the
    /// static fact.
    Unclassified,
}

/// Whether `name` starts with a lowercase `on` followed by an ASCII uppercase
/// letter (`onClick`) — the React-style convention this project already uses.
///
/// Used **only** for the [`HandlerForm`] split, never to decide whether something
/// is a handler: the emitter's [`crate::ir::event_name`] uses the same
/// `is_ascii_uppercase` test, so this is the one fact that predicts whether an
/// expression-valued attribute becomes a listener. It must stay ASCII: a Unicode
/// `is_uppercase` would call `onÖ={1}` a listener while the emitter lowers it to a
/// plain attribute and interpolates the value, so the message would say it is never
/// attached about an attribute the HTML carries. Deciding the attribute is a
/// handler happens on the lowercased name, above.
fn is_uppercase_handler_name(name: &str) -> bool {
    name.strip_prefix("on")
        .and_then(|rest| rest.chars().next())
        .is_some_and(|c| c.is_ascii_uppercase())
}

/// Every refused handler attribute and literal `<script>` element in the file.
///
/// `script` is matched only as a **lowercase** identifier tag: a capitalized
/// `<Script>` is a component tag, which `check_jsx` already refuses, and a
/// namespaced tag is not an HTML element.
#[derive(Default)]
struct StaticMarkupCollector<'f> {
    handlers: Vec<((u32, u32), HandlerForm)>,
    scripts: Vec<(u32, u32)>,
    /// `RA016`: attribute expressions whose value is a function, with the kind of
    /// expression it was, so the message can name the repair for that shape.
    ///
    /// Only non-literal expressions reach here — a string, a template with no
    /// substitutions, and a numeric or boolean literal are all values the build
    /// knows, so they are not collected. A bare identifier is resolved against
    /// `functions`, because `const cls = () => "hello"` is exactly the reported
    /// defect and a syntactic "is it a function here" test misses it.
    function_attributes: Vec<((u32, u32), FunctionValueKind)>,
    /// The names bound to a function expression anywhere in the file.
    ///
    /// Filled by [`FunctionBindings`] before the markup walk, so resolution does
    /// not depend on source order: `const cls = …` above the markup and a hoisted
    /// `function cls() {}` below it resolve the same way.
    functions: &'f [String],
}

/// What kind of expression produced a function-valued attribute.
///
/// The two shapes need different repairs, so the message says which one the file
/// has rather than one sentence that is wrong for the other.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FunctionValueKind {
    /// `cls={…}` where the value is an inline arrow or function expression:
    /// `class={() => "a"}`.
    Inline,
    /// `cls={name}` where `name` is bound to a function expression in this file.
    Binding,
}

/// Collect every name bound to a function expression in the file.
///
/// One walk, no semantic build: the declarator's initializer is enough to answer
/// "is this name a function value", and `RA016` is static-mode only, so the
/// precision a `SymbolId` would buy is not worth a semantic build on every static
/// route. A shadowed name therefore resolves to the outer binding's answer in the
/// rare case — worst case one extra refusal, never a silent wrong page.
struct FunctionBindings(Vec<String>);

impl FunctionBindings {
    fn collect(program: &oxc_ast::ast::Program<'_>) -> Self {
        let mut collector = FunctionBindingCollector(Vec::new());
        collector.visit_program(program);
        FunctionBindings(collector.0)
    }
}

struct FunctionBindingCollector(Vec<String>);

impl<'a> Visit<'a> for FunctionBindingCollector {
    fn visit_variable_declarator(&mut self, declarator: &oxc_ast::ast::VariableDeclarator<'a>) {
        if declarator
            .init
            .as_ref()
            .is_some_and(|init| is_function_expression(init))
            && let Some(binding) = declarator.id.get_binding_identifier()
        {
            self.0.push(binding.name.as_str().to_string());
        }
        oxc_ast_visit::walk::walk_variable_declarator(self, declarator);
    }

    fn visit_function(
        &mut self,
        function: &oxc_ast::ast::Function<'a>,
        flags: oxc_syntax::scope::ScopeFlags,
    ) {
        // A `function cls() {}` declaration (not an expression) also binds a
        // function value, and it hoists, so it must be found even when the markup
        // above it is what refers to it.
        if let Some(id) = &function.id {
            self.0.push(id.name.as_str().to_string());
        }
        oxc_ast_visit::walk::walk_function(self, function, flags);
    }
}

/// Whether `expression` is a function value — the thing `_setAttr` calls.
///
/// Parentheses are seen through, because `class={(cls)}` is the same value as
/// `class={cls}`.
fn is_function_expression(expression: &oxc_ast::ast::Expression<'_>) -> bool {
    match expression {
        oxc_ast::ast::Expression::ArrowFunctionExpression(_)
        | oxc_ast::ast::Expression::FunctionExpression(_) => true,
        oxc_ast::ast::Expression::ParenthesizedExpression(parenthesized) => {
            is_function_expression(&parenthesized.expression)
        }
        _ => false,
    }
}

impl<'f, 'a> Visit<'a> for StaticMarkupCollector<'f> {
    fn visit_jsx_attribute(&mut self, attribute: &oxc_ast::ast::JSXAttribute<'a>) {
        if let Some(form) = handler_form(attribute) {
            let span = match &attribute.name {
                oxc_ast::ast::JSXAttributeName::Identifier(name) => {
                    (name.span.start, name.span.end)
                }
                // `handler_form` already returns `None` for a namespaced name.
                oxc_ast::ast::JSXAttributeName::NamespacedName(_) => return,
            };
            self.handlers.push((span, form));
        } else {
            // `RA016`: a non-handler attribute whose value cannot be evaluated at
            // build time. A handler is `RA010`'s row and is skipped here, so one
            // root cause stays one diagnostic.
            if let Some(kind) = self.function_attribute(attribute) {
                self.function_attributes
                    .push(((attribute.span.start, attribute.span.end), kind));
            }
        }
        oxc_ast_visit::walk::walk_jsx_attribute(self, attribute);
    }

    fn visit_jsx_element(&mut self, element: &oxc_ast::ast::JSXElement<'a>) {
        if let oxc_ast::ast::JSXElementName::Identifier(name) = &element.opening_element.name
            && name.name.as_str() == "script"
        {
            self.scripts.push((name.span.start, name.span.end));
        }
        oxc_ast_visit::walk::walk_jsx_element(self, element);
    }
}

impl StaticMarkupCollector<'_> {
    /// The kind of function-valued attribute `attribute` carries, if any.
    ///
    /// Four literal shapes are values the build already knows and are therefore
    /// **not** refused, because refusing them would break legal static routes:
    /// a string (`class="a"`), a numeric literal (`tabindex={0}`), a boolean
    /// literal, and a template literal with no substitutions. Everything else is
    /// either a function value (refused) or a plain expression the prerender
    /// module evaluates at build time (`class={tone}`) — the second is
    /// `RA010`/`rechable`'s question, not this rule's.
    fn function_attribute(
        &self,
        attribute: &oxc_ast::ast::JSXAttribute<'_>,
    ) -> Option<FunctionValueKind> {
        let oxc_ast::ast::JSXAttributeValue::ExpressionContainer(container) =
            attribute.value.as_ref()?
        else {
            return None;
        };
        let expression = container.expression.to_expression();
        if is_function_expression(expression) {
            return Some(FunctionValueKind::Inline);
        }
        // A bare identifier is the reported shape: `const cls = () => "hello"`,
        // then `class={cls}`. `_setAttr` in an island calls it; a static template
        // would write the function's own source text.
        if let oxc_ast::ast::Expression::Identifier(identifier) = expression
            && self
                .functions
                .iter()
                .any(|name| name == identifier.name.as_str())
        {
            return Some(FunctionValueKind::Binding);
        }
        None
    }
}

/// `RA016_STATIC_FUNCTION_ATTRIBUTE`: an attribute value the build cannot
/// evaluate.
///
/// The static emitter writes one HTML string at build time, so a function value is
/// not text, and writing the function's source into the attribute is a page whose
/// class is `() => "hello"`. A hydrated island calls it and writes the result, so
/// the same source would render two different attributes with nothing reported.
/// The message names the shape the file has, because the repair differs: an inline
/// function is deleted, a binding is replaced with a literal.
fn static_function_attribute(span: (u32, u32), kind: FunctionValueKind) -> Diagnostic {
    let construct = match kind {
        FunctionValueKind::Inline => {
            "gives an attribute an inline function value, which a \"use static\" route cannot \
             call at build time"
        }
        FunctionValueKind::Binding => {
            "gives an attribute a function value, which a \"use static\" route cannot call at \
             build time"
        }
    };
    Diagnostic::new(
        Code::RA016,
        span,
        format!(
            "{construct}: a prerendered page writes the function's own source as the attribute"
        ),
    )
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

    /// The `RA016` fixtures, driven through `check_static_mode` on the original
    /// source — the same path `analyze` takes.
    fn function_attribute_codes(src: &str) -> Vec<String> {
        let alloc = oxc_allocator::Allocator::default();
        let program = parse_tsx(&alloc, src).unwrap();
        let b = find_signal_bindings(program);
        check_static_mode(crate::source::Mode::Static, program, &b)
            .iter()
            .filter(|d| d.code == Code::RA016)
            .map(|d| d.code.as_str().to_string())
            .collect()
    }

    /// The reported defect, verbatim: `const cls = () => "hello"` then
    /// `class={cls}` in a `"use static"` route was accepted and shipped
    /// `class="() =&gt; &quot;hello&quot;"`.
    #[test]
    fn a_function_valued_attribute_is_refused_in_a_static_route() {
        let src = "\"use static\"\nexport default function P(props: Record<never, never>) {\n  const cls = () => \"hello\"\n  return <div class={cls}>x</div>\n}\n";
        assert_eq!(
            function_attribute_codes(src),
            vec!["RA016_STATIC_FUNCTION_ATTRIBUTE"]
        );
    }

    /// An inline function is the same value with no name to resolve, so the rule
    /// cannot depend on finding a binding.
    #[test]
    fn an_inline_function_attribute_is_refused() {
        let src = "\"use static\"\nexport default function P(props: Record<never, never>) { return <div class={() => \"x\"}>y</div> }\n";
        assert_eq!(
            function_attribute_codes(src),
            vec!["RA016_STATIC_FUNCTION_ATTRIBUTE"]
        );
    }

    /// A `function` declaration hoists, so it can be referred to above its
    /// definition; the binding scan therefore cannot be source-order dependent.
    #[test]
    fn a_hoisted_function_binding_is_refused_too() {
        let src = "\"use static\"\nexport default function P(props: Record<never, never>) { return <div class={cls}>x</div> }\nfunction cls() { return \"a\" }\n";
        assert_eq!(
            function_attribute_codes(src),
            vec!["RA016_STATIC_FUNCTION_ATTRIBUTE"]
        );
    }

    /// The control that keeps the rule honest: every attribute form the existing
    /// static tests cover must still pass, or the refusal has broken a legal page.
    /// Each of these is a value the build evaluates — a string, a template with no
    /// substitutions, numeric and boolean literals, and a plain expression naming a
    /// local constant.
    #[test]
    fn legal_static_attribute_forms_are_not_refused() {
        for body in [
            "<div class=\"a\">x</div>",
            "<div title={`lit`}>x</div>",
            "<div tabindex={0}>x</div>",
            "<div hidden={true}>x</div>",
            "<div class={tone}>x</div>",
            "<div data-n={n}>x</div>",
            "<input disabled/>",
        ] {
            let src = format!(
                "\"use static\"\nexport default function P(props: Record<never, never>) {{ const tone = \"warm\"; const n = 3; return {body} }}\n"
            );
            assert_eq!(
                function_attribute_codes(&src),
                Vec::<String>::new(),
                "a legal static attribute was refused: {body}"
            );
        }
    }

    /// An island calls a function value through `_setAttr`, so the same source is
    /// correct there. This is the half that must **not** be refused: refusing it
    /// would make the framework's documented accessor pattern (`class={tone}`)
    /// depend on the value's kind.
    #[test]
    fn an_island_accepts_a_function_valued_attribute() {
        let src = "\"use island\"\nexport default function P(props: Record<never, never>) {\n  const cls = () => \"hello\"\n  return <div class={cls}>x</div>\n}\n";
        let alloc = oxc_allocator::Allocator::default();
        let program = parse_tsx(&alloc, src).unwrap();
        let b = find_signal_bindings(program);
        let diagnostics = check_static_mode(crate::source::Mode::Island, program, &b);
        assert!(
            diagnostics.iter().all(|d| d.code != Code::RA016),
            "an island calls the function, so this is not a refusal: {diagnostics:?}"
        );
    }

    /// A handler is `RA010`'s row. `RA016` must not add a second row for the same
    /// attribute, or one root cause reports twice.
    #[test]
    fn a_handler_attribute_is_ra010_and_not_also_ra016() {
        let src = "\"use static\"\nexport default function P(props: Record<never, never>) { return <button onClick={() => {}}>x</button> }\n";
        let alloc = oxc_allocator::Allocator::default();
        let program = parse_tsx(&alloc, src).unwrap();
        let b = find_signal_bindings(program);
        let diagnostics = check_static_mode(crate::source::Mode::Static, program, &b);
        assert!(
            diagnostics.iter().any(|d| d.code == Code::RA010),
            "the handler is RA010: {diagnostics:?}"
        );
        assert!(
            diagnostics.iter().all(|d| d.code != Code::RA016),
            "the handler must not also be RA016: {diagnostics:?}"
        );
    }
}
