//! JSX structural checks that run on the **original** source.
//!
//! `analyze` runs these before anything is transformed, so every span points at
//! what the agent actually wrote: the file the diagnostic quotes is the file the
//! fix edits. Three rules live here: the whitespace ambiguity, the refusals, and
//! "does a route body statement carry JSX".
//!
//! # `RA007`: a line break between inline siblings
//!
//! JSX has one whitespace rule, and it is not "keep the text": a text node
//! containing a line break is split, each line is trimmed, empty lines are
//! dropped, and the rest is joined with single spaces. So
//!
//! ```tsx
//! <p><b>a</b>
//! <i>b</i></p>
//! ```
//!
//! renders `a` and `b` with **no space** between them, while
//!
//! ```tsx
//! <p><b>a</b> <i>b</i></p>
//! ```
//!
//! renders `a b`. The two differ by one character and by every pixel of the
//! rendered page, and neither shape is a syntax error: the agent wrote the first
//! one and probably meant the second. That is a real ambiguity, so `RA007`
//! reports it instead of guessing. The repair is [`SPACE_FIX`].
//!
//! Only *inline* neighbours count. A block-level element starts its own box, so a
//! line break around it drops whitespace that was never visible — reporting that
//! would flag every multi-line page in the framework.
//!
//! # `RA014`: constructs this milestone does not lower
//!
//! The full list, which is exactly the list lowering refuses:
//!
//! - a capitalized tag (`<Greeting/>`) — component inlining comes with a later
//!   plan, and the runtime's `<For>`/`<Show>`/`<When>` are refused as primitives
//!   this milestone does not lower, not as components;
//! - a member-expression tag (`<Shared.Header/>`);
//! - a namespaced tag (`<svg:rect/>`) — this milestone emits unprefixed HTML;
//! - a `<this>` tag;
//! - JSX nested inside a `{…}` expression — it becomes JavaScript in the emitted
//!   module, where `<b/>` is a syntax error;
//! - an attribute whose value is itself JSX (`name=<Bar/>`);
//! - a spread attribute (`{...rest}`) or a spread child (`{...items}`);
//! - a JSX fragment as the route's returned markup — the IR's root is one node;
//! - a second JSX `return` in the route body — lowering emits one root, and
//!   dropping the other would render markup without part of a node;
//! - JSX anywhere else in a route body statement (`const header = <h1/>`, a
//!   nested renderer's `return`) — [`RouteIr::statements`] is copied into the
//!   emitted `.mjs` verbatim, where JSX is a syntax error;
//! - JSX outside the route's returned tree (a module-level `const header =
//!   <h1/>`, a module-level helper that returns JSX) — module-level statements
//!   *are* emitted now ([`RouteIr::module_statements`]), so the defect is no
//!   longer "dropped": it is that the emitted `.mjs` would contain JSX, which is
//!   a syntax error there exactly as it is in a body statement. JSX cannot be
//!   held in a plain JavaScript const.
//!
//! Both of the last two are the same defect at two scopes: JSX the build cannot
//! turn into JavaScript. They are refused only in a file where a route component
//! exists: a `.tsx` file with no default export is a component module, where a
//! module-level JSX declaration is legal and this milestone simply does not emit
//! that module.
//!
//! [`RouteIr::statements`]: crate::ir::RouteIr::statements
//! [`RouteIr::module_statements`]: crate::ir::RouteIr::module_statements
//!
//! `RA014_NOT_SUPPORTED_YET` is the right code, not `RA007`: the parser accepted
//! the file, and `RA007` means "the parser rejected this file" everywhere else.
//! The two must never be confused, because an agent that sees `RA007` starts
//! hunting for an unclosed tag in a file that parses.
//!
//! One construct is deliberately **absent**: an attribute with an empty
//! expression (`name={}`). That is a parse error — `RA007`, carrying Oxc's own
//! "must only be assigned a non-empty expression" — so it never reaches analysis
//! and must not be advertised as valid syntax this milestone merely lacks.
//!
//! ## Why this list must equal lowering's list
//!
//! `lower_route` assumes its input already passed every rule; it is the
//! emitters' entry point, not the agent's first signal. That only works if
//! `check_jsx` catches *everything* lowering refuses, so the two lists are kept
//! equal by construction: [`dom_tag`] is the one tag rule lowering also uses, and
//! `the_two_lists_are_equal` in `ir.rs` walks one fixture per refusal through
//! both functions.

use oxc_ast::ast::{
    ArrowFunctionBody, Declaration, ExportDefaultDeclaration, ExportDefaultDeclarationKind,
    Expression, Function, FunctionBody, JSXAttributeItem, JSXAttributeValue, JSXChild, JSXElement,
    JSXElementName, JSXExpression, JSXExpressionContainer, JSXFragment, Program, ReturnStatement,
    Statement,
};
use oxc_ast_visit::Visit;
use oxc_span::{GetSpan, Span};

use crate::diagnostic::{Code, Diagnostic};
use crate::source::{has_line_break, raw_text};

/// The tags the runtime itself provides, each with the repair its refusal carries.
///
/// They look like components to [`dom_tag`]'s capitalization rule, but they are
/// not: they are this framework's list, conditional, and async primitives, and no
/// later task in this milestone's plan gives the agent a way to write them. A
/// diagnostic that called them "a component" would point at an inlining
/// mechanism that does not exist for them.
///
/// One table, so "which tags are runtime primitives" and "what does this one
/// tell the agent to do" cannot disagree: the old shape kept the tag list in a
/// `const` and the repairs in a `match` ending in `unreachable!`, which would
/// panic *while producing a diagnostic* if the const gained a tag the `match`
/// did not.
///
/// `<When>`'s repair names the `pending` branch on purpose: a design.md
/// `resource` only has its `pending` branch reachable at build time (§7.1), so
/// "render the success branch" is not something the agent can apply.
const RUNTIME: &[(&str, &str)] = &[
    (
        "For",
        "Render the list one element at a time (spread the items into static markup), or wait \
         for `<For>` support in a later plan.",
    ),
    (
        "Show",
        "Return one of the two elements conditionally from the component, or wait for `<Show>` \
         support in a later plan.",
    ),
    (
        "When",
        "Render the `pending` branch directly (it is the only branch a prerendered page can \
         show), or wait for `<When>` support in a later plan.",
    ),
];

/// Elements that flow inline with their neighbours, so a dropped space is
/// visible. Everything else is block-level: a newline around it is layout, not
/// content, and JSX's own collapse is what the agent wants.
const INLINE_TAGS: &[&str] = &[
    "a", "abbr", "b", "code", "em", "i", "kbd", "label", "mark", "s", "small", "span", "strong",
    "sub", "sup", "time", "u",
];

/// The fix every `RA007` whitespace diagnostic carries.
///
/// Pinned verbatim by the task brief. [`Code::RA007`]'s canonical `fix_template`
/// is parser-oriented ("fix the syntax error the parser reported"), so the
/// whitespace case carries its own sentence: a diagnostic whose fix told an agent
/// to hunt for an unclosed tag in a file that parses repairs nothing. The
/// canonical text quotes this sentence too, and
/// `the_space_fix_is_part_of_the_canonical_ra007_fix` keeps them glued.
pub const SPACE_FIX: &str = "Put both on one line, or write {\" \"} where you want the space.";

/// Every JSX rule `analyze` applies before lowering.
///
/// One pass over the original program: the route's own render (fragment root,
/// dropped returns, body statements, module-level declarations), then each
/// `RA014` refusal and the `RA007` ambiguity in source order. Returning every
/// diagnostic — not just the first — means one run tells the agent everything to
/// fix.
///
/// Runs on **every** `.tsx` file, not only pages, so a `_shell.tsx` component is
/// held to the same rules. The scope rules are the exception: a fragment root, a
/// second `return`, a body statement, and module-level JSX are all questions
/// about *the route*, so they are asked only when [`route_render`] finds one. A
/// module with no route is not emitted by this milestone at all, and a
/// module-level JSX helper there is legal.
pub fn check_jsx(program: &Program, source: &str) -> Vec<Diagnostic> {
    let mut checker = JsxChecker {
        source,
        diagnostics: Vec::new(),
    };

    // The route's render is a separate question from "which elements exist".
    // Everything here is about the *returned root* or the statements the
    // returned root replaces, so they are answered from one walk.
    if let Some(render) = route_render(program) {
        checker.root(program, render);
    }

    checker.visit_program(program);
    checker
        .diagnostics
        .sort_by_key(|diagnostic| diagnostic.span.0);
    checker.diagnostics
}

/// The walk. Owns the diagnostics it finds and the source its spans index.
struct JsxChecker<'s> {
    source: &'s str,
    diagnostics: Vec<Diagnostic>,
}

impl JsxChecker<'_> {
    /// The source text a span names, or `""` for a span outside it.
    ///
    /// Spans here come from the program parsed from this very source, so the
    /// `None` arm is a constructed-AST case only. It is deliberately silent
    /// rather than panicking: this walk's job is to report, and a panic in a
    /// diagnostic pass would hide every diagnostic after it. The rule itself is
    /// [`crate::source::raw_text`], shared with lowering so the two cannot drift.
    fn slice(&self, span: Span) -> &str {
        raw_text(self.source, span).unwrap_or("")
    }

    /// The rules about the route's returned markup and the scopes around it.
    ///
    /// `program` is needed for the module-level walk: the route component's body
    /// is one scope, and everything outside it is the other.
    fn root(&mut self, program: &Program<'_>, render: RouteRender<'_>) {
        self.root_expression(render.root);

        // Module-level statements *are* emitted now, into the generated `.mjs`,
        // so the defect is not that the markup is dropped: it is that the emitted
        // `.mjs` would contain JSX, where it is a syntax error exactly as it is
        // in a body statement. Same scope-out defect as a body statement, one
        // level up. This runs for a concise arrow body too
        // (`(props) => <p>{header}</p>` has no body statements but does have a
        // module-level `header`).
        let root_span = render.root.span();
        if let Some(span) = jsx_outside_route_render(program, &render) {
            self.diagnostics.push(module_jsx(span));
        }

        let (Some(body), Some(return_span)) = (render.body, render.return_span) else {
            return;
        };
        let returned = jsx_returns(body);

        // One root is emitted, so a second JSX return would lose the other branch
        // with no report. Only the route body is walked, and nested functions are
        // skipped: their `return`s belong to another component's render.
        if let Some(dropped) = returned.iter().copied().find(|span| *span != root_span) {
            self.diagnostics.push(unsupported(
                dropped,
                "a second JSX return that lowering does not reach",
                "Return one element from the component body. Move the other markup into \
                 that element, or split the branch into a component of its own.",
            ));
        }

        // Everything else in the body before the `return` is copied into the
        // emitted module as JavaScript, so JSX there is a parse failure with no
        // other report. The root return's own JSX is lowered, not copied, and a
        // second return already has the more specific diagnostic above.
        if let Some(span) = jsx_in_body_statements(body, return_span, &returned) {
            self.diagnostics.push(body_jsx(span));
        }
    }

    /// The fragment-root rule and the "root is not JSX" invariant.
    fn root_expression(&mut self, root: &Expression<'_>) {
        match root {
            Expression::JSXFragment(fragment) => self.diagnostics.push(unsupported(
                fragment.span,
                "a fragment as the route's returned JSX",
                "Return exactly one element instead, wrapping the fragment's children in \
                 `<main>…</main>` or whichever parent suits the page.",
            )),
            // `route_render` sees through parentheses, so a JSX element is the
            // only other shape it can return.
            Expression::JSXElement(_) => {}
            // Unreachable, and kept so a future resolver change is a diagnostic.
            other => self.diagnostics.push(unsupported(
                other.span(),
                "a root that is not JSX",
                "Return one element from the route component, e.g. `return <main>…</main>`.",
            )),
        }
    }
}

impl<'a> Visit<'a> for JsxChecker<'_> {
    fn visit_jsx_element(&mut self, element: &JSXElement<'a>) {
        self.element_name(element);
        self.attributes(element);
        self.children(&element.children);
        oxc_ast_visit::walk::walk_jsx_element(self, element);
    }

    fn visit_jsx_fragment(&mut self, fragment: &JSXFragment<'a>) {
        self.children(&fragment.children);
        oxc_ast_visit::walk::walk_jsx_fragment(self, fragment);
    }

    fn visit_jsx_expression_container(&mut self, container: &JSXExpressionContainer<'a>) {
        // `RA014`: JSX inside a `{…}` expression becomes JavaScript in the
        // emitted module, where `<b/>` is a syntax error. The check is an AST
        // walk, not a substring search, so `{a < b}` stays legal.
        if let Some(span) = first_jsx(&container.expression) {
            self.diagnostics.push(unsupported(
                span,
                "JSX inside a `{…}` expression",
                "Move the JSX into the children of the element it belongs to. A conditional \
                 returns one of two elements: `<Show when={ok}>` or two branches of `<When>`; \
                 an attribute or prop takes a value, not an element.",
            ));
        }
        oxc_ast_visit::walk::walk_jsx_expression_container(self, container);
    }

    fn visit_jsx_spread_attribute(&mut self, attribute: &oxc_ast::ast::JSXSpreadAttribute<'a>) {
        // `RA014`: a spread cannot be expanded without knowing what it holds.
        self.diagnostics.push(unsupported(
            attribute.span,
            "a spread attribute (`{...expr}`)",
            "Write the attributes out, e.g. `<div id={rest.id}>`.",
        ));
        oxc_ast_visit::walk::walk_jsx_spread_attribute(self, attribute);
    }

    fn visit_jsx_spread_child(&mut self, child: &oxc_ast::ast::JSXSpreadChild<'a>) {
        // `RA014`: a spread child renders whatever the value happens to be, which
        // no template can hold.
        self.diagnostics.push(unsupported(
            child.span,
            "a spread child (`{...expr}`)",
            "Render the value explicitly, e.g. `<For each={items} key={by}>…</For>`.",
        ));
        oxc_ast_visit::walk::walk_jsx_spread_child(self, child);
    }
}

impl JsxChecker<'_> {
    /// `RA014` for a tag this milestone cannot emit.
    fn element_name(&mut self, element: &JSXElement<'_>) {
        if let Err(refusal) = dom_tag(&element.opening_element.name) {
            self.diagnostics.push(unsupported(
                element.opening_element.name.span(),
                refusal.construct,
                &refusal.repair,
            ));
        }
    }

    /// `RA014` for an attribute whose value is JSX (`foo=<Bar/>`).
    fn attributes(&mut self, element: &JSXElement<'_>) {
        for attribute in &element.opening_element.attributes {
            let JSXAttributeItem::Attribute(attribute) = attribute else {
                continue;
            };
            let Some(value) = attribute.value.as_ref() else {
                continue;
            };
            if matches!(
                value,
                JSXAttributeValue::Element(_) | JSXAttributeValue::Fragment(_)
            ) {
                self.diagnostics.push(unsupported(
                    value.span(),
                    "an attribute whose value is itself JSX (`name=<Bar />`)",
                    "Pass the element as a child instead, e.g. `<Parent><Bar /></Parent>`.",
                ));
            }
        }
    }

    /// The whitespace rule, applied to one element's child list.
    ///
    /// Only a *whitespace-only* text containing a line break can delete a space:
    /// that is the text JSX collapses to nothing, and it is exactly the case
    /// where the agent wrote a newline between two inline siblings. Text with
    /// content is trimmed and joined, not deleted, so its words survive.
    fn children(&mut self, children: &[JSXChild<'_>]) {
        for (index, child) in children.iter().enumerate() {
            let JSXChild::Text(text) = child else {
                continue;
            };
            let raw = self.slice(text.span);
            if !raw.trim().is_empty() || !has_line_break(raw) {
                continue;
            }

            // A line break between two inline siblings loses a visible space.
            // The *nearest* sibling on each side is what the space sat between;
            // a distant inline element behind a block sibling is not adjacent, and
            // counting it would flag an ordinary block layout.
            let before = nearest_inline(children[..index].iter().rev());
            let after = nearest_inline(children[index + 1..].iter());
            if !before || !after {
                continue;
            }

            self.diagnostics.push(space_ambiguity(text.span));
        }
    }
}

/// Whether a name is a tag this milestone emits, or why it is not.
///
/// The **one** tag rule. [`check_jsx`] reports a refusal; `ir.rs` turns the same
/// refusal into its invariant diagnostic. Sharing it is what keeps the analysis
/// list and the lowering list equal.
pub(crate) fn dom_tag<'a>(name: &'a JSXElementName<'a>) -> Result<&'a str, Refusal> {
    match name {
        JSXElementName::Identifier(identifier) => {
            let tag = identifier.name.as_str();
            // The parser lowercases nothing, and which *node kind* a name parses
            // as depends only on the name: Oxc's `is_reference` is `!contains_dash
            // &&` a first byte in `A-Z`, `_`, `$`, or non-ASCII
            // (`oxc_parser-0.159.0/src/jsx/mod.rs:178-198`) — never on whether
            // attributes are present. So this arm only sees a hyphenated name
            // (`<my-element/>`) or one starting `a-z`, and `this` is its own node.
            // Its capitalized branch is therefore reachable only for a hyphenated
            // capital such as `<My-Element/>`; the runtime's `<For>`/`<Show>`/
            // `<When>` and every other component name arrive at the arm below.
            if tag.starts_with(|c: char| c.is_ascii_uppercase()) {
                Err(Refusal::capitalized(tag))
            } else {
                Ok(tag)
            }
        }
        // No dash and a first byte in `A-Z`/`_`/`$`/non-ASCII: `<For/>` and
        // `<For each={x}/>` are **both** this arm, and both are refused. The same
        // capitalization rule applies, so the two arms cannot disagree about what
        // a name means.
        JSXElementName::IdentifierReference(identifier) => {
            Err(Refusal::capitalized(identifier.name.as_str()))
        }
        JSXElementName::MemberExpression(_) => Err(Refusal {
            construct: "a member-expression tag (`<Namespace.Component>`)",
            repair: "Inline the component's markup here; this milestone does not resolve \
                     components across modules or namespaces."
                .to_string(),
        }),
        JSXElementName::ThisExpression(_) => Err(Refusal {
            construct: "a `<this>` tag",
            repair: "Write the element out; a `<this>` tag is not a DOM element.".to_string(),
        }),
        JSXElementName::NamespacedName(_) => Err(Refusal {
            construct: "a namespaced tag (`<svg:rect>`)",
            repair: "Use the unprefixed element this milestone emits, e.g. `<rect>`.".to_string(),
        }),
    }
}

/// Why a tag has no DOM lowering, in the words the diagnostic carries.
pub(crate) struct Refusal {
    /// What the construct is, for `cannot lower {construct} yet`.
    pub construct: &'static str,
    /// Where the markup belongs instead.
    pub repair: String,
}

impl Refusal {
    /// A capitalized tag: a component, or one of the runtime's primitives.
    ///
    /// One entry point for both `JSXElementName` arms that can carry a capital
    /// (the `Identifier` arm only for a hyphenated name such as `<My-Element/>`,
    /// the `IdentifierReference` arm for everything else), so the two arms cannot
    /// disagree about what a name means.
    fn capitalized(tag: &str) -> Self {
        Self::runtime(tag).unwrap_or_else(|| Self::component(tag))
    }

    /// A capitalized tag: a component this milestone cannot inline.
    fn component(tag: &str) -> Self {
        Self {
            construct: "a capitalized tag (a component)",
            repair: format!(
                "`<{tag}>` names a component. Inline its markup here, or ask for component \
                 inlining in a later plan; this milestone renders DOM elements only."
            ),
        }
    }

    /// A runtime primitive: `<For>`, `<Show>`, or `<When>`, or `None` for any
    /// other tag.
    ///
    /// The honest repair is neither "inline it" (there is no markup to inline) nor
    /// "wait for component inlining" (this is not a component). The primitive
    /// lowers in a later plan, so the agent is told what to write until then and
    /// which plan brings it — both read from [`RUNTIME`], so a tag cannot be
    /// listed there without a repair.
    fn runtime(tag: &str) -> Option<Self> {
        let (_, repair) = RUNTIME.iter().find(|(name, _)| *name == tag)?;
        Some(Self {
            construct: "a runtime primitive (`<For>`, `<Show>`, `<When>`)",
            repair: format!(
                "`<{tag}>` is a runtime primitive this milestone does not lower. {repair}"
            ),
        })
    }
}

/// The route's returned markup and the body it was returned from.
///
/// [`check_jsx`] needs both (the root for the fragment and dropped-return rules)
/// and `ir.rs` needs both (the root to lower, the body for `statements`). One
/// walk answers both questions, so the two cannot disagree about which function
/// is the route or which node is its root.
pub(crate) struct RouteRender<'a> {
    /// The returned JSX, parentheses seen through.
    pub root: &'a Expression<'a>,
    /// The block body the root was returned from, or `None` for a concise arrow
    /// body (`(props) => <p/>`), where the JSX *is* the body.
    pub body: Option<&'a oxc_ast::ast::FunctionBody<'a>>,
    /// The span of the `return` statement the root came from, for slicing the
    /// statements before it. `None` for a concise body.
    pub return_span: Option<Span>,
}

/// The JSX the default export returns, or `None` when there is no such export.
///
/// Every accepted export shape is mirrored from `analyze::component`'s resolver:
/// an inline function declaration, function expression, or arrow function, any of
/// them parenthesized, or a name declared in this file — plainly or under
/// `export` — and exported as default.
pub(crate) fn route_render<'a>(program: &'a Program<'a>) -> Option<RouteRender<'a>> {
    for statement in &program.body {
        match statement {
            Statement::ExportDefaultDeclaration(declaration) => {
                return render_of_default(program, declaration);
            }
            Statement::ExportNamedDeclaration(declaration) => {
                // `?` here would abandon the whole walk when the first named
                // export is not `default` — `export { helper }` before a default
                // export is an ordinary file, and it would be reported as a
                // compiler defect. A non-default specifier means "keep looking".
                let Some(specifier) = declaration
                    .specifiers
                    .iter()
                    .find(|specifier| specifier.exported.name().as_str() == "default")
                else {
                    continue;
                };
                return render_of_name(program, specifier.local.name().as_str(), &mut Vec::new());
            }
            _ => {}
        }
    }
    None
}

/// [`route_render`] for `export default …`.
fn render_of_default<'a>(
    program: &'a Program<'a>,
    declaration: &'a ExportDefaultDeclaration<'a>,
) -> Option<RouteRender<'a>> {
    match &declaration.declaration {
        ExportDefaultDeclarationKind::FunctionDeclaration(function)
        | ExportDefaultDeclarationKind::FunctionExpression(function) => {
            render_of_function(function)
        }
        ExportDefaultDeclarationKind::ArrowFunctionExpression(arrow) => render_of_arrow(arrow),
        ExportDefaultDeclarationKind::ParenthesizedExpression(parenthesized) => {
            render_of_expression(program, &parenthesized.expression, &mut Vec::new())
        }
        ExportDefaultDeclarationKind::Identifier(identifier) => {
            render_of_name(program, identifier.name.as_str(), &mut Vec::new())
        }
        _ => None,
    }
}

/// [`route_render`] for an expression that may itself be a function or a name.
fn render_of_expression<'a>(
    program: &'a Program<'a>,
    expression: &'a Expression<'a>,
    seen: &mut Vec<&'a str>,
) -> Option<RouteRender<'a>> {
    match expression {
        Expression::ArrowFunctionExpression(arrow) => render_of_arrow(arrow),
        Expression::FunctionExpression(function) => render_of_function(function),
        Expression::ParenthesizedExpression(parenthesized) => {
            render_of_expression(program, &parenthesized.expression, seen)
        }
        Expression::Identifier(identifier) => {
            render_of_name(program, identifier.name.as_str(), seen)
        }
        _ => None,
    }
}

/// [`route_render`] for a name declared in this file.
///
/// `seen` stops `const Page = Page` from recursing forever: a self-reference
/// resolves to no JSX, which is what it is.
fn render_of_name<'a>(
    program: &'a Program<'a>,
    name: &'a str,
    seen: &mut Vec<&'a str>,
) -> Option<RouteRender<'a>> {
    if seen.contains(&name) {
        return None;
    }
    seen.push(name);

    program.body.iter().find_map(|statement| match statement {
        Statement::FunctionDeclaration(function) => function
            .id
            .as_ref()
            .filter(|identifier| identifier.name == name)
            .and_then(|_| render_of_function(function)),
        Statement::VariableDeclaration(declaration) => {
            render_of_declaration(program, declaration, name, seen)
        }
        // `export function Page() {…}` and `export const Page = …` are declared in
        // this file just as plainly as the unexported forms above, and
        // `analyze::component` resolves them. Without this arm a route exported
        // under `export` and then named in `export default Page` resolves in the
        // analyzer but not here, and lowering reports a compiler defect for a
        // legal route.
        Statement::ExportDeclaration(declaration) => match &declaration.declaration {
            Declaration::FunctionDeclaration(function) => function
                .id
                .as_ref()
                .filter(|identifier| identifier.name == name)
                .and_then(|_| render_of_function(function)),
            Declaration::VariableDeclaration(declaration) => {
                render_of_declaration(program, declaration, name, seen)
            }
            _ => None,
        },
        _ => None,
    })
}

/// [`render_of_name`] for the declarators of one `const`/`let`/`var`.
fn render_of_declaration<'a>(
    program: &'a Program<'a>,
    declaration: &'a oxc_ast::ast::VariableDeclaration<'a>,
    name: &'a str,
    seen: &mut Vec<&'a str>,
) -> Option<RouteRender<'a>> {
    declaration.declarations.iter().find_map(|declarator| {
        if !declarator
            .id
            .get_identifier_name()
            .is_some_and(|identifier| identifier.as_str() == name)
        {
            return None;
        }
        render_of_expression(program, declarator.init.as_ref()?, seen)
    })
}

/// [`route_render`] for a function declaration or expression.
fn render_of_function<'a>(function: &'a Function<'a>) -> Option<RouteRender<'a>> {
    let body = function.body.as_deref()?;
    let (returned, root) = jsx_return(body.statements.iter())?;
    Some(RouteRender {
        root,
        body: Some(body),
        return_span: Some(returned),
    })
}

/// [`route_render`] for an arrow function, whose body may be a block or the JSX.
fn render_of_arrow<'a>(
    arrow: &'a oxc_ast::ast::ArrowFunctionExpression<'a>,
) -> Option<RouteRender<'a>> {
    match &arrow.body {
        ArrowFunctionBody::FunctionBody(body) => {
            let (returned, root) = jsx_return(body.statements.iter())?;
            Some(RouteRender {
                root,
                body: Some(body),
                return_span: Some(returned),
            })
        }
        concise => {
            let expression = concise.as_expression()?;
            let root = jsx_expression(expression)?;
            Some(RouteRender {
                root,
                body: None,
                return_span: None,
            })
        }
    }
}

/// The first of these statements that returns JSX, with the statement's span.
fn jsx_return<'a>(
    mut statements: impl Iterator<Item = &'a Statement<'a>>,
) -> Option<(Span, &'a Expression<'a>)> {
    statements.find_map(|statement| match statement {
        Statement::ReturnStatement(returned) => returned
            .argument
            .as_ref()
            .and_then(jsx_expression)
            .map(|jsx| (returned.span, jsx)),
        _ => None,
    })
}

/// `expression` as JSX, seeing through the parentheses a multi-line return is
/// usually formatted with.
pub(crate) fn jsx_expression<'a>(expression: &'a Expression<'a>) -> Option<&'a Expression<'a>> {
    match expression {
        Expression::JSXElement(_) | Expression::JSXFragment(_) => Some(expression),
        Expression::ParenthesizedExpression(parenthesized) => {
            jsx_expression(&parenthesized.expression)
        }
        _ => None,
    }
}

/// Every JSX node the body returns, nested blocks included.
///
/// Descending into nested blocks is the point: `if (a) return <b/>` is a legal
/// body whose JSX return happens to be inside the `if`, and checking only the top
/// level would miss both it and a second one after it. Nested *functions* are not
/// descended into — their returns belong to another component's render.
pub(crate) fn jsx_returns<'a>(body: &'a oxc_ast::ast::FunctionBody<'a>) -> Vec<Span> {
    let mut found = JsxReturns::default();
    oxc_ast_visit::walk::walk_function_body(&mut found, body);
    found.returns
}

/// The span of the first JSX node in a body statement *before* `return_span` that
/// is not one of `excluded`, or `None`.
///
/// These statements are copied verbatim into [`crate::ir::RouteIr::statements`],
/// which the emitters write to a `.mjs`, where `<li>` is a syntax error. The check
/// is an AST walk over exactly the statements `ir::statements` slices, so the two
/// cannot disagree about which range was emitted. The route's own `return` is
/// excluded by span, and `excluded` carries the JSX of any second return: those
/// have their own, more specific diagnostic, and lowering never copies them.
pub(crate) fn jsx_in_body_statements<'a>(
    body: &'a FunctionBody<'a>,
    return_span: Span,
    excluded: &[Span],
) -> Option<Span> {
    jsx_spans(|finder| {
        for statement in body
            .statements
            .iter()
            .filter(|statement| statement.span().end <= return_span.start)
        {
            finder.visit_statement(statement);
        }
    })
    .into_iter()
    .find(|span| !excluded.contains(span))
}

/// The span of the first JSX node outside the route's own render, or `None`.
///
/// A route file emits one artifact: the route component's returned tree, plus (for
/// an island) its body statements, plus the module-level statements
/// [`RouteIr::module_statements`](crate::ir::RouteIr::module_statements) carries.
/// **JSX cannot be one of them**: the emitted `.mjs` would contain it, where it is
/// a syntax error, exactly as it would in a body statement. This is the same
/// defect as [`jsx_in_body_statements`], one scope out.
///
/// The excluded range is the route component's body — or, for a concise arrow
/// body, the returned JSX itself, since that *is* the body. Everything inside is
/// owned by the root, second-return, and body-statement rules; everything outside
/// is module level.
///
/// [`check_jsx`] only calls this when [`route_render`] found a route. A `.tsx`
/// file with no default export is a component module: this milestone does not
/// emit it at all, so a module-level JSX helper there is accepted.
pub(crate) fn jsx_outside_route_render<'a>(
    program: &'a Program<'a>,
    render: &RouteRender<'a>,
) -> Option<Span> {
    let route = render
        .body
        .map_or_else(|| render.root.span(), |body| body.span);

    jsx_spans(|finder| {
        for statement in &program.body {
            finder.visit_statement(statement);
        }
    })
    .into_iter()
    .find(|span| span.start < route.start || span.end > route.end)
}

/// Collects the JSX each `return` statement returns, skipping nested functions.
#[derive(Default)]
struct JsxReturns {
    returns: Vec<Span>,
}

impl<'a> Visit<'a> for JsxReturns {
    fn visit_return_statement(&mut self, returned: &ReturnStatement<'a>) {
        if let Some(jsx) = returned.argument.as_ref().and_then(jsx_expression) {
            // The JSX expression's own span, not the statement's: it is what the
            // lowering emits, so "is this the root?" is a span comparison.
            self.returns.push(jsx.span());
        }
        oxc_ast_visit::walk::walk_return_statement(self, returned);
    }

    // A nested function has its own render, so its `return`s are not this
    // body's. Not calling `walk_function`/`walk_arrow_function_expression` is
    // what skips them.
    fn visit_function(&mut self, _function: &Function<'a>, _flags: oxc_syntax::scope::ScopeFlags) {}
    fn visit_arrow_function_expression(
        &mut self,
        _arrow: &oxc_ast::ast::ArrowFunctionExpression<'a>,
    ) {
    }
}

/// Whether the *nearest* sibling in `children` is inline.
///
/// Nearest, not "any": the space a line break deletes sits between the two
/// siblings on either side of it. A distant inline element behind a block sibling
/// is not adjacent, and counting it would flag an ordinary block layout such as
/// `<div><p>a</p>\n<span>x</span>\n<p>b</p></div>`.
fn nearest_inline<'c, I: Iterator<Item = &'c JSXChild<'c>>>(mut children: I) -> bool {
    children
        .find(
            |child| !matches!(child, JSXChild::Text(text) if text.value.as_str().trim().is_empty()),
        )
        .is_some_and(is_inline_child)
}

/// Whether a child flows inline with its siblings.
fn is_inline_child(child: &JSXChild<'_>) -> bool {
    match child {
        JSXChild::Text(text) => !text.value.as_str().trim().is_empty(),
        JSXChild::ExpressionContainer(_) => true,
        JSXChild::Element(element) => is_inline(element),
        JSXChild::Fragment(fragment) => fragment.children.iter().any(is_inline_child),
        JSXChild::Spread(_) => false,
    }
}

/// Whether an element has an inline tag.
fn is_inline(element: &JSXElement<'_>) -> bool {
    match &element.opening_element.name {
        JSXElementName::Identifier(identifier) => INLINE_TAGS.contains(&identifier.name.as_str()),
        _ => false,
    }
}

/// The span of the first JSX node inside an expression, or `None`.
///
/// A walk, not a substring search: `a < b`, `(x) => x < 1`, and a `<`-containing
/// template literal are ordinary expressions whose text happens to contain `<`,
/// and refusing them would reject valid routes.
fn first_jsx(expression: &JSXExpression<'_>) -> Option<Span> {
    jsx_spans(|finder| finder.visit_jsx_expression(expression))
        .into_iter()
        .next()
}

/// Every JSX node the walk meets, in walk order.
///
/// One collector for every "where is the first JSX?" question in the crate: the
/// expression refusal here, the body-statement refusal, and lowering's invariant.
/// Callers take `.first()` when they only need one, so "first" has one meaning
/// rather than one implementation per caller.
#[derive(Default)]
pub(crate) struct JsxSpans {
    spans: Vec<Span>,
}

impl<'a> Visit<'a> for JsxSpans {
    fn visit_jsx_element(&mut self, element: &JSXElement<'a>) {
        self.spans.push(element.span);
    }

    fn visit_jsx_fragment(&mut self, fragment: &JSXFragment<'a>) {
        self.spans.push(fragment.span);
    }
}

/// The JSX spans `walk` reaches, in walk order.
///
/// The spans are a list rather than just the first because a second `return`'s JSX
/// is already reported by its own diagnostic: if it came first in a body it would
/// otherwise hide an ordinary JSX-valued statement after it.
pub(crate) fn jsx_spans(walk: impl FnOnce(&mut JsxSpans)) -> Vec<Span> {
    let mut finder = JsxSpans::default();
    walk(&mut finder);
    finder.spans
}

/// The `RA007` for a line break that deletes a space.
///
/// The `message` names this cause and carries [`SPACE_FIX`]; `fix` is
/// [`Code::RA007`]'s canonical text, which quotes the same sentence, so an agent
/// reading either form gets the repair.
fn space_ambiguity(span: Span) -> Diagnostic {
    Diagnostic::new(
        Code::RA007,
        (span.start, span.end),
        format!(
            "a line break between two inline elements removes the space between them: JSX \
             deletes whitespace-only text that contains a newline. {SPACE_FIX}"
        ),
    )
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

/// The `RA014` for JSX in a body statement that lowering copies verbatim.
///
/// The construct and repair are shared with [`crate::ir::lower_route`], so the
/// analysis list and the lowering list cannot describe the same refusal two ways.
pub(crate) const BODY_JSX_CONSTRUCT: &str = "JSX in a route body statement";
pub(crate) const BODY_JSX_REPAIR: &str = "the component body is copied into the generated \
                                          module — the island's client module, or the prerender \
                                          module a `\"use static\"` route runs at build time — \
                                          where JSX is a syntax error. Write the markup inline \
                                          in the returned tree instead.";

fn body_jsx(span: Span) -> Diagnostic {
    unsupported(span, BODY_JSX_CONSTRUCT, BODY_JSX_REPAIR)
}

/// The `RA014` for JSX in a module-level statement of a route file.
///
/// A route file's module-level declarations *are* copied into the generated
/// module now ([`crate::ir::RouteIr::module_statements`]), so the defect is no
/// longer that the markup is dropped: it is that the emitted `.mjs` would carry
/// JSX, where it is a syntax error exactly as it would be in a body statement.
/// A module-level JSX value cannot be expressed in plain JavaScript.
/// Shared with [`crate::ir::lower_route`], like the body-statement pair.
pub(crate) const MODULE_JSX_CONSTRUCT: &str = "JSX outside the route's returned tree";
pub(crate) const MODULE_JSX_REPAIR: &str = "module-level statements of a route file are copied \
                                            into the generated module, where JSX is a syntax \
                                            error exactly as it is in a route body statement, so \
                                            a module-level value cannot hold markup. Write the \
                                            markup inline in the returned tree, or move the helper \
                                            into a component module (a `.tsx` file with no default \
                                            export).";

fn module_jsx(span: Span) -> Diagnostic {
    unsupported(span, MODULE_JSX_CONSTRUCT, MODULE_JSX_REPAIR)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::parse_tsx;

    fn codes(src: &str) -> Vec<Code> {
        let alloc = oxc_allocator::Allocator::default();
        let program = parse_tsx(&alloc, src).unwrap();
        check_jsx(program, src).iter().map(|d| d.code).collect()
    }

    fn check(src: &str) -> Vec<Diagnostic> {
        let alloc = oxc_allocator::Allocator::default();
        let program = parse_tsx(&alloc, src).unwrap();
        check_jsx(program, src)
    }

    #[test]
    fn line_break_between_inline_siblings_is_ra007() {
        assert_eq!(
            codes("const a = <p><b>a</b>\n<i>b</i></p>\n"),
            vec![Code::RA007]
        );
    }

    #[test]
    fn explicit_space_is_clean() {
        assert!(codes("const a = <p><b>a</b>{\" \"}<i>b</i></p>\n").is_empty());
    }

    #[test]
    fn line_break_between_block_siblings_is_clean() {
        assert!(codes("const a = <div>\n  <p>a</p>\n  <p>b</p>\n</div>\n").is_empty());
    }

    #[test]
    fn component_tag_is_ra014() {
        assert_eq!(
            codes("const a = <div><Greeting name=\"Ada\" /></div>\n"),
            vec![Code::RA014]
        );
    }

    #[test]
    fn jsx_inside_an_expression_is_ra014() {
        assert_eq!(
            codes("const a = <div>{ok ? <b>y</b> : null}</div>\n"),
            vec![Code::RA014]
        );
    }

    // --- Everything below pins one half of the "two lists are equal" contract --

    /// The `RA007` message must name the cause and carry the pinned repair, and
    /// the canonical fix must quote that repair. One sentence, two forms.
    #[test]
    fn the_space_fix_is_part_of_the_canonical_ra007_fix() {
        assert!(
            Code::RA007.fix_template().contains(SPACE_FIX),
            "the canonical fix lost the whitespace repair: {}",
            Code::RA007.fix_template()
        );
        let diagnostics = check("const a = <p><b>a</b>\n<i>b</i></p>\n");
        assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
        assert_eq!(diagnostics[0].code, Code::RA007);
        assert!(
            diagnostics[0].message.contains(SPACE_FIX),
            "the message must carry the repair: {:?}",
            diagnostics[0].message
        );
        assert!(
            diagnostics[0].fix.contains(SPACE_FIX),
            "the fix must be actionable: {:?}",
            diagnostics[0].fix
        );
        // The span is the whitespace text, so the agent can find it.
        let span = diagnostics[0].span;
        assert!(
            span.1 > span.0,
            "a zero-width span points at nothing: {span:?}"
        );
    }

    /// A newline between an inline element and an expression container is the
    /// same deletion: `<b>a</b>` and `{n}` separated by a line break render with
    /// no space between them.
    #[test]
    fn a_line_break_around_an_expression_container_is_ra007() {
        assert_eq!(
            codes("const a = <p><b>a</b>\n{n}<i>b</i></p>\n"),
            vec![Code::RA007]
        );
    }

    /// A non-whitespace text with a line break is *not* this rule: its words are
    /// trimmed and joined, so nothing is lost. Reporting it would flag ordinary
    /// paragraphs.
    #[test]
    fn a_paragraph_with_line_breaks_is_clean() {
        assert!(codes("const a = <p>\n  hello\n  there\n</p>\n").is_empty());
    }

    /// A trailing line break at the end of a block is not between two inline
    /// siblings, and JSX drops it with nothing visible lost.
    #[test]
    fn a_trailing_line_break_is_clean() {
        assert!(codes("const a = <p><b>a</b>\n</p>\n").is_empty());
    }

    /// Adjacency is the *nearest* sibling, not any sibling. Here the line break
    /// sits between two block-level `<p>` elements while an inline `<span>` is
    /// elsewhere in the parent: no space was ever rendered between the `<p>`s, so
    /// nothing was deleted and nothing may be reported.
    #[test]
    fn a_line_break_between_block_siblings_is_clean_even_with_inline_distant_siblings() {
        assert!(codes("const a = <div><span>x</span>\n<p>a</p>\n<p>b</p>\n</div>\n").is_empty());
    }

    /// Every tag refusal, one fixture each. These are the RA014 arms of the
    /// contract, and `ir.rs`'s `the_two_lists_are_equal` refuses the same shapes.
    ///
    /// `<For>`/`<Show>`/`<When>` get one assertion per tag: they are the runtime's
    /// own primitives, not components, so their message must not send an agent to
    /// component inlining that does not exist for them.
    #[test]
    fn every_unsupported_tag_is_ra014() {
        for (src, construct) in [
            ("const a = <div><Greeting /></div>\n", "a capitalized tag"),
            (
                "const a = <div><Shared.Header /></div>\n",
                "member-expression",
            ),
            ("const a = <div><svg:rect /></div>\n", "namespaced tag"),
            ("const a = <div><this /></div>\n", "`<this>` tag"),
            (
                "const a = <div><For each={x} /></div>\n",
                "runtime primitive",
            ),
            (
                "const a = <div><Show when={x} /></div>\n",
                "runtime primitive",
            ),
            (
                "const a = <div><When value={x} /></div>\n",
                "runtime primitive",
            ),
        ] {
            let diagnostics = check(src);
            assert_eq!(diagnostics.len(), 1, "{src}: {diagnostics:?}");
            assert_eq!(diagnostics[0].code, Code::RA014, "{src}");
            assert!(
                diagnostics[0].message.contains(construct),
                "{src}: the message must name the construct, got {:?}",
                diagnostics[0].message
            );
        }
    }

    /// The runtime primitives say what the agent can actually do, and name the tag
    /// it wrote. "Inline it" is not a repair for a primitive, and "later plan"
    /// alone gives the agent nothing legal to try now.
    ///
    /// `<When>`'s repair names the `pending` branch, not the success branch: a
    /// design.md `resource` only has `pending` reachable at build time (§7.1), so
    /// "render the success branch" is advice the agent cannot apply.
    #[test]
    fn a_runtime_primitive_repair_is_actionable() {
        for (tag, repair) in [
            ("For", "one element at a time"),
            ("Show", "conditionally"),
            ("When", "`pending` branch"),
        ] {
            let src = format!("const a = <div><{tag} /></div>\n");
            let diagnostics = check(&src);
            assert_eq!(diagnostics.len(), 1, "{src}: {diagnostics:?}");
            assert!(
                diagnostics[0].message.contains(&format!("`<{tag}>`")),
                "{src}: the message must name the tag, got {:?}",
                diagnostics[0].message
            );
            assert!(
                diagnostics[0].message.contains(repair),
                "{src}: the message must carry the repair, got {:?}",
                diagnostics[0].message
            );
        }
    }

    /// The other RA014 arms.
    #[test]
    fn every_unsupported_construct_is_ra014() {
        for (src, construct) in [
            ("const a = <div {...rest} />\n", "spread attribute"),
            ("const a = <div>{...items}</div>\n", "spread child"),
            (
                "const a = <div title=<b/> />\n",
                "an attribute whose value is itself JSX",
            ),
            (
                "export default function P() { return <><b/><i/></> }\n",
                "fragment as the route's returned JSX",
            ),
            (
                "export default function P() { if (a) return <b/>; return <i/> }\n",
                "second JSX return",
            ),
        ] {
            let diagnostics = check(src);
            assert_eq!(diagnostics.len(), 1, "{src}: {diagnostics:?}");
            assert_eq!(diagnostics[0].code, Code::RA014, "{src}");
            assert!(
                diagnostics[0].message.contains(construct),
                "{src}: the message must name the construct, got {:?}",
                diagnostics[0].message
            );
        }
    }

    /// JSX in a body statement is copied into the emitted module as JavaScript,
    /// where it is a syntax error. Both shapes are refused, and the diagnostic
    /// names the construct.
    #[test]
    fn jsx_in_a_body_statement_is_ra014() {
        for src in [
            // An ordinary JSX-valued local.
            "export default function P() { const header = <h1>x</h1>; return <p>{header}</p> }\n",
            // A nested renderer's `return`, whose JSX never reaches the module.
            "export default function P() { function Item() { return <li/> } return <ul/> }\n",
            "export default function P() { const Item = () => { return <li/> }; return <ul/> }\n",
            // A component declared above the route's return.
            "export default function P() { const Item = () => <li/>; return <ul/> }\n",
        ] {
            let diagnostics = check(src);
            assert_eq!(diagnostics.len(), 1, "{src}: {diagnostics:?}");
            assert_eq!(diagnostics[0].code, Code::RA014, "{src}");
            assert!(
                diagnostics[0]
                    .message
                    .contains("JSX in a route body statement"),
                "{src}: the message must name the construct, got {:?}",
                diagnostics[0].message
            );
        }
    }

    /// A second return's JSX has its own diagnostic. If it comes first in the body
    /// it must not hide an ordinary JSX-valued statement after it: both are
    /// reported, each under its own construct and at its own span.
    #[test]
    fn a_second_return_does_not_hide_a_later_body_statement() {
        let src = "export default function P() { if (a) return <b/>; const header = <h1/>; return <i/> }\n";
        let diagnostics = check(src);
        assert_eq!(diagnostics.len(), 2, "{diagnostics:?}");
        assert!(diagnostics.iter().all(|d| d.code == Code::RA014));

        let body = diagnostics
            .iter()
            .find(|d| d.message.contains("JSX in a route body statement"))
            .expect("the body statement must be reported");
        let (start, end) = body.span;
        assert_eq!(
            &src[start as usize..end as usize],
            "<h1/>",
            "the body-statement span must be that statement's JSX, not the second return's: {body:?}"
        );

        let dropped = diagnostics
            .iter()
            .find(|d| d.message.contains("second JSX return"))
            .expect("the second return must be reported");
        let (start, end) = dropped.span;
        assert_eq!(&src[start as usize..end as usize], "<b/>", "{dropped:?}");
    }

    /// A nested renderer that is *not* JSX-valued is an ordinary function: a
    /// handler, a helper returning a number, a component defined in another
    /// declaration position. None of them carries JSX into `statements`.
    #[test]
    fn a_nested_renderer_without_jsx_is_clean() {
        for src in [
            "export default function P() { function twice(x) { return x * 2 } return <p>{twice(1)}</p> }\n",
            // A concise arrow body is the JSX; there is no return statement, and
            // nothing else in the body.
            "export default (props: Record<never, never>) => <p/>\n",
            "export default (props: Record<never, never>) => { return <p/> }\n",
            // A plain handler is not a renderer at all.
            "export default function P() { return <button onClick={() => go()}>x</button> }\n",
        ] {
            assert!(codes(src).is_empty(), "{src}: {:?}", check(src));
        }
    }

    /// The capitalized branch of `dom_tag`'s `Identifier` arm is reachable only
    /// for a *hyphenated* capital, and this is why the comment there is precise:
    /// Oxc decides `Identifier` vs `IdentifierReference` by the name alone
    /// (`!contains_dash &&` first byte in `A-Z`/`_`/`$`/non-ASCII), never by
    /// attributes. So `<For/>` and `<For each={x}/>` are the same node kind, while
    /// `<My-Element/>` is the only shape that lands in the other arm.
    #[test]
    fn a_hyphenated_capital_is_the_reachable_identifier_arm() {
        let diagnostics = check("const a = <div><My-Element /></div>\n");
        assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
        assert_eq!(diagnostics[0].code, Code::RA014);
        assert!(
            diagnostics[0].message.contains("a capitalized tag"),
            "{:?}",
            diagnostics[0].message
        );

        // A lowercase hyphenated name is a custom element, which a template can
        // hold: it is not refused by the tag rule.
        assert!(codes("const a = <div><my-element /></div>\n").is_empty());
    }

    /// A component tag nested inside another component is reported once per tag,
    /// not once per ancestor. Asserting only the *count* would pass if one tag
    /// were reported twice and the other never, so each tag's message is counted.
    #[test]
    fn each_unsupported_tag_is_reported_once() {
        let diagnostics = check("const a = <div><Greeting><Other /></Greeting></div>\n");
        assert_eq!(diagnostics.len(), 2, "{diagnostics:?}");
        assert!(diagnostics.iter().all(|d| d.code == Code::RA014));
        for tag in ["`<Greeting>`", "`<Other>`"] {
            assert_eq!(
                diagnostics
                    .iter()
                    .filter(|d| d.message.contains(tag))
                    .count(),
                1,
                "{tag} must be reported exactly once: {diagnostics:?}"
            );
        }
    }

    /// The spans are the agent's own: they index the *original* source, so the
    /// slice a span names is the text the agent wrote. An implementation that
    /// shifted every span would pass a code-only assertion, so one is checked
    /// against the fixture by hand.
    #[test]
    fn a_span_slices_the_text_the_agent_wrote() {
        let src = "const a = <p><b>a</b>\n<i>b</i></p>\n";
        let diagnostics = check(src);
        assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
        let (start, end) = diagnostics[0].span;
        assert_eq!(
            &src[start as usize..end as usize],
            "\n",
            "the span must be exactly the line break: {:?}",
            diagnostics[0]
        );
    }

    /// Module-level statements *are* emitted now, into the generated `.mjs`, so
    /// the defect is that the emitted module would contain JSX — a syntax error
    /// there, exactly as in a body statement. The reviewer's exact reproduction:
    /// lowering returned a tree with `Expr("header")` and this diagnostic, i.e. a
    /// module that could not parse.
    #[test]
    fn module_level_jsx_is_ra014() {
        for src in [
            // The reproduction, verbatim.
            "\"use island\"\nconst header = <h1>hi</h1>\nexport default function P(props: Record<never, never>) { return <p>{header}</p> }\n",
            // A module-level helper that renders: its body is JSX.
            "\"use island\"\nfunction Header(props: Record<never, never>) { return <h1>x</h1> }\nexport default function P(props: Record<never, never>) { return <p/> }\n",
            // The same, as a const-bound arrow.
            "\"use island\"\nconst Header = (props: Record<never, never>) => <h1>x</h1>\nexport default function P(props: Record<never, never>) { return <p/> }\n",
            // A concise route body has no statements, so only the module walk can
            // see this one.
            "\"use island\"\nconst header = <h1>hi</h1>\nexport default (props: Record<never, never>) => <p>{header}</p>\n",
        ] {
            let diagnostics = check(src);
            assert_eq!(diagnostics.len(), 1, "{src}: {diagnostics:?}");
            assert_eq!(diagnostics[0].code, Code::RA014, "{src}");
            assert!(
                diagnostics[0]
                    .message
                    .contains("JSX outside the route's returned tree"),
                "{src}: the message must name the construct, got {:?}",
                diagnostics[0].message
            );
        }
    }

    /// The refusal is scoped to files that **have** a route. A `.tsx` file with no
    /// default export is a component module: this milestone emits nothing from it,
    /// so a module-level JSX helper there is accepted, and a later plan that does
    /// emit component modules is free to accept the same shape.
    #[test]
    fn a_non_route_module_may_hold_module_level_jsx() {
        for src in [
            "const header = <h1>hi</h1>\nexport function Header(props: Record<never, never>) { return header }\n",
            "const header = <h1>hi</h1>\n",
            // Defined plainly, not under `export`: still no default export.
            "function Header(props: Record<never, never>) { return <h1>hi</h1> }\n",
        ] {
            assert!(codes(src).is_empty(), "{src}: {:?}", check(src));
        }
    }

    /// The span is the module-level JSX itself, so the agent can find it — not the
    /// route's `return` or the whole file.
    #[test]
    fn a_module_level_span_slices_the_offending_jsx() {
        let src = "\"use island\"\nconst header = <h1>hi</h1>\nexport default function P(props: Record<never, never>) { return <p>{header}</p> }\n";
        let diagnostics = check(src);
        assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
        let (start, end) = diagnostics[0].span;
        assert_eq!(&src[start as usize..end as usize], "<h1>hi</h1>");
    }

    /// A module-level statement with no JSX is ordinary: an import, a constant, a
    /// helper. Refusing those would reject every real page.
    ///
    /// This is a statement about **this walk only**: `check_jsx` has no business
    /// with a constant, and whether the markup may *name* it is a different
    /// question answered by `analyze::reachable` (which now accepts it, because
    /// `RouteIr::module_statements` emits it). The first fixture is kept here so
    /// a future reader does not move that rule into this walk, where it would
    /// refuse a component module's legal module-level value.
    #[test]
    fn module_level_statements_without_jsx_are_clean() {
        for src in [
            "\"use static\"\nconst title = \"About\"\nexport default function P(props: Record<never, never>) { return <h1>{title}</h1> }\n",
            "\"use island\"\nimport { signal } from \"runai\"\nconst n = signal(0)\nexport default function P(props: Record<never, never>) { return <p>{n()}</p> }\n",
            "\"use island\"\nfunction twice(x: number) { return x * 2 }\nexport default function P(props: Record<never, never>) { return <p>{twice(1)}</p> }\n",
        ] {
            assert!(codes(src).is_empty(), "{src}: {:?}", check(src));
        }
    }

    /// `\r` is a line break too. A classic-Mac file separates its JSX lines with
    /// it, and the whitespace rule deletes the space there exactly as it does for
    /// `\n`. Reporting only `\n` would keep the node verbatim and stay silent.
    #[test]
    fn a_carriage_return_is_a_line_break() {
        let src = "const a = <p><b>a</b>\r<i>b</i></p>\n";
        let diagnostics = check(src);
        assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
        assert_eq!(diagnostics[0].code, Code::RA007);
        let (start, end) = diagnostics[0].span;
        assert_eq!(&src[start as usize..end as usize], "\r");
    }

    /// `{a < b}` is a comparison, not JSX. A string heuristic would refuse it.
    #[test]
    fn a_comparison_is_not_jsx_in_an_expression() {
        for src in [
            "const a = <div>{a < b}</div>\n",
            "const a = <div>{`a</b`}</div>\n",
            "const a = <p class={a < b}>x</p>\n",
            "const a = <div>{[1, 2].filter((x) => x < 2)}</div>\n",
        ] {
            assert!(codes(src).is_empty(), "{src}: {:?}", check(src));
        }
    }
}
