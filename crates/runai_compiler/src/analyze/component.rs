//! What a file's components are, and whether its default export is one.
//!
//! A page file's **default export** is its route component. Nothing else is: a
//! named export is an ordinary module export, and importing another file's
//! component does not make it *this* route's component. So [`find_route_component`]
//! looks only for `export default …`, and a file without one reports
//! `RA011_BAD_ROUTE_EXPORT` rather than picking a plausible-looking named
//! function and validating the wrong thing.
//!
//! [`find_components`] answers the wider question: *every* function in the file
//! that returns JSX is a component, so every one of them owes a written props
//! type (`RA006`) — not only the route. A page's `_shell.tsx` declares components
//! too, which is why the props rule cannot live in route validation alone.
//!
//! The route component must be a component — a function that returns JSX — and
//! it must take at most one parameter, because that single parameter is the
//! route's props object. The router supplies nothing, so a second parameter can
//! never be filled.

use oxc_ast::ast::{
    ArrowFunctionBody, Declaration, ExportDefaultDeclaration, ExportDefaultDeclarationKind,
    Expression, Function, Program, Statement,
};
use oxc_span::GetSpan;

use crate::diagnostic::{Code, Diagnostic};

/// Whether a default export is a function this milestone can analyze.
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

/// The shape of a component found in a file.
///
/// This is a summary, not the AST node itself: it records only the answers the
/// analyses in this milestone ask, so later tasks do not have to re-walk the
/// program to answer them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComponentInfo<'a> {
    /// Whether this is a function at all. When it is
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
    /// From the start of the statement that declares this component through the
    /// opening of its body, parameter list in between: the `{` of a block body
    /// (`export default function Page(props) {`), or the whole concise expression
    /// of an arrow (`const Badge = (props) => <b/>`) — the parentheses included.
    ///
    /// Together with [`params_span`](Self::params_span) this is the range `RA006`
    /// rebuilds the exact signature line to write from, in the declaration form
    /// the file already uses. Splicing the source rather than synthesizing a
    /// `function` line is what stops the repair from naming a second declaration
    /// of the same binding, which is a redeclaration error.
    pub signature_span: (u32, u32),
    /// The span of the parameter list. For a propless component this is the empty
    /// `()`, which is exactly where the missing parameter goes.
    pub params_span: (u32, u32),
    /// The span of the declared name, so a diagnostic can point at the component
    /// itself rather than at a pair of empty parentheses. Anonymous components
    /// fall back to [`params_span`](Self::params_span).
    pub name_span: (u32, u32),
    /// The function body's span, for analyses that are scoped to a component
    /// body. A concise arrow body (`() => <p/>`) has no `FunctionBody` node, so
    /// this is the expression's span instead.
    pub body_span: (u32, u32),
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
    /// Whether this component is reachable as the file's default export.
    ///
    /// `true` for `export default function Page(…)`, `export default (props) =>
    /// …`, and for a declaration the default export refers to by name. `false`
    /// for every other component in the file, which owes a props type just the
    /// same but is not a route.
    pub is_default_export: bool,
    /// Whether the function carries the `async` modifier.
    ///
    /// Both emitters call the route component **synchronously** and use its return
    /// value: a `"use static"` route's `render()` returns one string, and an
    /// island's `mount` inserts the returned node. An `async` component returns a
    /// promise, and its `await` copied into a non-async `function render()` would
    /// be a syntax error. [`validate_route_component`] refuses it.
    pub is_async: bool,
    /// Whether the function is a generator (`function*`).
    ///
    /// Refused for the same reason as [`is_async`](Self::is_async): `yield` copied
    /// into a `function render()` that is not a generator is a syntax error, and a
    /// generator's return value is an iterator rather than markup. Recorded as its
    /// own field rather than folded into `is_async`, because the two are separate
    /// modifiers and a function can be both.
    pub is_generator: bool,
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

    /// The declaration's signature line, rebuilt from `source` with its parameter
    /// list replaced by `params`.
    ///
    /// `params` is written as it appears in the source, parentheses included
    /// (`(props: Record<never, never>)`) — the span being replaced is the whole
    /// list. This is the per-site half of `RA006`: the canonical fix says *what*
    /// to do ("exactly one typed parameter"), and this says *where* — the exact
    /// line to write. Slicing the source keeps `const Badge = (props) => …` a
    /// `const` and `export default function Page` an `export default`, so
    /// following the message cannot produce a second declaration of the same
    /// binding.
    pub fn signature(&self, source: &str, params: &str) -> String {
        format!(
            "{prefix}{params}{suffix}",
            prefix = slice(source, (self.signature_span.0, self.params_span.0)),
            suffix = slice(source, (self.params_span.1, self.signature_span.1)),
        )
    }

    /// The span `RA006` points at: the parameters it asks to type, or the name
    /// when there are none to point at.
    pub fn props_span(&self) -> (u32, u32) {
        if self.params == 0 {
            self.name_span
        } else {
            self.params_span
        }
    }

    /// The same summary, marked as reachable through the default export.
    fn mark_default_export(mut self) -> Self {
        self.is_default_export = true;
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
    /// `export default AboutPage` leave `AboutPage` unannotated — the next run
    /// would report the same code again.
    fn with_name(mut self, name: &'a str, span: (u32, u32)) -> Self {
        self.name = name;
        self.name_span = span;
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
                    return Some(info.mark_default_export());
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

/// Find every component in `program`: a function that returns JSX.
///
/// Exported or not, declaration or `const`-bound arrow/function expression. This
/// is the wider question `RA006` asks, because *every* component owes a written
/// props type — not only the route. A page's `_shell.tsx` declares components
/// too, and none of them are the default export.
///
/// What it catches: a top-level `function X(…) {…}`, `const X = (…) => …`,
/// `const X = (…) => {…}`, `const X = function (…) {…}`, each declared plainly or
/// under `export`/`export default`, and each through parentheses.
///
/// What it does not catch, deliberately: methods, nested functions (a `const`
/// inside another function's body is not a component a module reader can name),
/// components built by a factory (`const X = component(…)`), components reaching
/// this file through an `import`, and a `const A = …, B = …` list beyond its
/// first declarator. It also cannot count a body that returns JSX indirectly
/// (`return View`), for the same reason [`ComponentInfo::returns_jsx`] cannot.
///
/// A component that is the default export appears once. The default export is
/// often reached by name, so the walk summarizes the declaration that holds it
/// first; the duplicate is dropped by body span and the survivor is marked
/// [`is_default_export`](ComponentInfo::is_default_export).
pub fn find_components<'a>(program: &'a Program<'a>) -> Vec<ComponentInfo<'a>> {
    let mut components = Vec::new();
    for statement in &program.body {
        match statement {
            Statement::FunctionDeclaration(function) => push_if_component(
                &mut components,
                component_from_function(function, None, None),
            ),
            Statement::VariableDeclaration(declaration) => components_from_declaration(
                program,
                declaration,
                statement_start(statement),
                &mut components,
            ),
            // `export function Page(…)`, `export const Badge = …`
            Statement::ExportDeclaration(declaration) => match &declaration.declaration {
                Declaration::FunctionDeclaration(function) => push_if_component(
                    &mut components,
                    component_from_function(function, Some(declaration.span.start), None),
                ),
                Declaration::VariableDeclaration(variables) => components_from_declaration(
                    program,
                    variables,
                    declaration.span.start,
                    &mut components,
                ),
                _ => {}
            },
            // `export default function Page(…)`, `export default (props) => …`,
            // `export default Page`. The default export is a component too, and
            // the props rule is the same one.
            Statement::ExportDefaultDeclaration(declaration) => push_if_component(
                &mut components,
                component_from_default(program, declaration),
            ),
            _ => {}
        }
    }

    // One declaration can be summarized twice: once through its own statement
    // and once through the default export that names it. Two `RA006`s for one
    // untyped component would overstate the repair, and the body span identifies
    // the declaration, so the duplicate is dropped.
    let mut unique: Vec<ComponentInfo<'a>> = Vec::with_capacity(components.len());
    for component in components {
        if !unique
            .iter()
            .any(|seen| seen.body_span == component.body_span)
        {
            unique.push(component);
        }
    }

    // Mark the route. Reaching a component by name means the walk necessarily
    // summarized it as an ordinary declaration first, so this is the only place
    // that knows both answers at once.
    if let Some(route) = find_route_component(program) {
        for component in &mut unique {
            if component.body_span == route.body_span {
                component.is_default_export = true;
            }
        }
    }

    unique
}

/// Push `info` when it is a component (a function that returns JSX).
fn push_if_component<'a>(components: &mut Vec<ComponentInfo<'a>>, info: ComponentInfo<'a>) {
    if info.returns_jsx {
        components.push(info);
    }
}

/// The start of a top-level statement, used as the start of a signature line.
fn statement_start(statement: &Statement<'_>) -> u32 {
    statement.span().start
}

/// Every component bound by a `const`/`let`/`var` initializer in `declaration`.
///
/// `start` is the enclosing statement's start, so an exported `const` is rebuilt
/// as `export const Badge = …` and a plain one as `const Badge = …`.
///
/// A list (`const A = …, B = …`) is walked declarator by declarator. Only the
/// first one owns the statement's `const` prefix; for the rest the signature is
/// the declarator alone (`B = (props: …) => …`), which is the fragment to edit in
/// place. Naming the whole statement for every declarator would give `B` a line
/// that describes `A`.
fn components_from_declaration<'a>(
    program: &'a Program<'a>,
    declaration: &'a oxc_ast::ast::VariableDeclaration<'a>,
    start: u32,
    components: &mut Vec<ComponentInfo<'a>>,
) {
    for (index, declarator) in declaration.declarations.iter().enumerate() {
        let Some(init) = declarator.init.as_ref() else {
            continue;
        };
        // The first declarator starts where the statement does (`const`, or the
        // `export` before it); a later one starts at its own binding.
        let start = if index == 0 {
            start
        } else {
            declarator.span.start
        };
        let end = declarator.span.end;
        let info = match init {
            Expression::ArrowFunctionExpression(arrow) => component_from_arrow(arrow, start, end),
            Expression::FunctionExpression(function) => {
                component_from_function(function, Some(start), Some(end))
            }
            Expression::ParenthesizedExpression(parenthesized) => component_from_expression(
                program,
                &parenthesized.expression,
                start,
                end,
                &mut Vec::new(),
            ),
            _ => continue,
        };
        // The declarator names the literal; a followed name (`const Alias = Page`)
        // keeps the declaration it resolves to, whose parameters live there.
        let info = match (
            is_function_literal(init),
            declarator.id.get_identifier_name(),
        ) {
            (true, Some(name)) => info.with_name(name.as_str(), span_of(declarator.id.span())),
            _ => info,
        };
        push_if_component(components, info);
    }
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
    let (start, end) = (declaration.span.start, declaration.span.end);
    let info = match &declaration.declaration {
        // `export default function Page(props) {…}`, the anonymous
        // `export default function (props) {…}` (the same node with no `id`),
        // and a function *expression*. Oxc has separate variants for the
        // declaration and expression forms, but they summarize identically.
        ExportDefaultDeclarationKind::FunctionDeclaration(function)
        | ExportDefaultDeclarationKind::FunctionExpression(function) => {
            component_from_function(function, Some(start), Some(end))
        }
        // `export default (props) => <p/>`
        ExportDefaultDeclarationKind::ArrowFunctionExpression(arrow) => {
            component_from_arrow(arrow, start, end)
        }
        // `export default (function Page() {…})`, `export default ((props) => …)`
        // and `export default (Page)`: parentheses are not part of the function,
        // but they are part of the AST shape the parser produces (verified on
        // 0.159.0), so unwrap them rather than calling it "not a component".
        //
        // The signature is sliced from the enclosing `export default`, so a
        // rebuilt line keeps the parentheses it was written with.
        ExportDefaultDeclarationKind::ParenthesizedExpression(parenthesized) => {
            component_from_expression(
                program,
                &parenthesized.expression,
                start,
                end,
                &mut Vec::new(),
            )
        }
        // `export default Page`: a valid route when `Page` is a component
        // declared in this file. Resolving it is a body scan, not dataflow — if
        // it is declared here we can summarize it exactly, and if it is imported
        // we must not guess what it is.
        //
        // The signature is the *declaration's*, so the rebuilt line is the one
        // that declares the binding, not a second `export default`.
        ExportDefaultDeclarationKind::Identifier(identifier) => {
            resolve_local(program, identifier.name.as_str())
                .unwrap_or_else(|| not_a_component(identifier.name.as_str(), identifier.span))
        }
        // `export default 42`, `export default class X {}`, `export default
        // pageFactory()`: an export, but nothing that renders.
        other => not_a_component("default", other.span()),
    };
    info.mark_default_export()
}

/// Summarize a parenthesized expression by what is inside it.
///
/// `start` and `end` are the enclosing declaration's extent, so a rebuilt
/// signature keeps the parentheses the source wrote.
fn component_from_expression<'a>(
    program: &'a Program<'a>,
    expression: &'a Expression<'a>,
    start: u32,
    end: u32,
    seen: &mut Vec<&'a str>,
) -> ComponentInfo<'a> {
    match expression {
        Expression::ArrowFunctionExpression(arrow) => component_from_arrow(arrow, start, end),
        Expression::FunctionExpression(function) => {
            component_from_function(function, Some(start), Some(end))
        }
        Expression::ParenthesizedExpression(parenthesized) => {
            component_from_expression(program, &parenthesized.expression, start, end, seen)
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
    Some(resolve_local(program, local).unwrap_or_else(|| not_a_component(local, specifier.span)))
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
            .map(|_| component_from_function(function, None, None)),
        Statement::VariableDeclaration(declaration) => {
            resolve_local_declaration(program, declaration, statement.span().start, name, seen)
        }
        // `export const Page = …`: the statement an agent edits starts at
        // `export`, not at `const`, so the rebuilt signature keeps the export
        // keyword and cannot be applied as a second plain `const`.
        Statement::ExportDeclaration(declaration) => match &declaration.declaration {
            Declaration::VariableDeclaration(variables) => {
                resolve_local_declaration(program, variables, declaration.span.start, name, seen)
            }
            Declaration::FunctionDeclaration(function) => function
                .id
                .as_ref()
                .filter(|identifier| identifier.name == name)
                .map(|_| component_from_function(function, Some(declaration.span.start), None)),
            _ => None,
        },
        _ => None,
    })
}

/// [`resolve_local_inner`] for the declarators of one `const`/`let`/`var`.
///
/// `start` is the enclosing statement's start: `export const` or plain `const`.
fn resolve_local_declaration<'a>(
    program: &'a Program<'a>,
    declaration: &'a oxc_ast::ast::VariableDeclaration<'a>,
    start: u32,
    name: &'a str,
    seen: &mut Vec<&'a str>,
) -> Option<ComponentInfo<'a>> {
    declaration.declarations.iter().find_map(|declarator| {
        if !declarator
            .id
            .get_identifier_name()
            .is_some_and(|identifier| identifier.as_str() == name)
        {
            return None;
        }
        // A function literal is anonymous (`const AboutPage = (props) => …` has
        // no function name at all), so the *declarator* names it. Without this,
        // the summary says `"default"` and every diagnostic talks about a `Page`
        // that does not exist in the file — an agent following the fix would
        // annotate a new binding and leave the real one untouched.
        //
        // The declarator names the literal even when the literal names itself
        // (`const AboutPage = function Actual() {…}`): `Actual` is scoped to the
        // function body, so the module binding an agent can find and edit is
        // `AboutPage`. A followed name (`const Alias = Page`) keeps the
        // declaration it resolves to, because that is where the parameters live.
        let init = declarator.init.as_ref()?;
        let info = match init {
            Expression::ArrowFunctionExpression(arrow) => {
                component_from_arrow(arrow, start, declaration.span.end)
            }
            Expression::FunctionExpression(function) => {
                component_from_function(function, Some(start), Some(declaration.span.end))
            }
            Expression::ParenthesizedExpression(parenthesized) => component_from_expression(
                program,
                &parenthesized.expression,
                start,
                declaration.span.end,
                seen,
            ),
            _ => return None,
        };
        Some(
            match (
                is_function_literal(init),
                declarator.id.get_identifier_name(),
            ) {
                (true, Some(binding)) => {
                    info.with_name(binding.as_str(), span_of(declarator.id.span()))
                }
                _ => info,
            },
        )
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
        signature_span: span_of(span),
        params_span: span_of(span),
        name_span: span_of(span),
        body_span: span_of(span),
        returns_jsx: false,
        // `check_props` stops at `NotAFunction`; `RA011` owns this repair.
        is_default_export: true,
        // No function at all, so neither modifier exists.
        is_async: false,
        is_generator: false,
    }
}

/// Summarize an arrow function, whose body may be a block or a single
/// expression.
///
/// `(start, end)` is the enclosing declaration's extent: an `export default`, a
/// `const` statement, or a parenthesized expression.
fn component_from_arrow<'a>(
    arrow: &'a oxc_ast::ast::ArrowFunctionExpression<'a>,
    start: u32,
    end: u32,
) -> ComponentInfo<'a> {
    let body = arrow.body.span();
    let params = span_of(arrow.params.span);
    let returns_jsx = match &arrow.body {
        // `(props) => <p/>`: the JSX is the body, there is no `return` to find.
        ArrowFunctionBody::JSXElement(_) | ArrowFunctionBody::JSXFragment(_) => true,
        ArrowFunctionBody::FunctionBody(block) => returns_jsx(block.statements.iter()),
        // Any other concise body returns an expression. It is JSX only when it
        // is JSX itself; a `=> View` or `=> render()` is not followed (see
        // `ComponentInfo::returns_jsx`).
        concise => is_jsx_expression(concise.to_expression()),
    };

    ComponentInfo {
        kind: ComponentKind::Function,
        // An arrow function has no name of its own; only a binding can name it.
        name: "default",
        params: param_count(&arrow.params),
        has_props_type: has_single_annotated_param(&arrow.params),
        signature_span: match &arrow.body {
            // A block body opens with `{`: the signature line ends there.
            ArrowFunctionBody::FunctionBody(block) => (start, block.span.start.saturating_add(1)),
            // A concise body is the line to write up to the end of the
            // declaration (which includes a wrapping `)` if there is one).
            _ => (start, end),
        },
        params_span: params,
        // Replaced by `with_name` when a binding names it.
        name_span: params,
        body_span: span_of(body),
        returns_jsx,
        // Marked by `mark_default_export` or `find_components`, which are the
        // only places that know whether this declaration is the default export.
        is_default_export: false,
        // An arrow function cannot be a generator, and only `async` is written
        // before its parameter list.
        is_async: arrow.r#async,
        is_generator: false,
    }
}

/// Summarize a [`Function`] node (declaration or expression).
///
/// `start` and `end` are the enclosing declaration's extent when the caller knows
/// it; `None` starts at the function itself and ends at its body.
fn component_from_function<'a>(
    function: &'a Function<'a>,
    start: Option<u32>,
    end: Option<u32>,
) -> ComponentInfo<'a> {
    let body = function.body.as_deref();
    let params = span_of(function.params.span);
    let name_span = function
        .id
        .as_ref()
        .map_or(params, |identifier| span_of(identifier.span));

    ComponentInfo {
        kind: ComponentKind::Function,
        // An anonymous function is still exported as `default`.
        name: function
            .id
            .as_ref()
            .map_or("default", |identifier| identifier.name.as_str()),
        params: param_count(&function.params),
        has_props_type: has_single_annotated_param(&function.params),
        signature_span: match body {
            // The signature line ends at the `{` that opens the block body.
            Some(body) => (
                start.unwrap_or(function.span.start),
                body.span.start.saturating_add(1),
            ),
            // `function f(props): void;` has no body. There is nothing to open,
            // so the whole declaration is the signature.
            None => (
                start.unwrap_or(function.span.start),
                end.unwrap_or(function.span.end),
            ),
        },
        params_span: params,
        name_span,
        body_span: match body {
            Some(body) => span_of(body.span),
            // A bodyless declaration has nowhere to point and nothing to
            // analyze, so use the function's own span.
            None => span_of(function.span),
        },
        returns_jsx: body.is_some_and(|body| returns_jsx(body.statements.iter())),
        // Marked by `mark_default_export` or `find_components`, which are the only
        // places that know whether this declaration is the default export.
        is_default_export: false,
        is_async: function.r#async,
        is_generator: function.generator,
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

/// The `(start, end)` range of `source`, clamped to character boundaries.
///
/// Spans come from the parser that produced `source`, so they are in bounds by
/// construction. The clamping is defensive: a diagnostic that panics while
/// rendering is worse than one with a shorter quote, and a non-ASCII JSX
/// attribute could land a byte offset mid-character.
fn slice(source: &str, (start, end): (u32, u32)) -> &str {
    let mut start = (start as usize).min(source.len());
    let mut end = (end as usize).min(source.len());
    while start > 0 && !source.is_char_boundary(start) {
        start -= 1;
    }
    while end > start && !source.is_char_boundary(end) {
        end -= 1;
    }
    if end < start {
        return "";
    }
    &source[start..end]
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
        // An `async` or generator route component is refused before the shape
        // checks, because both are shape defects the emitters cannot write
        // around: a `render()` that returns one string cannot `await`, and a
        // `function render()` that is not a generator cannot `yield`. The rule is
        // the same in both modes — an island's `mount` calls the component
        // synchronously too.
        Some(info) if info.is_async || info.is_generator => vec![not_synchronous(info)],
        Some(info) if info.is_route_component() => Vec::new(),
        Some(info) if info.params > 1 => vec![too_many_params(info)],
        Some(info) => vec![does_not_return_jsx(info)],
    }
}

/// `RA014`: the route component is `async` or a generator, so it is not a
/// function either emitter can call.
///
/// The code is `RA014`, not `RA011`: the file parses, and the component *is* a
/// function that returns JSX — it is the modifier that the emitted module cannot
/// reproduce. `RA011`'s repair ("give it one annotated parameter and return JSX")
/// would leave the modifier in place and the next run would report the same row,
/// which is the repair loop this diagnostic exists to avoid.
fn not_synchronous(info: &ComponentInfo) -> Diagnostic {
    // Name the modifier the agent actually wrote, so the edit is unambiguous.
    let modifier = if info.is_generator && info.is_async {
        "`async` and a generator (`async function*`)"
    } else if info.is_generator {
        "a generator (`function*`)"
    } else {
        "`async`"
    };
    // The text to delete: `async` for an async function, `*` for a generator, and
    // both for the combination, so applying it leaves a plain function.
    let remove = match (info.is_async, info.is_generator) {
        (true, true) => "`async` and the `*`",
        (false, true) => "the `*`",
        (true, false) => "`async`",
        (false, false) => "",
    };
    Diagnostic::new(
        Code::RA014,
        info.body_span,
        format!(
            "route component `{name}` is {modifier}, but both render modes call the component \
             synchronously and use its return value: a `\"use static\"` route's `render()` returns \
             one string, and an island mounts the returned node. A promise or an iterator is \
             neither, and `await` or `yield` copied into `function render()` is a syntax error. \
             Remove {remove} and make the component return its markup directly.",
            name = info.display_name(),
        ),
    )
}

/// `RA011_BAD_ROUTE_EXPORT`: the file has no default export.
fn missing_component() -> Diagnostic {
    // There is no node to point at: the absence is the problem.
    Diagnostic::new(
        Code::RA011,
        (0, 0),
        "route file has no default-exported component",
    )
}

/// `RA011_BAD_ROUTE_EXPORT`: the route component expects props it cannot get.
fn too_many_params(info: &ComponentInfo) -> Diagnostic {
    Diagnostic::new(
        Code::RA011,
        info.body_span,
        format!(
            "route component `{name}` takes {params} parameters, but a route takes at most one \
             annotated `props` parameter",
            name = info.display_name(),
            params = info.params,
        ),
    )
}

/// `RA011_BAD_ROUTE_EXPORT`: the default export is not a component.
fn does_not_return_jsx(info: &ComponentInfo) -> Diagnostic {
    // "directly" is load-bearing: the component may well return JSX through
    // a variable or a conditional. Only a direct `return <…/>` is recognized
    // in this milestone, and the message must not assert what it cannot see.
    Diagnostic::new(
        Code::RA011,
        info.body_span,
        format!(
            "route component `{name}` does not return JSX directly",
            name = info.display_name()
        ),
    )
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

    Diagnostic::new(Code::RA011, info.body_span, message)
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
    /// point: absent says "add", present-but-wrong must not. The canonical fix
    /// covers both branches, so the distinction has to be in the message.
    #[test]
    fn an_existing_export_is_never_told_to_add_one() {
        let alloc = Allocator::default();
        let program = parse_tsx(&alloc, "export default 42\n").unwrap();
        let info = find_route_component(program);
        let diagnostic = &validate_route_component(info.as_ref())[0];
        assert!(
            !diagnostic.message.contains("Add "),
            "{:?}",
            diagnostic.message
        );
        assert!(
            diagnostic
                .message
                .contains("not a component the compiler can analyze"),
            "{:?}",
            diagnostic.message
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
        assert!(
            diagnostic
                .message
                .contains("has no default-exported component"),
            "{:?}",
            diagnostic.message
        );
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
            crate::analyze::props::check_props(&info, src)
                .iter()
                .map(|diagnostic| diagnostic.code),
        );
        assert_eq!(codes, vec![Code::RA006]);
    }

    /// An `async` route component is refused: both emitters call the component
    /// synchronously and use its return value. A `"use static"` route's
    /// `render()` returns one string; `await` copied out of the body into a
    /// non-async `function render()` is a syntax error, and a promise with no
    /// mount point is not markup. One rule for both modes, so it is `RA014` —
    /// the file parses, and the compiler declines the shape.
    #[test]
    fn an_async_route_component_is_ra014() {
        let alloc = Allocator::default();
        let src = "\"use island\"\nexport default async function P(props: Record<never, never>) { return <p>hi</p> }\n";
        let (info, codes) = analyze(&alloc, src);
        assert_eq!(codes, vec![Code::RA014], "{codes:?}");
        assert!(
            info.unwrap().is_async,
            "the summary must record the modifier"
        );

        let program = parse_tsx(&alloc, src).unwrap();
        let diagnostic = &validate_route_component(find_route_component(program).as_ref())[0];
        assert!(
            diagnostic.message.contains("synchronously"),
            "the message says both modes call the component synchronously: {:?}",
            diagnostic.message
        );
    }

    /// A generator route component is refused for the same reason: `yield` copied
    /// into a `function render()` that is not a generator is a syntax error.
    #[test]
    fn a_generator_route_component_is_ra014() {
        let alloc = Allocator::default();
        let src = "\"use island\"\nexport default function* P(props: Record<never, never>) { yield 1; return <p>hi</p> }\n";
        let (info, codes) = analyze(&alloc, src);
        assert_eq!(codes, vec![Code::RA014], "{codes:?}");
        assert!(
            info.unwrap().is_generator,
            "the summary must record the `*`"
        );

        let program = parse_tsx(&alloc, src).unwrap();
        let diagnostic = &validate_route_component(find_route_component(program).as_ref())[0];
        assert!(
            diagnostic.message.contains("synchronously"),
            "the message says both modes call the component synchronously: {:?}",
            diagnostic.message
        );
    }

    /// An `async` *arrow* route is the same shape in a different node: the modifier
    /// must be recorded there too, or the rule would accept
    /// `export default async (props: P) => <p/>` while refusing the function form.
    #[test]
    fn an_async_arrow_route_component_is_ra014() {
        let alloc = Allocator::default();
        let src =
            "\"use island\"\nexport default async (props: Record<never, never>) => <p>hi</p>\n";
        let (info, codes) = analyze(&alloc, src);
        assert_eq!(codes, vec![Code::RA014], "{codes:?}");
        assert!(info.unwrap().is_async);
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
                    && !diagnostic.message.contains("function default"),
                "{src}: illegal identifier in the text: {:?}",
                diagnostic.message,
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
                !diagnostic.message.contains("`Page`"),
                "{src}: {:?}",
                diagnostic.message
            );
        }
    }

    /// Every diagnostic uses the code's own skill file, so Task 15's generated
    /// docs and these paths cannot drift apart.
    #[test]
    fn every_diagnostic_points_at_the_skill_file() {
        for diagnostic in validate_route_component(None) {
            assert_eq!(diagnostic.doc, Code::RA011.doc_path());
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

    /// Every component in the file, exported or not — the wider question `RA006`
    /// asks. A `_shell.tsx` has no default export and still declares components.
    #[test]
    fn find_components_finds_every_jsx_function() {
        let alloc = Allocator::default();
        let src = "function Header(props: Props) { return <h1/> }\n\
                   export function Footer(props: Props) { return <f/> }\n\
                   const Badge = (props: Props) => <b/>\n\
                   export const Chip = function (props: Props) { return <c/> }\n\
                   function NotAComponent() { return 42 }\n\
                   export default function Page(props: Props) { return <p/> }\n";
        let program = parse_tsx(&alloc, src).unwrap();
        let names: Vec<&str> = find_components(program)
            .iter()
            .map(|component| component.name)
            .collect();
        assert_eq!(
            names,
            vec!["Header", "Footer", "Badge", "Chip", "Page"],
            "NotAComponent does not return JSX and is not a component"
        );
    }

    /// The default export is one of those components, and is marked as such —
    /// reaching it by name means the walk summarizes the declaration first, so a
    /// naive list would report it twice.
    #[test]
    fn find_components_marks_the_default_export_once() {
        for src in [
            "const AboutPage = (props: Props) => <p/>\nexport default AboutPage\n",
            "const AboutPage = (props: Props) => <p/>\nexport { AboutPage as default }\n",
            "export default function AboutPage(props: Props) { return <p/> }\n",
            "export default (props: Props) => <p/>\n",
        ] {
            let alloc = Allocator::default();
            let program = parse_tsx(&alloc, src).unwrap();
            let components = find_components(program);
            assert_eq!(components.len(), 1, "{src}: {components:?}");
            assert!(components[0].is_default_export, "{src}");
            assert!(components[0].returns_jsx, "{src}");
        }
    }

    /// A non-route component is not the default export, and a file with none has
    /// no marked component at all.
    #[test]
    fn find_components_does_not_mark_other_components_as_the_route() {
        let alloc = Allocator::default();
        let src = "export default function Page(props: Props) { return <p/> }\n\
                   function Badge(props: Props) { return <b/> }\n";
        let program = parse_tsx(&alloc, src).unwrap();
        let components = find_components(program);
        assert_eq!(components.len(), 2);
        assert_eq!(components[0].name, "Page");
        assert!(components[0].is_default_export);
        assert_eq!(components[1].name, "Badge");
        assert!(!components[1].is_default_export);
    }

    /// `signature_span` starts at the statement an agent edits and ends at the
    /// `{` of a block body, so slicing between it and `params_span` rebuilds the
    /// line to write rather than a fragment of it.
    #[test]
    fn signature_span_is_the_line_to_edit() {
        for (src, expected_prefix, params, expected) in [
            (
                "export default function Page(props) { return <p/> }\n",
                "export default function Page",
                "(props: Props)",
                "export default function Page(props: Props) {",
            ),
            (
                "function Badge(props) { return <b/> }\n",
                "function Badge",
                "(props: Props)",
                "function Badge(props: Props) {",
            ),
            (
                "const Badge = (props) => <b/>\n",
                "const Badge = ",
                "(props: Props)",
                "const Badge = (props: Props) => <b/>",
            ),
            (
                "const Badge = (props) => { return <b/> }\n",
                "const Badge = ",
                "(props: Props)",
                "const Badge = (props: Props) => {",
            ),
            (
                "export const Badge = () => <b/>\n",
                "export const Badge = ",
                "(props: Record<never, never>)",
                "export const Badge = (props: Record<never, never>) => <b/>",
            ),
        ] {
            let alloc = Allocator::default();
            let program = parse_tsx(&alloc, src).unwrap();
            let components = find_components(program);
            let component = components.first().expect("fixture declares a component");
            assert!(
                component
                    .signature(src, params)
                    .starts_with(expected_prefix),
                "{src}: {:?}",
                component.signature(src, params)
            );
            assert_eq!(
                component.signature(src, params),
                expected,
                "the rebuilt line must be the exact declaration to write"
            );
        }
    }

    /// The signature keeps a wrapping parenthesis, so the rebuilt line is still
    /// the expression the file has.
    #[test]
    fn a_parenthesized_signature_keeps_its_parentheses() {
        let alloc = Allocator::default();
        let src = "export default ((props) => <p/>)\n";
        let program = parse_tsx(&alloc, src).unwrap();
        let components = find_components(program);
        let component = components.first().unwrap();
        assert_eq!(
            component.signature(src, "(props: Props)"),
            "export default ((props: Props) => <p/>)"
        );
    }

    /// A propless component points `RA006` at its name; one with props points at
    /// the parameter list. Pointing at an empty `()` tells an agent nothing about
    /// which component is meant when a file has several.
    #[test]
    fn props_span_points_at_the_parameter_list_or_the_name() {
        let alloc = Allocator::default();
        let src = "export default function Page() { return <p/> }\n";
        let program = parse_tsx(&alloc, src).unwrap();
        let info = find_route_component(program).unwrap();
        assert_eq!(
            &src[info.props_span().0 as usize..info.props_span().1 as usize],
            "Page"
        );

        let alloc = Allocator::default();
        let src = "export default function Page(props) { return <p/> }\n";
        let program = parse_tsx(&alloc, src).unwrap();
        let info = find_route_component(program).unwrap();
        assert_eq!(
            &src[info.props_span().0 as usize..info.props_span().1 as usize],
            "(props)"
        );
    }
}
