//! JSX structural checks that run on the **original** source.
//!
//! `analyze` runs these before anything is transformed, so every span points at
//! what the agent actually wrote: the file the diagnostic quotes is the file the
//! fix edits. Two rules live here.
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
//! - a capitalized tag (`<Greeting/>`) or the runtime's `<For>`/`<Show>`/`<When>`
//!   — component inlining comes with a later plan;
//! - a member-expression tag (`<Shared.Header/>`);
//! - a namespaced tag (`<svg:rect/>`) — this milestone emits unprefixed HTML;
//! - a `<this>` tag;
//! - JSX nested inside a `{…}` expression — it becomes JavaScript in the emitted
//!   module, where `<b/>` is a syntax error;
//! - an attribute whose value is itself JSX (`name=<Bar/>`);
//! - a spread attribute (`{...rest}`) or a spread child (`{...items}`);
//! - a JSX fragment as the route's returned markup — the IR's root is one node;
//! - a second JSX `return` in the route body — lowering emits one root, and
//!   dropping the other would render markup without part of a node.
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
    ArrowFunctionBody, ExportDefaultDeclaration, ExportDefaultDeclarationKind, Expression,
    Function, JSXAttributeItem, JSXAttributeValue, JSXChild, JSXElement, JSXElementName,
    JSXExpression, JSXExpressionContainer, JSXFragment, Program, ReturnStatement, Statement,
};
use oxc_ast_visit::Visit;
use oxc_span::{GetSpan, Span};

use crate::diagnostic::{Code, Diagnostic};

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
/// dropped returns), then each `RA014` refusal and the `RA007` ambiguity in
/// source order. Returning every diagnostic — not just the first — means one run
/// tells the agent everything to fix.
///
/// Runs on **every** `.tsx` file, not only pages, so a `_shell.tsx` component is
/// held to the same rules.
pub fn check_jsx(program: &Program, source: &str) -> Vec<Diagnostic> {
    let mut checker = JsxChecker {
        source,
        diagnostics: Vec::new(),
    };

    // The route's render is a separate question from "which elements exist", and
    // both rules below are about the *returned root*.
    if let Some(render) = route_render(program) {
        checker.root(render);
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
    /// diagnostic pass would hide every diagnostic after it.
    fn slice(&self, span: Span) -> &str {
        self.source
            .get(span.start as usize..span.end as usize)
            .unwrap_or("")
    }

    /// The two rules about the route's returned markup.
    fn root(&mut self, render: RouteRender<'_>) {
        match render.root {
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

        // One root is emitted, so a second JSX return would lose the other branch
        // with no report. Only the route body is walked, and nested functions are
        // skipped: their `return`s belong to another component's render.
        if let Some(body) = render.body {
            let root_span = render.root.span();
            if let Some(dropped) = jsx_returns(body)
                .into_iter()
                .find(|span| *span != root_span)
            {
                self.diagnostics.push(unsupported(
                    dropped,
                    "a second JSX return that lowering does not reach",
                    "Return one element from the component body. Move the other markup into \
                     that element, or split the branch into a component of its own.",
                ));
            }
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
            if !raw.trim().is_empty() || !raw.contains('\n') {
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
            // The parser lowercases nothing: a JSX *identifier* starting with a
            // capital is React's component convention, whatever Oxc's node kind
            // is called.
            if tag.starts_with(|c: char| c.is_ascii_uppercase()) {
                Err(Refusal::component(tag))
            } else {
                Ok(tag)
            }
        }
        JSXElementName::IdentifierReference(identifier) => {
            Err(Refusal::component(identifier.name.as_str()))
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
/// them parenthesized, or a name declared in this file and exported as default.
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
        _ => None,
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
    let mut finder = JsxFinder::default();
    finder.visit_jsx_expression(expression);
    finder.found
}

/// Records the first JSX node it meets while walking an expression.
#[derive(Default)]
struct JsxFinder {
    found: Option<Span>,
}

impl<'a> Visit<'a> for JsxFinder {
    fn visit_jsx_element(&mut self, element: &JSXElement<'a>) {
        self.found.get_or_insert(element.span);
    }

    fn visit_jsx_fragment(&mut self, fragment: &JSXFragment<'a>) {
        self.found.get_or_insert(fragment.span);
    }
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
    #[test]
    fn every_unsupported_tag_is_ra014() {
        for (src, construct) in [
            ("const a = <div><Greeting /></div>\n", "a capitalized tag"),
            (
                "const a = <div><For each={x} /></div>\n",
                "a capitalized tag",
            ),
            (
                "const a = <div><Shared.Header /></div>\n",
                "member-expression",
            ),
            ("const a = <div><svg:rect /></div>\n", "namespaced tag"),
            ("const a = <div><this /></div>\n", "`<this>` tag"),
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

    /// A nested renderer is not a dropped return: its `return` belongs to another
    /// component. The block-bodied shapes are the ones that would fail if the
    /// walk descended into nested functions.
    #[test]
    fn a_nested_renderer_is_not_a_second_return() {
        for src in [
            "export default function P() { function Item() { return <li/> } return <ul><li>hi</li></ul> }\n",
            "export default function P() { const Item = () => { return <li/> }; return <ul><li>hi</li></ul> }\n",
            // A concise arrow body is the JSX; there is no return statement.
            "export default (props: Record<never, never>) => <p/>\n",
            "export default (props: Record<never, never>) => { return <p/> }\n",
            // A plain handler is not a renderer at all.
            "export default function P() { return <button onClick={() => go()}>x</button> }\n",
        ] {
            assert!(codes(src).is_empty(), "{src}: {:?}", check(src));
        }
    }

    /// A component tag nested inside another component is reported once per tag,
    /// not once per ancestor.
    #[test]
    fn each_unsupported_tag_is_reported_once() {
        let diagnostics = check("const a = <div><Greeting><Other /></Greeting></div>\n");
        assert_eq!(diagnostics.len(), 2, "{diagnostics:?}");
        assert!(diagnostics.iter().all(|d| d.code == Code::RA014));
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
