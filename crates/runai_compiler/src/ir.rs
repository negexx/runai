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
//! A spread (`{...rest}` as an attribute or a child), an attribute whose value
//! is itself JSX (`foo=<Bar />`), an attribute with an empty expression
//! (`foo={}`), and a fragment as the *returned* root all report `RA007`.
//! Dropping a prop or a node would emit markup that is wrong in a way nothing
//! reports, and an agent cannot repair output it never sees.

use oxc_allocator::Allocator;
use oxc_ast::ast::{
    ArrowFunctionBody, ExportDefaultDeclaration, ExportDefaultDeclarationKind, Expression,
    Function, JSXAttributeItem, JSXAttributeName, JSXAttributeValue, JSXChild, JSXElement,
    JSXExpression, Program, Statement,
};
use oxc_span::{GetSpan, Span};

use crate::analyze::{find_route_component, validate_route_component};
use crate::diagnostic::{Code, Diagnostic, Severity};
use crate::source::{Mode, mode_of, parse_tsx};

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
    Component {
        name: String,
        props: Vec<(String, IrNode)>,
        children: Vec<IrNode>,
    },
}

/// A lowered route file: its rendering mode, its component name, and its markup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteIr {
    pub mode: Mode,
    /// The route component's name, or `Page` when it is genuinely anonymous.
    /// `"default"` would name the export rather than a symbol in the file, and
    /// `export default function default(...)` is a parse error — so diagnostic
    /// text and codegen both need the readable name.
    pub name: String,
    pub tree: IrNode,
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
    let mode = mode_of(program).map_err(|diagnostic| vec![diagnostic])?;
    let tree = lower_jsx(program)?;

    // `lower_jsx` succeeds only for a default export it could locate, and both
    // walkers accept the same shapes, so the summary exists here.
    let name = find_route_component(program)
        .map(|info| info.display_name().to_string())
        .unwrap_or_else(|| "Page".to_string());

    Ok(RouteIr { mode, name, tree })
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
    let Some(root) = route_jsx(program) else {
        let info = find_route_component(program);
        let mut diagnostics = validate_route_component(info.as_ref());
        // Unreachable while `route_jsx` mirrors `find_route_component`. Kept as a
        // diagnostic rather than a panic or an empty success, so a future
        // divergence is reported instead of silently emitting nothing.
        if diagnostics.is_empty() {
            diagnostics.push(unlowerable_export());
        }
        return Err(diagnostics);
    };

    Lowerer {
        source: program.source_text,
    }
    .root(root)
}

/// The JSX node the default export returns, with parentheses removed.
///
/// This is a second walk of the same declarations because the summary in
/// `analyze::component` answers *whether* there is a component, not *which
/// node* it returns. Every accepted shape is mirrored exactly; when they agree,
/// the answer here exists.
fn route_jsx<'a>(program: &'a Program<'a>) -> Option<&'a Expression<'a>> {
    for statement in &program.body {
        match statement {
            Statement::ExportDefaultDeclaration(declaration) => {
                return jsx_of_default(program, declaration);
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
                return jsx_of_name(program, specifier.local.name().as_str(), &mut Vec::new());
            }
            _ => {}
        }
    }
    None
}

/// [`route_jsx`] for `export default …`.
fn jsx_of_default<'a>(
    program: &'a Program<'a>,
    declaration: &'a ExportDefaultDeclaration<'a>,
) -> Option<&'a Expression<'a>> {
    match &declaration.declaration {
        ExportDefaultDeclarationKind::FunctionDeclaration(function)
        | ExportDefaultDeclarationKind::FunctionExpression(function) => jsx_of_function(function),
        ExportDefaultDeclarationKind::ArrowFunctionExpression(arrow) => jsx_of_arrow(arrow),
        ExportDefaultDeclarationKind::ParenthesizedExpression(parenthesized) => {
            jsx_of_expression(program, &parenthesized.expression, &mut Vec::new())
        }
        ExportDefaultDeclarationKind::Identifier(identifier) => {
            jsx_of_name(program, identifier.name.as_str(), &mut Vec::new())
        }
        _ => None,
    }
}

/// [`route_jsx`] for an expression that may itself be a function or a name.
fn jsx_of_expression<'a>(
    program: &'a Program<'a>,
    expression: &'a Expression<'a>,
    seen: &mut Vec<&'a str>,
) -> Option<&'a Expression<'a>> {
    match expression {
        Expression::ArrowFunctionExpression(arrow) => jsx_of_arrow(arrow),
        Expression::FunctionExpression(function) => jsx_of_function(function),
        Expression::ParenthesizedExpression(parenthesized) => {
            jsx_of_expression(program, &parenthesized.expression, seen)
        }
        Expression::Identifier(identifier) => jsx_of_name(program, identifier.name.as_str(), seen),
        _ => None,
    }
}

/// [`route_jsx`] for a name declared in this file.
///
/// `seen` stops `const Page = Page` from recursing forever: a self-reference
/// resolves to no JSX, which is what it is.
fn jsx_of_name<'a>(
    program: &'a Program<'a>,
    name: &'a str,
    seen: &mut Vec<&'a str>,
) -> Option<&'a Expression<'a>> {
    if seen.contains(&name) {
        return None;
    }
    seen.push(name);

    program.body.iter().find_map(|statement| match statement {
        Statement::FunctionDeclaration(function) => function
            .id
            .as_ref()
            .filter(|identifier| identifier.name == name)
            .and_then(|_| jsx_of_function(function)),
        Statement::VariableDeclaration(declaration) => {
            declaration.declarations.iter().find_map(|declarator| {
                if !declarator
                    .id
                    .get_identifier_name()
                    .is_some_and(|identifier| identifier.as_str() == name)
                {
                    return None;
                }
                jsx_of_expression(program, declarator.init.as_ref()?, seen)
            })
        }
        _ => None,
    })
}

/// The first top-level `return` in `function` whose argument is JSX.
fn jsx_of_function<'a>(function: &'a Function<'a>) -> Option<&'a Expression<'a>> {
    let body = function.body.as_deref()?;
    jsx_return(body.statements.iter())
}

/// The body of `arrow` if it is JSX itself, or its first `return` if that is.
fn jsx_of_arrow<'a>(
    arrow: &'a oxc_ast::ast::ArrowFunctionExpression<'a>,
) -> Option<&'a Expression<'a>> {
    match &arrow.body {
        ArrowFunctionBody::FunctionBody(body) => jsx_return(body.statements.iter()),
        concise => concise.as_expression().and_then(jsx_expression),
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
    /// The span comes from the AST of this same source, so the range is valid by
    /// construction. Slicing rather than reading a decoded `value` is what keeps
    /// entities and formatting verbatim.
    fn slice(&self, span: Span) -> &'s str {
        &self.source[span.start as usize..span.end as usize]
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
                        .expression_source(&container.expression)
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
                    .expression_source(&container.expression)
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
                    let Some(expr) = self.expression_source(&container.expression) else {
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
    fn expression_source(&self, expression: &'s JSXExpression<'s>) -> Option<&'s str> {
        match expression {
            JSXExpression::EmptyExpression(_) => None,
            other => Some(self.slice(other.span())),
        }
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
fn event_name(name: &str) -> Option<String> {
    let rest = name.strip_prefix("on")?;
    if !rest.starts_with(|c: char| c.is_ascii_uppercase()) {
        return None;
    }
    Some(rest.to_ascii_lowercase())
}

/// A construct this milestone cannot lower.
///
/// `RA007` is the code for JSX the compiler cannot use. The text says plainly
/// that the file is *syntactically* fine, because an agent that reads "ambiguous
/// JSX" and hunts for an unclosed tag would repair nothing.
fn unsupported(span: Span, construct: &str, fix: &str) -> Diagnostic {
    Diagnostic {
        code: Code::RA007,
        severity: Severity::Error,
        span: (span.start, span.end),
        message: format!("cannot lower {construct} yet"),
        explain: "The parser accepted this file: this is a limitation of the compiler, not a \
                  syntax error. Lowering reports the construct instead of dropping it, because \
                  markup emitted without part of a node is wrong in a way nothing reports."
            .into(),
        fix: fix.into(),
        doc: Code::RA007.skill_file(),
        patch: None,
    }
}

/// `{…}` with nothing in it, where a value is required.
fn empty_expression(span: Span, kind: &str, fix_example: &str) -> Diagnostic {
    Diagnostic {
        code: Code::RA007,
        severity: Severity::Error,
        span: (span.start, span.end),
        message: format!("cannot lower an {kind} with an empty expression"),
        explain: "`{}` carries no value, so there is nothing for codegen to emit. The parser \
                  accepts it in a child position (where it simply renders nothing) but an \
                  attribute or prop needs something to set."
            .into(),
        fix: format!("Give the {kind} a value, e.g. {fix_example}."),
        doc: Code::RA007.skill_file(),
        patch: None,
    }
}

/// The route analyzer found a component, but this module could not locate the JSX
/// it returns.
///
/// Unreachable while the two walkers agree; it exists so a divergence is a
/// diagnostic an agent can read instead of a panic or empty output.
fn unlowerable_export() -> Diagnostic {
    Diagnostic {
        code: Code::RA011,
        severity: Severity::Error,
        span: (0, 0),
        message: "route component's returned JSX could not be located".into(),
        explain: "The route analyzer accepted the default export, but lowering could not find the \
                  JSX it returns. That is a compiler defect, not a problem with the route."
            .into(),
        fix: "Return JSX directly from the default export, e.g. `return <main>...</main>`, and \
              report the route if this diagnostic persists."
            .into(),
        doc: Code::RA011.skill_file(),
        patch: None,
    }
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
                bindings,
                children,
                ..
            } => {
                assert_eq!(tag, "div");
                assert!(bindings.is_empty());
                assert!(matches!(children[0], IrNode::Text(ref t) if t == "hi"));
            }
            other => panic!("expected element, got {other:?}"),
        }
    }

    #[test]
    fn expression_slot_becomes_a_text_binding() {
        let src = "\"use island\"\nexport default function P() { const n = signal(0); return <p>{n()}</p> }\n";
        let r = crate::ir::lower_route(src).unwrap();
        match r.tree {
            IrNode::Element { bindings, .. } => {
                assert_eq!(bindings.len(), 1);
                assert!(matches!(bindings[0], Binding::Text(_)));
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
        assert_eq!(errs[0].code, Code::RA007);
        assert!(
            errs[0].message.contains("fragment"),
            "the message must name the construct: {:?}",
            errs[0].message
        );
        // `RA007` normally means "the parser could not read this". Saying so here
        // would send an agent hunting for an unclosed tag in a file that parses.
        assert!(
            errs[0].explain.contains("parser accepted"),
            "the explain must not blame the syntax: {:?}",
            errs[0].explain
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
            assert_eq!(errs[0].code, Code::RA007, "{src}");
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

    /// Text is the source slice, so the whitespace JSX preserves is preserved
    /// here. Trimming it would make `<b>a</b> <i>b</i>` render `a`-`b` with no
    /// space — a difference the author can see and the compiler silently erased.
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
