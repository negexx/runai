//! The `"use static"` emitter: one self-contained ES module per route.
//!
//! A `"use static"` route ships **zero bytes of JavaScript**, so the artifact is
//! not a component — it is a module whose `render()` returns the page's HTML:
//!
//! ```js
//! const _esc = (v) => String(v).replace(/[&<>"']/g, (c) => ({ "&": "&amp;", … })[c])
//! const _attr = (name, v) => (v == null || v === false ? "" : ` ${name}="${v === true ? "" : _esc(v)}"`)
//! // module statements, verbatim
//! export function render() {
//!   // body statements, verbatim
//!   return `<p>${_esc(name)}</p>`
//! }
//! ```
//!
//! # No imports at all
//!
//! The emitted module imports **nothing**, not even `"runai"`. That is not a
//! stylistic choice: a static route has no runtime on the page, so there is
//! nothing to import, and [`crate::analyze::entry::analyze`] refuses a `"runai"`
//! name in a static route for exactly this reason. A test asserts the substring
//! `import` appears nowhere in the output, comments included.
//!
//! # The four regions, and why each is written
//!
//! [`RouteIr`] carries four things a module needs, and dropping any one is the
//! accept-then-reject failure this emitter exists to close:
//!
//! - [`RouteIr::module_statements`] — written at module scope, above `render()`;
//! - the escaping helpers — [`PRERENDER_NAMES`] — written between the module
//!   level and `render()`, so a route's own statements can use them;
//! - [`RouteIr::statements`] — written inside `render()`, before the template;
//! - [`RouteIr::tree`] — written as one template literal, the function's return.
//!
//! # `_attr` is why a dynamic attribute survives
//!
//! A template literal is one string, so an attribute cannot be omitted
//! conditionally inside it — the whole ` name="value"` segment has to come from a
//! function. [`_attr`] reproduces `_setAttr`'s documented policy (`null`,
//! `undefined`, and `false` write no attribute; `true` writes the bare attribute;
//! `0` and `""` are real values), so one source renders the same attribute in
//! both modes.
//!
//! # Verbatim is a requirement, not an accident
//!
//! [`RouteIr::statements`] and [`RouteIr::module_statements`] are already
//! type-stripped JavaScript. They are copied **byte for byte**: re-indenting them
//! would misplace a `//` comment's end, and reformatting them would re-print a
//! file the agent did not ask to change.

use crate::ir::{Binding, IrNode, RouteIr, VOID_ELEMENTS};

/// The `_esc` helper, exactly as emitted.
///
/// `String(v)` is load-bearing: a child slot is any expression, and a number or a
/// `null` has no `.replace`. Escaping runs at **render** time rather than at build
/// time because the value is only known then — [`crate::analyze::jsx::check_jsx`]
/// has already refused JSX in a slot, so `<b/>` in an expression cannot become
/// markup by passing through here; it becomes the text `<b/>`.
const ESCAPE_TABLE: &str =
    r#"(c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]"#;

/// The whole module, as text.
///
/// `pub(crate)` on purpose: a caller outside this crate cannot lower without
/// gating on [`crate::analyze::entry::analyze`] first, so the only public way to
/// reach an artifact is [`crate::codegen::emit_prerender_checked`]. The naming is
/// the invariant — a `pub` emitter name that does not end in `_checked` would be
/// a path from a source file to an artifact with no rule pass in between.
pub(crate) fn emit_prerender(route: &RouteIr) -> String {
    let mut out = String::new();

    out.push_str("const _esc = (v) => String(v).replace(/[&<>\"']/g, ");
    out.push_str(ESCAPE_TABLE);
    out.push_str(")\n");

    // The whole ` name="value"` segment comes from one function, because a
    // template literal cannot omit an attribute conditionally. The policy is
    // `_setAttr`'s: `null`, `undefined`, and `false` write nothing, `true` writes
    // the bare attribute, and `0` and `""` are real values.
    out.push_str(
        "const _attr = (name, v) => (v == null || v === false ? \"\" : ` ${name}=\"${v === true ? \"\" : _esc(v)}\"`)\n",
    );

    // Module scope, above the helpers' user so a module-level statement can call
    // them, and above `render()` so a `const` a body statement reads is
    // initialized first.
    if !route.module_statements.is_empty() {
        out.push_str(&route.module_statements);
        out.push('\n');
    }

    out.push_str("export function render() {\n");
    if !route.statements.is_empty() {
        out.push_str(&route.statements);
        out.push('\n');
    }
    out.push_str("  return `");
    out.push_str(&template_literal_body(&route.tree));
    out.push_str("`\n");
    out.push_str("}\n");

    out
}

/// The HTML, as the **body** of a JS template literal.
///
/// The caller wraps this in backticks; this function never emits the delimiters,
/// which is what lets a test compare the markup directly.
///
/// Three rules, and each is one place a naive copy would produce a broken module:
///
/// - **Static attributes** are written inline as ` name="value"`, quoted with
///   `"`. A literal `"` in the value becomes `&quot;`, so the quoting survives a
///   value the source wrote in single quotes. A bare attribute
///   (`<input disabled />`, lowered to `("disabled", "")`) is written bare, never
///   `disabled=""`.
/// - **Every literal chunk** — text nodes *and* static attribute values — has
///   `` ` ``, `\`, and `${` escaped, because both are characters of the one
///   template literal's body. Escaping text but not an attribute leaves `${x}` in
///   an attribute interpolating: a `ReferenceError` at build time, or a silently
///   wrong attribute, with no diagnostic.
/// - **Entities are never decoded or re-encoded.** `&amp;` in the source stays
///   `&amp;` in the output; decoding it and letting `_esc` re-encode it is how a
///   compiler writes `&amp;amp;` and the page shows a literal `&amp;`.
///
/// A void element (`img`, `br`, …) gets no closing tag. An [`IrNode::Element`]
/// whose tag is void and whose children are non-empty cannot occur:
/// [`crate::analyze::jsx::check_jsx`] refuses it (`RA014`) before lowering, and
/// [`lower_route`](crate::ir::lower_route) assumes the analysis passed. The arm
/// below writes the opening tag and then the children, which is what "unreachable"
/// means here — the `debug_assert!` makes a missed rule loud in a debug build
/// rather than silently producing `<br>x`, but no release path drops a node.
pub fn template_literal_body(node: &IrNode) -> String {
    let mut out = String::new();
    write_node(node, &mut out);
    out
}

/// [`template_literal_body`] for one node, appending to `out`.
fn write_node(node: &IrNode, out: &mut String) {
    match node {
        IrNode::Text(text) => escape_literal(text, out),
        IrNode::Expr(expr) => {
            out.push_str("${_esc(");
            out.push_str(expr);
            out.push_str(")}");
        }
        IrNode::Element {
            tag,
            static_attrs,
            bindings,
            children,
        } => {
            out.push('<');
            out.push_str(tag);

            for (name, value) in static_attrs {
                out.push(' ');
                out.push_str(name);
                // A bare attribute has no value at all: `<input disabled>`, never
                // `disabled=""`, because an empty value is a real value.
                if value.is_empty() {
                    continue;
                }
                out.push_str("=\"");
                // The quoting is `"`, so a `"` in the value has to be encoded.
                // Entities are left alone: this is the value the author wrote.
                escape_attribute_value(value, out);
                out.push('"');
            }

            for binding in bindings {
                match binding {
                    Binding::Attr { name, expr } => {
                        out.push_str("${_attr(\"");
                        out.push_str(name);
                        out.push_str("\", ");
                        out.push_str(expr);
                        out.push_str(")}");
                    }
                    // A `"use static"` route cannot carry a handler: `RA010`
                    // refuses every `on*` handler, so this arm is unreachable
                    // through the checked emitter. There is no HTML spelling of
                    // `onClick={fn}` that keeps the expression, and dropping it
                    // silently is what this comment exists to prevent — so a
                    // debug build panics, and a release build writes nothing for
                    // a construct the analysis already refused.
                    Binding::Event { .. } => {
                        debug_assert!(
                            false,
                            "an event binding reached the prerender emitter: RA010 refuses a \
                             handler in a `\"use static\"` route, so analysis should have \
                             stopped this file"
                        );
                    }
                }
            }

            out.push('>');

            let void = VOID_ELEMENTS.contains(&tag.as_str());
            if void {
                debug_assert!(
                    children.is_empty(),
                    "a void element with children reached the emitter: check_jsx refuses it (RA014)"
                );
            }

            for child in children {
                write_node(child, out);
            }

            if !void {
                out.push_str("</");
                out.push_str(tag);
                out.push('>');
            }
        }
    }
}

/// Append `text` to `out` with the template literal's own characters escaped.
///
/// Three characters matter: `\` and `` ` `` because they delimit and escape the
/// literal, and the `${` pair because it starts an interpolation. A lone `$` is
/// ordinary text, so it is left alone; escaping it would be valid but would make
/// the output differ from the source for no gain.
fn escape_literal(text: &str, out: &mut String) {
    let mut characters = text.chars().peekable();
    while let Some(character) = characters.next() {
        match character {
            '\\' => out.push_str("\\\\"),
            '`' => out.push_str("\\`"),
            '$' if characters.peek() == Some(&'{') => {
                out.push_str("\\$");
                // The `{` is consumed on the next iteration and pushed as-is.
            }
            other => out.push(other),
        }
    }
}

/// Append a static attribute value, with `"` encoded and the template literal's
/// characters escaped.
///
/// `&` is deliberately **not** touched: the source's `&amp;` is an entity the
/// author wrote, and re-encoding it would render `&amp;amp;`. The `$` + `{` pair
/// is escaped for the same reason it is in text — the value is part of the same
/// template literal, and an unescaped pair would interpolate a name that does not
/// exist in the emitted module.
fn escape_attribute_value(value: &str, out: &mut String) {
    let mut characters = value.chars().peekable();
    while let Some(character) = characters.next() {
        match character {
            '"' => out.push_str("&quot;"),
            '\\' => out.push_str("\\\\"),
            '`' => out.push_str("\\`"),
            '$' if characters.peek() == Some(&'{') => out.push_str("\\$"),
            other => out.push(other),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::lower_route;

    /// Every emitted module must parse as ESM. A test that only greps for a
    /// substring cannot tell an artifact from a syntax error.
    fn assert_parses(js: &str) {
        let alloc = oxc_allocator::Allocator::default();
        let ret = oxc_parser::Parser::new(&alloc, js, oxc_span::SourceType::mjs()).parse();
        assert!(
            ret.diagnostics.is_empty(),
            "emitted code must parse: {js}\n{:?}",
            ret.diagnostics
        );
    }

    #[test]
    fn emits_no_imports_for_static_route() {
        let src =
            "\"use static\"\nexport default function P() { return <div class=\"a\">hi</div> }\n";
        let js = emit_prerender(&lower_route(src).unwrap());
        assert!(
            !js.contains("import"),
            "static output must not import: {js}"
        );
        assert!(js.contains("export function render()"));
        assert_parses(&js);
    }

    fn body(src: &str) -> String {
        template_literal_body(&lower_route(src).unwrap().tree)
    }

    #[test]
    fn static_attrs_and_text_survive() {
        let src = "\"use static\"\nexport default function P() { return <div class=\"a\" id=\"b\">hi</div> }\n";
        assert_eq!(body(src), "<div class=\"a\" id=\"b\">hi</div>");
    }

    #[test]
    fn entities_are_not_reencoded() {
        let html =
            body("\"use static\"\nexport default function P() { return <p>a &amp; b</p> }\n");
        assert!(html.contains("a &amp; b"), "got {html}");
        assert!(!html.contains("&amp;amp;"));
    }

    #[test]
    fn self_closing_void_tags_are_valid_html() {
        let src = "\"use static\"\nexport default function P() { return <img src=\"/a.png\"/> }\n";
        assert_eq!(body(src), "<img src=\"/a.png\">");
    }

    #[test]
    fn a_bare_attribute_is_written_bare() {
        let src = "\"use static\"\nexport default function P() { return <input disabled/> }\n";
        assert_eq!(body(src), "<input disabled>");
    }

    #[test]
    fn a_double_quote_in_a_single_quoted_attribute_is_escaped() {
        let src =
            "\"use static\"\nexport default function P() { return <p title='say \"hi\"'>x</p> }\n";
        assert_eq!(body(src), "<p title=\"say &quot;hi&quot;\">x</p>");
    }

    #[test]
    fn expressions_are_escaped_at_render_time() {
        let src = "\"use static\"\nexport default function P() { const name = \"<Ada>\"; return <p>{name}</p> }\n";
        let js = emit_prerender(&lower_route(src).unwrap());
        assert!(js.contains("${_esc(name)}"), "{js}");
        assert!(js.contains("const name = \"<Ada>\""), "{js}");
        assert_parses(&js);
    }

    #[test]
    fn a_dynamic_attribute_survives_as_an_attribute() {
        let src = "\"use static\"\nexport default function P() { const cls = \"a\"; return <p class={cls}>hi</p> }\n";
        let html = body(src);
        assert_eq!(html, "<p${_attr(\"class\", cls)}>hi</p>");
        let js = emit_prerender(&lower_route(src).unwrap());
        assert!(js.contains("const _attr ="), "{js}");
        assert_parses(&js);
    }

    #[test]
    fn module_statements_are_written_above_render_and_body_statements_inside() {
        let src = "\"use static\"\nconst title = \"About\"\nexport default function P() { const upper = title; return <h1>{upper}</h1> }\n";
        let js = emit_prerender(&lower_route(src).unwrap());
        let module = js
            .find("const title = \"About\"")
            .expect("module statement written");
        let render = js.find("export function render()").expect("render written");
        let inner = js
            .find("const upper = title")
            .expect("body statement written");
        assert!(module < render, "module statements go above render(): {js}");
        assert!(render < inner, "body statements go inside render(): {js}");
        assert_parses(&js);
    }

    #[test]
    fn a_backtick_or_backslash_in_text_cannot_break_the_template_literal() {
        // A `$` followed by `{` cannot occur in JSX *text* (the `{` starts an
        // expression), but a backtick and a backslash can, and both are literal
        // characters of the template literal's body.
        let src = "\"use static\"\nexport default function P() { return <p>a ` b \\ c</p> }\n";
        let js = emit_prerender(&lower_route(src).unwrap());
        assert_parses(&js);
        assert!(js.contains("a \\` b \\\\ c"), "both escaped: {js}");
    }

    #[test]
    fn a_placeholder_in_a_static_attribute_is_escaped_not_interpolated() {
        // A `$` + `{` pair IS reachable in a quoted attribute value, and a raw copy would
        // interpolate `x` when the template literal is evaluated — a
        // ReferenceError at build time, or a silently wrong attribute, with no
        // diagnostic. Attribute values are literal text too.
        let src =
            "\"use static\"\nexport default function P() { return <p title=\"${x}\">hi</p> }\n";
        let js = emit_prerender(&lower_route(src).unwrap());
        assert_parses(&js);
        assert!(
            js.contains("\\${x}"),
            "the placeholder must be escaped: {js}"
        );
    }

    #[test]
    fn the_declared_helpers_are_exactly_the_reserved_names() {
        // The emitter and the refusal rule share one list, so a helper added
        // without a rule (or a rule for a helper that no longer exists) fails
        // here rather than shipping a shadowing hole.
        let js = emit_prerender(
            &lower_route("\"use static\"\nexport default function P() { return <p>hi</p> }\n")
                .unwrap(),
        );
        for name in crate::ir::PRERENDER_NAMES {
            assert!(
                js.contains(name),
                "reserved name `{name}` is not emitted: {js}"
            );
        }
        for helper in ["_esc", "_attr", "render"] {
            assert!(
                crate::ir::PRERENDER_NAMES.contains(&helper),
                "`{helper}` is emitted but not reserved"
            );
        }
    }
}
