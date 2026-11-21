//! TSX syntax tree to a template-and-bindings IR.
//!
//! [`crate::source`] answers "is this file valid TSX?" and [`crate::analyze`]
//! answers "is it a route this milestone can render?". This module answers the
//! next question: *what markup does it produce?*
//!
//! The IR splits a JSX tree into the two things codegen needs to keep apart:
//!
//! - **static structure** — tags, static attribute values, raw text — which
//!   becomes one template string that never enters the runtime, and
//! - **bindings** — expression slots — which are wired after it is cloned.
//!
//! Two properties of that split are load-bearing and pinned by tests here:
//!
//! - **Text is verbatim.** A text node carries the source slice exactly as
//!   written, so `&amp;` survives. Decoding entities is the browser's job; a
//!   compiler that re-encodes them writes `&amp;amp;` and the page shows
//!   `&amp;` instead of `&`.
//! - **Expression text comes from the source slice by span.** An expression is
//!   copied out of the file, not re-printed from the AST, so formatting and
//!   comments inside it survive for codegen and nothing is normalized into a
//!   shape no source line contains.
//!
//! # What is lowered
//!
//! | JSX | IR |
//! |---|---|
//! | `<div a="b">` | [`IrNode::Element`], attribute in `static_attrs` |
//! | `name={expr}` | [`Binding::Attr`] |
//! | `onEvent={expr}` | [`Binding::Event`], `name` being the DOM event name |
//! | `{expr}` as a child | [`Binding::Text`] plus an [`IrNode::Expr`] placeholder |
//! | `<Capitalized a={x}>` | [`IrNode::Component`] with props |
//! | `<></>` as a child | no node of its own; its children are spliced into the parent |
//!
//! # What is rejected instead of dropped
//!
//! These constructs are valid JSX that this milestone does not lower:
//!
//! - a spread (`{...rest}`) as an attribute or a child;
//! - an attribute whose value is itself JSX (`foo=<Bar />`);
//! - an attribute with an empty expression (`foo={}`);
//! - a fragment as the *returned* root;
//! - **JSX anywhere inside a `{…}` expression** — `{ready ? <b/> : <i/>}`,
//!   `{items.map(i => <li/>)}`, `bar={<p/>}`. The expression is copied into
//!   the emitted module as JavaScript, where `<b/>` is a syntax error, so the
//!   island would fail to parse at build time;
//! - **a second JSX return** the lowering does not reach
//!   (`if (a) return <b/>; return <i/>`). Lowering emits only the first, and
//!   dropping the other would render markup without part of a node.
//!
//! All of them report `RA014_NOT_SUPPORTED_YET`, whose identity says what they
//! are: valid syntax this milestone does not support. `RA007` stays what it is
//! everywhere else — "the parser rejected this file" — because a skill file that
//! sends an agent hunting for an unclosed tag in a file that parses repairs
//! nothing.

use oxc_allocator::Allocator;
use oxc_ast::ast::{
    ArrowFunctionBody, ExportDefaultDeclaration, ExportDefaultDeclarationKind, Expression,
    Function, JSXAttributeItem, JSXAttributeName, JSXAttributeValue, JSXChild, JSXElement,
    JSXExpression, JSXFragment, Program, ReturnStatement, Statement,
};
use oxc_ast_visit::Visit;
use oxc_span::{GetSpan, Span};

use crate::analyze::{ComponentInfo, find_route_component, validate_route_component};
use crate::diagnostic::{Code, Diagnostic};
use crate::source::{Mode, detect_mode, parse_tsx, raw_text};

/// One dynamic slot in an element.
///
/// An element's `bindings` is the whole list for that one level: attributes in
/// source order, then text slots in source order. A child element carries its
/// own list.
///
/// A [`Binding::Text`] and the matching [`IrNode::Expr`] hold the same source
/// text. The child list needs a placeholder so document order survives; the
/// binding list needs a slot so codegen can address it. The *k*-th
/// `Binding::Text` in `bindings` corresponds to the *k*-th [`IrNode::Expr`] in
/// `children`, and that correspondence is what Task 11 wires.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Binding {
    /// `{expr}` as a child: the element's content is set from it.
    Text(String),
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
    /// Raw text, exactly as written in the source.
    Text(String),
    /// A placeholder for a dynamic child slot.
    ///
    /// The expression text is the same as the matching [`Binding::Text`]'s; see
    /// [`Binding`] for why both exist.
    Expr(String),
    /// A capitalized tag: another component.
    ///
    /// `props` holds the attributes as written: a static value is
    /// [`IrNode::Text`], an expression is [`IrNode::Expr`], and a bare attribute
    /// (`<Foo flag />`) is [`IrNode::Expr`] `"true"`, which is what JSX means by
    /// it. There is no `bindings` list on a component — there is no DOM node to
    /// address, so a dynamic child is carried by its [`IrNode::Expr`] alone.
    ///
    /// `children` is `<Child><p/></Child>`'s markup: per design §7.2 it reaches
    /// the callee as `props.children`, so Task 11 emits it as that prop rather
    /// than as an unnamed child. A component's children are nodes like any
    /// element's, and an expression child (`<Child>{x}</Child>`) is an
    /// [`IrNode::Expr`] here just as it is on an element — there is simply no
    /// parent `bindings` list to pair it with, because the pairing belongs to
    /// whichever component inlines it.
    Component {
        name: String,
        props: Vec<(String, IrNode)>,
        children: Vec<IrNode>,
    },
}

/// A lowered route file: its rendering mode, its component name, its markup, and
/// the source range of the component body it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteIr {
    pub mode: Mode,
    /// The route component's name, or `Page` when it is genuinely anonymous.
    /// `"default"` would name the export rather than a symbol in the file, and
    /// `export default function default(...)` is a parse error — so diagnostic
    /// text and codegen both need the readable name.
    pub name: String,
    pub tree: IrNode,
    /// The route component's body span, for the island emitter.
    ///
    /// Task 11 emits the body's statements (`const n = signal(0)`) above the
    /// template construction, and taking the range from here means it slices the
    /// source instead of re-parsing the file. The span is the `FunctionBody` for
    /// a block body and the expression for a concise arrow body
    /// (`() => <p/>`), which has no statements to emit.
    pub body_span: (u32, u32),
}

/// The one entry point: parse the file, read its mode, lower its JSX.
///
/// The file is parsed exactly once — the mode is read from the parsed program, so
/// there is one parse path and one place a syntax error can surface. A file that
/// both lacks a directive and is broken reports the syntax error, which is the one
/// an agent has to fix first either way.
pub fn lower_route(source: &str) -> Result<RouteIr, Vec<Diagnostic>> {
    let alloc = Allocator::default();
    let program = parse_tsx(&alloc, source)?;
    let mode = detect_mode(program, source).map_err(|diagnostic| vec![diagnostic])?;

    // One summary, computed once and used three ways: the lowering walk
    // validates the component with it, and `RouteIr` takes its name and body
    // span. The first version walked the program a third time here and had a
    // `Page` fallback that no successful lowering could reach.
    let info = find_route_component(program);
    let tree = lower_component(program, info.as_ref())?;
    // `lower_component` refuses a missing or unusable component, so a success
    // here means `info` is `Some`, and its fields are exactly the ones lowered.
    let Some(info) = info else {
        return Err(vec![unlowerable_export()]);
    };

    Ok(RouteIr {
        mode,
        name: info.display_name().to_string(),
        tree,
        body_span: info.body_span,
    })
}

/// Lower the first JSX node the default export returns.
///
/// The default export is resolved through the same shapes
/// [`find_route_component`] accepts — an inline function declaration, function
/// expression, or arrow function, any of them parenthesized, or a name declared
/// in this file and exported as default. A default export that is none of those,
/// or a body that does not return JSX directly, reports that module's `RA011`
/// diagnostics rather than inventing a second opinion; `lowering_agrees_with_the_route_analyzer`
/// is what keeps the two answers glued together.
pub fn lower_jsx(program: &Program) -> Result<IrNode, Vec<Diagnostic>> {
    let info = find_route_component(program);
    lower_component(program, info.as_ref())
}

/// [`lower_jsx`] for a summary that has already been computed.
///
/// Splitting this out is what lets [`lower_route`] reuse one
/// `find_route_component` result for the lowering, the name, and the body span.
fn lower_component<'a>(
    program: &'a Program<'a>,
    info: Option<&ComponentInfo<'a>>,
) -> Result<IrNode, Vec<Diagnostic>> {
    let diagnostics = validate_route_component(info);
    if !diagnostics.is_empty() {
        return Err(diagnostics);
    }

    let root = route_jsx(program).ok_or_else(|| vec![unlowerable_export()])?;

    // One root is emitted, so a body with a second JSX return would lose the
    // other branch with no report. The analyzer accepts such a body (it asks
    // only whether *some* return is JSX), which is why the check lives here —
    // and why it descends into nested blocks rather than trusting the top-level
    // scan that found `root`.
    if let Some(dropped) = dropped_jsx_return(program, root.span()) {
        return Err(vec![unsupported(
            dropped,
            "a second JSX return that lowering does not reach",
            "Return one element from the component body. Move the other markup into that \
             element, or split the branch into a component of its own.",
        )]);
    }

    Lowerer {
        source: program.source_text,
    }
    .root(root)
}

/// The span of a JSX return in the route body that lowering will not emit.
///
/// The root is the one `route_jsx` found; every other JSX return in the body
/// would be dropped, so the first of them is what the diagnostic points at. It
/// is the *nested* one that matters: `route_jsx` scans only the top level, so
/// `if (a) return <b/>; return <i/>` lowers `<i/>` and drops `<b/>`.
fn dropped_jsx_return(program: &Program, root: Span) -> Option<Span> {
    route_body(program)
        .map(|body| body.jsx_returns())
        .and_then(|returns| returns.into_iter().find(|span| *span != root))
}

/// The JSX node the default export returns, with parentheses removed.
///
/// This is a second walk of the same declarations because the summary in
/// `analyze::component` answers *whether* there is a component, not *which
/// node* it returns. Every accepted shape is mirrored exactly; when they agree,
/// the answer here exists.
fn route_jsx<'a>(program: &'a Program<'a>) -> Option<&'a Expression<'a>> {
    route_body(program).and_then(|body| body.jsx())
}

/// The component body the route's JSX comes from.
///
/// This is [`route_jsx`]'s walk, one level up: it returns the body node so the
/// dropped-return check can inspect the same body the lowering does, without a
/// second opinion about which function is the route component.
fn route_body<'a>(program: &'a Program<'a>) -> Option<ResolvedBody<'a>> {
    for statement in &program.body {
        match statement {
            Statement::ExportDefaultDeclaration(declaration) => {
                return body_of_default(program, declaration);
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
                return body_of_name(program, specifier.local.name().as_str(), &mut Vec::new());
            }
            _ => {}
        }
    }
    None
}

/// The body a route's JSX is rendered from.
enum ResolvedBody<'a> {
    /// `function Page() { … }`: statements, one of which returns JSX.
    Block(&'a oxc_ast::ast::FunctionBody<'a>),
    /// `(props) => <p/>`: the JSX is the body, so there is no return to find.
    Concise(&'a Expression<'a>),
}

impl<'a> ResolvedBody<'a> {
    /// The JSX this body renders, exactly as [`jsx_of_function`] and
    /// [`jsx_of_arrow`] used to return it.
    fn jsx(&self) -> Option<&'a Expression<'a>> {
        match self {
            Self::Block(body) => jsx_return(body.statements.iter()),
            Self::Concise(expression) => jsx_expression(expression),
        }
    }

    /// Every JSX node this body returns, nested blocks included.
    ///
    /// Descending into nested blocks is the point: `if (a) return <b/>` is a
    /// legal body whose JSX return happens to be inside the `if`, and checking
    /// only the top level would miss both it and a second one after it. Nested
    /// *functions* are not descended into — their returns belong to another
    /// component's render, not this one's.
    fn jsx_returns(&self) -> Vec<Span> {
        let mut found = JsxReturns::default();
        match self {
            Self::Concise(expression) => {
                if let Some(jsx) = jsx_expression(expression) {
                    found.returns.push(jsx.span());
                }
            }
            Self::Block(body) => oxc_ast_visit::walk::walk_function_body(&mut found, body),
        }
        found.returns
    }
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

/// [`route_body`] for `export default …`.
fn body_of_default<'a>(
    program: &'a Program<'a>,
    declaration: &'a ExportDefaultDeclaration<'a>,
) -> Option<ResolvedBody<'a>> {
    match &declaration.declaration {
        ExportDefaultDeclarationKind::FunctionDeclaration(function)
        | ExportDefaultDeclarationKind::FunctionExpression(function) => body_of_function(function),
        ExportDefaultDeclarationKind::ArrowFunctionExpression(arrow) => body_of_arrow(arrow),
        ExportDefaultDeclarationKind::ParenthesizedExpression(parenthesized) => {
            body_of_expression(program, &parenthesized.expression, &mut Vec::new())
        }
        ExportDefaultDeclarationKind::Identifier(identifier) => {
            body_of_name(program, identifier.name.as_str(), &mut Vec::new())
        }
        _ => None,
    }
}

/// [`route_body`] for an expression that may itself be a function or a name.
fn body_of_expression<'a>(
    program: &'a Program<'a>,
    expression: &'a Expression<'a>,
    seen: &mut Vec<&'a str>,
) -> Option<ResolvedBody<'a>> {
    match expression {
        Expression::ArrowFunctionExpression(arrow) => body_of_arrow(arrow),
        Expression::FunctionExpression(function) => body_of_function(function),
        Expression::ParenthesizedExpression(parenthesized) => {
            body_of_expression(program, &parenthesized.expression, seen)
        }
        Expression::Identifier(identifier) => body_of_name(program, identifier.name.as_str(), seen),
        _ => None,
    }
}

/// [`route_body`] for a name declared in this file.
///
/// `seen` stops `const Page = Page` from recursing forever: a self-reference
/// resolves to no JSX, which is what it is.
fn body_of_name<'a>(
    program: &'a Program<'a>,
    name: &'a str,
    seen: &mut Vec<&'a str>,
) -> Option<ResolvedBody<'a>> {
    if seen.contains(&name) {
        return None;
    }
    seen.push(name);

    program.body.iter().find_map(|statement| match statement {
        Statement::FunctionDeclaration(function) => function
            .id
            .as_ref()
            .filter(|identifier| identifier.name == name)
            .and_then(|_| body_of_function(function)),
        Statement::VariableDeclaration(declaration) => {
            declaration.declarations.iter().find_map(|declarator| {
                if !declarator
                    .id
                    .get_identifier_name()
                    .is_some_and(|identifier| identifier.as_str() == name)
                {
                    return None;
                }
                body_of_expression(program, declarator.init.as_ref()?, seen)
            })
        }
        _ => None,
    })
}

/// [`route_body`] for a function declaration or expression.
fn body_of_function<'a>(function: &'a Function<'a>) -> Option<ResolvedBody<'a>> {
    function.body.as_deref().map(ResolvedBody::Block)
}

/// [`route_body`] for an arrow function, whose body may be a block or the JSX.
fn body_of_arrow<'a>(
    arrow: &'a oxc_ast::ast::ArrowFunctionExpression<'a>,
) -> Option<ResolvedBody<'a>> {
    match &arrow.body {
        ArrowFunctionBody::FunctionBody(body) => Some(ResolvedBody::Block(body)),
        concise => concise.as_expression().map(ResolvedBody::Concise),
    }
}

/// The first of these statements that is a `return` of JSX directly.
///
/// Only the top level is scanned, matching `ComponentInfo::returns_jsx`: a
/// `return` inside an `if` is not followed, and the `RA011` diagnostic says so.
fn jsx_return<'a>(
    mut statements: impl Iterator<Item = &'a Statement<'a>>,
) -> Option<&'a Expression<'a>> {
    statements.find_map(|statement| match statement {
        Statement::ReturnStatement(returned) => returned.argument.as_ref().and_then(jsx_expression),
        _ => None,
    })
}

/// `expression` as JSX, seeing through the parentheses a multi-line return is
/// usually formatted with.
fn jsx_expression<'a>(expression: &'a Expression<'a>) -> Option<&'a Expression<'a>> {
    match expression {
        Expression::JSXElement(_) | Expression::JSXFragment(_) => Some(expression),
        Expression::ParenthesizedExpression(parenthesized) => {
            jsx_expression(&parenthesized.expression)
        }
        _ => None,
    }
}

/// The tree walk. Holds the source `program` was parsed from, because every text
/// and expression node is a slice of it.
struct Lowerer<'s> {
    source: &'s str,
}

impl<'s> Lowerer<'s> {
    /// The source text a span names.
    ///
    /// The same rule as [`crate::source::raw_text`] — a span from this source's
    /// own AST is always in bounds, and sharing the helper keeps the two from
    /// drifting into different failure modes. Slicing rather than reading a
    /// decoded `value` is what keeps entities and formatting verbatim.
    fn slice(&self, span: Span) -> &'s str {
        raw_text(self.source, span).unwrap_or("")
    }

    /// The route's root node.
    ///
    /// A fragment is rejected here rather than spliced: it has no parent to be
    /// spliced into, and the IR's root is one node because both emitters write
    /// one root. As a *child*, a fragment is transparent — see [`Self::children`].
    fn root(&self, root: &'s Expression<'s>) -> Result<IrNode, Vec<Diagnostic>> {
        match root {
            Expression::JSXElement(element) => self.element(element),
            Expression::JSXFragment(fragment) => Err(vec![unsupported(
                fragment.span,
                "a fragment as the route's returned JSX",
                "Return exactly one element instead, wrapping the fragment's children in \
                 `<main>…</main>` or whichever parent suits the page.",
            )]),
            // `route_jsx` only returns JSX, so this is unreachable.
            other => Err(vec![unsupported(
                other.span(),
                "a non-JSX root",
                "Return JSX from the route component, e.g. `return <main>...</main>`.",
            )]),
        }
    }

    /// One JSX element, as an element or as a component.
    fn element(&self, element: &'s JSXElement<'s>) -> Result<IrNode, Vec<Diagnostic>> {
        let name = element.opening_element.name.to_string();
        // `<div>` is a DOM element; `<Div>` is another component. One rule, and
        // it is the one JSX itself uses.
        let is_component = name.chars().next().is_some_and(char::is_uppercase);

        let mut static_attrs: Vec<(String, String)> = Vec::new();
        let mut bindings: Vec<Binding> = Vec::new();
        let mut props: Vec<(String, IrNode)> = Vec::new();

        for attribute in &element.opening_element.attributes {
            let JSXAttributeItem::Attribute(attribute) = attribute else {
                return Err(vec![unsupported(
                    attribute.span(),
                    "a spread attribute (`{...expr}`)",
                    "Write out the attributes it carries, e.g. `<div id={rest.id}>`, or remove \
                     the spread.",
                )]);
            };

            let name = attribute_name(&attribute.name);
            let namespaced = matches!(attribute.name, JSXAttributeName::NamespacedName(_));

            if is_component {
                // A component's `onClick` is a prop, not a DOM listener: there is
                // no element to attach a listener to, so every attribute is a prop
                // exactly as written.
                let value = self.attribute_value(attribute.value.as_ref())?;
                props.push((name, value));
                continue;
            }

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
                            vec![empty_expression(
                                container.span,
                                "attribute",
                                "`name={value}`",
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
                        "Move the element into the children instead, e.g. \
                         `<Parent><Bar /></Parent>`.",
                    )]);
                }
            }
        }

        let children = self.children(&element.children, &mut bindings)?;

        if is_component {
            Ok(IrNode::Component {
                name,
                props,
                children,
            })
        } else {
            Ok(IrNode::Element {
                tag: name,
                static_attrs,
                bindings,
                children,
            })
        }
    }

    /// A component prop's value, as written.
    fn attribute_value(
        &self,
        value: Option<&JSXAttributeValue>,
    ) -> Result<IrNode, Vec<Diagnostic>> {
        match value {
            // `<Foo flag />` means `flag={true}` in JSX. Spelling it as an
            // expression keeps one representation of "dynamic-ish" on a component,
            // where there is no static-attribute list to hold a bare name.
            None => Ok(IrNode::Expr("true".to_string())),
            Some(JSXAttributeValue::StringLiteral(literal)) => {
                Ok(IrNode::Text(literal_value(self.source, literal)))
            }
            Some(JSXAttributeValue::ExpressionContainer(container)) => {
                let expr = self
                    .expression_source(&container.expression, "prop")?
                    .ok_or_else(|| {
                        vec![empty_expression(container.span, "prop", "`name={value}`")]
                    })?;
                Ok(IrNode::Expr(expr.to_string()))
            }
            Some(other) => Err(vec![unsupported(
                other.span(),
                "a prop whose value is itself JSX (`name=<Bar />`)",
                "Pass the element as a child instead, e.g. `<Parent><Bar /></Parent>`.",
            )]),
        }
    }

    /// Lower an element's children in document order.
    ///
    /// Text bindings are pushed onto `bindings` — the parent's list — while a
    /// nested element keeps its own. A fragment contributes no node of its own,
    /// so its children are spliced here with the same `bindings` list: that is
    /// what `<div><><b/></></div>` rendering exactly `<div><b/></div>` means.
    fn children(
        &self,
        children: &'s [JSXChild<'s>],
        bindings: &mut Vec<Binding>,
    ) -> Result<Vec<IrNode>, Vec<Diagnostic>> {
        let mut nodes = Vec::new();

        for child in children {
            match child {
                JSXChild::Text(text) => nodes.push(IrNode::Text(self.slice(text.span).to_string())),
                JSXChild::Element(element) => nodes.push(self.element(element)?),
                JSXChild::Fragment(fragment) => {
                    nodes.extend(self.children(&fragment.children, bindings)?);
                }
                JSXChild::ExpressionContainer(container) => {
                    // `{}` and `{/* a comment */}` both parse to an empty
                    // expression. There is nothing to render and no slot to
                    // address, so they contribute neither a node nor a binding —
                    // and the comment's text must not become an expression.
                    let Some(expr) = self.expression_source(&container.expression, "child slot")?
                    else {
                        continue;
                    };
                    nodes.push(IrNode::Expr(expr.to_string()));
                    bindings.push(Binding::Text(expr.to_string()));
                }
                JSXChild::Spread(spread) => {
                    return Err(vec![unsupported(
                        spread.span,
                        "a spread child (`{...expr}`)",
                        "Render the value explicitly, e.g. `<For each={items} key={by}>…</For>`, \
                         or remove the spread.",
                    )]);
                }
            }
        }

        Ok(nodes)
    }

    /// The source text of an expression inside `{…}`, or `None` when the braces
    /// are empty or hold only a comment.
    ///
    /// The *inner* expression span is used, not the container's, so the braces
    /// are not part of the text codegen receives.
    ///
    /// This is also where C1 is refused: the text becomes JavaScript in the
    /// emitted module, and JSX inside it (`{ready ? <b/> : <i/>}`,
    /// `{items.map(i => <li/>)}`) is a syntax error there. A string heuristic
    /// such as `contains("</")` would misfire on `a < b`, so the check walks the
    /// expression's AST. `position` names the slot for the message.
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
                        "Move the JSX out of `{…}`: it belongs in the children, e.g. \
                         `<Parent><b/></Parent>`, or in its own `return`. Expressions become \
                         JavaScript in the emitted module, where a JSX tag is a syntax error.",
                    )]);
                }
                Ok(Some(self.slice(other.span())))
            }
        }
    }
}

/// The span of the first `JSXElement`/`JSXFragment` anywhere inside `expression`,
/// or `None`.
///
/// A walk, not a substring search: `a < b` and `x => y < z` are ordinary
/// expressions whose text happens to contain `<`, and refusing them would reject
/// valid routes. Nested positions count, including inside a call, a conditional,
/// or a parenthesized group.
fn contains_jsx(expression: &Expression) -> Option<Span> {
    let mut finder = JsxFinder::default();
    finder.visit_expression(expression);
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

/// A JSX string literal's value, exactly as written between the quotes.
///
/// JSX attribute strings have no escape sequences, and the parser deliberately
/// leaves character references (`&amp;`) undecoded, so the parsed value *is* the
/// source text. The `None` case is a lone surrogate, which only a constructed
/// (not parsed) AST can hold; there the raw slice is the fallback.
fn literal_value(source: &str, literal: &oxc_ast::ast::StringLiteral) -> String {
    literal.value.as_str().map_or_else(
        || {
            let raw = &source[literal.span.start as usize..literal.span.end as usize];
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
///
/// The repair is per construct, so it goes in the message: `RA014`'s canonical
/// `fix_template` tells the agent to do what the message names.
fn unsupported(span: Span, construct: &str, repair: &str) -> Diagnostic {
    Diagnostic::new(
        Code::RA014,
        (span.start, span.end),
        format!("cannot lower {construct} yet: {repair}"),
    )
}

/// `{…}` with nothing in it, where a value is required.
fn empty_expression(span: Span, kind: &str, fix_example: &str) -> Diagnostic {
    Diagnostic::new(
        Code::RA014,
        (span.start, span.end),
        format!(
            "cannot lower an {kind} with an empty expression: give the {kind} a value, e.g. {fix_example}"
        ),
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
            } => {
                assert_eq!(tag, "div");
                // The static half is what the template carries: a lowering that
                // dropped it would still pass a bindings-only assertion.
                assert_eq!(static_attrs, vec![("class".to_string(), "a".to_string())]);
                assert!(bindings.is_empty());
                assert!(matches!(children.as_slice(), [IrNode::Text(t)] if t == "hi"));
            }
            other => panic!("expected element, got {other:?}"),
        }
    }

    #[test]
    fn expression_slot_becomes_a_text_binding() {
        let src = "\"use island\"\nexport default function P() { const n = signal(0); return <p>{n()}</p> }\n";
        let r = crate::ir::lower_route(src).unwrap();
        match r.tree {
            IrNode::Element {
                bindings, children, ..
            } => {
                // Both halves of the slot are the contract: the binding Task 11
                // wires, and the placeholder that keeps document order.
                assert_eq!(bindings, vec![Binding::Text("n()".to_string())]);
                assert!(
                    matches!(children.as_slice(), [IrNode::Expr(e)] if e == "n()"),
                    "got {children:?}"
                );
            }
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

    /// An attribute value is source text too. `&#39;` decodes to `'`, and a
    /// template that re-encodes it writes `&#39;amp;` — so the value must be the
    /// slice, not a decode-and-encode round trip.
    #[test]
    fn static_attribute_values_are_verbatim() {
        let src =
            "\"use static\"\nexport default function P() { return <div title=\"a &amp; b\" /> }\n";
        let r = crate::ir::lower_route(src).unwrap();
        let IrNode::Element { static_attrs, .. } = r.tree else {
            panic!("expected element")
        };
        assert_eq!(
            static_attrs,
            vec![("title".to_string(), "a &amp; b".to_string())]
        );
    }

    /// An expression attribute is a binding, not static markup — the distinction
    /// the whole IR exists for. `class={tone}` must not reach `static_attrs`.
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

    /// A bare attribute has no value and is not a binding. `<input disabled />`
    /// is static the way `disabled=""` is.
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
    /// lowercased: `onKeyDown` listens for `"keydown"`. A lowercased `onclick`
    /// survives as an attribute name because JSX says so.
    #[test]
    fn event_names_are_dom_event_names() {
        for (attr, expected) in [
            ("onClick", Some("click")),
            ("onKeyDown", Some("keydown")),
            ("onPointerEnter", Some("pointerenter")),
            // Not `on` + capital, so not an event. `once` would otherwise become a
            // listener for a `"ce"` event that does not exist.
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

    /// A child slot must keep its place among its siblings: Task 11 addresses the
    /// *k*-th placeholder, so a dropped or reordered placeholder wires the wrong
    /// node.
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
        assert_eq!(
            bindings,
            vec![
                Binding::Text("b".to_string()),
                Binding::Text("d".to_string())
            ]
        );
    }

    /// Expression text is sliced from the source, so comments and spacing inside
    /// it survive for codegen. Re-printing from the AST would drop the comment
    /// and collapse the spacing.
    #[test]
    fn expression_text_is_the_source_slice() {
        let src = "\"use island\"\nexport default function P() { return <p>{ f(a, /* keep */ 1) }</p> }\n";
        let r = crate::ir::lower_route(src).unwrap();
        let IrNode::Element { bindings, .. } = r.tree else {
            panic!("expected element")
        };
        let [Binding::Text(expr)] = bindings.as_slice() else {
            panic!("got {bindings:?}")
        };
        assert_eq!(expr, "f(a, /* keep */ 1)", "braces or formatting lost");
    }

    /// `{}` and `{/* a comment */}` parse to an empty expression. They must
    /// contribute neither a placeholder nor a binding — a comment turned into an
    /// expression becomes emitted JavaScript that references nothing.
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

    /// A nested element is a child node, not a flattened one. Flattening would
    /// put its text binding on the parent, and Task 11 would set the parent's
    /// content from the child's expression.
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
                bindings: child_bindings,
                ..
            },
        ] = children.as_slice()
        else {
            panic!("got {children:?}")
        };
        assert_eq!(tag, "p");
        assert!(matches!(child_bindings.as_slice(), [Binding::Text(t)] if t == "n()"));
    }

    /// A fragment is not an element: it contributes no tag and no nodes of its
    /// own, so its children land directly in the parent. Emitting a `<></>` would
    /// put a tag in the template that the DOM cannot parse.
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
        // The fragment's text slot is the parent's slot: the fragment has no
        // element to address, and dropping it would emit an empty `<b>`.
        assert!(
            bindings.is_empty(),
            "the fragment's text slot belongs to its own child, not the parent: {bindings:?}"
        );
    }

    /// A fragment as the *returned* root has no parent to be spliced into, and
    /// the IR's root is one node. Refusing is the only honest answer: emitting
    /// the fragment's children bare produces sibling roots nothing can mount.
    #[test]
    fn a_root_fragment_is_refused() {
        let src = "\"use island\"\nexport default function P() { return <><b/><i/></> }\n";
        let errs = crate::ir::lower_route(src).unwrap_err();
        assert_eq!(errs.len(), 1);
        assert_eq!(errs[0].code, Code::RA014);
        assert!(
            errs[0].message.contains("fragment"),
            "the message must name the construct: {:?}",
            errs[0].message
        );
        // The identity must not blame the syntax. `RA007` means "the parser could
        // not read this" and would send an agent hunting for an unclosed tag in a
        // file that parses; `RA014` names what this is.
        assert_eq!(
            errs[0].doc,
            "node_modules/runai/skills/RA014_NOT_SUPPORTED_YET.md"
        );
        assert!(
            errs[0].explain.contains("parser accepted"),
            "the explain must not blame the syntax: {:?}",
            errs[0].explain
        );
    }

    /// C1: JSX inside a `{…}` expression is copied into the emitted module as
    /// JavaScript, where a tag is a syntax error — the island would fail to
    /// parse at build time. Every nesting the AST can hold is refused: a
    /// conditional, a call argument, an array item, a nested parenthesized
    /// group, and a JSX-valued attribute.
    #[test]
    fn jsx_inside_an_expression_is_refused() {
        for (src, expected) in [
            (
                "\"use island\"\nexport default function P() { return <div>{ready ? <b/> : <i/>}</div> }\n",
                "child slot",
            ),
            (
                "\"use island\"\nexport default function P() { const items = []; return <ul>{items.map(i => <li>{i}</li>)}</ul> }\n",
                "child slot",
            ),
            (
                "\"use island\"\nexport default function P() { return <div>{f(<b/>)}</div> }\n",
                "child slot",
            ),
            (
                "\"use island\"\nexport default function P() { return <div>{[<b/>]}</div> }\n",
                "child slot",
            ),
            (
                "\"use island\"\nexport default function P() { return <div>{(<b/>)}</div> }\n",
                "child slot",
            ),
            (
                "\"use island\"\nexport default function P() { return <p class={x ? <b/> : <i/>} /> }\n",
                "attribute",
            ),
            (
                "\"use island\"\nexport default function P() { return <Child slot={<p/>} /> }\n",
                "prop",
            ),
        ] {
            let errs = crate::ir::lower_route(src).unwrap_err();
            assert_eq!(errs[0].code, Code::RA014, "{src}");
            assert!(
                errs[0].message.contains("JSX inside a `{…}` expression"),
                "{src}: {:?}",
                errs[0].message
            );
            assert!(
                errs[0].message.contains(expected),
                "{src}: the message must name the position, got {:?}",
                errs[0].message
            );
            assert!(
                errs[0].fix.contains("children"),
                "{src}: the fix must say where the JSX belongs: {:?}",
                errs[0].fix
            );
        }
    }

    /// The refusal is by AST walk, so a plain comparison is *not* JSX. A string
    /// heuristic (`contains("</")`, `contains('<')`) would refuse every one of
    /// these, all of which emit valid JavaScript.
    #[test]
    fn a_comparison_in_an_expression_is_not_jsx() {
        for src in [
            "\"use island\"\nexport default function P() { return <div>{a < b}</div> }\n",
            "\"use island\"\nexport default function P() { const f = (x) => x < 1; return <div>{f(0)}</div> }\n",
            "\"use island\"\nexport default function P() { return <p class={a < b}>{a < b}</p> }\n",
            "\"use island\"\nexport default function P() { return <div>{`a</b`}</div> }\n",
        ] {
            assert!(
                crate::ir::lower_route(src).is_ok(),
                "{src}: a comparison must not be mistaken for JSX"
            );
        }
    }

    /// I1: the analyzer accepts any body with *some* JSX return, so a second one
    /// is legal TSX but lowering would emit only the first and drop the other
    /// with no diagnostic. The nested `return` is the one the top-level scan does
    /// not reach, which is exactly the case that used to vanish.
    #[test]
    fn a_second_jsx_return_is_refused() {
        for src in [
            // The second return is nested where the top-level scan cannot see it.
            "\"use island\"\nexport default function P() { if (a) return <b/>; return <i/> }\n",
            // A second top-level return is unreachable code, but it is still a
            // return lowering did not emit.
            "\"use island\"\nexport default function P() { return <b/>; return <i/> }\n",
            // A conditional's nested return.
            "\"use island\"\nexport default function P() { if (a) { return <b/> } return <i/> }\n",
        ] {
            let errs = crate::ir::lower_route(src).unwrap_err();
            assert_eq!(errs[0].code, Code::RA014, "{src}");
            assert!(
                errs[0].message.contains("second JSX return"),
                "{src}: {:?}",
                errs[0].message
            );
            assert!(errs[0].span.0 > 0, "{src}: the diagnostic needs a location");
        }
    }

    /// The dropped-return check must not fire for the shapes it is not about: a
    /// nested *function* returns its own JSX from its own render, and a concise
    /// arrow body is the JSX.
    #[test]
    fn the_dropped_return_check_ignores_nested_renderers() {
        for src in [
            // A component declared inside the route *returns* JSX, but that
            // return belongs to `Item`'s render, not the route's. Counting it
            // would refuse a perfectly ordinary nested component.
            "\"use island\"\nexport default function P() { function Item() { return <li/> } return <ul><Item/></ul> }\n",
            "\"use island\"\nexport default function P() { const Item = () => { return <li/> }; return <ul><Item/></ul> }\n",
            // A concise arrow body is the JSX; there is no return statement.
            "\"use island\"\nexport default (props: {}) => <p/>\n",
            "\"use island\"\nexport default (props: {}) => { return <p/> }\n",
            // A plain handler is not a renderer at all, and must stay legal.
            "\"use island\"\nexport default function P() { return <button onClick={() => go()}>x</button> }\n",
        ] {
            assert!(
                crate::ir::lower_route(src).is_ok(),
                "{src}: a nested renderer is not a dropped return"
            );
        }
    }

    /// Task 11's positional contract, on the element it actually has to handle:
    /// one with *both* attribute bindings and text slots. The k-th
    /// [`Binding::Text`] must pair with the k-th [`IrNode::Expr`], whatever
    /// attribute bindings precede them — the *k* counts text slots only.
    #[test]
    fn the_kth_text_binding_pairs_with_the_kth_expr_placeholder() {
        let src = "\"use island\"\nexport default function P() { return <p class={tone} onClick={go}>{a}<b/>{c}</p> }\n";
        let r = crate::ir::lower_route(src).unwrap();
        let IrNode::Element {
            bindings, children, ..
        } = r.tree
        else {
            panic!("expected element")
        };

        let text_bindings: Vec<&str> = bindings
            .iter()
            .filter_map(|binding| match binding {
                Binding::Text(expr) => Some(expr.as_str()),
                _ => None,
            })
            .collect();
        let placeholders: Vec<&str> = children
            .iter()
            .filter_map(|child| match child {
                IrNode::Expr(expr) => Some(expr.as_str()),
                _ => None,
            })
            .collect();

        assert_eq!(
            text_bindings,
            vec!["a", "c"],
            "attribute bindings must not shift the text slots: {bindings:?}"
        );
        assert_eq!(
            placeholders, text_bindings,
            "the k-th text binding must pair with the k-th placeholder: {children:?}"
        );
        // And the attribute bindings are the leading two, in source order.
        assert!(
            matches!(
                bindings.as_slice(),
                [
                    Binding::Attr { name: class, .. },
                    Binding::Event { name: click, .. },
                    Binding::Text(_),
                    Binding::Text(_),
                ] if class == "class" && click == "click"
            ),
            "got {bindings:?}"
        );
    }

    /// C2: Task 11 emits the component body's statements above the template, so
    /// `RouteIr` must carry their range. It is the body, not the whole function:
    /// `function Page(props: …)` is not a statement.
    #[test]
    fn the_body_span_is_the_component_body() {
        let src = "\"use island\"\nexport default function Page(props: {}) { const n = signal(0); return <p>{n()}</p> }\n";
        let r = crate::ir::lower_route(src).unwrap();
        assert_eq!(
            &src[r.body_span.0 as usize..r.body_span.1 as usize],
            "{ const n = signal(0); return <p>{n()}</p> }"
        );

        // A concise arrow body has no statements, so the span is the expression.
        let concise = "\"use island\"\nexport default (props: {}) => <p/>\n";
        let r = crate::ir::lower_route(concise).unwrap();
        assert_eq!(
            &concise[r.body_span.0 as usize..r.body_span.1 as usize],
            "<p/>"
        );
    }

    /// A capitalized tag is another component, and its attributes are props: a
    /// static value is text, an expression is an expression, and a bare attribute
    /// means `true`. Reading `onClick` as an event here would name a DOM listener
    /// that no component has.
    #[test]
    fn a_capitalized_tag_is_a_component_with_props() {
        let src = "\"use island\"\nexport default function P() { return <Greeting name=\"Ada\" tone={t} flag onClick={go} /> }\n";
        let r = crate::ir::lower_route(src).unwrap();
        let IrNode::Component {
            name,
            props,
            children,
        } = r.tree
        else {
            panic!("expected component")
        };
        assert_eq!(name, "Greeting");
        assert!(children.is_empty());
        assert_eq!(
            props,
            vec![
                ("name".to_string(), IrNode::Text("Ada".to_string())),
                ("tone".to_string(), IrNode::Expr("t".to_string())),
                ("flag".to_string(), IrNode::Expr("true".to_string())),
                ("onClick".to_string(), IrNode::Expr("go".to_string())),
            ]
        );
    }

    /// A component's children are nodes; a lowercase tag's are not the same
    /// thing. This pins that `<Greeting>` is not silently lowered as a `<div>`.
    #[test]
    fn a_lowercase_tag_is_an_element_and_a_capitalized_one_is_not() {
        for (src, is_component) in [
            (
                "\"use static\"\nexport default function P() { return <div /> }\n",
                false,
            ),
            (
                "\"use static\"\nexport default function P() { return <Div /> }\n",
                true,
            ),
        ] {
            let r = crate::ir::lower_route(src).unwrap();
            assert_eq!(
                matches!(r.tree, IrNode::Component { .. }),
                is_component,
                "{src}"
            );
        }
    }

    /// `<Child><p/></Child>` is how a component receives markup: per design
    /// §7.2 it arrives at the callee as `props.children`. A lowering that put
    /// the nodes anywhere else would silently drop them when Task 11 inlines.
    #[test]
    fn a_component_child_is_its_children() {
        let src =
            "\"use static\"\nexport default function P() { return <Child><p>hi</p></Child> }\n";
        let r = crate::ir::lower_route(src).unwrap();
        let IrNode::Component {
            name,
            props,
            children,
        } = r.tree
        else {
            panic!("expected component")
        };
        assert_eq!(name, "Child");
        assert!(props.is_empty(), "got {props:?}");
        assert!(
            matches!(
                children.as_slice(),
                [IrNode::Element { tag, children, .. }]
                    if tag == "p" && matches!(children.as_slice(), [IrNode::Text(t)] if t == "hi")
            ),
            "got {children:?}"
        );
    }

    /// An expression child of a component is an `IrNode::Expr` placeholder, the
    /// same as on an element. There is no parent `bindings` list to pair it
    /// with: the pairing belongs to whichever component inlines `<Child>`.
    #[test]
    fn a_component_expression_child_is_an_expr_placeholder() {
        let src = "\"use island\"\nexport default function P() { return <Child>{x}</Child> }\n";
        let r = crate::ir::lower_route(src).unwrap();
        let IrNode::Component { children, .. } = r.tree else {
            panic!("expected component")
        };
        assert!(
            matches!(children.as_slice(), [IrNode::Expr(e)] if e == "x"),
            "got {children:?}"
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

    /// A route exported through a name is still a route: `export default Page`
    /// and `export { Page as default }` must lower the same tree as the inline
    /// form, and the name is the binding a reader can find.
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
    /// rather than a symbol: `export default function default(…)` is a parse
    /// error. The conventional `Page` is what a reader can act on.
    #[test]
    fn an_anonymous_component_is_named_page() {
        let src = "\"use static\"\nexport default function () { return <p/> }\n";
        assert_eq!(crate::ir::lower_route(src).unwrap().name, "Page");
    }

    /// `lower_jsx` must agree with `find_route_component` on every shape: it
    /// returns JSX exactly when that analyzer says the export returns JSX. A
    /// divergence is what the `unreachable` branch in `lower_jsx` guards, and a
    /// reachable one would report a compiler defect for a legal route.
    #[test]
    fn lowering_agrees_with_the_route_analyzer() {
        use crate::analyze::find_route_component;
        use crate::source::parse_tsx;

        for src in [
            "export default function Page() { return <p/> }\n",
            "export default (props: {}) => <p/>\n",
            "export default (props: {}) => { return <p/> }\n",
            "export default (function Page() { return <p/> })\n",
            "const Page = () => <p/>\nexport default Page\n",
            "function Page() { return <p/> }\nexport { Page as default }\n",
            // Not JSX: the analyzer reports RA011 and lowering must not succeed.
            "export default 42\n",
            "export default function Page() { return View }\n",
            "export default function Page() { if (x) return <p/> }\n",
            "const Page = Page\nexport default Page\n",
        ] {
            let alloc = oxc_allocator::Allocator::default();
            let program = parse_tsx(&alloc, src).unwrap();
            let analyzer_says_jsx = find_route_component(program).is_some_and(|info| {
                crate::analyze::validate_route_component(Some(&info)).is_empty()
            });
            let lowered = lower_jsx(program).is_ok();
            assert_eq!(analyzer_says_jsx, lowered, "{src}");
            if !analyzer_says_jsx {
                // The refusal must be the analyzer's own diagnostic, not a
                // compiler-defect one.
                let errs = lower_jsx(program).unwrap_err();
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

    /// A spread would have to be expanded by codegen, which cannot know what
    /// `rest` holds. Dropping it silently loses props with no report.
    #[test]
    fn a_spread_is_refused() {
        for (src, expected) in [
            (
                "\"use island\"\nexport default function P() { return <div {...rest} /> }\n",
                "spread attribute",
            ),
            (
                "\"use island\"\nexport default function P() { return <div>{...items}</div> }\n",
                "spread child",
            ),
            (
                "\"use island\"\nexport default function P() { return <Greeting {...rest} /> }\n",
                "spread attribute",
            ),
        ] {
            let errs = crate::ir::lower_route(src).unwrap_err();
            assert_eq!(errs[0].code, Code::RA014, "{src}");
            assert!(
                errs[0].message.contains(expected),
                "{src}: {:?}",
                errs[0].message
            );
        }
    }

    /// An attribute with no expression is rejected *by the parser*, before
    /// lowering runs: `title={}` is a syntax error, not a legal attribute with no
    /// value. `empty_expression`'s attribute branch therefore only exists for a
    /// constructed AST, and this test pins which side of the boundary the case
    /// falls on — if the parser ever starts accepting it, this fails and the
    /// branch becomes reachable.
    ///
    /// The message is Oxc's own English text, pinned to `oxc_parser = 0.159.0`
    /// (see `Cargo.toml`); a parser upgrade that rewords it fails here, which is
    /// the point — `RA007` is the code that carries the parser's message verbatim.
    #[test]
    fn an_empty_attribute_expression_is_a_parse_error() {
        let src = "\"use island\"\nexport default function P() { return <div title={} /> }\n";
        let errs = crate::ir::lower_route(src).unwrap_err();
        assert_eq!(errs[0].code, Code::RA007);
        assert!(
            errs[0]
                .message
                .contains("JSX attributes must only be assigned a non-empty"),
            "the parser's own message must reach the agent: {:?}",
            errs[0].message
        );
    }

    /// A named export that is not `default` must not stop the walk before the
    /// default export is reached. `export { helper }` above the route component is
    /// an ordinary file; with `?` in the specifier lookup it was reported as a
    /// compiler defect.
    #[test]
    fn a_non_default_named_export_does_not_hide_the_route() {
        let src = "\"use island\"\nconst helper = () => 1\nexport { helper }\nexport default function Page() { return <p>hi</p> }\n";
        let r = crate::ir::lower_route(src).unwrap();
        assert_eq!(r.name, "Page");
        assert!(matches!(r.tree, IrNode::Element { tag, .. } if tag == "p"));
    }

    /// The mode and the tree come from one parse: a route's `mode` must be the
    /// directive the file actually carries, not a default.
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

    /// A namespaced attribute is never an event listener, even when it carries an
    /// expression: `onClick:x` names a namespaced attribute, not a `"click:x"`
    /// event that no DOM knows. The guard is what keeps the two apart. A namespaced
    /// *static* value is a static attribute like any other.
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

        // A namespaced static value keeps its namespace in the name.
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

    /// Text is the source slice, so the raw slice deliberately includes
    /// whitespace that JSX itself would strip. Trimming it here would make the
    /// emit step's job ambiguous: `<b>a</b> <i>b</i>` and `<b>a</b><i>b</i>`
    /// would become indistinguishable, and a page could lose a visible space
    /// with nothing reporting it.
    #[test]
    fn text_whitespace_is_preserved() {
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

    /// The contract a multi-line element's indentation has to satisfy: every
    /// inter-element whitespace run is its own [`IrNode::Text`], raw and
    /// un-collapsed. React collapses whitespace-only lines; this IR records that
    /// it deliberately does **not**, so Task 10 writes exactly these nodes.
    #[test]
    fn indented_multiline_element_keeps_raw_whitespace() {
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
        assert!(
            matches!(
                children.as_slice(),
                [
                    IrNode::Text(lead),
                    IrNode::Element { tag: b, .. },
                    IrNode::Text(mid),
                    IrNode::Element { tag: i, .. },
                    IrNode::Text(tail),
                ] if lead == "\n      "
                    && b == "b"
                    && mid == "\n      "
                    && i == "i"
                    && tail == "\n    "
            ),
            "got {children:?}"
        );
    }

    /// A syntax error is `RA007` from `parse_tsx`, not an empty tree: lowering
    /// must not report success for a file it could not read.
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
}
