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
//! it must accept no props, because the route owns its own render and has no
//! parent to supply anything.

use oxc_ast::ast::{
    ArrowFunctionBody, ExportDefaultDeclarationKind, Expression, Function, Program, Statement,
};
use oxc_span::GetSpan;

use crate::diagnostic::{Code, Diagnostic, Severity};

/// The shape of the route's default-exported component.
///
/// This is a summary, not the AST node itself: it records only the answers the
/// analyses in this milestone ask, so later tasks do not have to re-walk the
/// program to answer them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComponentInfo<'a> {
    /// The function's own name, or `"default"` when it is anonymous.
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
    /// blocks: `if (x) return <p/>` is not detected.
    pub returns_jsx: bool,
}

impl ComponentInfo<'_> {
    /// Whether this component may render as a route: at most one parameter and
    /// it returns JSX.
    fn is_route_component(&self) -> bool {
        self.params <= 1 && self.returns_jsx
    }
}

/// Find the route component in `program`: the default export, when it is a
/// function.
///
/// Returns `None` when the file has no default export, or when the default
/// export is not a function this milestone can analyze (a class, a component
/// imported and re-exported, a plain object). Both are `RA011` at the caller,
/// which is correct: a route whose default export is `export default {…}` has no
/// route component either.
pub fn find_route_component<'a>(program: &'a Program<'a>) -> Option<ComponentInfo<'a>> {
    program.body.iter().find_map(|statement| match statement {
        Statement::ExportDefaultDeclaration(declaration) => {
            component_from_default(&declaration.declaration)
        }
        _ => None,
    })
}

/// Summarize one `export default …` declaration, if it is a function.
fn component_from_default<'a>(
    declaration: &'a ExportDefaultDeclarationKind<'a>,
) -> Option<ComponentInfo<'a>> {
    match declaration {
        // `export default function Page(props) {…}`, the anonymous
        // `export default function (props) {…}` (the same node with no `id`),
        // and a function *expression*. Oxc has separate variants for the
        // declaration and expression forms, but they summarize identically.
        ExportDefaultDeclarationKind::FunctionDeclaration(function)
        | ExportDefaultDeclarationKind::FunctionExpression(function) => {
            Some(component_from_function(function))
        }
        // `export default (props) => <p/>`
        ExportDefaultDeclarationKind::ArrowFunctionExpression(arrow) => {
            Some(component_from_arrow(arrow))
        }
        // `export default (props => <p/>)` and `export default (function (…) {…})`:
        // parentheses are not part of the function, but they are part of the
        // AST shape the parser produces (verified on 0.159.0), so unwrap them
        // rather than calling a parenthesized component "missing".
        ExportDefaultDeclarationKind::ParenthesizedExpression(parenthesized) => {
            match &parenthesized.expression {
                Expression::ArrowFunctionExpression(arrow) => Some(component_from_arrow(arrow)),
                Expression::FunctionExpression(function) => Some(component_from_function(function)),
                _ => None,
            }
        }
        _ => None,
    }
}

/// Summarize an arrow function, whose body may be a block or a single
/// expression.
fn component_from_arrow<'a>(
    arrow: &'a oxc_ast::ast::ArrowFunctionExpression<'a>,
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
        name: "default",
        params: param_count(&arrow.params),
        has_props_type: has_single_annotated_param(&arrow.params),
        body_span: span_of(arrow.body.span()),
        returns_jsx,
    }
}

/// Summarize a [`Function`] node (declaration or expression).
fn component_from_function<'a>(function: &'a Function<'a>) -> ComponentInfo<'a> {
    let body = function.body.as_deref();

    ComponentInfo {
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

/// A [`Span`] as the `(start, end)` pair every diagnostic carries.
fn span_of(span: oxc_span::Span) -> (u32, u32) {
    (span.start, span.end)
}

/// Validate the route component's export shape.
///
/// Three separate questions, one code, because they have one repair shape —
/// "make the default export a component":
///
/// - there is no default export at all,
/// - the default export takes more than the no props a route may take,
/// - the default export does not return JSX.
pub fn validate_route_component(info: Option<&ComponentInfo>) -> Vec<Diagnostic> {
    match info {
        None => vec![missing_component()],
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
            "route component `{name}` takes {params} parameters, but a route takes no props",
            name = info.name,
            params = info.params,
        ),
        explain: "A route is rendered by the router, not by a parent component, so nothing can \
                  supply props. Data a route needs must be read inside the route rather than \
                  passed into it."
            .into(),
        fix: format!(
            "Drop the extra parameters so `{name}` takes at most one, annotated \
             `props: Record<never, never>`.",
            name = info.name,
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
        message: format!(
            "route component `{name}` does not return JSX",
            name = info.name
        ),
        explain: "A component is a function that returns JSX. Returning a value, a conditional, \
                  or calling a renderer is not a component in this milestone, and the compiler \
                  does not guess where a returned value's JSX comes from."
            .into(),
        fix: format!(
            "Return JSX directly from `{name}`, e.g. `return <main>...</main>`, so the router \
             has markup to render.",
            name = info.name,
        ),
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

    /// A default export that is not a function is not a route component.
    #[test]
    fn non_function_default_export_is_none() {
        let alloc = Allocator::default();
        let (info, codes) = analyze(&alloc, "export default 42\n");
        assert!(info.is_none());
        assert_eq!(codes, vec![Code::RA011]);

        let alloc = Allocator::default();
        let (info, _) = analyze(&alloc, "export default class Page {}\n");
        assert!(info.is_none());
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
        let (info, _) = analyze(&alloc, src);
        assert!(info.is_none());
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

    /// `returns_jsx` is syntactic: a returned identifier, conditional, call, or
    /// no return at all is not JSX in this milestone, and pretending otherwise
    /// would be a guess.
    #[test]
    fn returning_a_non_jsx_value_is_ra011() {
        for src in [
            "export default function Page(props: Props) { return View }\n",
            "export default function Page(props: Props) { return ready ? <p/> : <i/> }\n",
            "export default function Page(props: Props) { return render() }\n",
            "export default function Page(props: Props) {}\n",
        ] {
            let alloc = Allocator::default();
            let (info, codes) = analyze(&alloc, src);
            assert!(!info.unwrap().returns_jsx, "{src}");
            assert_eq!(codes, vec![Code::RA011], "{src}");
        }
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
