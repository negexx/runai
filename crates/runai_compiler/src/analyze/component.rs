//! What the route's component is, and whether its export actually is one.
//!
//! A page file's **default export** is its route component. Nothing else is: a
//! named export is an ordinary module export, and importing another file's
//! component does not make it *this* route's component. So this module looks
//! only for `export default …`, and a file without one reports
//! `RA011_BAD_ROUTE_EXPORT` rather than picking a plausible-looking named
//! function and validating the wrong thing.
//!
//! The route component must be a component — a function that returns JSX — and
//! it must take at most one parameter, because that single parameter is the
//! route's props object. The router supplies nothing, so a second parameter can
//! never be filled.

use oxc_ast::ast::{
    ArrowFunctionBody, ExportDefaultDeclaration, ExportDefaultDeclarationKind, Expression,
    Function, Program, Statement,
};
use oxc_span::GetSpan;

use crate::diagnostic::{Code, Diagnostic, Severity};

/// Whether the default export is a function this milestone can analyze.
///
/// This distinction is load-bearing for `RA011`: "there is no default export" and
/// "the default export is not a component" need different messages and different
/// repairs. Telling an agent to add a default export when one already exists
/// sends it into a duplicate-export parse error, which is the repair loop this
/// diagnostic exists to prevent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComponentKind {
    /// A function declaration, function expression, or arrow function (possibly
    /// parenthesized), or an identifier/`export { X as default }` that resolves
    /// to one of those in the same file.
    Function,
    /// `export default` exists, but is not a function: a class, a literal, a
    /// call, or an identifier that cannot be resolved in this file (an import or
    /// a re-export from another module).
    NotAFunction,
}

/// The shape of the route's default-exported component.
///
/// This is a summary, not the AST node itself: it records only the answers the
/// analyses in this milestone ask, so later tasks do not have to re-walk the
/// program to answer them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComponentInfo<'a> {
    /// Whether the default export is a function at all. When it is
    /// [`ComponentKind::NotAFunction`], every other field is meaningless: there
    /// is no parameter list and no body to report, and the caller's only job is
    /// to emit the "existing export is not a component" repair.
    pub kind: ComponentKind,
    /// The name a reader of the file can find: the function's own name, the
    /// binding that holds it (`const AboutPage = (props) => …`), or the exported
    /// identifier when [`ComponentKind::NotAFunction`] and one could be named.
    ///
    /// `"default"` means genuinely anonymous — `export default (props) => <p/>`
    /// declares no name at all — and only then does [`display_name`](Self::display_name)
    /// substitute the conventional `Page`.
    pub name: &'a str,
    /// How many declared parameters it takes, including a rest parameter.
    ///
    /// `function Page(props)` is 1, `function Page()` is 0, and
    /// `function Page(props, ...rest)` is 2 — the rest parameter is a parameter
    /// too, and a route that collects one is not propless.
    pub params: usize,
    /// Whether the single parameter carries a type annotation.
    ///
    /// `true` means `params == 1` *and* that parameter is annotated. A
    /// `type Props` alias existing elsewhere in the file does not count: the
    /// annotation has to be on the parameter, which is the thing a reader sees
    /// when they ask "where does this value come from".
    pub has_props_type: bool,
    /// The function body's span, for analyses that are scoped to a component
    /// body. A concise arrow body (`() => <p/>`) has no `FunctionBody` node, so
    /// this is the expression's span instead.
    pub body_span: (u32, u32),
    /// Whether the default export *is* the function literal, rather than a name
    /// exported as default.
    ///
    /// `export default function Page(props) {…}` and `export default (props) =>
    /// …` are inline: the fix text may say `export default function Page(…)`
    /// because there is nothing to add — the existing declaration is the one to
    /// edit. `export default Page` and `export { Page as default }` are not:
    /// there the function is declared elsewhere and already exported, so a fix
    /// that says `export default …` would tell an agent to write a **second**
    /// default export, which is a parse error. Only meaningful when
    /// [`kind`](Self::kind) is [`ComponentKind::Function`].
    pub inline_default: bool,
    /// Whether some `return` statement in the body returns JSX directly.
    ///
    /// Deliberately syntactic, not dataflow: a body that returns an identifier
    /// (`return View`), a conditional (`return ready ? <p/> : <i/>`), or a call
    /// (`return render()`) is **not** counted as returning JSX in this
    /// milestone. Such a component reports `RA011`, and the fix says to return
    /// JSX from the component itself. The alternative — guessing at what a value
    /// resolves to — is how a compiler ends up asserting something it cannot
    /// know, and this milestone has no type information to resolve it with.
    ///
    /// Concise arrow bodies (`() => <p/>`) *are* counted: there the JSX is
    /// literally the body, no return statement is required, and reporting that
    /// as a missing component would be false.
    ///
    /// Only the top level of the body is scanned (`body.statements`), not nested
    /// blocks: `if (x) return <p/>`, a `switch` arm, or a `try` block is not
    /// detected.
    pub returns_jsx: bool,
}

impl<'a> ComponentInfo<'a> {
    /// Whether this component may render as a route: at most one parameter and
    /// it returns JSX.
    fn is_route_component(&self) -> bool {
        self.params <= 1 && self.returns_jsx
    }

    /// A legal identifier for this component in diagnostic text.
    ///
    /// An anonymous component's [`name`](Self::name) is `"default"`, which names
    /// the export, not a binding: `function default(props) { ... }` is a parse
    /// error because `default` is a reserved word. Emitting that verbatim sends
    /// an agent following the fix into a syntax error it cannot interpret, so the
    /// conventional route component name stands in.
    pub fn display_name(&self) -> &'a str {
        if self.name == "default" {
            "Page"
        } else {
            self.name
        }
    }

    /// The same summary, marked as reached through a name rather than an inline
    /// `export default function …`.
    fn not_inline(mut self) -> Self {
        self.inline_default = false;
        self
    }

    /// The same summary, renamed to the binding that holds it.
    ///
    /// A function's own name is only one of the ways a reader can refer to it:
    /// `const AboutPage = (props) => <p/>` is anonymous, yet the file names it
    /// `AboutPage`, and `export default (props) => <p/>` genuinely has no name.
    /// Resolving a binding therefore renames the summary, so diagnostic text
    /// points at the declaration the agent must edit instead of inventing a
    /// `Page` that is not in the file. Not renaming here would make a fix for
    /// `export default AboutPage` say `function Page(props: Props)` and leave
    /// `AboutPage` unannotated — the next run would report the same code again.
    fn with_name(mut self, name: &'a str) -> Self {
        self.name = name;
        self
    }
}

/// Find the route component in `program`: its default export.
///
/// Returns `None` only when the file has no default export at all. When one
/// exists but is not a function ([`ComponentKind::NotAFunction`]), a summary is
/// still returned: presence and absence are different mistakes with different
/// repairs, and telling an agent to add an export that already exists produces a
/// duplicate-default-export parse error.
pub fn find_route_component<'a>(program: &'a Program<'a>) -> Option<ComponentInfo<'a>> {
    for statement in &program.body {
        match statement {
            Statement::ExportDefaultDeclaration(declaration) => {
                return Some(component_from_default(program, declaration));
            }
            // `export { Page as default }`
            Statement::ExportNamedDeclaration(declaration) => {
                if let Some(info) = component_from_specifiers(program, &declaration.specifiers) {
                    return Some(info);
                }
            }
            // `export { Page as default } from "./page"`: a default export that
            // lives in another module. The export exists, but its shape is not in
            // this file, so it cannot be analyzed — and it must not be resolved
            // against a *local* `Page`, which is a different binding.
            Statement::ExportFromDeclaration(declaration) => {
                if let Some(specifier) = default_specifier(&declaration.specifiers) {
                    let local = specifier.local.name().as_str();
                    return Some(not_a_component(local, specifier.span));
                }
            }
            _ => {}
        }
    }
    None
}

/// Summarize one `export default …` declaration.
///
/// Every accepted shape produces a summary: a function literal one describing the
/// function, anything else [`ComponentKind::NotAFunction`]. `None` is not an
/// option here because the export exists — only its shape varies.
fn component_from_default<'a>(
    program: &'a Program<'a>,
    declaration: &'a ExportDefaultDeclaration<'a>,
) -> ComponentInfo<'a> {
    match &declaration.declaration {
        // `export default function Page(props) {…}`, the anonymous
        // `export default function (props) {…}` (the same node with no `id`),
        // and a function *expression*. Oxc has separate variants for the
        // declaration and expression forms, but they summarize identically.
        ExportDefaultDeclarationKind::FunctionDeclaration(function)
        | ExportDefaultDeclarationKind::FunctionExpression(function) => {
            component_from_function(function, true)
        }
        // `export default (props) => <p/>`
        ExportDefaultDeclarationKind::ArrowFunctionExpression(arrow) => {
            component_from_arrow(arrow, true)
        }
        // `export default (function Page() {…})`, `export default ((props) => …)`
        // and `export default (Page)`: parentheses are not part of the function,
        // but they are part of the AST shape the parser produces (verified on
        // 0.159.0), so unwrap them rather than calling it "not a component".
        ExportDefaultDeclarationKind::ParenthesizedExpression(parenthesized) => {
            component_from_expression(program, &parenthesized.expression, true, &mut Vec::new())
        }
        // `export default Page`: a valid route when `Page` is a component
        // declared in this file. Resolving it is a body scan, not dataflow — if
        // it is declared here we can summarize it exactly, and if it is imported
        // we must not guess what it is.
        //
        // Resolved or not, the export is a *name*, not an inline declaration:
        // the fix may not tell the agent to write `export default …` again.
        ExportDefaultDeclarationKind::Identifier(identifier) => {
            resolve_local(program, identifier.name.as_str())
                .map(|info| info.not_inline())
                .unwrap_or_else(|| not_a_component(identifier.name.as_str(), identifier.span))
        }
        // `export default 42`, `export default class X {}`, `export default
        // pageFactory()`: an export, but nothing that renders.
        other => not_a_component("default", other.span()),
    }
}

/// Summarize a parenthesized default-export expression by what is inside it.
///
/// `inline_default` says whether the expression *is* the default export (it is,
/// when this is reached from an `export default (…)`) or merely the initializer
/// of a name that some export refers to.
fn component_from_expression<'a>(
    program: &'a Program<'a>,
    expression: &'a Expression<'a>,
    inline_default: bool,
    seen: &mut Vec<&'a str>,
) -> ComponentInfo<'a> {
    match expression {
        Expression::ArrowFunctionExpression(arrow) => component_from_arrow(arrow, inline_default),
        Expression::FunctionExpression(function) => {
            component_from_function(function, inline_default)
        }
        Expression::ParenthesizedExpression(parenthesized) => {
            component_from_expression(program, &parenthesized.expression, inline_default, seen)
        }
        Expression::Identifier(identifier) => {
            resolve_local_inner(program, identifier.name.as_str(), seen)
                .unwrap_or_else(|| not_a_component(identifier.name.as_str(), identifier.span))
        }
        other => not_a_component("default", other.span()),
    }
}

/// Summarize `export { X as default }` when the specifiers include one.
///
/// `Page` is resolved in this file the same way `export default Page` is. A
/// re-export (`export { Page as default } from "./page"`) is handled separately:
/// its binding is imported, not this file's `Page`.
fn component_from_specifiers<'a>(
    program: &'a Program<'a>,
    specifiers: &'a oxc_allocator::Vec<'a, oxc_ast::ast::ExportSpecifier<'a>>,
) -> Option<ComponentInfo<'a>> {
    let specifier = default_specifier(specifiers)?;
    let local = specifier.local.name().as_str();
    Some(
        resolve_local(program, local)
            .map(|info| info.not_inline())
            .unwrap_or_else(|| not_a_component(local, specifier.span)),
    )
}

/// The specifier exported as `default`, if there is one.
fn default_specifier<'a>(
    specifiers: &'a oxc_allocator::Vec<'a, oxc_ast::ast::ExportSpecifier<'a>>,
) -> Option<&'a oxc_ast::ast::ExportSpecifier<'a>> {
    specifiers
        .iter()
        .find(|specifier| specifier.exported.name().as_str() == "default")
}

/// Resolve a name to a function declared in this file, if it is one.
///
/// Handles `function Page() {…}`, `const Page = () => …`, `const Page =
/// function () {…}`, and parenthesized forms of those. This is a scan of
/// `program.body`, not scope analysis: a name shadowed inside a block cannot be
/// what a module-level export refers to, so the top level is the right place to
/// look. A name declared by an import or a re-export is deliberately unresolved —
/// its shape lives in another file.
///
/// The scan is order-insensitive on purpose. `export default Page` above `const
/// Page = …` is a runtime `ReferenceError` (the binding is in its temporal dead
/// zone), and a function declaration is hoisted, so a strict reading would
/// resolve one and not the other. This milestone's question is "is this route a
/// component?", and answering "it resolves to a component" for both is the
/// useful answer: the ordering bug is a JavaScript error the parser cannot be
/// asked about here, and it is not what `RA011` exists to report.
fn resolve_local<'a>(program: &'a Program<'a>, name: &'a str) -> Option<ComponentInfo<'a>> {
    resolve_local_inner(program, name, &mut Vec::new())
}

/// [`resolve_local`], tracking the names already followed.
///
/// The resolved summary is named after the declaration that holds the parameters,
/// so diagnostic text can point at it: a declarator names its literal
/// (`const AboutPage = (props) => …`), while a function declaration and a
/// followed alias already carry the right name.
///
/// `const Page = Page` and `const Page = (Page)` resolve a name to itself. Without
/// the guard they recurse until the stack overflows; a self-reference is not a
/// component, so it resolves to `None` and the caller reports the export as
/// unanalyzable instead of crashing.
fn resolve_local_inner<'a>(
    program: &'a Program<'a>,
    name: &'a str,
    seen: &mut Vec<&'a str>,
) -> Option<ComponentInfo<'a>> {
    if seen.contains(&name) {
        return None;
    }
    seen.push(name);

    program.body.iter().find_map(|statement| match statement {
        // The `id == name` filter means the summary already carries the right
        // name: a function declaration's `id` *is* the binding.
        Statement::FunctionDeclaration(function) => function
            .id
            .as_ref()
            .filter(|identifier| identifier.name == name)
            .map(|_| component_from_function(function, false)),
        Statement::VariableDeclaration(declaration) => {
            declaration.declarations.iter().find_map(|declarator| {
                if !declarator
                    .id
                    .get_identifier_name()
                    .is_some_and(|identifier| identifier.as_str() == name)
                {
                    return None;
                }
                // A function literal is anonymous (`const AboutPage = (props) =>
                // …` has no function name at all), so the *declarator* names it.
                // Without this, the summary says `"default"` and every diagnostic
                // talks about a `Page` that does not exist in the file — an agent
                // following the fix would annotate a new binding and leave the real
                // one untouched.
                //
                // The declarator names the literal even when the literal names
                // itself (`const AboutPage = function Actual() {…}`): `Actual` is
                // scoped to the function body, so the module binding an agent can
                // find and edit is `AboutPage`. A followed name (`const Alias =
                // Page`) keeps the declaration it resolves to, because that is
                // where the parameters live.
                let init = declarator.init.as_ref()?;
                let info = match init {
                    Expression::ArrowFunctionExpression(arrow) => {
                        component_from_arrow(arrow, false)
                    }
                    Expression::FunctionExpression(function) => {
                        component_from_function(function, false)
                    }
                    Expression::ParenthesizedExpression(parenthesized) => {
                        component_from_expression(program, &parenthesized.expression, false, seen)
                    }
                    _ => return None,
                };
                Some(if is_function_literal(init) {
                    info.with_name(name)
                } else {
                    info
                })
            })
        }
        _ => None,
    })
}

/// A default export that exists but is not a function this milestone can render.
fn not_a_component<'a>(name: &'a str, span: oxc_span::Span) -> ComponentInfo<'a> {
    ComponentInfo {
        kind: ComponentKind::NotAFunction,
        name,
        // No function, so no parameters and no return: these fields are only
        // read for `ComponentKind::Function`, and are stated as zero rather
        // than fabricated from a call's arguments or a class's constructor.
        params: 0,
        has_props_type: false,
        // Point at the export itself rather than an absent body, so the agent
        // sees which declaration to replace.
        body_span: span_of(span),
        returns_jsx: false,
        // There is no inline component to edit; the fix must say "replace".
        inline_default: false,
    }
}

/// Summarize an arrow function, whose body may be a block or a single
/// expression.
fn component_from_arrow<'a>(
    arrow: &'a oxc_ast::ast::ArrowFunctionExpression<'a>,
    inline_default: bool,
) -> ComponentInfo<'a> {
    let returns_jsx = match &arrow.body {
        // `(props) => <p/>`: the JSX is the body, there is no `return` to find.
        ArrowFunctionBody::JSXElement(_) | ArrowFunctionBody::JSXFragment(_) => true,
        ArrowFunctionBody::FunctionBody(body) => returns_jsx(body.statements.iter()),
        // Any other concise body returns an expression. It is JSX only when it
        // is JSX itself; a `=> View` or `=> render()` is not followed (see
        // `ComponentInfo::returns_jsx`).
        concise => is_jsx_expression(concise.to_expression()),
    };

    ComponentInfo {
        kind: ComponentKind::Function,
        name: "default",
        params: param_count(&arrow.params),
        has_props_type: has_single_annotated_param(&arrow.params),
        body_span: span_of(arrow.body.span()),
        returns_jsx,
        inline_default,
    }
}

/// Summarize a [`Function`] node (declaration or expression).
fn component_from_function<'a>(
    function: &'a Function<'a>,
    inline_default: bool,
) -> ComponentInfo<'a> {
    let body = function.body.as_deref();

    ComponentInfo {
        kind: ComponentKind::Function,
        // An anonymous function is still exported as `default`.
        name: function
            .id
            .as_ref()
            .map_or("default", |identifier| identifier.name.as_str()),
        params: param_count(&function.params),
        has_props_type: has_single_annotated_param(&function.params),
        body_span: match body {
            Some(body) => span_of(body.span),
            // `function f(props): void;` has no body. There is nowhere to point
            // and nothing to analyze, so use the function's own span.
            None => span_of(function.span),
        },
        returns_jsx: body.is_some_and(|body| returns_jsx(body.statements.iter())),
        inline_default,
    }
}

/// How many parameters a function takes, counting a rest parameter.
///
/// Oxc stores `function Page(props, ...rest)` as one `items` entry plus a
/// separate `rest` field, so `items.len()` alone would say 1 and let a route
/// that collects variadic props look propless.
fn param_count(params: &oxc_ast::ast::FormalParameters<'_>) -> usize {
    params.items.len() + usize::from(params.rest.is_some())
}

/// `true` when the function takes exactly one parameter and it is annotated.
///
/// A type alias elsewhere in the file is irrelevant; the annotation must be on
/// the parameter. A rest-only signature (`function Page(...props)`) is one
/// parameter too, and its annotation lives on the rest element.
fn has_single_annotated_param(params: &oxc_ast::ast::FormalParameters<'_>) -> bool {
    match (params.items.as_slice(), &params.rest) {
        ([only], None) => only.type_annotation.is_some(),
        ([], Some(rest)) => rest.type_annotation.is_some(),
        _ => false,
    }
}

/// Whether any of `statements` is a `return` whose argument is JSX.
///
/// Only the top level of the body is scanned. A `return` nested inside an `if`
/// or a block is not followed, which matches [`ComponentInfo::returns_jsx`]'s
/// documented limitation: this milestone asks "does the body literally return
/// JSX?", not "can the body only return JSX?".
fn returns_jsx<'a>(mut statements: impl Iterator<Item = &'a Statement<'a>>) -> bool {
    statements.any(|statement| match statement {
        Statement::ReturnStatement(returned) => {
            returned.argument.as_ref().is_some_and(is_jsx_expression)
        }
        _ => false,
    })
}

/// Whether an expression is JSX, seeing through parentheses.
///
/// `return (<p/>)` is the usual way to format a multi-line JSX return, and Oxc
/// keeps those parentheses as a `ParenthesizedExpression`. Counting it as
/// "not JSX" would reject the most idiomatic component body in the framework.
fn is_jsx_expression(expression: &Expression<'_>) -> bool {
    match expression {
        Expression::JSXElement(_) | Expression::JSXFragment(_) => true,
        Expression::ParenthesizedExpression(parenthesized) => {
            is_jsx_expression(&parenthesized.expression)
        }
        _ => false,
    }
}

/// Whether an expression *is* a function literal, seeing through parentheses.
///
/// The distinction matters when a declarator is named: `const AboutPage =
/// (props) => …` and `const Alias = (AboutPage)` both resolve to a function, but
/// only the first gives that function a name a reader can find — the second
/// aliases a declaration that is already named, and the parameters to annotate
/// belong to it.
fn is_function_literal(expression: &Expression<'_>) -> bool {
    match expression {
        Expression::ArrowFunctionExpression(_) | Expression::FunctionExpression(_) => true,
        Expression::ParenthesizedExpression(parenthesized) => {
            is_function_literal(&parenthesized.expression)
        }
        _ => false,
    }
}

/// A [`Span`] as the `(start, end)` pair every diagnostic carries.
fn span_of(span: oxc_span::Span) -> (u32, u32) {
    (span.start, span.end)
}

/// Validate the route component's export shape.
///
/// Four separate questions, one code, because they have one theme — "the default
/// export is not a route component":
///
/// - there is no default export at all,
/// - the default export is not a function this milestone can analyze,
/// - the default export takes more parameters than a route can be given,
/// - the default export does not return JSX directly.
///
/// Presence and absence are deliberately separate branches: their repairs differ,
/// and telling an agent to add a default export that already exists produces a
/// duplicate-export parse error.
pub fn validate_route_component(info: Option<&ComponentInfo>) -> Vec<Diagnostic> {
    match info {
        None => vec![missing_component()],
        Some(info) if info.kind == ComponentKind::NotAFunction => {
            vec![existing_export_is_not_a_component(info)]
        }
        Some(info) if info.is_route_component() => Vec::new(),
        Some(info) if info.params > 1 => vec![too_many_params(info)],
        Some(info) => vec![does_not_return_jsx(info)],
    }
}

/// `RA011_BAD_ROUTE_EXPORT`: the file has no default export.
fn missing_component() -> Diagnostic {
    Diagnostic {
        code: Code::RA011,
        severity: Severity::Error,
        // There is no node to point at: the absence is the problem.
        span: (0, 0),
        message: "route file has no default-exported component".into(),
        explain: "A page file's default export is its route component. Without one the router \
                  has nothing to render, and a named export such as `export function Page` is \
                  not picked up as the route."
            .into(),
        fix: "Add `export default function Page(props: Record<never, never>) { ... }` returning \
              JSX, and remove or keep the named export as an ordinary module export."
            .into(),
        doc: Code::RA011.skill_file(),
        patch: None,
    }
}

/// `RA011_BAD_ROUTE_EXPORT`: the route component expects props it cannot get.
fn too_many_params(info: &ComponentInfo) -> Diagnostic {
    Diagnostic {
        code: Code::RA011,
        severity: Severity::Error,
        span: info.body_span,
        message: format!(
            "route component `{name}` takes {params} parameters, but a route takes at most one \
             annotated `props` parameter",
            name = info.display_name(),
            params = info.params,
        ),
        explain: "A route is rendered by the router, not by a parent component, so the only value \
                  that can reach it is its props object. A second parameter can never be filled, \
                  and data the route needs must be read inside the route."
            .into(),
        fix: format!(
            "Drop the extra parameters so `{name}` takes at most one annotated \
             `props: Record<never, never>` parameter.",
            name = info.display_name(),
        ),
        doc: Code::RA011.skill_file(),
        patch: None,
    }
}

/// `RA011_BAD_ROUTE_EXPORT`: the default export is not a component.
fn does_not_return_jsx(info: &ComponentInfo) -> Diagnostic {
    Diagnostic {
        code: Code::RA011,
        severity: Severity::Error,
        span: info.body_span,
        // "directly" is load-bearing: the component may well return JSX through
        // a variable or a conditional. Only a direct `return <…/>` is recognized
        // in this milestone, and the message must not assert what it cannot see.
        message: format!(
            "route component `{name}` does not return JSX directly",
            name = info.display_name()
        ),
        explain: "A component is a function that returns JSX. Only a direct `return <…/>` is \
                  recognized in this milestone: a returned identifier, conditional, call, array, \
                  or a `return` nested inside an `if` is not followed, because resolving what a \
                  value renders requires type information this compiler does not have yet. The \
                  repair is to return JSX from the component body itself, not to hunt for a bug \
                  in the returned value."
            .into(),
        fix: format!(
            "Return JSX directly from `{name}`, e.g. `return <main>...</main>`, so the router \
             has markup to render.",
            name = info.display_name(),
        ),
        doc: Code::RA011.skill_file(),
        patch: None,
    }
}

/// `RA011_BAD_ROUTE_EXPORT`: a default export exists, but it is not a component.
///
/// The repair must never say "add a default export": one already exists, and a
/// second is a parse error. It says to replace the existing one instead.
///
/// The message claims only what is known. A class, a literal and a call are not
/// functions, but an identifier may name a component imported from another
/// module, which this milestone cannot see — so "not a component this compiler
/// can analyze" rather than "is not a component".
fn existing_export_is_not_a_component(info: &ComponentInfo) -> Diagnostic {
    let message = if info.name == "default" {
        "route file's default export is not a component the compiler can analyze".to_string()
    } else {
        format!(
            "route file's default export `{name}` is not a component the compiler can analyze",
            name = info.name,
        )
    };

    Diagnostic {
        code: Code::RA011,
        severity: Severity::Error,
        span: info.body_span,
        message,
        explain: "A page file's default export must be the route component — a function that \
                  returns JSX. A class, a literal, a call, or a component imported from another \
                  module is not analyzed as a component in this milestone, and the compiler does \
                  not guess what a value declared elsewhere renders."
            .into(),
        fix: "Replace the existing default export with a component instead of declaring a second \
              default export: `export default function Page(props: Record<never, never>) { ... }` \
              returning JSX."
            .into(),
        doc: Code::RA011.skill_file(),
        patch: None,
    }
}

#[cfg(test)]
mod tests {
    use oxc_allocator::Allocator;

    use super::*;
    use crate::source::parse_tsx;

    /// The parsed program borrows from `alloc`, so the test owns both and this
    /// helper just hands back the summary plus the codes it produces.
    fn analyze<'a>(alloc: &'a Allocator, src: &'a str) -> (Option<ComponentInfo<'a>>, Vec<Code>) {
        let program = parse_tsx(alloc, src).unwrap();
        let info = find_route_component(program);
        let codes = validate_route_component(info.as_ref())
            .iter()
            .map(|diagnostic| diagnostic.code)
            .collect();
        (info, codes)
    }

    #[test]
    fn function_declaration_reports_its_own_name_and_shape() {
        let alloc = Allocator::default();
        let src = "type Props = { id: string }\nexport default function Page(props: Props) { return <p/> }\n";
        let (info, codes) = analyze(&alloc, src);
        let info = info.expect("default export is a function");
        assert_eq!(info.name, "Page");
        assert_eq!(info.params, 1);
        assert!(info.has_props_type);
        assert!(info.returns_jsx);
        assert!(codes.is_empty());
    }

    /// The body span is the **body**, not the whole function: Task 8 scopes
    /// signal analysis to it, so including `function Page(props: …)` would widen
    /// that scope. An anonymous function is still exported as `default`.
    #[test]
    fn body_span_is_the_body_and_anonymous_is_named_default() {
        let alloc = Allocator::default();
        let src = "export default function Page(props: Props) { return <p/> }\n";
        let (info, _) = analyze(&alloc, src);
        let info = info.unwrap();
        assert_eq!(
            &src[info.body_span.0 as usize..info.body_span.1 as usize],
            "{ return <p/> }"
        );

        let anonymous = "export default function (props: Props) { return <p/> }\n";
        let alloc = Allocator::default();
        let (info, codes) = analyze(&alloc, anonymous);
        assert_eq!(info.unwrap().name, "default");
        assert!(codes.is_empty());
    }

    /// An arrow function is a function. A concise body (`=> <p/>`) has no
    /// statements at all, so it is recognized by shape, not by a return scan.
    #[test]
    fn arrow_function_default_export_is_a_component() {
        let alloc = Allocator::default();
        let src = "export default (props: Props) => <p/>\n";
        let (info, codes) = analyze(&alloc, src);
        let info = info.expect("arrow function is a component");
        assert_eq!(info.name, "default");
        assert_eq!(info.params, 1);
        assert!(info.has_props_type);
        assert!(info.returns_jsx);
        assert!(codes.is_empty());
        // A concise body has no `FunctionBody`, so the span is the expression's.
        assert_eq!(
            &src[info.body_span.0 as usize..info.body_span.1 as usize],
            "<p/>"
        );
    }

    #[test]
    fn arrow_function_block_body_counts_its_return() {
        let alloc = Allocator::default();
        let src = "export default (props: Props) => { return <p/> }\n";
        let (info, codes) = analyze(&alloc, src);
        assert!(info.unwrap().returns_jsx);
        assert!(codes.is_empty());
    }

    /// A named function expression keeps its name; an anonymous one is `default`.
    /// Both are wrapped in a `ParenthesizedExpression` by the parser — verified
    /// on oxc 0.159.0, not assumed — so the unwrapping is load-bearing.
    #[test]
    fn parenthesized_function_expression_is_a_component() {
        let alloc = Allocator::default();
        let src = "export default (function Page(props: Props) { return <p/> })\n";
        let (info, codes) = analyze(&alloc, src);
        assert_eq!(info.unwrap().name, "Page");
        assert!(codes.is_empty());

        let alloc = Allocator::default();
        let src = "export default (function (props: Props) { return <p/> })\n";
        let (info, codes) = analyze(&alloc, src);
        assert_eq!(info.unwrap().name, "default");
        assert!(codes.is_empty());
    }

    /// The same wrapping applies to a parenthesized arrow function.
    #[test]
    fn parenthesized_arrow_function_is_a_component() {
        let alloc = Allocator::default();
        let src = "export default ((props: Props) => <p/>)\n";
        let (info, codes) = analyze(&alloc, src);
        let info = info.unwrap();
        assert_eq!(info.name, "default");
        assert!(info.returns_jsx);
        assert!(codes.is_empty());
    }

    /// A default export that is not a function is present but unanalyzable: the
    /// diagnostic must say the existing export is the problem, not that one is
    /// missing. Telling an agent to add a default export here produces a
    /// duplicate-export parse error.
    #[test]
    fn non_function_default_export_is_reported_as_not_a_component() {
        for src in [
            "export default 42\n",
            "export default class Page {}\n",
            "export default pageFactory()\n",
            // An identifier that this file does not declare: it is imported, so
            // its shape lives elsewhere and guessing would be a lie.
            "export default Page\n",
        ] {
            let alloc = Allocator::default();
            let (info, codes) = analyze(&alloc, src);
            let info = info.expect("the export exists, so a summary is returned");
            assert_eq!(info.kind, ComponentKind::NotAFunction, "{src}");
            assert_eq!(codes, vec![Code::RA011], "{src}");
        }
    }

    /// The two `RA011` repairs are different, and the difference is the whole
    /// point: absent says "add", present-but-wrong must not.
    #[test]
    fn an_existing_export_is_never_told_to_add_one() {
        let alloc = Allocator::default();
        let program = parse_tsx(&alloc, "export default 42\n").unwrap();
        let info = find_route_component(program);
        let diagnostic = &validate_route_component(info.as_ref())[0];
        assert!(!diagnostic.fix.contains("Add "), "{:?}", diagnostic.fix);
        assert!(
            diagnostic
                .fix
                .contains("Replace the existing default export"),
            "{:?}",
            diagnostic.fix
        );
        assert!(
            !diagnostic
                .message
                .contains("has no default-exported component"),
            "{:?}",
            diagnostic.message
        );

        let alloc = Allocator::default();
        let program = parse_tsx(&alloc, "export function Page() { return <p/> }\n").unwrap();
        let info = find_route_component(program);
        let diagnostic = &validate_route_component(info.as_ref())[0];
        assert!(info.is_none());
        assert!(diagnostic.fix.contains("Add "), "{:?}", diagnostic.fix);
    }

    /// A named export is not a route: picking it would validate the wrong node.
    #[test]
    fn named_export_alone_is_not_a_route_component() {
        let alloc = Allocator::default();
        let (info, codes) = analyze(&alloc, "export function Page() { return <p/> }\n");
        assert!(info.is_none());
        assert_eq!(codes, vec![Code::RA011]);

        let alloc = Allocator::default();
        let src = "const Page = () => <p/>\nexport { Page }\n";
        let (info, codes) = analyze(&alloc, src);
        assert!(info.is_none());
        assert_eq!(codes, vec![Code::RA011]);
    }

    /// `export default Page` is a valid route when `Page` is a component declared
    /// in the same file: resolving a module-level name is a body scan, not
    /// dataflow, so it is done rather than guessed at.
    #[test]
    fn a_default_export_of_a_local_component_resolves() {
        for src in [
            "function Page(props: Props) { return <p/> }\nexport default Page\n",
            "const Page = (props: Props) => <p/>\nexport default Page\n",
            "const Page = (props: Props) => <p/>\nexport { Page as default }\n",
            "const Page = (props: Props) => <p/>\nexport default (Page)\n",
        ] {
            let alloc = Allocator::default();
            let (info, codes) = analyze(&alloc, src);
            let info = info.expect("the export exists");
            assert_eq!(info.kind, ComponentKind::Function, "{src}");
            assert_eq!(info.params, 1, "{src}");
            assert!(info.returns_jsx, "{src}");
            assert!(codes.is_empty(), "{src}: {codes:?}");
        }
    }

    /// The same shapes stay broken when the resolver cannot see the function:
    /// `export { Page as default }` from another module is present but
    /// unanalyzable, so it must be reported as exactly that.
    #[test]
    fn a_reexport_as_default_is_present_but_not_analyzable() {
        let alloc = Allocator::default();
        let src = "export { Page as default } from \"./page\"\n";
        let (info, codes) = analyze(&alloc, src);
        assert_eq!(info.unwrap().kind, ComponentKind::NotAFunction);
        assert_eq!(codes, vec![Code::RA011]);
    }

    /// A default export resolving to something that is not a function is still
    /// "present, not a component", even though the declaration was found.
    #[test]
    fn resolving_to_a_non_function_is_not_a_component() {
        for src in [
            "const Page = 42\nexport default Page\n",
            "const Page = pageFactory()\nexport { Page as default }\n",
        ] {
            let alloc = Allocator::default();
            let (info, codes) = analyze(&alloc, src);
            assert_eq!(info.unwrap().kind, ComponentKind::NotAFunction, "{src}");
            assert_eq!(codes, vec![Code::RA011], "{src}");
        }
    }

    /// A self-referential declarator resolves no further. Without the visited-name
    /// guard, following `const Page = Page` recurses until the stack overflows —
    /// a crash in the analyzer for a file an agent can write by accident.
    #[test]
    fn a_self_referential_default_export_does_not_recurse() {
        for src in [
            "const Page = Page\nexport default Page\n",
            "const Page = (Page)\nexport { Page as default }\n",
        ] {
            let alloc = Allocator::default();
            let (info, codes) = analyze(&alloc, src);
            assert_eq!(info.unwrap().kind, ComponentKind::NotAFunction, "{src}");
            assert_eq!(codes, vec![Code::RA011], "{src}");
        }
    }

    /// The first default export wins; a second is not consulted.
    #[test]
    fn only_the_default_export_is_considered() {
        let alloc = Allocator::default();
        let src = "export default function Page(props: Props) { return <p/> }\nexport function Other() {}\n";
        let (info, codes) = analyze(&alloc, src);
        assert_eq!(info.unwrap().name, "Page");
        assert!(codes.is_empty());
    }

    /// A real route file opens with its mode directive, which Oxc hoists out of
    /// `program.body` into `program.directives`. Walking `body` must therefore
    /// still find the export, and the directive must not shift any span.
    #[test]
    fn a_mode_directive_before_the_export_is_found_through() {
        let alloc = Allocator::default();
        let src = "\"use island\"\ntype Props = { id: string }\nexport default function Page(props: Props) { return <p/> }\n";
        let (info, codes) = analyze(&alloc, src);
        let info = info.expect("the directive is hoisted, not in the way");
        assert_eq!(info.name, "Page");
        assert!(codes.is_empty());
        assert_eq!(
            &src[info.body_span.0 as usize..info.body_span.1 as usize],
            "{ return <p/> }"
        );
    }

    #[test]
    fn too_many_params_is_ra011() {
        let alloc = Allocator::default();
        let src = "export default function Page(a: A, b: B) { return <p/> }\n";
        let (info, codes) = analyze(&alloc, src);
        assert_eq!(info.unwrap().params, 2);
        assert_eq!(codes, vec![Code::RA011]);
    }

    /// The message and the fix must state the same rule. They used to disagree —
    /// "a route takes no props" against "takes at most one" — leaving the agent
    /// to guess which one it should implement.
    #[test]
    fn too_many_params_states_one_rule_both_places() {
        let alloc = Allocator::default();
        let src = "export default function Page(a: A, b: B) { return <p/> }\n";
        let program = parse_tsx(&alloc, src).unwrap();
        let info = find_route_component(program);
        let diagnostic = &validate_route_component(info.as_ref())[0];
        assert!(
            diagnostic.message.contains("at most one"),
            "{:?}",
            diagnostic.message
        );
        assert!(
            diagnostic.fix.contains("at most one"),
            "{:?}",
            diagnostic.fix
        );
        assert!(
            !diagnostic.message.contains("takes no props"),
            "the message must not claim a stricter rule than the fix: {:?}",
            diagnostic.message
        );
    }

    /// `returns_jsx` is syntactic: a returned identifier, conditional, call, or
    /// no return at all is not JSX in this milestone, and pretending otherwise
    /// would be a guess. The message must say "directly" — the code may well
    /// return JSX, and asserting otherwise is what sends an agent hunting for a
    /// bug that does not exist.
    #[test]
    fn returning_a_non_jsx_value_is_ra011() {
        for src in [
            "export default function Page(props: Props) { return View }\n",
            "export default function Page(props: Props) { return ready ? <p/> : <i/> }\n",
            "export default function Page(props: Props) { return render() }\n",
            "export default function Page(props: Props) { return [<p/>] }\n",
            "export default function Page(props: Props) { if (x) return <p/> }\n",
            "export default function Page(props: Props) {}\n",
        ] {
            let alloc = Allocator::default();
            let (info, codes) = analyze(&alloc, src);
            assert!(!info.unwrap().returns_jsx, "{src}");
            assert_eq!(codes, vec![Code::RA011], "{src}");
        }
    }

    /// The diagnosis must not assert something the implementation cannot see:
    /// these bodies *do* return JSX, just not directly.
    #[test]
    fn the_non_direct_return_message_is_honest() {
        for src in [
            "export default function Page(props: Props) { return View }\n",
            "export default function Page(props: Props) { return ready ? <p/> : <i/> }\n",
            "export default function Page(props: Props) { return [<p/>] }\n",
            "export default function Page(props: Props) { if (x) return <p/> }\n",
        ] {
            let alloc = Allocator::default();
            let program = parse_tsx(&alloc, src).unwrap();
            let info = find_route_component(program);
            let diagnostic = &validate_route_component(info.as_ref())[0];
            assert!(
                diagnostic.message.contains("does not return JSX directly"),
                "{src}: {:?}",
                diagnostic.message
            );
            assert!(
                diagnostic.explain.contains("direct `return"),
                "the explain must name the limitation: {:?}",
                diagnostic.explain
            );
        }
    }

    /// The controller ruling: a return type annotation is not a props type.
    /// `P(): Record<never, never>` takes zero parameters, and `RA006` says so.
    /// A future change that inspected `function.return_type` would pass every
    /// other test in this file and fail this one.
    ///
    /// Run through the same two-pass shape Task 9's `analyze` uses: route
    /// validation, then `check_props` for a default export that is a function.
    #[test]
    fn a_return_type_annotation_is_still_zero_params() {
        let alloc = Allocator::default();
        let src = "export default function P(): Record<never, never> { return <p/> }\n";
        let program = parse_tsx(&alloc, src).unwrap();
        let info = find_route_component(program);
        let info = info.unwrap();
        assert_eq!(info.params, 0);
        assert!(!info.has_props_type);

        let mut codes: Vec<Code> = validate_route_component(Some(&info))
            .iter()
            .map(|diagnostic| diagnostic.code)
            .collect();
        codes.extend(
            crate::analyze::props::check_props(&info)
                .iter()
                .map(|diagnostic| diagnostic.code),
        );
        assert_eq!(codes, vec![Code::RA006]);
    }

    /// An async route is an ordinary function shape: the modifier changes nothing
    /// about its params, body, or whether it returns JSX.
    #[test]
    fn async_component_is_analyzed_the_same_way() {
        let alloc = Allocator::default();
        let src = "export default async function Page(props: Props) { return <p/> }\n";
        let (info, codes) = analyze(&alloc, src);
        let info = info.unwrap();
        assert_eq!(info.name, "Page");
        assert_eq!(info.params, 1);
        assert!(info.has_props_type);
        assert!(info.returns_jsx);
        assert!(codes.is_empty());
    }

    /// A fragment body is JSX too.
    #[test]
    fn jsx_fragment_counts_as_returning_jsx() {
        let alloc = Allocator::default();
        let src = "export default function Page(props: Props) { return <></> }\n";
        let (info, codes) = analyze(&alloc, src);
        assert!(info.unwrap().returns_jsx);
        assert!(codes.is_empty());
    }

    /// `return (<p/>)` is how a multi-line JSX return is formatted, and Oxc
    /// keeps the parentheses as a `ParenthesizedExpression`. Treating that as
    /// "not JSX" would reject the most idiomatic component body, so it is
    /// unwrapped — verified against the parser, not assumed.
    #[test]
    fn a_parenthesized_jsx_return_is_still_jsx() {
        for src in [
            "export default function Page(props: Props) {\n  return (\n    <p/>\n  )\n}\n",
            "export default (props: Props) => (<p/>)\n",
        ] {
            let alloc = Allocator::default();
            let (info, codes) = analyze(&alloc, src);
            assert!(info.unwrap().returns_jsx, "{src}");
            assert!(codes.is_empty(), "{src}");
        }
    }

    /// `RA011`'s text for an anonymous component was pinned only through `RA006`.
    /// The agent triages on this message, so it must name a symbol it can act on:
    /// `default` is a reserved word and names the export, not anything in the
    /// file. `Page` is the conventional stand-in for a component that genuinely
    /// has no binding name.
    #[test]
    fn an_anonymous_component_is_named_page_in_ra011() {
        for (src, expected_fragment) in [
            (
                // Two parameters: the complaint is about the signature.
                "export default (a, b) => <p/>\n",
                "route component `Page` takes 2 parameters",
            ),
            (
                // No direct JSX return: the complaint is about the body.
                "export default function () { return View }\n",
                "route component `Page` does not return JSX directly",
            ),
            (
                // A concise arrow whose body is a call.
                "export default () => render()\n",
                "route component `Page` does not return JSX directly",
            ),
        ] {
            let alloc = Allocator::default();
            let program = parse_tsx(&alloc, src).unwrap();
            let info = find_route_component(program);
            let diagnostic = &validate_route_component(info.as_ref())[0];
            assert_eq!(diagnostic.code, Code::RA011, "{src}");
            assert!(
                diagnostic.message.contains(expected_fragment),
                "{src}: {:?}",
                diagnostic.message
            );
            assert!(
                !diagnostic.message.contains("`default`")
                    && !diagnostic.fix.contains("function default"),
                "{src}: illegal identifier in the text: {:?} / {:?}",
                diagnostic.message,
                diagnostic.fix
            );
        }
    }

    /// `RA011` names the binding too. A file whose component is `AboutPage` must
    /// not be told that `Page` has the wrong signature — there is no `Page` to
    /// edit, and the diagnostic would send the agent to invent one.
    #[test]
    fn a_resolved_component_is_named_by_its_binding_in_ra011() {
        for (src, expected) in [
            (
                "const AboutPage = (a: A, b: B) => <p/>\nexport default AboutPage\n",
                "route component `AboutPage` takes 2 parameters",
            ),
            (
                "const AboutPage = (props: Props) => { return View }\nexport { AboutPage as default }\n",
                "route component `AboutPage` does not return JSX directly",
            ),
        ] {
            let alloc = Allocator::default();
            let program = parse_tsx(&alloc, src).unwrap();
            let info = find_route_component(program);
            let info = info.unwrap();
            assert_eq!(info.name, "AboutPage", "{src}");
            assert_eq!(info.display_name(), "AboutPage", "{src}");
            let diagnostic = &validate_route_component(Some(&info))[0];
            assert_eq!(diagnostic.code, Code::RA011, "{src}");
            assert!(
                diagnostic.message.contains(expected),
                "{src}: {:?}",
                diagnostic.message
            );
            assert!(
                !diagnostic.fix.contains("`Page`"),
                "{src}: {:?}",
                diagnostic.fix
            );
        }
    }

    /// Every diagnostic uses the code's own skill file, so Task 15's generated
    /// docs and these paths cannot drift apart.
    #[test]
    fn every_diagnostic_points_at_the_skill_file() {
        for diagnostic in validate_route_component(None) {
            assert_eq!(diagnostic.doc, Code::RA011.skill_file());
        }
    }

    /// `RA011`'s fix names a full signature. Applying it must be clean on the
    /// next run — otherwise a missing export sends the agent into a repair loop
    /// with `RA006`, which would fire if that signature were unannotated.
    #[test]
    fn following_the_missing_export_fix_is_clean() {
        let alloc = Allocator::default();
        let src = "export default function Page(props: Record<never, never>) { return <p/> }\n";
        let (info, codes) = analyze(&alloc, src);
        assert!(codes.is_empty());
        assert!(info.unwrap().has_props_type);
    }
}
