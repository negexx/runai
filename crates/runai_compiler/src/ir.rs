//! Typed TSX to a template-and-bindings IR.
//!
//! [`crate::source`] answers "is this file valid TSX?", [`crate::analyze`]
//! answers "is it a route this milestone can render, and does it use only
//! constructs this milestone lowers?", and [`crate::strip`] turns typed TSX into
//! plain JavaScript with the JSX intact. This module answers the last question:
//! *what markup does it produce?*
//!
//! The IR splits a JSX tree into the two things codegen needs to keep apart:
//!
//! - **static structure** — tags, static attribute values, text — which becomes
//!   one template string that never enters the runtime, and
//! - **bindings and expression slots** — attributes and events wired after the
//!   template is cloned, and child expressions that keep their position.
//!
//! # The three rules that shape this IR
//!
//! - **Lowering runs on type-stripped JavaScript.** [`RouteIr::statements`] and
//!   every [`IrNode::Expr`] are emitted into a `.mjs` module, so `n: number` or
//!   `props: Record<never, never>` there is a syntax error. [`strip_types`] runs
//!   first and this module walks its output; it never walks the typed source.
//! - **Text follows the standard JSX whitespace rule.** Text with no line break
//!   is kept verbatim; text with a line break is split, each line trimmed, empty
//!   lines dropped, and the rest joined with single spaces. So
//!   `<div>\n  <p>a</p>\n</div>` has no whitespace child at all, and
//!   `<p> a  b </p>` keeps its spaces. This is the rule TypeScript and React
//!   apply, and [`crate::analyze::jsx::check_jsx`] refuses the one case where it
//!   deletes a space the author probably wanted.
//! - **Entities are never decoded or re-encoded.** `&amp;` survives as written.
//!   A compiler that re-encodes it writes `&amp;amp;` and the page shows
//!   `&amp;`.
//!
//! # What is lowered
//!
//! | JSX | IR |
//! |---|---|
//! | `<div a="b">` | [`IrNode::Element`], attribute in `static_attrs` |
//! | `name={expr}` | [`Binding::Attr`] |
//! | `onEvent={expr}` | [`Binding::Event`], `name` being the DOM event name |
//! | `{expr}` as a child | [`IrNode::Expr`], in its position |
//! | `<></>` as a child | no node of its own; its children are spliced into the parent |
//!
//! A component tag (`<Greeting/>`) is not lowered: it has no DOM node to
//! address, and inlining it needs a module graph this milestone does not have.
//! [`crate::analyze::jsx::check_jsx`] refuses it first, so lowering never sees
//! it — the arm here exists so a missed rule is a diagnostic rather than markup
//! the DOM cannot parse.

use oxc_allocator::Allocator;
use oxc_ast::ast::{
    Declaration, Expression, JSXAttributeItem, JSXAttributeName, JSXAttributeValue, JSXChild,
    JSXElement, JSXExpression, Program, Statement,
};
use oxc_ast_visit::Visit;
use oxc_span::{GetSpan, Span};

use crate::analyze::jsx::{
    BODY_JSX_CONSTRUCT, BODY_JSX_REPAIR, MARKER_HOSTILE, MODULE_JSX_CONSTRUCT, MODULE_JSX_REPAIR,
    ancestor_refusal, dom_tag, element_has_binding, jsx_in_body_statements,
    jsx_outside_route_render, jsx_returns, jsx_spans, marker_hostile_message, parent_child_refusal,
    route_render, self_closing_refusal, text_refusal,
};
use crate::analyze::{ComponentInfo, find_route_component, validate_route_component};
use crate::diagnostic::{Code, Diagnostic};
use crate::source::{Mode, detect_mode, has_line_break, lines, parse_tsx, raw_text};
use crate::strip::strip_types;

/// One dynamic slot on an element.
///
/// An element's `bindings` holds attributes *only*: a dynamic child is an
/// [`IrNode::Expr`] in `children`, at its source position, so nothing has to
/// pair a separate list against the child order. That positional design is what
/// keeps `a {n} b` from turning into `a b` with `n` appended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Binding {
    /// `name={expr}` on an element: an attribute set from an expression.
    Attr { name: String, expr: String },
    /// `onEvent={expr}` on an element. `name` is the **DOM event name**
    /// (`"click"`, `"keydown"`), not the JSX attribute name (`onClick`).
    Event { name: String, expr: String },
}

/// One node of the route's markup.
///
/// Static content lives in an [`IrNode::Element`]'s `tag`, `static_attrs`, and
/// in [`IrNode::Text`]; everything that has to run lives in `bindings` or in an
/// [`IrNode::Expr`]. There is deliberately no serde here: the IR is an internal
/// compiler type, and only `codegen` reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IrNode {
    /// A lowercase tag: a real DOM element.
    ///
    /// `static_attrs` and `bindings` are each in source order, but their
    /// *relative* order is not kept — they are separate lists by design, because
    /// the template only ever holds the static half. `static_attrs` keeps each
    /// value exactly as written, entities included; a bare attribute
    /// (`<input disabled />`) is `("disabled", "")`.
    Element {
        tag: String,
        static_attrs: Vec<(String, String)>,
        bindings: Vec<Binding>,
        children: Vec<IrNode>,
    },
    /// Text, after the standard JSX whitespace rule.
    ///
    /// A single-line text node is verbatim. A node containing a line break has
    /// had each line trimmed, empty lines dropped, and the rest joined with
    /// single spaces. Entities are never touched.
    Text(String),
    /// A dynamic child slot, in its position among its siblings.
    ///
    /// The string is JavaScript with types already stripped, because it is
    /// copied into the emitted module. An attribute-style slot is a
    /// [`Binding`]; this is the child-style one, and its position in `children`
    /// is the whole point — the emitter walks `children` in order.
    Expr(String),
}

/// The element names that have no closing tag, per the HTML standard.
///
/// One list, two consumers: [`crate::codegen::prerender`] omits the closing tag
/// for these, and [`crate::analyze::jsx::check_jsx`] refuses one that has
/// children (a void element cannot hold them, and the static template and an
/// island's DOM would disagree about what happens to them). It lives here rather
/// than in `codegen` so `analyze` can reach it without depending on the emitter.
pub(crate) const VOID_ELEMENTS: &[&str] = &[
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "source", "track",
    "wbr",
];

/// The names a `"use static"` route's emitted module declares for itself.
///
/// The prerender module is one `_esc` helper, one `_attr` helper, and
/// `export function render()`, so a route file that declares or references one of
/// these names would shadow a helper the template calls — or export `render`
/// twice. [`crate::analyze::emitted_names`] refuses them from this one list, and
/// `codegen::prerender`'s test asserts the emitter defines exactly these names, so
/// a helper added without a rule (or a rule for a helper that no longer exists)
/// fails there rather than shipping a shadowing hole.
///
/// The island module's own names are [`ISLAND_LOCALS`], kept separate because they
/// are a different emitter's locals; the island set a route owes also includes
/// every import specifier the module writes, which is data, not a constant.
pub const PRERENDER_NAMES: &[&str] = &["_esc", "_attr", "render"];

/// The names the **prerendered** module *exports*, as opposed to binds.
///
/// Only `render` is both declared and exported: `_esc` and `_attr` are
/// module-scope `const`s the template calls, and the island module exports nothing
/// of the compiler's at all (its only exports are the route's own, sliced from the
/// source). So only exporting `render` is a *duplicate export*; exporting any
/// other reserved name exports a binding the artifact owns but does not export.
/// The distinction is a constant because a message that asserted "duplicate
/// export" for `_esc` was a shipped false diagnostic.
pub const PRERENDER_EXPORTS: &[&str] = &["render"];

/// The names the **island** module exports of its own: none.
///
/// The island emitter writes one import, the helpers, the template, the module
/// statements, the component, and the `mount` call. Nothing it writes is an export
/// of the compiler's, so an export of a reserved name there is a collision with
/// the module's bindings, never a duplicate export.
pub const ISLAND_EXPORTS: &[&str] = &[];

/// The names an **island** module declares for itself, independent of the route.
///
/// The island emitter's own locals: `_c` (the marker collector), `_a` (the marker
/// list), `_el` (the cloned fragment), and `_t0` (the hoisted template). They are
/// deliberately prefixed so a route's own `el`, `a`, or `t0` cannot collide — Task
/// 11's renaming ruling. [`crate::analyze::emitted_names`] reserves these **plus**
/// every import specifier the module actually writes, which is data rather than a
/// constant: it depends on the route.
pub const ISLAND_LOCALS: &[&str] = &["_c", "_a", "_el", "_t0"];

/// A lowered route file: its rendering mode, its component name, the
/// component's own statements, the file's module-level statements, and its
/// markup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteIr {
    pub mode: Mode,
    /// The route component's name, or `Page` when it is genuinely anonymous.
    /// `"default"` would name the export rather than a symbol in the file, and
    /// `export default function default(...)` is a parse error — so diagnostic
    /// text and codegen both need the readable name.
    pub name: String,
    /// Every statement in the route component's body *before* its JSX `return`,
    /// as stripped JavaScript, statements separated by newlines.
    ///
    /// This is what the island emitter puts above the template, so it must be
    /// plain JavaScript: `const n: number = signal(0)` here would not parse in
    /// a `.mjs`. A concise arrow body (`(props) => <p/>`) has no statements and
    /// this is empty.
    pub statements: String,
    /// Every statement at **module scope** in the route file, as stripped
    /// JavaScript, statements separated by newlines and kept in source order.
    ///
    /// Design §7.6 expects module-level state — "module-scope signals are
    /// per-island too, since each island is its own module instance" — so the
    /// emitters write this above the component rather than dropping it. A
    /// module-level `const title = "About"` referenced from the markup is
    /// therefore emittable, and [`crate::analyze::reachable`] treats it as
    /// reachable.
    ///
    /// Four things are **not** here, each for a reason the emitters own:
    ///
    /// - the route component itself (its own declaration, and the `export
    ///   default …`/`export { … as default }` statement that names it) — the
    ///   emitters declare the component from `name` and `statements`;
    /// - `import` declarations — the island emitter writes exactly one import,
    ///   from the runtime URL, and the prerender module writes none at all;
    /// - a re-export (`export … from "./x"`, `export * from "./x"`) — it points
    ///   at another module, and the emitted module either has one import (an
    ///   island's, from the runtime URL) or none. `check_imports` refuses it,
    ///   `"runai"` sources included, so this is the invariant, not the agent's
    ///   first signal;
    /// - type declarations, which [`strip_types`] already erased.
    ///
    /// It is guaranteed **JSX-free**: [`crate::analyze::jsx::check_jsx`] refuses
    /// module-level JSX in a route file before lowering runs, and
    /// [`lower_route`] repeats the refusal as an invariant. JSX here would be a
    /// syntax error in the emitted `.mjs` exactly as it would be in `statements`.
    ///
    /// **Ordering note for the emitters:** a module-level statement may read an
    /// earlier one (`const raw = …; const items = raw.split(…)`), so these must
    /// keep their relative order. Writing them above the component is the natural
    /// placement; a module-level statement that *names the route component* is
    /// only safe then if the route is a function declaration, which is hoisted —
    /// no test pins that edge yet, and refusing it is Task 10/11's call.
    pub module_statements: String,
    pub tree: IrNode,
}

/// The one entry point: strip types, parse the result as JSX, lower the route.
///
/// The caller is expected to have run [`crate::analyze::jsx::check_jsx`] on the
/// original source. This function therefore reports errors that are *internal
/// invariants* — a file the parser rejects, a route the analyzer rejects, a
/// construct a rule was supposed to refuse — and never the agent's first signal.
/// [`lower_route`] runs on [`strip_types`]'s output because every expression
/// here is emitted as JavaScript.
///
/// **Its spans index the stripped source, not the agent's file.** Type stripping
/// rewrites the text, so an offset surfaced from here may not point at the same
/// byte in the file the agent opened. That is why `check_jsx` runs first on the
/// original source: its spans are the agent's own, and it refuses every construct
/// that would produce a diagnostic here.
///
/// # `lower_route` is not a gate, and the emitters are `pub(crate)`
///
/// `lower_route` is deliberately **not** a verdict: it reads
/// [`RouteIr::statements`] and [`RouteIr::module_statements`] but does not know
/// whether a name in them resolves, because that needs a semantic pass. So a
/// caller must gate on [`crate::analyze::analyze`] first — and Task 10/11's
/// emitters (`emit_prerender`, `emit_client`) are `pub(crate)` **by design**,
/// with only their `*_checked` forms public. Those checked forms are the ones
/// that call `analyze`; the unchecked pair exists only for their own unit tests.
/// The naming is the invariant: a `pub` emitter name that does not end in
/// `_checked` would be a path from a source file to an artifact with no rule
/// pass in between, which is the accept-then-reject class this task closes.
///
/// # Errors
///
/// A `Vec<Diagnostic>` rather than one diagnostic, because a parse failure can
/// report several and a route validation reports whichever reason it found.
pub fn lower_route(source: &str) -> Result<RouteIr, Vec<Diagnostic>> {
    let stripped = strip_types(source)?;

    let alloc = Allocator::default();
    let program = parse_tsx(&alloc, &stripped)?;
    let mode = detect_mode(program, &stripped).map_err(|diagnostic| vec![diagnostic])?;

    let info = find_route_component(program);
    let diagnostics = validate_route_component(info.as_ref());
    if !diagnostics.is_empty() {
        return Err(diagnostics);
    }
    // `validate_route_component` refuses a missing component, so this is `Some`.
    let info = info.ok_or_else(|| vec![unlowerable_export()])?;

    // The walk that finds the returned root is the same walk `check_jsx` uses,
    // so "which function is the route, and which node does it return" has one
    // answer in this crate.
    let render = route_render(program).ok_or_else(|| vec![unlowerable_export()])?;

    // `statements` is copied into the emitted module as JavaScript, so JSX there
    // is the same hazard as JSX inside an expression: a parse failure with no
    // other report. `check_jsx` refuses it on the original source; this is the
    // invariant that keeps the two lists equal. The construct and repair are
    // shared with the analysis, so the two cannot describe it differently.
    if let (Some(body), Some(return_span)) = (render.body, render.return_span) {
        let returned = jsx_returns(body);
        if let Some(span) = jsx_in_body_statements(body, return_span, &returned) {
            return Err(vec![unsupported(span, BODY_JSX_CONSTRUCT, BODY_JSX_REPAIR)]);
        }
    }

    // A module-level statement's JSX would land in `module_statements`, which is
    // copied into the emitted `.mjs`, where it is a syntax error — the same
    // hazard as a body statement, one scope out. `check_jsx` refuses it on the
    // original source; this is the invariant that keeps the two lists equal, and
    // the construct and repair are shared so they cannot be described two ways.
    if let Some(span) = jsx_outside_route_render(program, &render) {
        return Err(vec![unsupported(
            span,
            MODULE_JSX_CONSTRUCT,
            MODULE_JSX_REPAIR,
        )]);
    }

    let tree = Lowerer {
        source: &stripped,
        island: matches!(mode, Mode::Island),
    }
    .root(render.root)?;

    Ok(RouteIr {
        mode,
        name: info.display_name().to_string(),
        statements: statements(&stripped, render.body, render.return_span),
        module_statements: module_statements(&stripped, program, &info),
        tree,
    })
}

/// The body statements both emitters copy into the generated component: every
/// statement before the JSX `return` that replaces the template.
///
/// One definition, two consumers: [`statements`] slices this range to text and
/// [`crate::analyze::reachable`] walks the same statements for references. A
/// statement after the `return` is unreachable code and is never emitted, so it
/// is not walked either. A concise body (`(props) => <p/>`) has no body
/// statements — the JSX *is* the body — and yields an empty iterator.
pub(crate) fn emitted_body_statements<'a>(
    body: &'a oxc_ast::ast::FunctionBody<'a>,
    return_span: Span,
) -> impl Iterator<Item = &'a Statement<'a>> {
    body.statements
        .iter()
        .filter(move |statement| statement.span().end <= return_span.start)
}

/// Every statement in the route body before the JSX `return`, as stripped
/// JavaScript.
///
/// Slicing from the first statement to the `return` keeps multi-line statements
/// (a nested function, a multi-line call) exactly as they are and drops the
/// `return` itself, which is the one statement the template replaces. A concise
/// body (`(props) => <p/>`) is the JSX, so there is nothing to emit.
///
/// The statements sliced are exactly [`emitted_body_statements`] — the range the
/// reference walk visits — so the two cannot disagree about what the generated
/// component declares.
fn statements(
    source: &str,
    body: Option<&oxc_ast::ast::FunctionBody>,
    return_span: Option<Span>,
) -> String {
    let (Some(body), Some(return_span)) = (body, return_span) else {
        return String::new();
    };

    let Some(first) = emitted_body_statements(body, return_span).next() else {
        return String::new();
    };

    let start = first.span().start as usize;
    let end = return_span.start as usize;
    source.get(start..end).map_or_else(String::new, |text| {
        text.trim_end_matches(|c: char| c.is_whitespace())
            .to_string()
    })
}

/// Every module-scope statement of the route file that the emitters should
/// write, as stripped JavaScript, in source order.
///
/// Design §7.6 expects module-level state ("module-scope signals are per-island
/// too"), so the emitters need the file's module level, not just the component
/// body. Three kinds of statement are **not** included, each because an emitter
/// owns it, and a fourth is absent for a different reason — the same list
/// [`RouteIr::module_statements`] documents:
///
/// - `import` declarations — the island emitter writes exactly one import from
///   the runtime URL and the prerender module writes none, so an import here
///   would be a duplicate or an unresolvable module;
/// - the route component's own declaration, and the `export default …` /
///   `export { … as default }` statement that names it — the emitters declare
///   the component from [`RouteIr::name`] and [`RouteIr::statements`], so
///   keeping the original statement would declare it twice;
/// - a re-export (`export … from "x"`, `export * from "x"`) — it names a second
///   module, and the emitted module writes one import (the island's) or none, so
///   there is nothing to re-export from. `check_imports` refuses the statement
///   first, `"runai"` sources included;
/// - type declarations — not excluded here but **absent**, because
///   [`strip_types`] already erased them from the source this function slices.
///
/// Slicing each statement's own span keeps multi-line declarations exactly as
/// written, and keeps source order, which matters for a module-level `const` a
/// later one reads.
fn module_statements(source: &str, program: &Program<'_>, route: &ComponentInfo<'_>) -> String {
    let mut parts: Vec<String> = Vec::new();
    for part in emitted_module_parts(program, route) {
        match part {
            EmittedModulePart::Statement(statement) => {
                if let Some(text) = raw_text(source, statement.span()) {
                    parts.push(
                        text.trim_end_matches(|c: char| c.is_whitespace())
                            .to_string(),
                    );
                }
            }
            EmittedModulePart::Declarators {
                kind,
                exported,
                declarators,
            } => {
                let kind = match kind {
                    oxc_ast::ast::VariableDeclarationKind::Const => "const",
                    oxc_ast::ast::VariableDeclarationKind::Let => "let",
                    oxc_ast::ast::VariableDeclarationKind::Var => "var",
                    oxc_ast::ast::VariableDeclarationKind::Using => "using",
                    oxc_ast::ast::VariableDeclarationKind::AwaitUsing => "await using",
                };
                let mut kept: Vec<&str> = Vec::new();
                for declarator in declarators {
                    match raw_text(source, declarator.span()) {
                        Some(text) => kept.push(text.trim()),
                        None => return String::new(),
                    }
                }
                let export = if exported { "export " } else { "" };
                parts.push(format!("{export}{kind} {};", kept.join(", ")));
            }
        }
    }
    parts.join("\n")
}

/// The part of the file's module level that one emitter writes.
///
/// [`module_statements`] renders these to text and
/// [`crate::analyze::reachable`] walks them for references. Two consumers, one
/// definition of "emitted", so a statement cannot be carried into the module
/// without its names being checked, or checked while never being written.
pub(crate) enum EmittedModulePart<'a, 'b> {
    /// One whole statement, sliced from its own span and emitted verbatim.
    Statement(&'b Statement<'a>),
    /// The non-route declarators of a statement that also declares the route.
    /// Rebuilt with the same declaration kind and `export` modifier.
    Declarators {
        kind: oxc_ast::ast::VariableDeclarationKind,
        exported: bool,
        declarators: Vec<&'b oxc_ast::ast::VariableDeclarator<'a>>,
    },
}

impl<'a, 'b> EmittedModulePart<'a, 'b> {
    /// Visit exactly the nodes the emitted module carries for this part.
    ///
    /// A declarator's own binding id is a `BindingIdentifier`, not an
    /// `IdentifierReference`, so a visitor that records references does not see
    /// the name being *declared* — only the names it *reads*.
    pub(crate) fn visit(&self, visitor: &mut impl Visit<'a>) {
        match self {
            EmittedModulePart::Statement(statement) => visitor.visit_statement(statement),
            EmittedModulePart::Declarators { declarators, .. } => {
                for declarator in declarators {
                    visitor.visit_variable_declarator(declarator);
                }
            }
        }
    }
}

/// Every module-level part the emitters write, in source order.
///
/// Three kinds of statement are **not** here, each because an emitter owns it:
/// an `import` ([`is_emitter_owned_statement`]); the route component's own
/// declaration and its `export … default` (the emitters rebuild the component
/// from [`RouteIr::name`] and [`RouteIr::statements`]); and a re-export
/// (`export … from "x"`, `export * from "x"`), which names a second module the
/// emitted file does not import. `check_imports` refuses that last kind first,
/// `"runai"` sources included.
///
/// A **type declaration** is a fourth kind [`RouteIr::module_statements`] never
/// contains, but this function does not exclude it: [`strip_types`] erased it
/// before the emitters' tree exists. The typed tree `analyze` parses still has
/// it, along with the other statements `strip_types` erases — a `declare`
/// statement and a bodyless overload signature — so the two trees differ by
/// those statements, not by the type declaration alone. That is why the
/// cross-tree test asserts the difference rather than ignoring it: it counts the
/// typed tree's extra parts, so any *other* difference fails.
///
/// This is the one definition of "emitted" **among the parts a caller can
/// ask about**: [`module_statements`] renders these parts to text and
/// [`crate::analyze::reachable`] walks them for references, so the walk and the
/// artifact agree for any one tree.
///
/// **What it does not prove:** `analyze` and `lower_route` do not hand this
/// function the same tree. `analyze` parses the *typed* source, and
/// `lower_route` parses [`strip_types`]'s output, which is a re-print — so the
/// spans differ, and a type declaration exists in one and not the other. The
/// agreement is per-tree. What keeps the two callers' *decisions* equal is that
/// neither re-derives the exclusion set: both call this one function. A type
/// annotation changing which statements are emitted would be a `strip_types`
/// bug, not a second definition of "emitted". Cross-tree equality for a typed
/// fixture is asserted by
/// `emitted_parts_agree_across_the_typed_and_stripped_trees`.
pub(crate) fn emitted_module_parts<'a, 'b>(
    program: &'b Program<'a>,
    route: &ComponentInfo<'a>,
) -> Vec<EmittedModulePart<'a, 'b>> {
    let mut parts = Vec::new();
    for statement in &program.body {
        if is_emitter_owned_statement(statement) {
            continue;
        }
        // A declaration that *is* the route is the emitters' own. One statement
        // can declare both the route and an ordinary value
        // (`const title = "A", Page = …`), and only the route declarator is
        // dropped: dropping the whole statement would lose `title` while the
        // tree still names it.
        if !contains_start(statement.span(), route.signature_span.0) {
            parts.push(EmittedModulePart::Statement(statement));
            continue;
        }
        if let Some(part) = rebuilt_declaration(statement, route) {
            parts.push(part);
        }
    }
    parts
}

/// The part of a `const`/`let`/`var` statement that is **not** the route, or
/// `None` when there is nothing left to emit.
///
/// Only a multi-declarator statement that also declares the route can produce
/// this: `const a = 1, Page = …` keeps `const a = 1`, with the same declaration
/// kind and the same `export` modifier. A single-declarator statement that *is*
/// the route is dropped entirely, which is the ordinary shape.
fn rebuilt_declaration<'a, 'b>(
    statement: &'b Statement<'a>,
    route: &ComponentInfo<'a>,
) -> Option<EmittedModulePart<'a, 'b>> {
    let (declaration, exported) = match statement {
        Statement::VariableDeclaration(declaration) => (declaration.as_ref(), false),
        Statement::ExportDeclaration(named) => match &named.declaration {
            Declaration::VariableDeclaration(declaration) => (declaration.as_ref(), true),
            // `export function Page(…)` and `export default …` are the route's
            // own declaration: there is nothing to rebuild.
            _ => return None,
        },
        _ => return None,
    };

    let declarators: Vec<&'b oxc_ast::ast::VariableDeclarator<'a>> = declaration
        .declarations
        .iter()
        .filter(|declarator| !is_route_declarator(declarator, route.name_span))
        .collect();
    if declarators.is_empty() {
        return None;
    }

    Some(EmittedModulePart::Declarators {
        kind: declaration.kind,
        exported,
        declarators,
    })
}

/// Whether `statement` is one the emitters own outright, so
/// [`RouteIr::module_statements`] never contains it.
///
/// - an `import`: the island emitter writes exactly one, from the runtime URL,
///   and the prerender module writes none;
/// - `export default …` and `export { … as default }`: the route's export, which
///   the emitters rebuild from [`RouteIr::name`] and [`RouteIr::statements`];
/// - `export … from "./x"` and `export * from "./x"`: a re-export names another
///   module, and the emitted module imports nothing from it. A route file has
///   nothing legitimate to re-export in this milestone, and `check_imports`
///   refuses the statement first, so this is the invariant rather than the
///   agent's first signal. `export { helper }` for a *local* binding is a
///   different statement and stays.
fn is_emitter_owned_statement(statement: &Statement<'_>) -> bool {
    match statement {
        Statement::ImportDeclaration(_) => true,
        Statement::ExportDefaultDeclaration(_) => true,
        Statement::ExportAllDeclaration(_) | Statement::ExportFromDeclaration(_) => true,
        Statement::ExportNamedDeclaration(declaration)
            if declaration
                .specifiers
                .iter()
                .any(|specifier| specifier.exported.name().as_str() == "default") =>
        {
            true
        }
        _ => false,
    }
}

/// Whether a `const`/`let`/`var` declarator declares the route component.
///
/// One statement can declare both the route and an ordinary value
/// (`const title = "A", Page = (props) => …`), and only the route declarator is
/// the emitters' own: [`emitted_module_parts`] drops just that one, and
/// [`crate::analyze::reachable`] walks just the others.
pub(crate) fn is_route_declarator(
    declarator: &oxc_ast::ast::VariableDeclarator<'_>,
    route_name: (u32, u32),
) -> bool {
    let id_span = declarator.id.span();
    id_span.start <= route_name.0 && route_name.0 < id_span.end
}

/// Whether `start` falls inside `span`.
fn contains_start(span: Span, start: u32) -> bool {
    span.start <= start && start < span.end
}

/// The tree walk. Holds the **stripped** source `program` was parsed from,
/// because every text and expression node is a slice of it and every slice
/// becomes JavaScript.
struct Lowerer<'s> {
    source: &'s str,
    /// Whether the route is an island, which decides the marker-hostile rule: a
    /// `"use static"` template interpolates a string and never places a `<!>`
    /// marker, so only an island can be broken by a binding in a text-only
    /// element. `mode` is already in scope in [`lower_route`]; this carries it to
    /// the walk rather than re-detecting it.
    island: bool,
}

impl<'s> Lowerer<'s> {
    /// The source text a span names.
    ///
    /// The same rule as [`crate::source::raw_text`]; sharing it is what keeps the
    /// two from drifting into different failure modes. A span from this source's
    /// own AST is always in bounds, so the `None` arm is a constructed-AST case:
    /// it is a `debug_assert!` plus an empty slice rather than a panic, because a
    /// panic here would replace every diagnostic with a backtrace.
    fn slice(&self, span: Span) -> &'s str {
        match raw_text(self.source, span) {
            Some(text) => text,
            None => {
                debug_assert!(false, "span {span:?} fell outside the lowered source");
                ""
            }
        }
    }

    /// The route's root node.
    ///
    /// A fragment is refused here rather than spliced: it has no parent to be
    /// spliced into, and the IR's root is one node because both emitters write
    /// one root. As a *child*, a fragment is transparent — see [`Self::children`].
    /// `check_jsx` refuses this shape too, so an agent sees that diagnostic
    /// rather than this one.
    fn root(&self, root: &'s Expression<'s>) -> Result<IrNode, Vec<Diagnostic>> {
        match root {
            Expression::JSXElement(element) => self.element(element, &[]),
            Expression::JSXFragment(fragment) => Err(vec![unsupported(
                fragment.span,
                "a fragment as the route's returned JSX",
                crate::analyze::jsx::FRAGMENT_ROOT_REPAIR,
            )]),
            // `route_render` only returns JSX, so this is unreachable.
            other => Err(vec![unlowerable_root(other.span())]),
        }
    }

    /// One JSX element.
    ///
    /// `ancestors` is the tag of every enclosing element, outermost first — the
    /// same stack [`crate::analyze::jsx::check_jsx`] keeps — so the reparse rules
    /// ([`crate::analyze::jsx::parent_child_refusal`] and
    /// [`crate::analyze::jsx::ancestor_refusal`]) can be repeated here as the
    /// invariant the analysis first enforces. A caller that skips the analysis gets
    /// a diagnostic rather than a template the browser will reshape.
    fn element(
        &self,
        element: &'s JSXElement<'s>,
        ancestors: &[String],
    ) -> Result<IrNode, Vec<Diagnostic>> {
        // The one tag rule, shared with `check_jsx`: `<div>` is a DOM element,
        // anything else has no DOM lowering and this milestone cannot inline it.
        // `check_jsx` reports the refusal first; this is the invariant that keeps
        // it out of the template even if a caller skips the analysis, which is
        // why the two lists cannot drift — they are the same function.
        let tag = dom_tag(&element.opening_element.name).map_err(|refusal| {
            vec![unsupported(
                element.opening_element.name.span(),
                &refusal.construct,
                &refusal.repair,
            )]
        })?;
        let name = tag.to_string();

        // The reparse refusals, repeated as invariants. `check_jsx` reports them
        // first; sharing the messages means a caller that skips analysis sees the
        // same words, not a second rule. The same-kind rule runs first, at the
        // same precedence `check_jsx` uses, so the two lists cannot disagree.
        if let Some(message) = self_closing_refusal(&name, ancestors) {
            return Err(vec![reparse_invariant(element.span, message)]);
        }
        if let Some(parent) = ancestors.last()
            && let Some(message) = parent_child_refusal(parent, &name)
        {
            return Err(vec![reparse_invariant(element.span, message)]);
        }
        if let Some(message) = ancestor_refusal(&name, ancestors) {
            return Err(vec![reparse_invariant(element.span, message)]);
        }
        if let Some(message) = text_refusal(&name) {
            let fostered = element.children.iter().any(|child| {
                matches!(child, JSXChild::Text(text) if !text.value.as_str().trim().is_empty())
            });
            if fostered {
                return Err(vec![reparse_invariant(element.span, message)]);
            }
        }
        // The marker-hostile rule, repeated here because `template_html` and
        // `lower_route` are both `pub`: without it a caller could get a template
        // whose marker count differs from `sites()`. Island-only, and the same
        // predicate and message `check_jsx` uses.
        if self.island && MARKER_HOSTILE.contains(&name.as_str()) && element_has_binding(element) {
            return Err(vec![reparse_invariant(
                element.span,
                marker_hostile_message(&name),
            )]);
        }

        let mut static_attrs: Vec<(String, String)> = Vec::new();
        let mut bindings: Vec<Binding> = Vec::new();

        for attribute in &element.opening_element.attributes {
            let JSXAttributeItem::Attribute(attribute) = attribute else {
                return Err(vec![unsupported(
                    attribute.span(),
                    "a spread attribute (`{...expr}`)",
                    crate::analyze::jsx::SPREAD_ATTRIBUTE_REPAIR,
                )]);
            };

            let name = attribute_name(&attribute.name);
            let namespaced = matches!(attribute.name, JSXAttributeName::NamespacedName(_));

            match attribute.value.as_ref() {
                // `<input disabled />`: present with no value. The empty string is
                // what HTML itself means by it, and codegen decides the spelling.
                None => static_attrs.push((name, String::new())),
                Some(JSXAttributeValue::StringLiteral(literal)) => {
                    static_attrs.push((name, literal_value(self.source, literal)));
                }
                Some(JSXAttributeValue::ExpressionContainer(container)) => {
                    let expr = self
                        .expression_source(&container.expression, "attribute")?
                        .ok_or_else(|| {
                            vec![unsupported(
                                container.span,
                                "an attribute with an empty expression (`name={}`)",
                                crate::analyze::jsx::EMPTY_ATTRIBUTE_REPAIR,
                            )]
                        })?
                        .to_string();

                    // `xml:lang` has no listener form, so a namespaced name is
                    // always an attribute even when it looks like `on…`.
                    match event_name(&name).filter(|_| !namespaced) {
                        Some(event) => bindings.push(Binding::Event { name: event, expr }),
                        None => bindings.push(Binding::Attr { name, expr }),
                    }
                }
                Some(other) => {
                    return Err(vec![unsupported(
                        other.span(),
                        "an attribute whose value is itself JSX (`name=<Bar />`)",
                        crate::analyze::jsx::JSX_ATTRIBUTE_REPAIR,
                    )]);
                }
            }
        }

        let mut nested = ancestors.to_vec();
        nested.push(name.clone());
        let children = self.children(&element.children, &nested)?;

        Ok(IrNode::Element {
            tag: name,
            static_attrs,
            bindings,
            children,
        })
    }

    /// Lower an element's children in document order.
    ///
    /// A nested element keeps its own `bindings`; a fragment contributes no node
    /// of its own, so its children are spliced here with the same list: that is
    /// what `<div><><b/></></div>` rendering exactly `<div><b/></div>` means.
    ///
    /// `ancestors` is the enclosing tag stack, passed through unchanged to a child
    /// element and to a spliced fragment's children — a fragment adds no tag to the
    /// ancestry, exactly as [`crate::analyze::jsx::check_jsx`] keeps the two in
    /// step.
    fn children(
        &self,
        children: &'s [JSXChild<'s>],
        ancestors: &[String],
    ) -> Result<Vec<IrNode>, Vec<Diagnostic>> {
        let mut nodes = Vec::new();

        for child in children {
            match child {
                JSXChild::Text(text) => {
                    if let Some(text) = lower_text(self.slice(text.span)) {
                        nodes.push(IrNode::Text(text));
                    }
                }
                JSXChild::Element(element) => nodes.push(self.element(element, ancestors)?),
                JSXChild::Fragment(fragment) => {
                    nodes.extend(self.children(&fragment.children, ancestors)?);
                }
                JSXChild::ExpressionContainer(container) => {
                    // `{}` and `{/* a comment */}` both parse to an empty
                    // expression. There is nothing to render, so they contribute
                    // no node — and the comment's text must not become an
                    // expression.
                    let Some(expr) = self.expression_source(&container.expression, "child slot")?
                    else {
                        continue;
                    };
                    nodes.push(IrNode::Expr(expr.to_string()));
                }
                JSXChild::Spread(spread) => {
                    return Err(vec![unsupported(
                        spread.span,
                        "a spread child (`{...expr}`)",
                        crate::analyze::jsx::SPREAD_CHILD_REPAIR,
                    )]);
                }
            }
        }

        Ok(nodes)
    }

    /// The source text of an expression inside `{…}`, or `None` when the braces
    /// are empty.
    ///
    /// The *inner* expression span is used, so the braces are not part of the
    /// text codegen receives. `check_jsx` refuses JSX inside such an expression
    /// first — the text becomes JavaScript in the emitted module, where `<b/>` is
    /// a syntax error. The walk here is the invariant, not a second opinion:
    /// both walk the same expression AST.
    fn expression_source(
        &self,
        expression: &'s JSXExpression<'s>,
        position: &str,
    ) -> Result<Option<&'s str>, Vec<Diagnostic>> {
        match expression {
            JSXExpression::EmptyExpression(_) => Ok(None),
            other => {
                if let Some(span) = contains_jsx(other.to_expression()) {
                    return Err(vec![unsupported(
                        span,
                        &format!("JSX inside a `{{…}}` expression ({position})"),
                        crate::analyze::jsx::JSX_IN_EXPRESSION_REPAIR,
                    )]);
                }
                Ok(Some(self.slice(other.span())))
            }
        }
    }
}

/// The standard JSX whitespace rule, applied to one text node.
///
/// `None` means the node disappears. Text with no line break is kept verbatim;
/// text with a line break is split, each line trimmed, empty lines dropped, and
/// the rest joined with single spaces — the rule TypeScript and React apply, and
/// the rule `check_jsx` reports the ambiguous case of.
fn lower_text(raw: &str) -> Option<String> {
    if !has_line_break(raw) {
        return (!raw.is_empty()).then(|| raw.to_string());
    }

    let joined = lines(raw)
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ");

    (!joined.is_empty()).then_some(joined)
}

/// The span of the first `JSXElement`/`JSXFragment` anywhere inside `expression`,
/// or `None`.
///
/// A walk, not a substring search: `a < b` and `x => y < z` are ordinary
/// expressions whose text happens to contain `<`, and refusing them would reject
/// valid routes.
fn contains_jsx(expression: &Expression) -> Option<Span> {
    jsx_spans(|finder| finder.visit_expression(expression))
        .into_iter()
        .next()
}

/// A JSX string literal's value, exactly as written between the quotes.
///
/// JSX attribute strings have no escape sequences, and the parser deliberately
/// leaves character references (`&amp;`) undecoded, so the parsed value *is* the
/// source text. The `None` case is a lone surrogate, which only a constructed
/// (not parsed) AST can hold; there the raw slice is the fallback.
fn literal_value(source: &str, literal: &oxc_ast::ast::StringLiteral) -> String {
    literal.value.as_str().map_or_else(
        || {
            let raw = raw_text(source, literal.span).unwrap_or("");
            let inner = raw
                .strip_prefix('"')
                .and_then(|rest| rest.strip_suffix('"'))
                .or_else(|| {
                    raw.strip_prefix('\'')
                        .and_then(|rest| rest.strip_suffix('\''))
                });
            inner.unwrap_or(raw).to_string()
        },
        str::to_string,
    )
}

/// An attribute's name as written, namespace included (`xml:lang`).
fn attribute_name(name: &JSXAttributeName) -> String {
    match name {
        JSXAttributeName::Identifier(identifier) => identifier.name.to_string(),
        JSXAttributeName::NamespacedName(namespaced) => namespaced.to_string(),
    }
}

/// The DOM event name an `on…` attribute listens for, or `None` if it is an
/// ordinary attribute.
///
/// `on` plus a capital is the listener form (`onClick`, `onKeyDown`); the event
/// name is the rest lowercased — `"click"`, `"keydown"`. Requiring the capital
/// is what keeps `once={x}` an *attribute* rather than a listener for a `"ce"`
/// event that does not exist.
///
/// There is deliberately **no whitelist** of real DOM events: the rule is this
/// milestone's specification, and a typo or an invented name (`onDoubleClick` →
/// `"doubleclick"`) becomes a listener that never fires. `_on` attaches it
/// without complaint, because the browser does. A whitelist would turn a legal
/// custom event (`onCustomThing`) into a compiler error, and validating event
/// names is not this milestone's question.
fn event_name(name: &str) -> Option<String> {
    let rest = name.strip_prefix("on")?;
    if !rest.starts_with(|c: char| c.is_ascii_uppercase()) {
        return None;
    }
    Some(rest.to_ascii_lowercase())
}

/// A construct this milestone cannot lower.
///
/// The code is `RA014_NOT_SUPPORTED_YET`, whose whole identity is "valid syntax
/// this milestone does not support". `RA007` means "the parser rejected this
/// file" everywhere else, and reusing it here would send an agent hunting for an
/// unclosed tag in a file that parses.
fn unsupported(span: Span, construct: &str, repair: &str) -> Diagnostic {
    Diagnostic::new(
        Code::RA014,
        (span.start, span.end),
        format!("cannot lower {construct} yet: {repair}"),
    )
}

/// The route analyzer found a component, but this module could not locate the JSX
/// it returns.
///
/// Unreachable while the two walkers agree; it exists so a divergence is a
/// diagnostic an agent can read instead of a panic or empty output.
fn unlowerable_export() -> Diagnostic {
    // `span: (0, 0)` renders as `file:1:1`, which claims a real location.
    Diagnostic::new(
        Code::RA011,
        (0, 0),
        "route component's returned JSX could not be located: return JSX directly from the \
         default export, and report the route if this diagnostic persists",
    )
}

/// A root expression that is not JSX, after [`route_render`] already proved it is.
fn unlowerable_root(span: Span) -> Diagnostic {
    Diagnostic::new(
        Code::RA014,
        (span.start, span.end),
        "cannot lower a root that is not JSX: return exactly one element from the route \
         component",
    )
}

/// A markup shape the HTML parser would rewrite, repeated as lowering's invariant.
///
/// The message is [`crate::analyze::jsx::parent_child_refusal`],
/// [`crate::analyze::jsx::ancestor_refusal`], or
/// [`crate::analyze::jsx::text_refusal`]'s — the same words `check_jsx` reports, so a
/// caller that skipped analysis sees one rule, not two. The spans here index the
/// **stripped** source, but the checked emitters run `check_jsx` on the original
/// first, so an agent's first signal is the analysis's.
fn reparse_invariant(span: Span, message: String) -> Diagnostic {
    Diagnostic::new(Code::RA014, (span.start, span.end), message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostic::Code;

    #[test]
    fn static_element_has_no_bindings() {
        let src =
            "\"use static\"\nexport default function P() { return <div class=\"a\">hi</div> }\n";
        let r = crate::ir::lower_route(src).unwrap();
        match r.tree {
            IrNode::Element {
                tag,
                static_attrs,
                bindings,
                children,
                ..
            } => {
                assert_eq!(tag, "div");
                assert_eq!(
                    static_attrs,
                    vec![("class".to_string(), "a".to_string())],
                    "the static half is what the template carries"
                );
                assert!(bindings.is_empty());
                assert!(matches!(children[0], IrNode::Text(ref t) if t == "hi"));
            }
            other => panic!("expected element, got {other:?}"),
        }
    }

    #[test]
    fn expression_slot_becomes_an_expr_child_in_position() {
        let src = "\"use island\"\nexport default function P() { const n = signal(0); return <p>a {n()} b</p> }\n";
        let r = crate::ir::lower_route(src).unwrap();
        match r.tree {
            IrNode::Element {
                bindings, children, ..
            } => {
                assert!(bindings.is_empty());
                assert_eq!(children.len(), 3);
                assert!(matches!(children[1], IrNode::Expr(ref e) if e.trim() == "n()"));
            }
            other => panic!("expected element, got {other:?}"),
        }
        assert!(r.statements.contains("signal(0)"));
    }

    #[test]
    fn types_are_stripped_before_lowering() {
        let src = "\"use island\"\nexport default function P(props: Record<never, never>) { const n: number = 1; return <p>{n as number}</p> }\n";
        let r = crate::ir::lower_route(src).unwrap();
        assert!(!r.statements.contains(": number"), "{}", r.statements);
        match r.tree {
            IrNode::Element { children, .. } => {
                assert!(matches!(children[0], IrNode::Expr(ref e) if e.trim() == "n"));
            }
            other => panic!("expected element, got {other:?}"),
        }
    }

    #[test]
    fn block_level_line_breaks_are_dropped_like_jsx() {
        let src = "\"use static\"\nexport default function P() { return <div>\n  <p>a</p>\n  <p>b</p>\n</div> }\n";
        let r = crate::ir::lower_route(src).unwrap();
        match r.tree {
            IrNode::Element { children, .. } => assert_eq!(children.len(), 2),
            other => panic!("expected element, got {other:?}"),
        }
    }

    #[test]
    fn event_attribute_becomes_an_event_binding() {
        let src = "\"use island\"\nexport default function P() { return <button onClick={go}>x</button> }\n";
        let r = crate::ir::lower_route(src).unwrap();
        match r.tree {
            IrNode::Element { bindings, .. } => {
                assert!(matches!(bindings[0], Binding::Event { ref name, .. } if name == "click"));
            }
            other => panic!("expected element, got {other:?}"),
        }
    }

    #[test]
    fn text_entities_are_preserved_verbatim() {
        let src = "\"use static\"\nexport default function P() { return <p>a &amp; b</p> }\n";
        let r = crate::ir::lower_route(src).unwrap();
        match r.tree {
            IrNode::Element { children, .. } => {
                assert!(matches!(children[0], IrNode::Text(ref t) if t.contains("&amp;")));
            }
            other => panic!("expected element, got {other:?}"),
        }
    }

    // --- Salvaged from the shipped lowering, still real behaviour ---------------

    /// The static half is what the template carries: a lowering that dropped it
    /// would still pass a bindings-only assertion.
    #[test]
    fn static_attributes_are_verbatim() {
        let src =
            "\"use static\"\nexport default function P() { return <div title=\"a &amp; b\" /> }\n";
        let r = crate::ir::lower_route(src).unwrap();
        let IrNode::Element {
            static_attrs,
            bindings,
            ..
        } = r.tree
        else {
            panic!("expected element")
        };
        assert_eq!(
            static_attrs,
            vec![("title".to_string(), "a &amp; b".to_string())]
        );
        assert!(bindings.is_empty());
    }

    /// An expression attribute is a binding, not static markup — the distinction
    /// the whole IR exists for.
    #[test]
    fn expression_attribute_becomes_an_attr_binding() {
        let src = "\"use island\"\nexport default function P() { return <p class={tone}>x</p> }\n";
        let r = crate::ir::lower_route(src).unwrap();
        let IrNode::Element {
            static_attrs,
            bindings,
            ..
        } = r.tree
        else {
            panic!("expected element")
        };
        assert!(static_attrs.is_empty(), "got {static_attrs:?}");
        assert_eq!(
            bindings,
            vec![Binding::Attr {
                name: "class".to_string(),
                expr: "tone".to_string(),
            }]
        );
    }

    /// A bare attribute has no value and is not a binding.
    #[test]
    fn bare_attribute_is_static() {
        let src = "\"use static\"\nexport default function P() { return <input disabled /> }\n";
        let r = crate::ir::lower_route(src).unwrap();
        let IrNode::Element {
            static_attrs,
            bindings,
            children,
            ..
        } = r.tree
        else {
            panic!("expected element")
        };
        assert_eq!(static_attrs, vec![("disabled".to_string(), String::new())]);
        assert!(bindings.is_empty());
        assert!(children.is_empty(), "self-closing element has no children");
    }

    /// `on` + a capital is the listener form, and the event name is the rest
    /// lowercased. A lowercased `onclick` survives as an attribute name.
    #[test]
    fn event_names_are_dom_event_names() {
        for (attr, expected) in [
            ("onClick", Some("click")),
            ("onKeyDown", Some("keydown")),
            ("onPointerEnter", Some("pointerenter")),
            // Not `on` + capital, so not an event.
            ("once", None),
            ("on", None),
        ] {
            let src = format!(
                "\"use island\"\nexport default function P() {{ return <button {attr}={{h}}>x</button> }}\n"
            );
            let r = crate::ir::lower_route(&src).unwrap();
            let IrNode::Element { bindings, .. } = r.tree else {
                panic!("expected element")
            };
            match (bindings.as_slice(), expected) {
                ([Binding::Event { name, .. }], Some(event)) => assert_eq!(name, event, "{attr}"),
                ([Binding::Attr { name, .. }], None) => assert_eq!(name, attr, "{attr}"),
                _ => panic!("{attr} lowered to {bindings:?}"),
            }
        }
    }

    /// A child slot must keep its place among its siblings: the emitter walks
    /// `children` in order, so a dropped or reordered slot wires the wrong node.
    #[test]
    fn dynamic_children_keep_document_order() {
        let src = "\"use island\"\nexport default function P() { return <p>a{b}c{d}e</p> }\n";
        let r = crate::ir::lower_route(src).unwrap();
        let IrNode::Element {
            bindings, children, ..
        } = r.tree
        else {
            panic!("expected element")
        };
        assert!(
            matches!(
                children.as_slice(),
                [
                    IrNode::Text(a),
                    IrNode::Expr(b),
                    IrNode::Text(c),
                    IrNode::Expr(d),
                    IrNode::Text(e),
                ] if a == "a" && b == "b" && c == "c" && d == "d" && e == "e"
            ),
            "got {children:?}"
        );
        // A dynamic child is not a binding: positions live in `children`.
        assert!(bindings.is_empty(), "got {bindings:?}");
    }

    /// An expression is the source slice of the *stripped* source, so a comment
    /// inside it survives and the braces do not become part of the text.
    #[test]
    fn expression_text_is_the_source_slice() {
        let src = "\"use island\"\nexport default function P() { return <p>{ f(a, /* keep */ 1) }</p> }\n";
        let r = crate::ir::lower_route(src).unwrap();
        let IrNode::Element { children, .. } = r.tree else {
            panic!("expected element")
        };
        let [IrNode::Expr(expr)] = children.as_slice() else {
            panic!("got {children:?}")
        };
        assert!(expr.contains("/* keep */"), "comment lost: {expr:?}");
        assert!(!expr.contains('{'), "braces leaked into the text: {expr:?}");
    }

    /// `{}` and `{/* a comment */}` parse to an empty expression. They must
    /// contribute no node — a comment turned into an expression becomes emitted
    /// JavaScript that references nothing.
    #[test]
    fn empty_expression_children_contribute_nothing() {
        for src in [
            "\"use island\"\nexport default function P() { return <p>{}</p> }\n",
            "\"use island\"\nexport default function P() { return <p>{/* hi */}</p> }\n",
        ] {
            let r = crate::ir::lower_route(src).unwrap();
            let IrNode::Element {
                bindings, children, ..
            } = r.tree
            else {
                panic!("expected element")
            };
            assert!(bindings.is_empty(), "{src}: {bindings:?}");
            assert!(children.is_empty(), "{src}: {children:?}");
        }
    }

    /// A nested element is a child node, not a flattened one: putting its slots
    /// on the parent would set the parent's content from the child's expression.
    #[test]
    fn a_nested_element_keeps_its_own_bindings() {
        let src = "\"use island\"\nexport default function P() { return <div id=\"o\"><p>{n()}</p></div> }\n";
        let r = crate::ir::lower_route(src).unwrap();
        let IrNode::Element {
            bindings, children, ..
        } = r.tree
        else {
            panic!("expected element")
        };
        assert!(
            bindings.is_empty(),
            "the child's slot leaked up: {bindings:?}"
        );
        let [
            IrNode::Element {
                tag,
                children: child_children,
                ..
            },
        ] = children.as_slice()
        else {
            panic!("got {children:?}")
        };
        assert_eq!(tag, "p");
        assert!(
            matches!(child_children.as_slice(), [IrNode::Expr(e)] if e == "n()"),
            "got {child_children:?}"
        );
    }

    /// A fragment is not an element: it contributes no tag and no nodes of its
    /// own, so its children land directly in the parent. Emitting a `<></>`
    /// would put a tag in the template that the DOM cannot parse.
    #[test]
    fn a_fragment_child_is_spliced_into_its_parent() {
        let src = "\"use island\"\nexport default function P() { return <div><><b>{a}</b><i>c</i></></div> }\n";
        let r = crate::ir::lower_route(src).unwrap();
        let IrNode::Element {
            tag,
            bindings,
            children,
            ..
        } = r.tree
        else {
            panic!("expected element")
        };
        assert_eq!(tag, "div");
        assert!(
            matches!(
                children.as_slice(),
                [IrNode::Element { tag: b, .. }, IrNode::Element { tag: i, children: i_children, .. }]
                    if b == "b" && i == "i" && matches!(i_children.as_slice(), [IrNode::Text(t)] if t == "c")
            ),
            "got {children:?}"
        );
        assert!(
            bindings.is_empty(),
            "the fragment's slot belongs to its own child, not the parent: {bindings:?}"
        );
    }

    /// A parenthesized return is how a multi-line body is formatted. Rejecting it
    /// would refuse the most idiomatic route in the framework.
    #[test]
    fn a_parenthesized_return_is_lowered() {
        let src =
            "\"use static\"\nexport default function P() {\n  return (\n    <p>hi</p>\n  )\n}\n";
        let r = crate::ir::lower_route(src).unwrap();
        assert!(matches!(r.tree, IrNode::Element { tag, children, .. }
            if tag == "p" && matches!(children.as_slice(), [IrNode::Text(t)] if t == "hi")));
    }

    /// A route exported through a name is still a route, and the name is the
    /// binding a reader can find.
    #[test]
    fn a_named_default_export_is_lowered_and_named() {
        for src in [
            "\"use static\"\nfunction Page() { return <p>hi</p> }\nexport default Page\n",
            "\"use static\"\nconst Page = () => <p>hi</p>\nexport default Page\n",
            "\"use static\"\nconst Page = () => <p>hi</p>\nexport { Page as default }\n",
            "\"use static\"\nconst Page = () => (<p>hi</p>)\nexport default (Page)\n",
        ] {
            let r = crate::ir::lower_route(src).unwrap();
            assert_eq!(r.name, "Page", "{src}");
            assert!(
                matches!(r.tree, IrNode::Element { tag, .. } if tag == "p"),
                "{src}"
            );
        }
    }

    /// An anonymous component has no binding name, and `default` names the export
    /// rather than a symbol.
    #[test]
    fn an_anonymous_component_is_named_page() {
        let src = "\"use static\"\nexport default function () { return <p/> }\n";
        assert_eq!(crate::ir::lower_route(src).unwrap().name, "Page");
    }

    /// A namespaced attribute is never an event listener, even when it carries an
    /// expression; a namespaced *static* value is a static attribute.
    #[test]
    fn a_namespaced_attribute_is_never_an_event() {
        for (src, expected) in [
            (
                "\"use island\"\nexport default function P() { return <div onClick:x={h} /> }\n",
                Binding::Attr {
                    name: "onClick:x".to_string(),
                    expr: "h".to_string(),
                },
            ),
            (
                "\"use island\"\nexport default function P() { return <use xlink:href={u} /> }\n",
                Binding::Attr {
                    name: "xlink:href".to_string(),
                    expr: "u".to_string(),
                },
            ),
        ] {
            let r = crate::ir::lower_route(src).unwrap();
            let IrNode::Element { bindings, .. } = r.tree else {
                panic!("expected element")
            };
            assert_eq!(bindings, vec![expected], "{src}");
        }

        let src =
            "\"use static\"\nexport default function P() { return <div xml:lang=\"en\" /> }\n";
        let r = crate::ir::lower_route(src).unwrap();
        let IrNode::Element {
            static_attrs,
            bindings,
            ..
        } = r.tree
        else {
            panic!("expected element")
        };
        assert_eq!(
            static_attrs,
            vec![("xml:lang".to_string(), "en".to_string())]
        );
        assert!(bindings.is_empty());
    }

    /// The mode and the tree come from one file: a route's `mode` must be the
    /// directive it actually carries.
    #[test]
    fn the_mode_comes_from_the_directive() {
        for (src, mode) in [
            (
                "\"use static\"\nexport default function P() { return <p/> }\n",
                Mode::Static,
            ),
            (
                "'use island'\nexport default function P() { return <p/> }\n",
                Mode::Island,
            ),
        ] {
            assert_eq!(crate::ir::lower_route(src).unwrap().mode, mode);
        }
    }

    /// A syntax error is `RA007`, not an empty tree.
    #[test]
    fn a_syntax_error_is_ra007() {
        let errs = crate::ir::lower_route(
            "\"use static\"\nexport default function P() { return <div> }\n",
        )
        .unwrap_err();
        assert_eq!(errs[0].code, Code::RA007);
        assert!(errs[0].span.0 > 0);
    }

    /// A missing directive is still `RA012`: lowering does not invent a mode.
    #[test]
    fn a_missing_directive_is_ra012() {
        let errs =
            crate::ir::lower_route("export default function P() { return <p/> }\n").unwrap_err();
        assert_eq!(errs[0].code, Code::RA012);
    }

    /// A named export that is not `default` must not stop the walk before the
    /// default export is reached.
    #[test]
    fn a_non_default_named_export_does_not_hide_the_route() {
        let src = "\"use island\"\nconst helper = () => 1\nexport { helper }\nexport default function Page() { return <p>hi</p> }\n";
        let r = crate::ir::lower_route(src).unwrap();
        assert_eq!(r.name, "Page");
        assert!(matches!(r.tree, IrNode::Element { tag, .. } if tag == "p"));
    }

    // --- The whitespace rule, as the new spec defines it ----------------------

    /// Text with no line break is kept verbatim, ` a  b ` included: the JSX rule
    /// only collapses text that contains a line break.
    #[test]
    fn single_line_text_is_verbatim() {
        let src = "\"use static\"\nexport default function P() { return <p> a  b </p> }\n";
        let r = crate::ir::lower_route(src).unwrap();
        let IrNode::Element { children, .. } = r.tree else {
            panic!("expected element")
        };
        assert!(
            matches!(children.as_slice(), [IrNode::Text(t)] if t == " a  b "),
            "got {children:?}"
        );
    }

    /// Text containing a line break is split, each line trimmed, empty lines
    /// dropped, and the rest joined with single spaces — the React rule. A
    /// whitespace-only node containing a line break disappears entirely.
    #[test]
    fn multiline_text_is_collapsed_and_indentation_disappears() {
        let src = "\"use static\"\nexport default function P() {\n  return (\n    <div>\n      <b>a</b>\n      <i>b</i>\n    </div>\n  )\n}\n";
        let r = crate::ir::lower_route(src).unwrap();
        let IrNode::Element {
            tag,
            children,
            bindings,
            ..
        } = r.tree
        else {
            panic!("expected element")
        };
        assert_eq!(tag, "div");
        assert!(bindings.is_empty());
        // Exactly two element children and no indentation text at all: the
        // shipped version kept five nodes, one of which was `"\n      "`.
        assert!(
            matches!(
                children.as_slice(),
                [IrNode::Element { tag: b, .. }, IrNode::Element { tag: i, .. }]
                    if b == "b" && i == "i"
            ),
            "got {children:?}"
        );

        // Real text around a line break is joined with one space, not dropped.
        let src = "\"use static\"\nexport default function P() { return <p>\n    hi\n    there\n  </p> }\n";
        let r = crate::ir::lower_route(src).unwrap();
        let IrNode::Element { children, .. } = r.tree else {
            panic!("expected element")
        };
        assert!(
            matches!(children.as_slice(), [IrNode::Text(t)] if t == "hi there"),
            "got {children:?}"
        );
    }

    /// `\r` is a line break for the whitespace rule too: a classic-Mac file
    /// separates its JSX lines with it, and the rule must collapse and drop them
    /// exactly as it does for `\n`.
    #[test]
    fn a_carriage_return_is_a_line_break() {
        let src = "\"use static\"\nexport default function P() { return <div>\r  <b>a</b>\r  <i>b</i>\r</div> }\n";
        let r = crate::ir::lower_route(src).unwrap();
        let IrNode::Element { children, .. } = r.tree else {
            panic!("expected element")
        };
        assert!(
            matches!(
                children.as_slice(),
                [IrNode::Element { tag: b, .. }, IrNode::Element { tag: i, .. }]
                    if b == "b" && i == "i"
            ),
            "got {children:?}"
        );

        let src =
            "\"use static\"\nexport default function P() { return <p>\r hi\r there\r </p> }\n";
        let r = crate::ir::lower_route(src).unwrap();
        let IrNode::Element { children, .. } = r.tree else {
            panic!("expected element")
        };
        assert!(
            matches!(children.as_slice(), [IrNode::Text(t)] if t == "hi there"),
            "got {children:?}"
        );
    }

    // --- `statements`: stripped JavaScript, not a span -----------------------

    /// The island emitter puts `statements` above the template in a `.mjs`, so
    /// they must be JavaScript: no `: number`, no type annotation, no JSX
    /// `return`. This is the hazard the shipped `body_span` had — it indexed the
    /// *typed* source, so Task 11 would have emitted `signal<number>(0)`.
    #[test]
    fn statements_are_stripped_javascript_without_the_return() {
        let src = "\"use island\"\nexport default function Page(props: Record<never, never>) { const n: number = signal(0); return <p>{n()}</p> }\n";
        let r = crate::ir::lower_route(src).unwrap();
        assert!(!r.statements.contains(": number"), "{}", r.statements);
        assert!(
            !r.statements.contains("Record<never, never>"),
            "{}",
            r.statements
        );
        assert!(r.statements.contains("signal(0)"), "{}", r.statements);
        assert!(
            !r.statements.contains("return"),
            "the JSX return must not be part of statements: {:?}",
            r.statements
        );
        assert!(
            !r.statements.contains("<p>"),
            "markup leaked into statements: {:?}",
            r.statements
        );
    }

    /// A generic call is the exact shape the reviewer proved: `signal<number>(0)`
    /// left in a `.mjs` is a syntax error, because a `.mjs` has no type
    /// parameters. `statements` must carry the stripped form.
    #[test]
    fn a_generic_call_is_stripped_out_of_the_statements() {
        let src = "\"use island\"\nexport default function Page(props: Record<never, never>) { const n = signal<number>(0); return <p>{n()}</p> }\n";
        let r = crate::ir::lower_route(src).unwrap();
        assert!(
            r.statements.contains("signal(0)"),
            "the generic argument survived, which would not parse in a .mjs: {:?}",
            r.statements
        );
        assert!(
            !r.statements.contains("signal<number>"),
            "{:?}",
            r.statements
        );
    }

    /// Every statement before the return is kept, in order, including a nested
    /// function and a multi-line declaration.
    #[test]
    fn statements_keep_every_declaration_before_the_return() {
        let src = "\"use island\"\nexport default function Page(props: Record<never, never>) {\n  const n: number = 1\n  function twice(x: number) { return x * 2 }\n  return <p>{twice(n)}</p>\n}\n";
        let r = crate::ir::lower_route(src).unwrap();
        assert!(r.statements.contains("const n = 1"), "{:?}", r.statements);
        assert!(
            r.statements.contains("function twice(x)"),
            "the nested function lost its stripped signature: {:?}",
            r.statements
        );
        assert!(!r.statements.contains("number"), "{:?}", r.statements);
        assert!(
            r.statements.find("const n").unwrap() < r.statements.find("function twice").unwrap(),
            "statements must keep source order: {:?}",
            r.statements
        );
    }

    /// A concise arrow body has no statements at all — emitting the JSX as a
    /// statement would be a second `return`. The positive half is load-bearing:
    /// asserting `""` alone would also pass if `statements` were always empty, so
    /// the block-bodied route next to it must be non-empty.
    #[test]
    fn a_concise_body_has_no_statements() {
        let src = "\"use island\"\nexport default (props: Record<never, never>) => <p>hi</p>\n";
        let r = crate::ir::lower_route(src).unwrap();
        assert_eq!(r.statements, "");

        let src = "\"use island\"\nexport default function P() { const n = 1; return <p>hi</p> }\n";
        let r = crate::ir::lower_route(src).unwrap();
        assert!(
            r.statements.contains("const n = 1"),
            "a block body must contribute its statements: {:?}",
            r.statements
        );
    }

    // --- The analysis/lowering contract -------------------------------------

    /// The two lists must be equal: every construct lowering refuses is one
    /// `check_jsx` catches first, and every construct `check_jsx` accepts is one
    /// lowering handles. A construct lowering refuses that `check_jsx` misses is
    /// a defect, because `lower_route` is documented to assume analysis passed.
    ///
    /// **One table drives both loops.** Driving only the analysis half from the
    /// table (and the lowering half from a second, hand-maintained list) left the
    /// claim half-checked: a construct added to lowering's arms but forgotten in
    /// the analysis list failed nothing. Every fixture here is a whole route, so
    /// both functions have something to say about it. The comparison is on code
    /// *and* on a named-construct substring, so a refusal that changed identity
    /// cannot pass by both being `RA014`.
    #[test]
    fn the_two_lists_are_equal() {
        use crate::analyze::jsx::check_jsx;
        use crate::source::parse_tsx;

        /// What lowering is expected to do with a fixture `check_jsx` refuses.
        enum Lowering {
            /// The same construct, under the same code: the two lists agree.
            Refuses,
            /// Analysis only, and lowering **deliberately accepts** it: the shape
            /// is legal JavaScript, so it is one of the documented two.
            Accepts,
        }

        let table: &[(&str, Code, &str, Lowering)] = &[
            (
                "\"use island\"\nexport default function P() { return <Greeting /> }\n",
                Code::RA014,
                "capitalized tag",
                Lowering::Refuses,
            ),
            (
                "\"use island\"\nexport default function P() { return <For each={x} /> }\n",
                Code::RA014,
                "runtime primitive",
                Lowering::Refuses,
            ),
            (
                "\"use island\"\nexport default function P() { return <Show when={x} /> }\n",
                Code::RA014,
                "runtime primitive",
                Lowering::Refuses,
            ),
            (
                "\"use island\"\nexport default function P() { return <When value={x} /> }\n",
                Code::RA014,
                "runtime primitive",
                Lowering::Refuses,
            ),
            (
                "\"use island\"\nexport default function P() { return <Shared.Header /> }\n",
                Code::RA014,
                "member-expression",
                Lowering::Refuses,
            ),
            (
                "\"use island\"\nexport default function P() { return <svg:rect /> }\n",
                Code::RA014,
                "namespaced tag",
                Lowering::Refuses,
            ),
            (
                "\"use island\"\nexport default function P() { return <this /> }\n",
                Code::RA014,
                "`<this>` tag",
                Lowering::Refuses,
            ),
            (
                "\"use island\"\nexport default function P() { return <div {...rest} /> }\n",
                Code::RA014,
                "spread attribute",
                Lowering::Refuses,
            ),
            (
                "\"use island\"\nexport default function P() { return <div>{...items}</div> }\n",
                Code::RA014,
                "spread child",
                Lowering::Refuses,
            ),
            (
                "\"use island\"\nexport default function P() { return <div title=<b/> /> }\n",
                Code::RA014,
                "an attribute whose value is itself JSX",
                Lowering::Refuses,
            ),
            (
                "\"use island\"\nexport default function P() { return <div>{ok ? <b/> : null}</div> }\n",
                Code::RA014,
                "JSX inside a `{…}` expression",
                Lowering::Refuses,
            ),
            (
                "\"use island\"\nexport default function P() { return <><b/><i/></> }\n",
                Code::RA014,
                "fragment as the route's returned JSX",
                Lowering::Refuses,
            ),
            // A body statement is copied into the emitted `.mjs`, where its JSX
            // would be a syntax error.
            (
                "\"use island\"\nexport default function P() { const header = <h1>x</h1>; return <p>{header}</p> }\n",
                Code::RA014,
                "JSX in a route body statement",
                Lowering::Refuses,
            ),
            (
                "\"use island\"\nexport default function P() { function Item() { return <li/> } return <ul/> }\n",
                Code::RA014,
                "JSX in a route body statement",
                Lowering::Refuses,
            ),
            // Module level: emitted nowhere, so the tree refers to a symbol the
            // build never wrote. This is the shape round 2 added.
            (
                "\"use island\"\nconst header = <h1>x</h1>\nexport default function P() { return <p>{header}</p> }\n",
                Code::RA014,
                "JSX outside the route's returned tree",
                Lowering::Refuses,
            ),
            (
                "\"use island\"\nfunction Header() { return <h1>x</h1> }\nexport default function P() { return <p/> }\n",
                Code::RA014,
                "JSX outside the route's returned tree",
                Lowering::Refuses,
            ),
            // The two documented analysis-only shapes, both asserted to *lower*
            // below so "analysis only" is a claim about lowering, not an excuse.
            //
            // A whitespace ambiguity is not an unsupported construct: JSX deletes
            // the space, which is exactly what lowering's whitespace rule does.
            (
                "\"use island\"\nexport default function P() { return <p><b>a</b>\n<i>b</i></p> }\n",
                Code::RA007,
                "line break between two inline elements",
                Lowering::Accepts,
            ),
            // One root is emitted; lowering follows the first `return` and never
            // sees the second.
            (
                "\"use island\"\nexport default function P() { if (a) return <b/>; return <i/> }\n",
                Code::RA014,
                "second JSX return",
                Lowering::Accepts,
            ),
        ];

        for (src, code, construct, lowering) in table {
            let alloc = oxc_allocator::Allocator::default();
            let program = parse_tsx(&alloc, src).unwrap();
            let analysis = check_jsx(program, src);
            let matched = analysis
                .iter()
                .filter(|d| d.code == *code && d.message.contains(construct))
                .count();
            assert_eq!(
                matched, 1,
                "check_jsx must report {construct} ({code:?}) exactly once: {src}\n{analysis:?}"
            );

            match lowering {
                Lowering::Refuses => {
                    let errs = crate::ir::lower_route(src).unwrap_err();
                    assert_eq!(errs[0].code, *code, "{src}: {errs:?}");
                    assert!(
                        errs[0].message.contains(construct),
                        "lowering refuses {src} without naming {construct}: {:?}",
                        errs[0].message
                    );
                }
                Lowering::Accepts => assert!(
                    crate::ir::lower_route(src).is_ok(),
                    "documented as analysis-only, but lowering refused it: {src}"
                ),
            }
        }

        // The comparison above is on `(code, construct)`, and the code has to
        // discriminate: an implementation that reported everything as `RA014`
        // would pass if every fixture were `RA014`. The table's `RA007` row is
        // that proof, and this asserts the code it carries is not `RA014`.
        let whitespace =
            "\"use island\"\nexport default function P() { return <p><b>a</b>\n<i>b</i></p> }\n";
        let alloc = oxc_allocator::Allocator::default();
        let program = parse_tsx(&alloc, whitespace).unwrap();
        assert!(
            check_jsx(program, whitespace)
                .iter()
                .any(|d| d.code == Code::RA007),
            "the code half of the comparison does not discriminate"
        );

        // Every shape lowering handles, with a mode so it is a whole route. Each
        // must be clean, or lowering would be the agent's first signal.
        let accepted = [
            "\"use static\"\nexport default function P() { return <div class=\"a\">hi</div> }\n",
            "\"use island\"\nexport default function P() { const n = signal(0); return <p>a {n()} b</p> }\n",
            "\"use island\"\nexport default function P() { return <button onClick={go}>x</button> }\n",
            "\"use island\"\nexport default function P() { return <div><><b>{a}</b><i>c</i></></div> }\n",
            "\"use static\"\nexport default function P() { return <p>a &amp; b</p> }\n",
            "\"use island\"\nexport default function P() { return <p class={tone}>{tone}</p> }\n",
            "\"use island\"\nexport default function P() { return <p>a{b}c</p> }\n",
            "\"use island\"\nexport default function P() { function twice(x) { return x * 2 } return <p>{twice(1)}</p> }\n",
            "\"use static\"\nexport function Page(props: Record<never, never>) { return <p/> }\nexport default Page\n",
        ];
        for src in accepted {
            let alloc = oxc_allocator::Allocator::default();
            let program = parse_tsx(&alloc, src).unwrap();
            let analysis = check_jsx(program, src);
            assert!(analysis.is_empty(), "{src}: {:?}", analysis);
            assert!(
                crate::ir::lower_route(src).is_ok(),
                "lowering refused a construct check_jsx accepted: {src}"
            );
        }

        // The route/non-route boundary, asserted in both directions: a `.tsx` file
        // with no default export is a component module, this milestone emits
        // nothing from it, and a module-level JSX helper there is accepted —
        // while `lower_route` still refuses it for the unrelated reason that it
        // has no route (the analyzer's `RA011`, not this round's refusal).
        let module = "\"use island\"\nconst header = <h1>x</h1>\nexport function Header(props: Record<never, never>) { return header }\n";
        let alloc = oxc_allocator::Allocator::default();
        let program = parse_tsx(&alloc, module).unwrap();
        assert_eq!(
            crate::analyze::find_route_component(program),
            None,
            "the fixture must have no route, or the boundary is not what is pinned"
        );
        assert!(
            check_jsx(program, module).is_empty(),
            "a non-route module's module-level JSX must stay accepted: {:?}",
            check_jsx(program, module)
        );
        assert_eq!(
            crate::ir::lower_route(module).unwrap_err()[0].code,
            Code::RA011,
            "a non-route module must be refused by the analyzer's rule, not by RA014"
        );
    }

    /// A nested renderer with JSX is refused: its `return`'s markup is not part of
    /// the tree, and `statements` would carry `<li/>` into the emitted `.mjs`,
    /// where it is a syntax error. A nested renderer *without* JSX is an ordinary
    /// helper and lowers.
    #[test]
    fn a_nested_renderer_is_refused_unless_it_has_no_jsx() {
        for src in [
            "\"use island\"\nexport default function P() { function Item() { return <li>hi</li> } return <ul><li>hi</li></ul> }\n",
            "\"use island\"\nexport default function P() { const Item = () => { return <li>hi</li> }; return <ul><li>hi</li></ul> }\n",
            "\"use island\"\nexport default function P() { const header = <h1>x</h1>; return <p>{header}</p> }\n",
        ] {
            let errs = crate::ir::lower_route(src).unwrap_err();
            assert_eq!(errs[0].code, Code::RA014, "{src}");
            assert!(
                errs[0].message.contains("JSX in a route body statement"),
                "{src}: {:?}",
                errs[0].message
            );
        }

        for src in [
            "\"use island\"\nexport default function P() { function twice(x) { return x * 2 } return <p>{twice(1)}</p> }\n",
            "\"use island\"\nexport default (props: Record<never, never>) => <p/>\n",
            "\"use island\"\nexport default function P() { return <button onClick={() => go()}>x</button> }\n",
        ] {
            assert!(
                crate::ir::lower_route(src).is_ok(),
                "{src}: a helper without JSX is not a body-statement refusal"
            );
        }
    }

    /// `lower_route` must agree with `find_route_component` on every shape: it
    /// lowers exactly when that analyzer says the export is a valid route. A
    /// divergence is what `unlowerable_export` guards, and a reachable one would
    /// report a compiler defect for a legal route.
    ///
    /// The `export function` / `export const` shapes are the ones that used to
    /// diverge: the analyzer resolves a declaration under `export`, while the
    /// lowering walk matched only a plain declaration, so a legal route got
    /// `RA011` telling it to return JSX from the default export it had already
    /// returned.
    ///
    /// Separately, and for a different reason, a page whose markup names a
    /// module-level value is emitted by both: `analyze::reachable` treats it as
    /// reachable and `module_statements` carries the declaration. Round 2 refused
    /// it in `analyze` while lowering accepted it; the plan ruling reversed that,
    /// and `finding_a_module_values_are_emitted_and_reachable` pins the pair.
    #[test]
    fn lowering_agrees_with_the_route_analyzer() {
        use crate::analyze::find_route_component;
        use crate::source::parse_tsx;

        for src in [
            "\"use static\"\nexport default function Page(props: Record<never, never>) { return <p/> }\n",
            "\"use static\"\nexport default (props: Record<never, never>) => <p/>\n",
            "\"use static\"\nexport default (props: Record<never, never>) => { return <p/> }\n",
            "\"use static\"\nexport default (function Page(props: Record<never, never>) { return <p/> })\n",
            "\"use static\"\nconst Page = (props: Record<never, never>) => <p/>\nexport default Page\n",
            "\"use static\"\nfunction Page(props: Record<never, never>) { return <p/> }\nexport { Page as default }\n",
            // A declaration under `export`, then named as the default.
            "\"use static\"\nexport function Page(props: Record<never, never>) { return <p/> }\nexport default Page\n",
            "\"use static\"\nexport function Page(props: Record<never, never>) { return <p/> }\nexport { Page as default }\n",
            "\"use static\"\nexport const Page = (props: Record<never, never>) => <p/>\nexport default Page\n",
            // Not JSX: the analyzer reports RA011 and lowering must not succeed.
            "\"use static\"\nexport default 42\n",
            "\"use static\"\nexport default function Page(props: Record<never, never>) { return View }\n",
            "\"use static\"\nexport default function Page(props: Record<never, never>) { if (x) return <p/> }\n",
            "\"use static\"\nconst Page = Page\nexport default Page\n",
        ] {
            let alloc = oxc_allocator::Allocator::default();
            let program = parse_tsx(&alloc, src).unwrap();
            let analyzer_says_jsx = find_route_component(program).is_some_and(|info| {
                crate::analyze::validate_route_component(Some(&info)).is_empty()
            });
            let lowered = crate::ir::lower_route(src).is_ok();
            assert_eq!(analyzer_says_jsx, lowered, "{src}");
            if !analyzer_says_jsx {
                // The refusal must be the analyzer's own diagnostic, not a
                // compiler-defect one.
                let errs = crate::ir::lower_route(src).unwrap_err();
                assert!(
                    errs.iter().all(|diagnostic| diagnostic.code == Code::RA011),
                    "{src}: {errs:?}"
                );
                assert!(
                    !errs
                        .iter()
                        .any(|diagnostic| diagnostic.message.contains("could not be located")),
                    "{src}: compiler-defect diagnostic reached a legal route"
                );
            }
        }
    }

    /// The module-level constant from the brief, now legal. Round 2 refused it;
    /// the plan ruling removed that refusal because the emitters carry
    /// `module_statements`. Both halves are asserted, because the old defect was
    /// `analyze` and `lower_route` disagreeing: lowering's `statements` stays
    /// body-only **and** `module_statements` now declares what the tree names.
    ///
    /// The body-local mirror is the control: the same expression declared inside
    /// the body lands in `statements`, not `module_statements`.
    #[test]
    fn finding_a_module_values_are_emitted_and_reachable() {
        let module_level = "\"use static\"\nconst title = \"About\"\nexport default function Page(props: Record<never, never>) { return <h1>{title}</h1> }\n";
        assert!(
            crate::analyze::analyze(module_level, "src/routes/about.page.tsx").is_empty(),
            "a module-level value is emitted, so the tree may name it"
        );

        let route = crate::ir::lower_route(module_level).expect("a module-level value lowers");
        assert_eq!(
            route.statements, "",
            "the component body's own statements are still body-only"
        );
        assert_eq!(
            route.module_statements, "const title = \"About\";",
            "the module level carries exactly the declaration the tree names"
        );
        assert!(
            matches!(&route.tree, IrNode::Element { children, .. }
                if matches!(children.as_slice(), [IrNode::Expr(expr)] if expr == "title")),
            "{:?}",
            route.tree
        );

        let body_local = "\"use static\"\nexport default function Page(props: Record<never, never>) { const title = \"About\"; return <h1>{title}</h1> }\n";
        assert!(
            crate::analyze::analyze(body_local, "src/routes/about.page.tsx").is_empty(),
            "a body local is legal too"
        );
        let route = crate::ir::lower_route(body_local).expect("a body local lowers");
        assert!(
            route.statements.contains("const title = \"About\""),
            "a body declaration is emitted inside the component: {:?}",
            route.statements
        );
        assert_eq!(
            route.module_statements, "",
            "a body declaration is not module scope: {:?}",
            route.module_statements
        );
    }

    // --- `module_statements`: the file's module level, emitted ---------------

    /// The `const title` fixture, with its exact `module_statements` content so
    /// Task 10 and Task 11 can be written against it. The `;` is Oxc's codegen,
    /// not the source's.
    #[test]
    fn module_statements_carry_a_module_level_constant() {
        let src = "\"use static\"\nconst title = \"About\"\nexport default function Page(props: Record<never, never>) { return <h1>{title}</h1> }\n";
        let r = crate::ir::lower_route(src).unwrap();
        assert_eq!(r.module_statements, "const title = \"About\";");
        assert_eq!(r.statements, "", "the body is empty");
    }

    /// `emitted_module_parts` is the one definition of "emitted" **per tree**:
    /// [`module_statements`] renders it, and `crate::analyze::reachable` walks it
    /// for references. Pinned here so the pair cannot drift into "carried but
    /// unchecked" or "checked but never written": the parts list is compared
    /// against the text it renders.
    ///
    /// It does **not** compare the two callers' trees to each other — `analyze`
    /// parses the typed source and `lower_route` parses `strip_types`'s re-print.
    /// That boundary is pinned separately, by
    /// `emitted_parts_agree_across_the_typed_and_stripped_trees`.
    #[test]
    fn emitted_module_parts_and_rendered_statements_agree() {
        use oxc_allocator::Allocator;

        for src in [
            // A plain declaration.
            "\"use static\"\nconst title = \"A\"\nexport default function Page(props: Record<never, never>) { return <h1>{title}</h1> }\n",
            // One statement declaring both the route and a value.
            "\"use static\"\nconst title = \"A\", Page = (props: Record<never, never>) => <h1>{title}</h1>\nexport default Page\n",
            // An import and a re-export that must be excluded.
            "\"use static\"\nimport { owner } from \"runai\"\nexport * from \"./data\"\nconst title = owner\nexport default function Page(props: Record<never, never>) { return <h1>{title}</h1> }\n",
            // Nothing at module scope at all.
            "\"use static\"\nexport default function Page(props: Record<never, never>) { return <p>hi</p> }\n",
        ] {
            let alloc = Allocator::default();
            let stripped = crate::strip::strip_types(src).unwrap();
            let program = crate::source::parse_tsx(&alloc, &stripped).unwrap();
            let route = crate::analyze::find_route_component(program).unwrap();
            let parts = emitted_module_parts(program, &route);
            let rendered = module_statements(&stripped, program, &route);

            // A parts list and its rendering carry the same statements: the
            // declarator form renders to one `kind …;` line per part, and an
            // empty list renders to `""`. Mismatches would mean the walk and the
            // text disagree about what the emitted module holds.
            if parts.is_empty() {
                assert_eq!(rendered, "", "{src}");
            } else {
                assert!(!rendered.is_empty(), "{src}: the parts list is not empty");
            }

            // The excluded statements never render: an `import`, the route's own
            // declaration, and a re-export.
            for forbidden in [
                "import ",
                "function Page",
                "const Page",
                "export *",
                "from \"./data\"",
            ] {
                assert!(
                    !rendered.contains(forbidden),
                    "{forbidden} leaked into the emitted text: {rendered:?}"
                );
            }
        }
    }

    /// The typed→stripped boundary does not change **which value declarations**
    /// the emitters carry.
    ///
    /// `analyze` and `lower_route` see different trees: `analyze` parses the
    /// typed source, `lower_route` parses [`crate::strip::strip_types`]'s
    /// re-print. Comparing those trees byte for byte is meaningless — spans and
    /// type syntax differ — so the claim that survives is the one the consumers
    /// depend on: for the same file, the parts list selects the same
    /// **declarations**. A type annotation must not add, remove, or reorder an
    /// emitted declaration.
    ///
    /// The one difference the typed tree does have is named and asserted rather
    /// than tolerated: `type Alias = string` is a statement in the typed tree and
    /// is erased from the stripped one. `analyze` therefore walks it, and every
    /// reference inside it is type-only, which is why the names and the verdicts
    /// still agree. Pinning the count difference keeps that the *only* one.
    #[test]
    fn emitted_parts_agree_across_the_typed_and_stripped_trees() {
        use oxc_allocator::Allocator;

        let src = "\"use static\"\nimport { owner } from \"runai\"\nimport type { Data } from \"./data\"\nexport * from \"./data\"\ntype Alias = string\nconst title: string = \"A\"\nconst n: number = 1\nexport default function Page(props: Record<never, never>) { return <h1>{title}{n}</h1> }\n";

        // The tree `analyze` parses: the file as the agent wrote it.
        let typed_alloc = Allocator::default();
        let typed = crate::source::parse_tsx(&typed_alloc, src).unwrap();
        let typed_route = crate::analyze::find_route_component(typed).unwrap();
        let typed_names = declared_names(&module_statements(src, typed, &typed_route));
        let typed_parts = emitted_module_parts(typed, &typed_route);

        // The tree `lower_route` parses: strip_types' re-print.
        let stripped = crate::strip::strip_types(src).unwrap();
        let stripped_alloc = Allocator::default();
        let program = crate::source::parse_tsx(&stripped_alloc, &stripped).unwrap();
        let route = crate::analyze::find_route_component(program).unwrap();
        let stripped_names = declared_names(&module_statements(&stripped, program, &route));
        let stripped_parts = emitted_module_parts(program, &route);

        assert_eq!(
            typed_names, stripped_names,
            "the typed and stripped trees must select the same declarations:\n\
             typed: {typed_names:?}\nstripped: {stripped_names:?}"
        );
        assert_eq!(
            typed_names,
            vec!["title".to_string(), "n".to_string()],
            "the two values, in source order, and nothing else"
        );

        // The named difference: the typed tree carries the type declaration, and
        // the stripped tree does not. Any *other* difference would mean a type
        // annotation changed the emitted module, which is the defect this pins.
        let typed_texts: Vec<&str> = typed_parts
            .iter()
            .map(|part| match part {
                EmittedModulePart::Statement(statement) => {
                    crate::source::raw_text(src, statement.span()).unwrap_or("")
                }
                EmittedModulePart::Declarators { .. } => "<declarators>",
            })
            .collect();
        assert_eq!(
            typed_texts.len(),
            stripped_parts.len() + 1,
            "the typed tree carries exactly one extra part: {typed_texts:?}"
        );
        assert!(
            typed_texts
                .iter()
                .any(|text| text.trim_start().starts_with("type Alias")),
            "the extra part is the type declaration: {typed_texts:?}"
        );
    }

    /// The names a rendered `module_statements` declares, in the order the
    /// declarations appear.
    fn declared_names(rendered: &str) -> Vec<String> {
        let mut names: Vec<(usize, String)> = Vec::new();
        for keyword in ["const ", "let ", "var ", "function ", "class "] {
            for (at, _) in rendered.match_indices(keyword) {
                let after = &rendered[at + keyword.len()..];
                let name: String = after
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '$')
                    .collect();
                if !name.is_empty() {
                    names.push((at, name));
                }
            }
        }
        names.sort_by_key(|(at, _)| *at);
        names.into_iter().map(|(_, name)| name).collect()
    }

    /// Source order, and every module-level declaration shape: a const, a
    /// function, a class, and a module-level signal. A later declaration may read
    /// an earlier one, so order is part of the contract.
    #[test]
    fn module_statements_keep_source_order() {
        let src = "\"use island\"\nimport { signal } from \"runai\"\nconst a = 1\nfunction twice(x) { return x * 2 }\nclass Store {}\nconst n = signal(0)\nexport default function P(props: Record<never, never>) { return <p>{a}{twice(1)}{n()}</p> }\n";
        let r = crate::ir::lower_route(src).unwrap();
        for (earlier, later) in [
            ("const a = 1", "function twice"),
            ("function twice", "class Store"),
            ("class Store", "const n = signal(0)"),
        ] {
            let earlier_at = r
                .module_statements
                .find(earlier)
                .unwrap_or_else(|| panic!("{earlier} missing: {:?}", r.module_statements));
            let later_at = r
                .module_statements
                .find(later)
                .unwrap_or_else(|| panic!("{later} missing: {:?}", r.module_statements));
            assert!(
                earlier_at < later_at,
                "module statements must keep source order: {:?}",
                r.module_statements
            );
        }
    }

    /// Three of the four kinds of statement that are the emitters' own must not
    /// appear: imports (the island emitter writes one, the prerender module
    /// none), the route component's declaration (the emitters rebuild it from
    /// `name` and `statements`), and its `export default`/`export { … as default
    /// }`. The fourth — a re-export — has its own fixture,
    /// `module_statements_exclude_a_reexport`, because it is a different
    /// statement shape rather than a different route shape; the `type` kind is
    /// never in this source at all, `strip_types` having erased it.
    ///
    /// Written for every route shape the analyzer accepts, because the excluded
    /// statement's span differs per shape — an inline export, a named declaration
    /// under `export`, and a `const` the default export refers to.
    ///
    /// The last fixture is the one that makes the import clause load-bearing:
    /// `strip_types` drops an **unused** import, so the earlier fixtures would
    /// pass with the exclusion deleted. Here `signal` is called in the body, the
    /// stripped source keeps the import, and it must still not appear.
    #[test]
    fn module_statements_exclude_imports_and_the_route_itself() {
        for src in [
            "\"use static\"\nimport { signal } from \"runai\"\nconst title = \"A\"\nexport default function Page(props: Record<never, never>) { return <h1>{title}</h1> }\n",
            "\"use static\"\nconst title = \"A\"\nexport function Page(props: Record<never, never>) { return <h1>{title}</h1> }\nexport default Page\n",
            "\"use static\"\nconst title = \"A\"\nconst Page = (props: Record<never, never>) => <h1>{title}</h1>\nexport default Page\n",
            "\"use static\"\nconst title = \"A\"\nconst Page = (props: Record<never, never>) => <h1>{title}</h1>\nexport { Page as default }\n",
            // A surviving import: used in the body, so stripping keeps it.
            "\"use island\"\nimport { signal } from \"runai\"\nconst title = \"A\"\nexport default function Page(props: Record<never, never>) { const n = signal(0); return <h1>{title}{n()}</h1> }\n",
        ] {
            let r = crate::ir::lower_route(src).unwrap();
            assert!(
                r.module_statements.contains("const title"),
                "the module level must keep a non-route declaration: {:?}",
                r.module_statements
            );
            assert!(
                !r.module_statements.contains("import"),
                "imports are the emitter's: {:?}",
                r.module_statements
            );
            assert!(
                !r.module_statements.contains("function Page")
                    && !r.module_statements.contains("const Page")
                    && !r.module_statements.contains("export"),
                "the route's own declaration is the emitter's: {:?}",
                r.module_statements
            );
        }
    }

    /// A re-export is never emitted. `export … from` and `export * from` name a
    /// second module, and the emitted file writes one import (the island's, from
    /// the runtime URL) or none, so the statement would re-export from a module
    /// nothing resolves. This is the `module_statements` half of Finding 2; the
    /// `analyze` half is pinned in `entry::tests::a_reexport_…`.
    ///
    /// The control is the `export { helper }` local re-export: a different
    /// statement, naming this file's own binding, which *is* emitted.
    #[test]
    fn module_statements_exclude_a_reexport() {
        for statement in [
            "export * from \"./data\"",
            "export { team } from \"./data\"",
        ] {
            let src = format!(
                "\"use static\"\n{statement}\nexport default function Page(props: Record<never, never>) {{ return <p>hi</p> }}\n"
            );
            let route = crate::ir::lower_route(&src).unwrap();
            assert_eq!(
                route.module_statements, "",
                "{statement} must not be emitted: {:?}",
                route.module_statements
            );
        }

        // `export * from "runai"` is the same statement shape: nothing is
        // re-exported. The import rule refuses the source too, so this is the
        // artifact's half of one refusal.
        let runai = "\"use static\"\nexport * from \"runai\"\nexport default function Page(props: Record<never, never>) { return <p>hi</p> }\n";
        assert_eq!(
            crate::ir::lower_route(runai).unwrap().module_statements,
            "",
            "a re-export is never emitted, whatever its source"
        );

        let local = "\"use static\"\nconst helper = 1\nexport { helper }\nexport default function Page(props: Record<never, never>) { return <p>{helper}</p> }\n";
        let route = crate::ir::lower_route(local).unwrap();
        assert!(
            route.module_statements.contains("export { helper }"),
            "a local re-export names this file's binding and is emitted: {:?}",
            route.module_statements
        );
    }

    /// One statement can declare both the route and an ordinary value. Dropping
    /// the whole statement would lose the value while the tree still names it —
    /// the exact accept-then-reject shape this change removes, one level down. So
    /// the non-route declarators are rebuilt with the same kind and `export`.
    #[test]
    fn module_statements_keep_a_value_declared_beside_the_route() {
        let src = "\"use static\"\nconst title = \"A\", Page = (props: Record<never, never>) => <h1>{title}</h1>\nexport default Page\n";
        let r = crate::ir::lower_route(src).unwrap();
        assert_eq!(r.module_statements, "const title = \"A\";");
        assert!(
            crate::analyze::analyze(src, "src/routes/index.page.tsx").is_empty(),
            "the tree names `title`, which is emitted"
        );

        // An exported multi-declarator keeps the `export`, because a reader of the
        // emitted module may import the value.
        let exported = "\"use static\"\nexport const title = \"A\", Page = (props: Record<never, never>) => <h1>{title}</h1>\nexport default Page\n";
        assert_eq!(
            crate::ir::lower_route(exported).unwrap().module_statements,
            "export const title = \"A\";"
        );
    }

    /// A file with nothing at module scope has empty `module_statements`, and the
    /// route-only file is the control that `""` is not simply always returned.
    #[test]
    fn module_statements_are_empty_when_there_is_nothing_to_emit() {
        let src = "\"use static\"\nexport default function P(props: Record<never, never>) { return <p>hi</p> }\n";
        assert_eq!(crate::ir::lower_route(src).unwrap().module_statements, "");

        let with_value = "\"use island\"\nconst n = 1\nexport default function P(props: Record<never, never>) { return <p>{n}</p> }\n";
        assert_eq!(
            crate::ir::lower_route(with_value)
                .unwrap()
                .module_statements,
            "const n = 1;"
        );
    }

    /// Module-level JSX is still refused, and now for the reason that stays true
    /// after this change: `module_statements` is copied into the emitted `.mjs`,
    /// where JSX is a syntax error. It is **one** diagnostic, owned by
    /// `check_jsx`; lowering repeats it as an invariant.
    #[test]
    fn module_level_jsx_is_refused_and_never_reaches_module_statements() {
        let src = "\"use static\"\nconst header = <h1>x</h1>\nexport default function P(props: Record<never, never>) { return <p>{header}</p> }\n";
        let errs = crate::ir::lower_route(src).unwrap_err();
        assert_eq!(errs.len(), 1, "{errs:?}");
        assert_eq!(errs[0].code, Code::RA014);
        assert!(errs[0].message.contains(MODULE_JSX_CONSTRUCT), "{errs:?}");
    }

    /// Lowering's own refusals are invariants — analysis reports them first — but
    /// an invariant that no test reaches is an arm nobody has read. Each one is
    /// called directly here, so removing it fails this test rather than emitting
    /// markup the DOM cannot parse.
    #[test]
    fn lowering_refuses_each_construct_even_without_the_analysis() {
        for (src, expected) in [
            (
                "\"use island\"\nexport default function P() { return <Greeting name=\"Ada\" /> }\n",
                "capitalized tag",
            ),
            (
                "\"use island\"\nexport default function P() { return <For each={x} /> }\n",
                "runtime primitive",
            ),
            (
                "\"use island\"\nexport default function P() { return <Show when={x} /> }\n",
                "runtime primitive",
            ),
            (
                "\"use island\"\nexport default function P() { return <When value={x} /> }\n",
                "runtime primitive",
            ),
            (
                "\"use island\"\nexport default function P() { return <Shared.Header /> }\n",
                "member-expression",
            ),
            (
                "\"use island\"\nexport default function P() { return <div {...rest} /> }\n",
                "spread attribute",
            ),
            (
                "\"use island\"\nexport default function P() { return <div>{...items}</div> }\n",
                "spread child",
            ),
            (
                // The JSX-valued-attribute arm, exercised without the analysis.
                "\"use island\"\nexport default function P() { return <div title=<b/> /> }\n",
                "an attribute whose value is itself JSX",
            ),
            (
                "\"use island\"\nexport default function P() { return <div>{ready ? <b/> : <i/>}</div> }\n",
                "JSX inside a `{…}` expression",
            ),
            (
                "\"use island\"\nexport default function P() { return <><b/><i/></> }\n",
                "fragment",
            ),
            (
                // `statements` would carry this into the emitted `.mjs`.
                "\"use island\"\nexport default function P() { const header = <h1>x</h1>; return <p>{header}</p> }\n",
                "JSX in a route body statement",
            ),
            (
                // A binding in a text-only element breaks the marker ordinal, and
                // `mode` is in scope in `lower_route`; the refusal must not be
                // analysis-only. `template_html` is `pub`, so a caller could
                // otherwise get a template whose marker count differs from
                // `sites()`.
                "\"use island\"\nexport default function P() { const n = signal(0); return <div><script>{n()}</script></div> }\n",
                "`<script>`",
            ),
            (
                // A `<td>` outside `<tr>`: the parser keeps the marker but
                // re-parents it, so the attribute would land on the enclosing div.
                "\"use island\"\nexport default function P() { const n = signal(0); return <div><td class={n()}>x</td></div> }\n",
                "`<td>`",
            ),
        ] {
            let errs = crate::ir::lower_route(src).unwrap_err();
            assert_eq!(errs[0].code, Code::RA014, "{src}");
            assert!(
                errs[0].message.contains(expected),
                "{src}: the message must name the construct, got {:?}",
                errs[0].message
            );
        }
    }

    /// Two of the "closes its own kind" rows asserted end to end: `lower_route`
    /// returns `Err` carrying the **same message** `check_jsx` reports. The shared
    /// list test above covers four more rows by looping; this one pins the exact
    /// text for two families.
    #[test]
    fn lowering_refuses_an_element_that_closes_its_own_kind() {
        use crate::analyze::jsx::check_jsx;
        use crate::source::parse_tsx;

        for src in [
            "\"use island\"\nexport default function P(props: Record<never, never>) { return <ul><li>a<li>b</li></li></ul> }\n",
            "\"use island\"\nexport default function P(props: Record<never, never>) { return <table><tbody><tr><td>a<td>b</td></td></tr></tbody></table> }\n",
        ] {
            let alloc = oxc_allocator::Allocator::default();
            let program = parse_tsx(&alloc, src).unwrap();
            let analysis = check_jsx(program, src);
            assert_eq!(analysis.len(), 1, "{src}: {analysis:?}");

            let errs = crate::ir::lower_route(src).unwrap_err();
            assert_eq!(errs[0].code, Code::RA014, "{src}");
            assert_eq!(
                errs[0].message, analysis[0].message,
                "{src}: lowering and analysis must report one refusal, not two"
            );
            assert!(
                errs[0].message.contains("sibling"),
                "{src}: names the shape: {}",
                errs[0].message
            );
        }
    }

    /// `lower_route` refuses the marker-hostile and required-ancestor shapes with
    /// the **same words** `check_jsx` reports. `template_html` and `lower_route`
    /// are both `pub`, so this is a public path to a template whose marker count
    /// differs from `sites()` unless lowering repeats the rules.
    #[test]
    fn lowering_repeats_the_rules_check_jsx_reports() {
        use crate::analyze::jsx::check_jsx;
        use crate::source::parse_tsx;

        for src in [
            "\"use island\"\nexport default function P() { const n = signal(0); return <script>{n()}</script> }\n",
            "\"use island\"\nexport default function P() { const n = signal(0); return <textarea value={n()}>static</textarea> }\n",
            "\"use island\"\nexport default function P() { const n = signal(0); return <div><td class={n()}>x</td></div> }\n",
            "\"use island\"\nexport default function P() { return <div><tr><td>x</td></tr></div> }\n",
            "\"use island\"\nexport default function P() { return <td>x</td> }\n",
            "\"use island\"\nexport default function P() { return <div><li>x</li></div> }\n",
            // The "closes its own kind" rows, repeated by lowering.
            "\"use island\"\nexport default function P() { return <ul><li>a<li>b</li></li></ul> }\n",
            "\"use island\"\nexport default function P() { return <div><h1>a<h3>b</h3></h1></div> }\n",
            "\"use island\"\nexport default function P() { return <select><option>a<option>b</option></option></select> }\n",
            "\"use island\"\nexport default function P() { return <table><tbody><tr><td>a<td>b</td></td></tr></tbody></table> }\n",
            // The cap-adjudication rows: lowering must refuse each with the same
            // words the analysis does.
            "\"use island\"\nexport default function P() { const n = signal(0); return <div><noscript>{n()}</noscript></div> }\n",
            "\"use island\"\nexport default function P() { return <table><td>x</td></table> }\n",
            "\"use island\"\nexport default function P() { return <table><tbody><td>x</td></tbody></table> }\n",
            "\"use island\"\nexport default function P() { return <p>a<menu>x</menu></p> }\n",
            "\"use island\"\nexport default function P() { return <select><select><option>a</option></select></select> }\n",
        ] {
            let alloc = oxc_allocator::Allocator::default();
            let program = parse_tsx(&alloc, src).unwrap();
            let analysis = check_jsx(program, src);
            assert_eq!(analysis.len(), 1, "{src}: {analysis:?}");

            let errs = crate::ir::lower_route(src).unwrap_err();
            assert_eq!(errs[0].code, analysis[0].code, "{src}");
            assert_eq!(
                errs[0].message, analysis[0].message,
                "{src}: lowering and analysis must report one refusal, not two"
            );
        }
    }
}
