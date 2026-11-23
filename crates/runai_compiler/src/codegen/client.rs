//! The `"use island"` emitter: one ES module per route, mounted into `#app`.
//!
//! A `"use island"` route ships JavaScript: the module imports the runtime from a
//! URL, builds its markup **once** from a hoisted `<template>`, wires the signal
//! bindings, and mounts the result into `#app`.
//!
//! ```js
//! import { _on, _setAttr, _setText, mount, signal, template } from "/assets/runtime.js"
//! const _c = (root) => { … }
//! const _t0 = template(`<p class="a"><!></p>`)
//! function Page(props) {
//!   const n = signal(0)
//!   const _el = _t0.clone()
//!   const _a = _c(_el)
//!   _setAttr(_a[0].parentNode, "class", () => n() > 0 ? "on" : "off")
//!   _setText(_a[1], () => String(n()))
//!   _on(_a[2].parentNode, "click", () => n(n() + 1))
//!   return _el
//! }
//! mount(Page, document.getElementById("app"))
//! ```
//!
//! # Bindings are addressed by marker ordinal, never by an index path
//!
//! The template is HTML **text**, so the browser's parser rebuilds the tree — and
//! it does not always rebuild the tree the IR describes. It closes a `<p>` at a
//! block child (`<p><div>` → two siblings), inserts a `<tbody>` under a `<table>`,
//! fosters a `<div>` out of a `<table>`, and splits nested anchors. An index path
//! like `el.firstChild.firstChild` then addresses the wrong node, or `undefined`,
//! **silently**. So the emitter writes a `<!>` marker for every binding site and
//! resolves them by document-order ordinal: the k-th binding the emitter wires is
//! the k-th comment in the cloned fragment, read by the `_c` TreeWalker. A comment
//! is the one node the parser never renames, re-parents, or closes early.
//!
//! [Step 0]: [`crate::analyze::jsx::reparse_refusal`] — the analyzer refuses the
//! markup shapes whose *element* tree the parser relocates, so a marker only ever
//! has to survive comment-preserving edits, which it does.
//!
//! # The ordinal contract, and where each marker's target comes from
//!
//! [`template_html`] writes one `<!>` per [`IrNode::Expr`] child and per
//! [`Binding`] (an `Attr` or an `Event`), and [`sites`] walks the same tree in the
//! same order — so the k-th wiring line reads `_a[k]`. A text marker is addressed
//! directly (`_setText(_a[k], …)` replaces the comment with a text node). An
//! attribute or event has no position among **children**, so its marker is the
//! element's **first child**: `_a[k].parentNode` is then the element itself. A
//! **void** element (`<img>`, `<input>`) has no children at all, so its markers are
//! the element's following siblings and the target is `_a[k].previousElementSibling`
//! — the void element the marker was written for. See [`Target`].
//!
//! # The import list is an input, not something this module computes
//!
//! [`emit_client`] sees only the [`RouteIr`], and the IR cannot see an import that
//! only a **module statement** uses (`const start = signal(5); effect(…)` with no
//! markup use of `effect`). So the caller computes the merged import list from
//! `runai_imports(program)` (every named specifier the file declares) plus the
//! helpers the artifact needs, and passes it in: [`crate::codegen::client_imports`].
//!
//! # Verbatim is a requirement
//!
//! [`RouteIr::statements`] and [`RouteIr::module_statements`] are already
//! type-stripped JavaScript. They are copied **byte for byte**, for the same reason
//! the prerender emitter copies them: re-indenting would misplace a `//` comment's
//! end, and reformatting would re-print a file the agent did not ask to change.

use crate::codegen::prerender::{TemplateSites, write_node};
use crate::ir::{Binding, IrNode, RouteIr, VOID_ELEMENTS};

/// The whole island module, as text.
///
/// `pub(crate)` on purpose: a caller outside this crate cannot lower without
/// gating on [`crate::analyze::entry::analyze`] first, so the only public way to
/// reach an artifact is [`crate::codegen::emit_client_checked`]. The naming is the
/// invariant — a `pub` emitter name that does not end in `_checked` would be a
/// path from a source file to an artifact with no rule pass in between.
pub(crate) fn emit_client(route: &RouteIr, runtime_url: &str, imports: &[String]) -> String {
    let sites = sites(&route.tree);

    let mut out = String::new();

    // 1. The one import, from the runtime URL. `imports` is already merged, sorted,
    //    and deduped by the caller; a specifier keeps its alias (`owner as who`)
    //    because the runtime exports the *imported* name and the code calls the
    //    *local* one.
    out.push_str("import { ");
    out.push_str(&imports.join(", "));
    out.push_str(" } from \"");
    out.push_str(runtime_url);
    out.push_str("\"\n");

    // 2. The marker collector, emitted only when there is a marker to collect. Three
    //    lines, and it belongs to the artifact that needs it rather than to the
    //    runtime: a page with no bindings should not carry it.
    if !sites.is_empty() {
        // `128` is `NodeFilter.SHOW_COMMENT`, spelled as its number because the
        // emitted module must not depend on a `NodeFilter` global being installed.
        // `createTreeWalker(root, 128)` walks `root`'s subtree in document order,
        // so the k-th comment pushed is the k-th marker in the template.
        out.push_str(
            "const _c = (root) => { const w = document.createTreeWalker(root, 128), a = []; \
             for (let n = w.nextNode(); n; n = w.nextNode()) a.push(n); return a }\n",
        );
    }

    // 3. The hoisted template. One template this milestone, named `_t0`. The body is
    //    the shared writer's output (markers included), inside a template literal so
    //    its `"`-quoted attributes need no second escaping — the writer already
    //    escapes the literal's own `` ` ``, `\`, and `${`.
    out.push_str("const _t0 = template(`");
    out.push_str(&template_html(&route.tree));
    out.push_str("`)\n");

    // 4. Module-level statements, verbatim, above the component so a module-level
    //    `const` a body statement reads is initialized first.
    if !route.module_statements.is_empty() {
        out.push_str(&route.module_statements);
        out.push('\n');
    }

    // 5. The component.
    out.push_str("function ");
    out.push_str(&route.name);
    out.push_str("(props) {\n");
    if !route.statements.is_empty() {
        out.push_str(&route.statements);
        out.push('\n');
    }
    out.push_str("  const _el = _t0.clone()\n");
    if !sites.is_empty() {
        out.push_str("  const _a = _c(_el)\n");
    }
    for (index, site) in sites.iter().enumerate() {
        write_site(index, site, &mut out);
    }
    out.push_str("  return _el\n");
    out.push_str("}\n");

    // 6. Mount into `#app`.
    out.push_str("mount(");
    out.push_str(&route.name);
    out.push_str(", document.getElementById(\"app\"))\n");

    out
}

/// The HTML the hoisted template holds: the element/text/static-attribute markup
/// the prerender writer produces, with a `<!>` marker at every binding site.
///
/// The markers appear in the **same document order** [`sites`] produces, which is
/// the ordering contract the emitter rests on. An element's attribute/event markers
/// are written as its first children (in binding order) before any child marker; a
/// void element's markers follow it, because it has no children to hold them.
pub fn template_html(node: &IrNode) -> String {
    let mut out = String::new();
    write_node::<IslandSites>(node, &mut out);
    out
}

/// [`TemplateSites`] for the island template: every dynamic site is a `<!>` marker.
///
/// The element, text, and static-attribute writing is [`write_node`]'s, shared with
/// the prerender emitter so the two cannot drift on entity handling, void elements,
/// or the whitespace rule.
struct IslandSites;

impl TemplateSites for IslandSites {
    fn expr(_expr: &str, out: &mut String) {
        out.push_str("<!>");
    }

    // An attribute is not a child, so its marker is written after `>` — as the
    // element's first children — rather than inside the start tag.
    fn attrs_in_tag() -> bool {
        false
    }

    fn attr(_name: &str, _expr: &str, out: &mut String) {
        // Never called: `attrs_in_tag()` is `false`, so `write_node` writes a
        // `marker` per binding instead. Delegating keeps the two paths identical if
        // that ever changes.
        out.push_str("<!>");
    }

    fn event(_name: &str, _expr: &str, out: &mut String) {
        out.push_str("<!>");
    }

    fn marker(out: &mut String) {
        out.push_str("<!>");
    }
}

/// What a marker stands for, and how the emitter reaches the node it wires.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Target {
    /// A non-void element: the marker is its first child, so `parentNode` is the
    /// element the attribute or listener belongs to.
    Parent,
    /// A void element: it has no children, so the marker is its following sibling
    /// and `previousElementSibling` is the element.
    PreviousElement,
}

/// One binding site, in the order [`template_html`] writes its marker.
#[derive(Debug, Clone, PartialEq, Eq)]
enum SiteKind<'a> {
    /// An [`IrNode::Expr`] child: `_setText(_a[k], () => expr)`.
    Text(&'a str),
    /// A dynamic attribute: `_setAttr(_a[k].target, "name", value)`.
    Attr { name: &'a str, expr: &'a str },
    /// An event listener: `_on(_a[k].target, "name", handler)`.
    Event { name: &'a str, expr: &'a str },
}

/// A binding site plus the resolution its marker needs.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Site<'a> {
    kind: SiteKind<'a>,
    target: Target,
}

/// Every binding site of `node`, in the document order the template writes them.
///
/// The walk mirrors [`crate::codegen::prerender::write_node`] exactly — an
/// element's own `Attr`/`Event` bindings first, in binding order, then its children
/// in order — so the k-th site is the k-th marker. A test asserts the marker count
/// equals this list's length, which is the one invariant the whole design rests on.
fn sites(node: &IrNode) -> Vec<Site<'_>> {
    let mut found = Vec::new();
    collect_sites(node, &mut found);
    found
}

/// The runtime helpers this tree's emitted module calls, besides `mount` and
/// `template` (which every island imports).
///
/// `_setText` for a text slot, `_setAttr` for a dynamic attribute, `_on` for a
/// listener. The caller merges these into the import list, so a page that uses no
/// listener does not import `_on`, and the reserved-name set (which includes the
/// merged list) does not refuse a name it never imports.
pub(crate) fn used_helpers(node: &IrNode) -> Vec<&'static str> {
    let mut helpers = Vec::new();
    collect_helpers(node, &mut helpers);
    helpers.sort_unstable();
    helpers.dedup();
    helpers
}

fn collect_helpers(node: &IrNode, out: &mut Vec<&'static str>) {
    match node {
        IrNode::Text(_) => {}
        IrNode::Expr(_) => out.push("_setText"),
        IrNode::Element {
            bindings, children, ..
        } => {
            for binding in bindings {
                match binding {
                    Binding::Attr { .. } => out.push("_setAttr"),
                    Binding::Event { .. } => out.push("_on"),
                }
            }
            for child in children {
                collect_helpers(child, out);
            }
        }
    }
}

fn collect_sites<'a>(node: &'a IrNode, out: &mut Vec<Site<'a>>) {
    match node {
        IrNode::Text(_) => {}
        IrNode::Expr(expr) => out.push(Site {
            kind: SiteKind::Text(expr),
            // A text marker is always a child of a non-void element (a void element
            // has no children, so an `Expr` cannot be one), so `Parent` is right; the
            // text path does not read `target` anyway.
            target: Target::Parent,
        }),
        IrNode::Element {
            tag,
            bindings,
            children,
            ..
        } => {
            let target = if VOID_ELEMENTS.contains(&tag.as_str()) {
                Target::PreviousElement
            } else {
                Target::Parent
            };
            for binding in bindings {
                let kind = match binding {
                    Binding::Attr { name, expr } => SiteKind::Attr { name, expr },
                    Binding::Event { name, expr } => SiteKind::Event { name, expr },
                };
                out.push(Site {
                    kind,
                    target: target.clone(),
                });
            }
            for child in children {
                collect_sites(child, out);
            }
        }
    }
}

/// One wiring line, `_a[index]` addressed by ordinal.
fn write_site(index: usize, site: &Site<'_>, out: &mut String) {
    match &site.kind {
        SiteKind::Text(expr) => {
            out.push_str("  _setText(_a[");
            out.push_str(&index.to_string());
            out.push_str("], () => ");
            out.push_str(expr);
            out.push_str(")\n");
        }
        SiteKind::Attr { name, expr } => {
            out.push_str("  _setAttr(_a[");
            out.push_str(&index.to_string());
            out.push_str(target_suffix(&site.target));
            out.push_str(", \"");
            out.push_str(name);
            out.push_str("\", ");
            out.push_str(&attr_value(expr));
            out.push_str(")\n");
        }
        SiteKind::Event { name, expr } => {
            out.push_str("  _on(_a[");
            out.push_str(&index.to_string());
            out.push_str(target_suffix(&site.target));
            out.push_str(", \"");
            out.push_str(name);
            out.push_str("\", ");
            out.push_str(expr);
            out.push_str(")\n");
        }
    }
}

/// The `.target` a site's marker is read through.
fn target_suffix(target: &Target) -> &'static str {
    match target {
        Target::Parent => "].parentNode",
        Target::PreviousElement => "].previousElementSibling",
    }
}

/// The value form `_setAttr` receives for a dynamic attribute.
///
/// An expression that **evaluates now** — `n() > 0 ? "on" : "off"`, `String(n())` —
/// is wrapped in `() => …` so the runtime's `_setAttr` subscribes and re-runs it.
/// An expression that **is** the value — a literal (`"a"`, `0`, `true`) — is passed
/// unwrapped, so a constant stays a constant and a string literal is not turned
/// into a function. A bare signal accessor (`class={tone}`) is left unwrapped too,
/// and is still tracked: `_setAttr` calls a function value inside its effect.
///
/// The test for "evaluates now" is a `(` in the expression: it means a call, so the
/// expression produces a value this instant and must be deferred to track its
/// dependencies. A literal, an identifier, or a member access has no `(`, is a
/// value (or a function the runtime itself calls), and needs no wrapper.
fn attr_value(expr: &str) -> String {
    let trimmed = expr.trim();
    if trimmed.contains('(') || trimmed.contains('`') {
        format!("() => {trimmed}")
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codegen::emit_client_checked;
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

    /// The import list for a route, computed the way the checked wrapper does it.
    fn imports_for(src: &str) -> Vec<String> {
        crate::codegen::client_imports(src).expect("analysis accepted")
    }

    #[test]
    fn imports_only_the_runtime_url_in_one_statement() {
        let src = "\"use island\"\nimport { signal } from \"runai\"\nexport default function P(props: Record<never, never>) { const n = signal(0); return <p>{n()}</p> }\n";
        let js = emit_client(
            &lower_route(src).unwrap(),
            "/assets/runtime.js",
            &imports_for(src),
        );
        assert!(js.contains("from \"/assets/runtime.js\""));
        assert!(!js.contains("\"runai\""), "{js}");
        assert_eq!(js.matches("import ").count(), 1, "{js}");
        assert!(js.contains("signal"), "{js}");
        assert_parses(&js);
    }

    #[test]
    fn mounts_into_app() {
        let src = "\"use island\"\nexport default function (props: Record<never, never>) { return <p>hi</p> }\n";
        let js = emit_client(
            &lower_route(src).unwrap(),
            "/assets/runtime.js",
            &imports_for(src),
        );
        assert!(
            js.contains("mount(Page, document.getElementById(\"app\"))"),
            "{js}"
        );
    }

    #[test]
    fn template_is_hoisted_to_module_scope() {
        let src = "\"use island\"\nexport default function (props: Record<never, never>) { return <p>hi</p> }\n";
        let js = emit_client(
            &lower_route(src).unwrap(),
            "/assets/runtime.js",
            &imports_for(src),
        );
        let t = js.find("const _t0 = template(").expect("hoisted template");
        assert!(t < js.find("function Page").unwrap(), "{js}");
    }

    #[test]
    fn a_nested_text_binding_addresses_the_second_marker() {
        let src = "\"use island\"\nimport { signal } from \"runai\"\nexport default function P(props: Record<never, never>) { const n = signal(0); return <div><span>{n()}</span></div> }\n";
        let js = emit_client(
            &lower_route(src).unwrap(),
            "/assets/runtime.js",
            &imports_for(src),
        );
        // No index paths: the marker ordinal is the contract.
        assert!(!js.contains(".firstChild"), "{js}");
        assert!(js.contains("_setText(_a[0]"), "{js}");
        assert_parses(&js);
    }

    #[test]
    fn static_text_is_in_the_template_not_the_runtime() {
        let src = "\"use island\"\nexport default function P(props: Record<never, never>) { return <p>hello</p> }\n";
        let js = emit_client(
            &lower_route(src).unwrap(),
            "/assets/runtime.js",
            &imports_for(src),
        );
        assert!(js.contains("hello"));
        assert!(!js.contains("_setText"));
        assert!(
            !js.contains("_c("),
            "no markers means no collector call: {js}"
        );
    }

    #[test]
    fn event_handler_is_wired_via_on_with_a_marker_ordinal() {
        let src = "\"use island\"\nexport default function P(props: Record<never, never>) { return <button onClick={go}>x</button> }\n";
        let js = emit_client(
            &lower_route(src).unwrap(),
            "/assets/runtime.js",
            &imports_for(src),
        );
        assert!(js.contains("_on("));
        assert!(js.contains("\"click\""));
        assert!(js.contains("_a[0]"));
    }

    #[test]
    fn a_dynamic_attribute_targets_the_markers_parent() {
        let src = "\"use island\"\nimport { signal } from \"runai\"\nexport default function P(props: Record<never, never>) { const n = signal(0); return <p class={n() > 0 ? \"on\" : \"off\"}>x</p> }\n";
        let js = emit_client(
            &lower_route(src).unwrap(),
            "/assets/runtime.js",
            &imports_for(src),
        );
        assert!(js.contains("_setAttr(_a[0].parentNode, \"class\""), "{js}");
    }

    #[test]
    fn bindings_in_ir_order_map_to_ascending_markers() {
        // The one contract the whole design rests on: the k-th binding the emitter
        // writes is the k-th marker in the template. Attr before child text inside
        // one element, then the next element's text.
        let src = "\"use island\"\nimport { signal } from \"runai\"\nexport default function P(props: Record<never, never>) { const n = signal(0); return <div id={String(n())}><span>{n()}</span><b>{n() + 1}</b></div> }\n";
        let route = lower_route(src).unwrap();
        let html = template_html(&route.tree);
        assert_eq!(html.matches("<!>").count(), 3, "{html}");
        let js = emit_client(&route, "/assets/runtime.js", &imports_for(src));
        let attr = js.find("_setAttr(_a[0]").expect("attr is first");
        let first = js.find("_setText(_a[1]").expect("first text is second");
        let second = js.find("_setText(_a[2]").expect("second text is third");
        assert!(attr < first && first < second, "{js}");
    }

    #[test]
    fn module_statements_are_written_above_the_component() {
        let src = "\"use island\"\nimport { signal } from \"runai\"\nconst start = signal(5)\nexport default function (props: Record<never, never>) { return <p>{start()}</p> }\n";
        let js = emit_client(
            &lower_route(src).unwrap(),
            "/assets/runtime.js",
            &imports_for(src),
        );
        let module = js
            .find("const start = signal(5)")
            .expect("module statement written");
        let component = js.find("function Page").expect("component written");
        assert!(
            module < component,
            "module statements go above the component: {js}"
        );
        assert_parses(&js);
    }

    #[test]
    fn an_import_used_only_by_a_module_statement_is_still_imported() {
        // Task 9's cross-task risk: merging from markup-used names drops this.
        let src = "\"use island\"\nimport { signal, effect } from \"runai\"\nconst start = signal(5); effect(() => { console.log(start()) })\nexport default function P(props: Record<never, never>) { return <p>hi</p> }\n";
        let imports = imports_for(src);
        assert!(imports.iter().any(|i| i == "signal"), "{imports:?}");
        assert!(imports.iter().any(|i| i == "effect"), "{imports:?}");
        let js = emit_client(&lower_route(src).unwrap(), "/assets/runtime.js", &imports);
        assert_parses(&js);
    }

    #[test]
    fn the_emitters_locals_cannot_collide_with_author_names() {
        // Verified by node: `const el` beside the emitter's `const el` is a
        // redeclaration, and a module `const mount` beside the import is too. The
        // emitter therefore uses `_el`, `_a`, `_t0` — none of which a real route
        // writes — and this fixture's author-local `el` is copied verbatim without
        // colliding.
        let src = "\"use island\"\nimport { signal } from \"runai\"\nconst el = 1\nexport default function P(props: Record<never, never>) { const el2 = signal(0); return <p>{el}{el2()}</p> }\n";
        let js = emit_client(
            &lower_route(src).unwrap(),
            "/assets/runtime.js",
            &imports_for(src),
        );
        // The author's module-level `el` is emitted (it is a module statement), but
        // the emitter's own element local is `_el`, not `el`, so nothing redeclares.
        assert!(js.contains("const _el = _t0.clone()"), "{js}");
        assert!(!js.contains("const el = _t0"), "{js}");
        assert_parses(&js);
    }

    #[test]
    fn typed_source_emits_parseable_javascript() {
        let src = "\"use island\"\nimport { signal } from \"runai\"\ntype Props = Record<never, never>\nexport default function P(props: Props) { const n = signal<number>(0); return <p class=\"a\">{n() as number}</p> }\n";
        let js = emit_client(
            &lower_route(src).unwrap(),
            "/assets/runtime.js",
            &imports_for(src),
        );
        assert_parses(&js);
    }

    #[test]
    fn refuses_to_emit_when_analysis_reports_errors() {
        let src = "\"use island\"\nimport { signal } from \"runai\"\nexport default function P(props: Record<never, never>) { const n = signal(0); const x = n(); return <p>x</p> }\n";
        let err = emit_client_checked(src, "src/routes/index.page.tsx", "/assets/runtime.js")
            .unwrap_err();
        assert_eq!(err[0].code, crate::diagnostic::Code::RA001);
    }

    #[test]
    fn refuses_a_static_route() {
        let src = "\"use static\"\nexport default function P(props: Record<never, never>) { return <p>hi</p> }\n";
        let err = emit_client_checked(src, "src/routes/about.page.tsx", "/assets/runtime.js")
            .unwrap_err();
        assert_eq!(err[0].code, crate::diagnostic::Code::RA014);
        assert_eq!(err[0].line, 1);
    }

    #[test]
    fn refuses_a_non_page_path() {
        let src = "\"use island\"\nexport default function P(props: Record<never, never>) { return <p>hi</p> }\n";
        let err = emit_client_checked(src, "index.tsx", "/assets/runtime.js").unwrap_err();
        assert_eq!(err[0].code, crate::diagnostic::Code::RA014);
        assert!(err[0].message.contains("index.tsx"));
    }
}
