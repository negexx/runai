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
//! # `RA014`: markup the HTML parser would reshape
//!
//! The island emitter writes markup as HTML **text**, so the browser's parser
//! rebuilds the tree — and for these shapes it does not rebuild the tree the
//! author wrote. [`reparse_refusal`], [`parent_child_refusal`],
//! [`ancestor_refusal`], [`required_ancestors`], [`text_refusal`], and
//! [`self_closing_refusal`] refuse them, because the emitter cannot fix a shape
//! the parser rewrites:
//!
//! - `<p>` containing a block element — the parser closes the paragraph, so the
//!   child becomes a sibling;
//! - a tag that **closes an open element of its own kind** — `<li>` in `<li>`,
//!   `<dt>`/`<dd>` in either, any heading in any other, `<button>` in `<button>`,
//!   `<option>`/`<optgroup>` in either, `<td>`/`<th>` in either, `<tr>` in `<tr>`,
//!   and the obsolete `<nobr>` in `<nobr>`. The author wrote nesting and the
//!   parser produces siblings; `<label>` is deliberately **not** in the set,
//!   because HTML allows it and the parser keeps the nesting;
//! - `<table>` containing anything but `caption`/`colgroup`/`thead`/`tbody`/
//!   `tfoot`/`tr`, or bare text — the parser fosters it out of the table;
//! - `<thead>`/`<tbody>`/`<tfoot>` containing anything but `<tr>`, `<tr>`
//!   anything but `<td>`/`<th>`, `<colgroup>` anything but `<col>`, `<select>`
//!   anything but `<option>`/`<optgroup>`, `<ul>`/`<ol>` anything but `<li>`,
//!   `<dl>` anything but `<dt>`/`<dd>` — each is moved out or wrapped;
//! - `<a>` inside `<a>` — the parser splits it into siblings; `<form>` inside
//!   `<form>` — the parser **drops** the nested form;
//! - `<li>` outside `<ul>`/`<ol>`, `<dt>`/`<dd>` outside `<dl>` — a stray entry
//!   with no list to belong to;
//! - a tag the parser only builds under one parent ([`required_ancestors`]):
//!   `<td>`/`<th>` outside `<tr>`, `<tr>` outside a table, a table
//!   section/`<caption>`/`<colgroup>`/`<col>` outside `<table>`,
//!   `<option>`/`<optgroup>` outside `<select>`/`<datalist>`, `<figcaption>`
//!   outside `<figure>`, `<legend>` outside `<fieldset>`, `<summary>` outside
//!   `<details>`. What happens outside differs by element, verified against
//!   parse5 8.0.1: a table part is dropped with its content, a `<td>`/`<th>`
//!   loses the cell but keeps its content in the enclosing element, and
//!   `<option>`/`<optgroup>`/`<figcaption>`/`<legend>`/`<summary>` are **kept in
//!   place** — outside its parent each is simply not the control or caption its
//!   tag names. In every case the tree differs from the one the author wrote, so
//!   the position is refused rather than emitted. The route **root** has no
//!   ancestors, which is not a legal parent either;
//! - **in an island**, a dynamic binding inside a text-only element (`script`,
//!   `style`, `title`, `textarea`, `template`, …) — the `<!>` marker the emitter
//!   needs is swallowed as text or moved into a fragment the marker walker never
//!   reaches, so `_a[k]` would address the wrong site.
//!
//! They are repeated by [`crate::ir::lower_route`] as invariants, the way
//! [`dom_tag`] is, so a caller that skips the analysis gets a diagnostic rather
//! than a template the browser will reshape. Lowering carries the route's
//! [`crate::source::Mode`], so the island-only rule is repeated there too, not
//! just the tree-shape ones: `template_html` and `lower_route` are both `pub`.
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
    Expression, Function, FunctionBody, JSXAttributeItem, JSXAttributeName, JSXAttributeValue,
    JSXChild, JSXElement, JSXElementName, JSXExpression, JSXExpressionContainer, JSXFragment,
    Program, ReturnStatement, Statement,
};
use oxc_ast_visit::Visit;
use oxc_span::{GetSpan, Span};

use crate::diagnostic::{Code, Diagnostic};
use crate::ir::VOID_ELEMENTS;
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

/// The block-level start tags the HTML parser closes an open `<p>` for.
///
/// A `<p>` cannot contain a block element: the parser ends the paragraph at the
/// block start tag, so the child becomes the paragraph's **sibling** and the
/// parsed DOM is not the tree the author wrote. `<p><p>`, `<p><div>`, and
/// `<p><table>` are all the same defect. This is the one content model that is
/// a *deny*-list rather than an allow-list — `<p>` may contain any phrasing
/// element or text — so it lives here as its own constant, not inline in the
/// rule that reads it.
pub(crate) const BLOCK_IN_P: &[&str] = &[
    "address",
    "article",
    "aside",
    "blockquote",
    "details",
    "div",
    "dl",
    "fieldset",
    "figcaption",
    "figure",
    "footer",
    "form",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "header",
    "hr",
    "main",
    "nav",
    "ol",
    "p",
    "pre",
    "section",
    "table",
    "ul",
];

/// Parents whose content model the HTML parser enforces by moving, splitting, or
/// closing nodes — so the parsed DOM would not be the tree the author wrote.
///
/// `None` means "any child this milestone can emit". `Some(&[])` is the `<p>`
/// case, whose children are governed by [`BLOCK_IN_P`] rather than an
/// allow-list.
///
/// One table, read by [`JsxChecker`]'s nesting rule and repeated by
/// [`crate::ir::lower_route`] as an invariant, the way [`dom_tag`] is shared: a
/// caller that skips the analysis gets a diagnostic rather than a template the
/// browser will reshape.
pub(crate) fn reparse_refusal(tag: &str) -> Option<&'static [&'static str]> {
    match tag {
        "p" => Some(&[]), // no block-level children; see BLOCK_IN_P
        "table" => Some(&["caption", "colgroup", "thead", "tbody", "tfoot", "tr"]),
        "thead" | "tbody" | "tfoot" => Some(&["tr"]),
        "tr" => Some(&["td", "th"]),
        "colgroup" => Some(&["col"]),
        "select" => Some(&["option", "optgroup"]),
        "optgroup" => Some(&["option"]),
        "ul" | "ol" => Some(&["li"]),
        "dl" => Some(&["dt", "dd"]),
        _ => None,
    }
}

/// Elements whose content the HTML parser reads as **raw text** or **RCDATA**,
/// or keeps in a separate fragment.
///
/// A `<!>` marker inside one of these is not a comment: `script` and `style`
/// read it as literal text, `textarea` and `title` read RCDATA the same way, and
/// `template` puts its children in a `.content` fragment the island's
/// `TreeWalker` never descends into. In every case the marker count or its
/// position in the cloned fragment differs from what the emitter wrote, so the
/// `_a[k]` ordinal would address the wrong site — or `undefined`. An island
/// therefore refuses a **binding** inside one of these; a static template may
/// still hold one, because it interpolates a string and never places a marker.
pub(crate) const MARKER_HOSTILE: &[&str] = &[
    "script",
    "style",
    "title",
    "textarea",
    "xmp",
    "iframe",
    "noembed",
    "noframes",
    "plaintext",
    "template",
];

/// The tag of a plain DOM element, or `None` for anything that is not one.
///
/// The nesting rules are about real HTML tags, so a capitalized or namespaced name
/// — refused elsewhere by [`dom_tag`] — yields `None` here rather than a second
/// diagnostic.
fn element_tag<'a>(element: &'a JSXElement<'a>) -> Option<&'a str> {
    match &element.opening_element.name {
        JSXElementName::Identifier(identifier) => Some(identifier.name.as_str()),
        _ => None,
    }
}

/// Why `child` may not be a **direct child** of `parent`, or `None` when the
/// parser keeps the tree the author wrote.
///
/// Returns the full message: the parent, the child, what the parser does, and the
/// legal repair. It is shared by [`check_jsx`] and [`crate::ir::lower_route`] so a
/// caller that skips analysis gets the same refusal the analysis would have
/// produced.
pub(crate) fn parent_child_refusal(parent: &str, child: &str) -> Option<String> {
    // `<p>` is the deny-list case: a block-level child closes the paragraph.
    if parent == "p" {
        if BLOCK_IN_P.contains(&child) {
            return Some(format!(
                "`<p>` cannot contain `<{child}>`: the HTML parser closes the paragraph at a \
                 block-level child, so `<{child}>` becomes a sibling of the `<p>` and the page \
                 renders a different shape than you wrote. Move `<{child}>` out of the `<p>`, or \
                 use a `<span>` if its content is inline."
            ));
        }
        return None;
    }

    let allowed = reparse_refusal(parent)?;
    if allowed.is_empty() || allowed.contains(&child) {
        return None;
    }

    // Each family says what the parser does with a child it does not accept, so
    // the repair is specific rather than a generic "use the right tag".
    let message = match parent {
        "table" => format!(
            "`<table>` cannot contain `<{child}>`: the HTML parser moves a non-table child out of \
             the table, so it is fostered before the `<table>` and the page renders a different \
             shape. Put `<{child}>` outside the `<table>`, and use `<tr>` with `<td>` or `<th>` \
             for rows and cells."
        ),
        "thead" | "tbody" | "tfoot" => format!(
            "`<{parent}>` cannot contain `<{child}>`: the HTML parser only accepts `<tr>` inside \
             `<{parent}>`, so it relocates `<{child}>` and the page renders a different shape. Use \
             `<tr>` here, and move `<{child}>` outside the `<table>`."
        ),
        "tr" => format!(
            "`<tr>` cannot contain `<{child}>`: the HTML parser only accepts `<td>` or `<th>` inside \
             `<tr>`, so it moves `<{child}>` out and the page renders a different shape. Use `<td>` \
             or `<th>` for the cell, and move `<{child}>` outside the row."
        ),
        "colgroup" => format!(
            "`<colgroup>` cannot contain `<{child}>`: the HTML parser only accepts `<col>` inside \
             `<colgroup>`, so it moves `<{child}>` out and the page renders a different shape. Use \
             `<col>` here, and move `<{child}>` outside the `<colgroup>`."
        ),
        "select" => {
            // Three consequences inside `<select>`, verified against parse5 8.0.1
            // (`startTagInSelect`): `input`/`keygen`/`textarea`/`select` pop the
            // open `<select>` and reprocess the tag, so it becomes the select's
            // **sibling**; every other start tag falls through the `default` arm
            // and is **discarded** with only its text kept (`<select><p>b</p>` →
            // `(select "b")`). Saying "moves it out" for the discard class — the
            // old wording — was false; a `<p>`-inside-`<select>` does not become a
            // sibling, it is gone.
            if matches!(child, "input" | "keygen" | "textarea" | "select") {
                format!(
                    "`<select>` cannot contain `<{child}>`: the HTML parser closes the `<select>` at \
                     `<{child}>`, so `<{child}>` becomes a sibling after the `<select>` and the page \
                     renders a different shape. Move `<{child}>` outside the `<select>`, and keep \
                     only `<option>` and `<optgroup>` inside it."
                )
            } else {
                format!(
                    "`<select>` cannot contain `<{child}>`: the HTML parser discards `<{child}>` \
                     inside `<select>` and keeps only its text, so the page renders a different shape \
                     without the `<{child}>`. Move `<{child}>` outside the `<select>`, and keep only \
                     `<option>` and `<optgroup>` inside it."
                )
            }
        }
        "optgroup" => {
            // Same two classes as `<select>`, verified against parse5 8.0.1:
            // `input`/`keygen`/`textarea`/`select`/`hr` leave the open
            // `<optgroup>` (`startTagInSelect` pops it; `hr` becomes a sibling of
            // the group), and every other tag is **discarded** with only its text
            // kept (`<select><optgroup><div>x</div></optgroup></select>` →
            // `(select (optgroup "x"))`). "Moves it out" was false for the
            // discard class.
            if matches!(child, "input" | "keygen" | "textarea" | "select" | "hr") {
                format!(
                    "`<optgroup>` cannot contain `<{child}>`: the HTML parser closes the open \
                     `<optgroup>` at `<{child}>`, so `<{child}>` becomes its sibling and the page \
                     renders a different shape. Move `<{child}>` outside the `<optgroup>`, and keep \
                     only `<option>` inside it."
                )
            } else {
                format!(
                    "`<optgroup>` cannot contain `<{child}>`: the HTML parser discards `<{child}>` \
                     inside `<optgroup>` and keeps only its text, so the page renders a different \
                     shape without the `<{child}>`. Move `<{child}>` outside the `<optgroup>`, and \
                     keep only `<option>` inside it."
                )
            }
        }
        "ul" | "ol" => format!(
            "`<{parent}>` cannot contain `<{child}>`: a list holds `<li>` children, so the HTML \
             parser treats `<{child}>` as an item wrapper and the page renders a different shape. \
             Put `<{child}>` inside an `<li>`, or move it outside the `<{parent}>`."
        ),
        "dl" => format!(
            "`<dl>` cannot contain `<{child}>`: a description list holds `<dt>` and `<dd>` children, \
             so the HTML parser treats `<{child}>` as an entry and the page renders a different \
             shape. Put `<{child}>` inside a `<dt>` or `<dd>`, or move it outside the `<dl>`."
        ),
        // `p` is handled above; this arm is unreachable for any other tag because
        // `reparse_refusal` returns a list only for the tags above.
        _ => return None,
    };
    Some(message)
}

/// The tags that **close an open element of their own kind**, with the ancestors
/// that bound the search.
///
/// This is the wrong-**shape** class, the sibling of [`BLOCK_IN_P`]: the author
/// wrote nesting, and the HTML parser ends the outer element at the inner start
/// tag, so the inner element becomes the outer's **sibling**. `<li>a<li>b` inside
/// one `<ul>` is two items, not one item containing another; `<h1>a<h3>b` is two
/// headings, not one. Each row is verified against parse5 8.0.1, whose algorithms
/// this table transcribes:
///
/// - `list` rows (`li`, `dt`, `dd`) walk the open stack and stop at the first
///   *special* element that is not `address`/`div`/`p` — the
///   `listItemStartTagInBody` loop in parse5's parser;
/// - `html`/`button`/`table`/`template` rows stop at the corresponding *scope*
///   set — the `hasInScope`/`hasInButtonScope`/`hasInTableScope` boundaries;
/// - `option`, `optgroup` and the headings look at the **innermost open element
///   only**: parse5 pops at most one `option`, and a heading closes a heading only
///   when it is the current node.
///
/// `<label>` is deliberately **absent**: HTML allows a label to hold a label for a
/// control inside it, and parse5 keeps the nesting, so refusing it would be a
/// false positive. `<nobr>` is present even though it is obsolete: the parser
/// still closes an open `<nobr>` at a second one (`nobrStartTagInBody`), so the
/// shape is the same defect and is refused as an element rather than special-cased
/// as a tag.
///
/// The `stop` field names the boundary by scope: `Some(SCOPE_*)` for a set of
/// tags, `None` for the *special element* walk the list rows use, and
/// [`Scan::Innermost`] for the rows that only look at the parent.
pub(crate) enum Scan {
    /// Walk the stack inward-out; stop at the first special element that is not
    /// `address`, `div`, or `p` (parse5's `listItemStartTagInBody`).
    Special,
    /// Walk the stack inward-out; stop at any tag in this scope set.
    Scope(&'static [&'static str]),
    /// Only the innermost open element is consulted (parse5 pops one at most).
    Innermost,
}

/// Tags that close an open element of the same kind; see [`Scan`].
///
/// One table, read by [`self_closing_refusal`] and therefore by
/// [`crate::ir::lower_route`] too, so the analysis and the lowering cannot
/// disagree about which nesting the parser would split.
pub(crate) fn self_closing_refusal_for(tag: &str) -> Option<(&'static [&'static str], Scan)> {
    const HEADINGS: &[&str] = &["h1", "h2", "h3", "h4", "h5", "h6"];
    // parse5's scope sets (open-element-stack.js), plus `select` in the two
    // flow sets only. The in-select insertion mode discards every start tag but
    // its own children, so a `<p>`/`<a>`/`<nobr>`/`<button>` above a `<select>`
    // is never closed by a tag inside it: the inner element is discarded, and
    // claiming a "sibling" would be false. `select` is **not** added to
    // [`SCOPE_TABLE`]: verified against parse5, a `<td>` inside `<td><select>`
    // still closes the outer cell, so the table rows must keep walking past it.
    const SCOPE_HTML: &[&str] = &[
        "applet", "caption", "html", "marquee", "object", "table", "td", "template", "th", "select",
    ];
    const SCOPE_BUTTON: &[&str] = &[
        "applet", "caption", "html", "marquee", "object", "table", "td", "template", "th",
        "button", "select",
    ];
    const SCOPE_TABLE: &[&str] = &["table", "template"];

    match tag {
        "li" => Some((&["li"], Scan::Special)),
        "dt" | "dd" => Some((&["dt", "dd"], Scan::Special)),
        "p" => Some((&["p"], Scan::Scope(SCOPE_BUTTON))),
        "a" => Some((&["a"], Scan::Scope(SCOPE_HTML))),
        "nobr" => Some((&["nobr"], Scan::Scope(SCOPE_HTML))),
        "button" => Some((&["button"], Scan::Scope(SCOPE_HTML))),
        "td" | "th" => Some((&["td", "th"], Scan::Scope(SCOPE_TABLE))),
        "tr" => Some((&["tr"], Scan::Scope(SCOPE_TABLE))),
        "option" => Some((&["option"], Scan::Innermost)),
        "optgroup" => Some((&["option", "optgroup"], Scan::Innermost)),
        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => Some((HEADINGS, Scan::Innermost)),
        _ => None,
    }
}

/// The tags parse5 treats as *special*, for the [`Scan::Special`] walk.
///
/// An **exact mirror** of parse5 8.0.1's `SPECIAL_ELEMENTS[NS.HTML]`
/// (`dist/common/html.js`): the same 81 tags, no additions and no omissions.
/// This is data from the parser we are modelling, not a rule of ours, and the
/// `Scan::Special` walk's correctness depends on the set being identical — one
/// extra tag makes the walk stop early and silently loses a real nesting
/// (`<li>a<search><li>b` splits in parse5, so a `search` here instead reports the
/// markup clean and the page renders a shape the author did not write), and one
/// missing tag makes the walk run past a boundary and refuses legal markup.
/// `the_special_list_is_exactly_parse5s` pins every member so a hand-edit cannot
/// drift; a tag that needs different handling than parse5 gives it belongs in its
/// own named row with its own parse5 evidence, never quietly here.
///
/// Two absences are deliberate and verified, not oversights: `search` and
/// `dialog` are **not** in parse5's set. `<search>` is still an *address-like*
/// block that closes an open `<p>` (parse5's `addressStartTagInBody`, a separate
/// list — see [`BLOCK_IN_P`]), and `<dialog>` closes an `<li>` through the same
/// `Scan::Special` walk only when it is itself special; it is not, so
/// `<ul><li>a<dialog><li>b</li></dialog></li></ul>` does split in parse5
/// (`(ul (li (dialog)) (li))`) and the split is refused by the *li* row, not by
/// anything `dialog` contributes.
const SPECIAL: &[&str] = &[
    "address",
    "applet",
    "area",
    "article",
    "aside",
    "base",
    "basefont",
    "bgsound",
    "blockquote",
    "body",
    "br",
    "button",
    "caption",
    "center",
    "col",
    "colgroup",
    "dd",
    "details",
    "dir",
    "div",
    "dl",
    "dt",
    "embed",
    "fieldset",
    "figcaption",
    "figure",
    "footer",
    "form",
    "frame",
    "frameset",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "head",
    "header",
    "hgroup",
    "hr",
    "html",
    "iframe",
    "img",
    "input",
    "li",
    "link",
    "listing",
    "main",
    "marquee",
    "menu",
    "meta",
    "nav",
    "noembed",
    "noframes",
    "noscript",
    "object",
    "ol",
    "p",
    "param",
    "plaintext",
    "pre",
    "script",
    "section",
    "select",
    "source",
    "style",
    "summary",
    "table",
    "tbody",
    "td",
    "template",
    "textarea",
    "tfoot",
    "th",
    "thead",
    "title",
    "tr",
    "track",
    "ul",
    "wbr",
    "xmp",
];

/// Why `tag` may not open under `ancestors` because the parser would **close an
/// open element of its own kind** first, or `None`.
///
/// `ancestors` is every enclosing element, outermost first — the same stack
/// [`check_jsx`] and [`crate::ir::lower_route`] already keep. The innermost entry
/// is the direct parent; the rest is what the scope walk sees.
///
/// The message names the tag that closes, the tag it closes, what the parser does,
/// and a repair that is legal as written.
pub(crate) fn self_closing_refusal(tag: &str, ancestors: &[String]) -> Option<String> {
    let (closes, scan) = self_closing_refusal_for(tag)?;

    let closed = match scan {
        Scan::Innermost => {
            let parent = ancestors.last()?;
            closes
                .contains(&parent.as_str())
                .then_some(parent.clone())?
        }
        Scan::Special => {
            let mut found = None;
            for ancestor in ancestors.iter().rev() {
                if closes.contains(&ancestor.as_str()) {
                    found = Some(ancestor.clone());
                    break;
                }
                if !matches!(ancestor.as_str(), "address" | "div" | "p")
                    && SPECIAL.contains(&ancestor.as_str())
                {
                    break;
                }
            }
            found?
        }
        Scan::Scope(scope) => {
            let mut found = None;
            for ancestor in ancestors.iter().rev() {
                if closes.contains(&ancestor.as_str()) {
                    found = Some(ancestor.clone());
                    break;
                }
                if scope.contains(&ancestor.as_str()) {
                    break;
                }
            }
            found?
        }
    };

    let repair = match tag {
        "li" => {
            "Put the second `<li>` after the first one's closing `</li>`, or nest it inside the \
                 first item's own `<ul>` or `<ol>`."
                .to_string()
        }
        "dt" | "dd" => "Put the second entry after the first one's closing tag, or nest it in a \
                        `<dl>` inside the first entry."
            .to_string(),
        "td" | "th" => {
            "Put the second cell after the first one's closing tag; a row holds sibling \
                        cells."
                .to_string()
        }
        "tr" => "Put the second row after the first one's closing `</tr>`; a table section holds \
                 sibling rows."
            .to_string(),
        "option" => "Put the second `<option>` after the first one's closing tag; a select holds \
                     sibling options."
            .to_string(),
        "optgroup" => "Put the second `<optgroup>` after the first one's closing tag; a select \
                       holds sibling groups."
            .to_string(),
        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => "Put the second heading after the first one's \
                        closing tag, and choose one heading level for both; a heading cannot \
                        contain another heading."
            .to_string(),
        "nobr" => "Put the second `<nobr>` after the first one's closing tag, or drop `<nobr>` \
                   entirely: it is obsolete, and `<span style=\"white-space: nowrap\">` (or a CSS \
                   class) is the supported way to keep text on one line."
            .to_string(),
        _ => format!("Put the second `<{tag}>` outside the first one, after its closing tag."),
    };

    Some(format!(
        "`<{closed}>` cannot contain `<{tag}>`: the HTML parser closes the outer `<{closed}>` at \
         the inner `<{tag}>`, so the inner `<{tag}>` becomes a sibling of the outer `<{closed}>` \
         and the page renders a different shape than you wrote. {repair}"
    ))
}

/// Why bare text may not be a **direct child** of `parent`, or `None`.
///
/// Only the table family fosters text: `<table>text` parses with the text moved
/// *before* the table. A `<ul>`, `<select>`, or `<dl>` keeps a text child where it
/// is, so refusing text there would reject a page that renders as written.
pub(crate) fn text_refusal(parent: &str) -> Option<String> {
    match parent {
        "table" => Some(
            "`<table>` cannot contain bare text: the HTML parser fosters text out of a table, so it \
             renders before the `<table>` and the page renders a different shape. Put the text \
             inside a `<td>` or `<th>` cell, or in another element outside the table."
                .to_string(),
        ),
        "thead" | "tbody" | "tfoot" => Some(format!(
            "`<{parent}>` cannot contain bare text: the HTML parser fosters text out of a table \
             section, so it renders outside the `<{parent}>` and the page renders a different shape. \
             Put the text inside a `<td>` or `<th>` cell."
        )),
        "tr" => Some(
            "`<tr>` cannot contain bare text: the HTML parser fosters text out of a row, so it \
             renders outside the `<tr>` and the page renders a different shape. Put the text inside \
             a `<td>` or `<th>` cell."
                .to_string(),
        ),
        "colgroup" => Some(
            "`<colgroup>` cannot contain bare text: a `<colgroup>` holds only `<col>` elements, so \
             the HTML parser moves text out and the page renders a different shape. Remove the text, \
             or move it outside the `<colgroup>`."
                .to_string(),
        ),
        _ => None,
    }
}

/// The parents a tag is only legal under, or `None` when any parent is.
///
/// These are the elements the HTML parser refuses to build in place, verified
/// against parse5 8.0.1: a table part outside a table is dropped with its
/// content, a `<td>`/`<th>` outside a `<tr>` loses the cell but keeps its content
/// in the enclosing element, and an `<option>`/`<optgroup>`/`<figcaption>`/
/// `<legend>`/`<summary>` is **kept** where it stands but is not the control or
/// caption its tag names. The route **root** has no ancestors, which is not one
/// of the legal parents — a root `<td>` leaves the marker's `parentNode` a
/// document fragment, where `setAttribute` does not exist.
///
/// Shared by [`check_jsx`] and [`crate::ir::lower_route`], like
/// [`reparse_refusal`], so the two cannot disagree about a legal position.
pub(crate) fn required_ancestors(tag: &str) -> Option<&'static [&'static str]> {
    match tag {
        "td" | "th" => Some(&["tr"]),
        // A row needs a table section, or a table (the parser inserts the
        // section itself, which the marker scheme survives: the marker stays
        // inside the row).
        "tr" => Some(&["table", "thead", "tbody", "tfoot"]),
        "caption" | "colgroup" | "col" => Some(&["table"]),
        "thead" | "tbody" | "tfoot" => Some(&["table"]),
        "option" | "optgroup" => Some(&["select", "datalist"]),
        "figcaption" => Some(&["figure"]),
        "legend" => Some(&["fieldset"]),
        "summary" => Some(&["details"]),
        _ => None,
    }
}

/// The refusal for `tag` under ancestors that do not include one it requires.
///
/// The message names the tag, the parent it needs, what actually happens, and the
/// repair. Three consequences, because the parser treats three families
/// differently — verified against parse5 8.0.1, the spec-compliant parser:
///
/// - `<td>`/`<th>` outside `<tr>`: the **cell** is dropped and its content (with
///   the marker) stays in the enclosing element, so an attribute binding would be
///   set on that element instead;
/// - `<tr>` and the table sections, `<caption>`, `<colgroup>`, `<col>` outside a
///   table: the element **and its content** are dropped, so the page renders
///   without them;
/// - `<option>`/`<optgroup>`/`<figcaption>`/`<legend>`/`<summary>`: the parser
///   keeps the element in place, but outside its parent it is not the control or
///   caption it names and browsers render it inert. Saying "the parser drops it"
///   for these would be false.
fn required_ancestor_message(tag: &str, required: &[&str]) -> String {
    let named: Vec<String> = required
        .iter()
        .map(|parent| format!("`<{parent}>`"))
        .collect();
    let joined = match named.split_last() {
        Some((last, [])) => last.clone(),
        Some((last, rest)) => format!("{} or {last}", rest.join(", ")),
        None => String::new(),
    };
    let wrapper = required.first().copied().unwrap_or("");

    match tag {
        "td" | "th" => format!(
            "`<{tag}>` must be inside {joined}: the HTML parser drops a cell that is not in a row \
             and keeps its content in the enclosing element, so an attribute bound inside `<{tag}>` \
             would be set on the enclosing element instead. Wrap the `<{tag}>` in a `<tr>` inside a \
             `<table>`."
        ),
        "option" | "optgroup" | "figcaption" | "legend" | "summary" => format!(
            "`<{tag}>` must be inside {joined}: outside it the element is not the control or caption \
             it names, so browsers render something other than you wrote. Wrap the `<{tag}>` in a \
             `<{wrapper}>`."
        ),
        _ => format!(
            "`<{tag}>` must be inside {joined}: the HTML parser drops the element outside its \
             required parent, so the page renders without it and every later marker points at a \
             different node. Wrap the `<{tag}>` in a `<{wrapper}>`."
        ),
    }
}

/// Why `tag` may not appear under `ancestors`, or `None`.
///
/// Kinds of rule: `<a>` inside `<a>`, `<form>` inside `<form>`, a
/// list/description item with no list parent (`<li>` outside `<ul>`/`<ol>`,
/// `<dt>`/`<dd>` outside `<dl>`), and a tag the parser only builds under one
/// parent ([`required_ancestors`]: `<td>` outside `<tr>`, `<option>` outside
/// `<select>`, …). The parser splits nested anchors into siblings and **drops**
/// a nested form; a stray list item has no list to belong to; and a table part is
/// dropped while `<option>`/`<figcaption>`/`<legend>`/`<summary>` are kept but
/// not the control their tag names.
pub(crate) fn ancestor_refusal(tag: &str, ancestors: &[String]) -> Option<String> {
    let inside = |name: &str| ancestors.iter().any(|ancestor| ancestor == name);
    // A `<select>` on the ancestor path means the parser is in its in-select
    // insertion mode, whose default arm **discards** a start tag that is not
    // `option`/`optgroup` (parse5 `startTagInSelect`): `<a href="/1">x<select><a
    // href="/2">y</a></select></a>` parses as `(a href=/1 "x" (select "y"))` —
    // the inner `<a>` is gone, not a sibling. So the "becomes a sibling" claim
    // below is false there, and this rule stays silent; the `<select>` content
    // model already refuses the inner element (its element is discarded, which is
    // itself a shape change worth reporting).
    let in_select = inside("select");

    // A tag the parser only builds under one parent. The root has no ancestors,
    // which is not one of those parents — so a root `<td>` is refused rather than
    // emitted with a marker whose `parentNode` is the document fragment. If the
    // immediate parent already carries a more specific parent/child refusal, that
    // one stands: one element, one diagnostic.
    if let Some(required) = required_ancestors(tag) {
        let parent_refuses = ancestors
            .last()
            .is_some_and(|parent| parent_child_refusal(parent, tag).is_some());
        if !parent_refuses && !required.iter().any(|parent| inside(parent)) {
            return Some(required_ancestor_message(tag, required));
        }
    }

    match tag {
        "a" if inside("a") && !in_select => Some(
            "`<a>` cannot contain `<a>`: the HTML parser closes the outer anchor at the inner one, \
             so the inner `<a>` becomes a sibling and the page renders a different shape. Use a \
             `<span>` for the inner content, or move the inner anchor out of the outer one."
                .to_string(),
        ),
        "form" if inside("form") => Some(
            "`<form>` cannot contain `<form>`: the HTML parser drops the nested form, so its content \
             renders without a form and the page renders a different shape. Move the inner `<form>` \
             out of the outer one."
                .to_string(),
        ),
        "li" if !ancestors.is_empty() && !inside("ul") && !inside("ol") => Some(
            "`<li>` must be inside a `<ul>` or `<ol>`: outside a list it has no list to belong to, \
             so the parser renders a stray item. Wrap the `<li>` in a `<ul>` or `<ol>`, or use a \
             `<div>` if it is not a list item."
                .to_string(),
        ),
        "dt" | "dd" if !ancestors.is_empty() && !inside("dl") => Some(format!(
            "`<{tag}>` must be inside a `<dl>`: a description term or value only belongs to a \
             description list, so outside one the parser renders a stray entry. Wrap the `<{tag}>` \
             in a `<dl>`, or use a `<div>` if it is not part of a description list."
        )),
        _ => None,
    }
}

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
    // The marker-hostile rule is island-only: a static template interpolates a
    // string, never a `<!>` marker, so a binding in a text-only element is fine
    // there. A snippet with no directive is not an island (`detect_mode` errs),
    // which is the right default — those tests exercise fragments of markup, not
    // route modes.
    let island = matches!(
        crate::source::detect_mode(program, source),
        Ok(crate::source::Mode::Island)
    );
    let mut checker = JsxChecker {
        source,
        diagnostics: Vec::new(),
        ancestors: Vec::new(),
        island,
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
    /// The tag of every element currently open, outermost first.
    ///
    /// Maintained by [`JsxChecker::visit_jsx_element`] around the recursive walk
    /// so the ancestor rules — `<a>` inside `<a>`, `<form>` inside `<form>`,
    /// `<li>` outside a list — can be asked at any depth. It is the element
    /// stack, not a span list, because the rules are about tags and the nearest
    /// enclosing parent, not positions.
    ancestors: Vec<String>,
    /// Whether the file is an island, which decides the marker-hostile rule: a
    /// `"use static"` template interpolates a string and never places a `<!>`
    /// marker, so only an island can be broken by a binding in a text-only element.
    island: bool,
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

        // Every `return` before the root is refused, JSX or not: `render()`
        // returns one string, so an early exit could only return another value —
        // and lowering copies the statement verbatim into a function whose job is
        // to return one template literal.
        if let Some(span) = early_return(body, return_span) {
            self.diagnostics.push(unsupported(
                span,
                "an early `return` before the route's JSX return",
                "`render()` returns one string, so the component body cannot exit early. Replace \
                 the early return with a value computed by a conditional expression, or move the \
                 conditional into the returned markup.",
            ));
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
        self.void_with_children(element);
        self.attributes(element);
        self.duplicate_attributes(element);
        self.nesting(element);
        self.children(&element.children);

        // Keep the open-element stack accurate around the recursive walk: push
        // before descending so a child sees this element as its parent, pop after.
        // A non-HTML tag (a component, a namespaced name) is pushed too, so the
        // ancestor rules see the real ancestry; `element_tag` only gates the
        // rules that compare tags, and a component is never one of those tags.
        if let Some(tag) = element_tag(element) {
            self.ancestors.push(tag.to_string());
            oxc_ast_visit::walk::walk_jsx_element(self, element);
            self.ancestors.pop();
        } else {
            oxc_ast_visit::walk::walk_jsx_element(self, element);
        }
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

    /// `RA014` for a void element that has children.
    ///
    /// A void element (`br`, `img`, `input`, …) has no closing tag in HTML, so the
    /// parser closes it at the first `>`. `<br>x</br>` therefore renders `x`
    /// **after** the `br`, as a sibling — while an island's DOM build would put it
    /// inside. The two modes would render different pages from one source, and the
    /// only honest answer is to refuse it. The list is [`VOID_ELEMENTS`], the same
    /// one the emitter uses, so the two cannot disagree about which tags are void.
    fn void_with_children(&mut self, element: &JSXElement<'_>) {
        if !element.children.is_empty()
            && let JSXElementName::Identifier(identifier) = &element.opening_element.name
            && VOID_ELEMENTS.contains(&identifier.name.as_str())
        {
            self.diagnostics.push(unsupported(
                element.span,
                "children inside a void element",
                format!(
                    "`<{tag}>` has no closing tag in HTML, so the browser closes it at the \
                     first `>` and these children become its siblings. Move them out of \
                     `<{tag}>`, or use an element that accepts children.",
                    tag = identifier.name.as_str(),
                )
                .as_str(),
            ));
        }
    }

    /// `RA014` for an attribute written twice on one element.
    ///
    /// A static template keeps the *first* (`<div class="a" class="b">` writes both
    /// and the browser takes the first), while an island's `_setAttr` applies the
    /// *last*. One source would render `class="a"` in a prerendered page and
    /// `class="b"` in a hydrated one. The names are compared **as written**, so
    /// `class` beside `className` is not a duplicate — they are different
    /// attributes to this milestone.
    fn duplicate_attributes(&mut self, element: &JSXElement<'_>) {
        let mut seen: Vec<&str> = Vec::new();
        for attribute in &element.opening_element.attributes {
            let JSXAttributeItem::Attribute(attribute) = attribute else {
                continue;
            };
            let name = match &attribute.name {
                JSXAttributeName::Identifier(identifier) => identifier.name.as_str(),
                JSXAttributeName::NamespacedName(_) => {
                    // A namespaced name cannot be compared to an identifier
                    // without borrowing temporary text, and two spellings of one
                    // namespace are rare enough that skipping them here is
                    // honest: this rule's job is the ordinary duplicate.
                    continue;
                }
            };
            if seen.contains(&name) {
                self.diagnostics.push(unsupported(
                    attribute.span,
                    &format!("a duplicate attribute (`{name}`)"),
                    "the prerendered template keeps the first value and a hydrated island keeps \
                     the last, so the two modes would render different attributes. Keep one.",
                ));
            } else {
                seen.push(name);
            }
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

    /// The reparse refusals: a shape the HTML parser would rewrite.
    ///
    /// Four checks for one element: whether it **closes an open element of its own
    /// kind**, its **parent/child** tag pair, a non-blank **text** child in a table
    /// family element, and its **ancestors** (a nested anchor or form, or a stray
    /// list item). Each is `RA014` — the file parses; this is the compiler
    /// declining markup whose parsed DOM would differ from what the author wrote,
    /// which the island emitter cannot fix because it emits HTML text the browser
    /// re-parses.
    ///
    /// The messages come from [`self_closing_refusal`], [`parent_child_refusal`],
    /// [`text_refusal`], and [`ancestor_refusal`], shared with
    /// [`crate::ir::lower_route`] so a caller that skips the analysis gets the same
    /// words rather than a different rule.
    fn nesting(&mut self, element: &JSXElement<'_>) {
        let Some(tag) = element_tag(element) else {
            return;
        };

        // A tag that closes an open element of its own kind: the author wrote
        // nesting and the parser produces siblings. `self.ancestors` is the
        // enclosing stack (this element is pushed only after the checks), so its
        // innermost entry is this element's parent. This is checked **first**
        // because it is the precise rule for the same-kind cases; the parent/child
        // allow-list would describe `<optgroup>` in `<optgroup>` as "not an
        // `<option>`", which is true but buries the actual defect.
        if let Some(message) = self_closing_refusal(tag, &self.ancestors) {
            self.diagnostics.push(reshaped(element.span, message));
            return;
        }

        // The parent is the innermost open element, if any. The route root has no
        // parent, so a root `<p>`, `<table>`, or `<li>` is never the parent/child
        // rule's business here — only its children are.
        if let Some(parent) = self.ancestors.last()
            && let Some(message) = parent_child_refusal(parent, tag)
        {
            self.diagnostics.push(reshaped(element.span, message));
        }
        if let Some(message) = ancestor_refusal(tag, &self.ancestors) {
            self.diagnostics.push(reshaped(element.span, message));
        }

        // A text child is refused only by the table family, where the parser
        // fosters it. Whitespace-only text is not content — an indented `<table>`
        // has such children — so it is not refused.
        if let Some(message) = text_refusal(tag) {
            let fostered = element.children.iter().any(|child| {
                matches!(child, JSXChild::Text(text) if !text.value.as_str().trim().is_empty())
            });
            if fostered {
                self.diagnostics.push(reshaped(element.span, message));
            }
        }

        // A binding inside a text-only / raw-text / fragment element breaks the
        // island's marker ordinal: the parser reads the `<!>` as text (script,
        // style, title, textarea) or moves it into a fragment the TreeWalker never
        // reaches (template), so `_a[k]` addresses the wrong site. Island-only: a
        // static template interpolates a string, never a marker.
        if self.island && MARKER_HOSTILE.contains(&tag) && element_has_binding(element) {
            self.diagnostics
                .push(marker_hostile_refusal(element.span, tag));
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
    /// The route component's own parameter list — the fourth region the
    /// emitters handle, and the only one they **do not copy**.
    ///
    /// Carried from the same walk that finds the root, because a caller asking
    /// "what does the generated component declare?" needs the signature from the
    /// node [`route_render`] already resolved. `RouteIr` carries the component's
    /// name, statements, and markup but not its parameter text, so the emitted
    /// signature is written by the emitter rather than sliced from the file —
    /// which is why [`crate::analyze::reachable`] checks this region's *shape*
    /// and not just its references.
    pub params: &'a oxc_ast::ast::FormalParameters<'a>,
    /// The scope cell of the route component itself, filled in by the semantic
    /// pass.
    ///
    /// This is the one scope the emitted `render()` shares with the source: the
    /// emitter copies the component body into it, so a name bound **here** is in
    /// scope at every emitted call site. `var` is why the distinction matters — it
    /// hoists to this scope from a nested block, a `for` init, or a `try`/`catch`,
    /// while a block-scoped `const` stays in its block. The cell is read after the
    /// caller's semantic build ([`crate::analyze::emitted_names`] does exactly
    /// that), so it is carried as the cell rather than the resolved value.
    pub scope_id: &'a std::cell::Cell<Option<oxc_semantic::ScopeId>>,
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
        params: &function.params,
        scope_id: &function.scope_id,
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
                params: &arrow.params,
                scope_id: &arrow.scope_id,
            })
        }
        concise => {
            let expression = concise.as_expression()?;
            let root = jsx_expression(expression)?;
            Some(RouteRender {
                root,
                body: None,
                return_span: None,
                params: &arrow.params,
                scope_id: &arrow.scope_id,
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

/// The span of the first `return` in the body **before** `return_span` that
/// returns something other than JSX, or `None`.
///
/// Every such return is an early exit: the route's own `return` is the one at
/// `return_span`, and anything before it runs first. `render()` in the emitted
/// module returns one template literal, so an early `return null` or
/// `return 42` would be copied verbatim into a function that must return a string.
/// Nested blocks are descended into — an `if (on) return null` is exactly the
/// shape — but nested *functions* are not: their returns belong to another
/// component.
///
/// A return whose argument **is** JSX is not this rule's: it is the "second JSX
/// return" construct, which already has its own diagnostic and its own repair
/// (move that markup into the returned tree). Reporting it here as well would put
/// two rows on one return, which is the duplication this project refuses.
fn early_return<'a>(body: &'a FunctionBody<'a>, return_span: Span) -> Option<Span> {
    let mut finder = EarlyReturns::default();
    for statement in body
        .statements
        .iter()
        .filter(|statement| statement.span().end <= return_span.start)
    {
        finder.visit_statement(statement);
    }
    finder
        .returns
        .into_iter()
        .find(|(_, returns_jsx)| !returns_jsx)
        .map(|(span, _)| span)
}

/// Collects every `return` statement before the root, with whether it returns
/// JSX, skipping nested functions.
#[derive(Default)]
struct EarlyReturns {
    returns: Vec<(Span, bool)>,
}

impl<'a> Visit<'a> for EarlyReturns {
    fn visit_return_statement(&mut self, returned: &ReturnStatement<'a>) {
        let jsx = returned
            .argument
            .as_ref()
            .is_some_and(|argument| jsx_expression(argument).is_some());
        self.returns.push((returned.span, jsx));
        oxc_ast_visit::walk::walk_return_statement(self, returned);
    }

    // A nested function has its own render, so its `return`s are not this body's.
    fn visit_function(&mut self, _function: &Function<'a>, _flags: oxc_syntax::scope::ScopeFlags) {}

    fn visit_arrow_function_expression(
        &mut self,
        _arrow: &oxc_ast::ast::ArrowFunctionExpression<'a>,
    ) {
    }
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

/// The `RA014` for a markup shape the HTML parser would rewrite.
///
/// `message` is already a full sentence naming the parent, the child, what the
/// parser does, and the repair — it comes from [`parent_child_refusal`],
/// [`text_refusal`], or [`ancestor_refusal`], shared with lowering. Unlike
/// [`unsupported`], it does not take a "cannot lower X yet" prefix: this is not an
/// unimplemented construct but markup that means something other than it says, and
/// the message's own opening clause is the construct.
fn reshaped(span: Span, message: String) -> Diagnostic {
    Diagnostic::new(Code::RA014, (span.start, span.end), message)
}

/// Whether `element` carries any dynamic binding: an attribute or event binding,
/// or an `Expr` child (recursively, or as a bare expression).
///
/// This is the test the marker-hostile rule needs: only a **binding** inside a
/// text-only element puts a marker where the parser will not keep it as a comment.
/// Static text is fine — it is written verbatim either way. Shared with
/// [`crate::ir::lower_route`] so the rule is one predicate, not two.
pub(crate) fn element_has_binding(element: &JSXElement<'_>) -> bool {
    for attribute in &element.opening_element.attributes {
        if let JSXAttributeItem::Attribute(attribute) = attribute
            && matches!(
                attribute.value.as_ref(),
                Some(JSXAttributeValue::ExpressionContainer(_))
            )
            && !matches!(
                attribute.value.as_ref(),
                Some(JSXAttributeValue::ExpressionContainer(container))
                    if matches!(container.expression, JSXExpression::EmptyExpression(_))
            )
        {
            return true;
        }
    }
    element.children.iter().any(child_has_binding)
}

/// Whether `child` is a bare expression, or an element that itself carries a
/// binding. Static text and plain elements do not.
fn child_has_binding(child: &JSXChild<'_>) -> bool {
    match child {
        JSXChild::ExpressionContainer(container) => {
            !matches!(container.expression, JSXExpression::EmptyExpression(_))
        }
        JSXChild::Element(element) => element_has_binding(element),
        // A fragment's children are spliced into the text-only element, so a
        // binding inside one is the same defect; a spread is refused elsewhere.
        JSXChild::Fragment(fragment) => fragment.children.iter().any(child_has_binding),
        JSXChild::Text(_) | JSXChild::Spread(_) => false,
    }
}

/// The `RA014` for a binding inside a text-only element an island cannot place a
/// marker in.
///
/// The message is [`marker_hostile_message`], shared with
/// [`crate::ir::lower_route`] so the rule is one sentence, not two.
fn marker_hostile_refusal(span: Span, tag: &str) -> Diagnostic {
    Diagnostic::new(
        Code::RA014,
        (span.start, span.end),
        marker_hostile_message(tag),
    )
}

/// Why a binding inside `tag` cannot survive an island's marker scheme.
///
/// The two repairs offered are the only two that are **reachable**: static text,
/// or a binding on a sibling element. Moving the binding into an attribute is not
/// a repair — [`element_has_binding`] counts attribute bindings, so the whole
/// element is refused again with the same words, and an agent following that fix
/// loops forever.
pub(crate) fn marker_hostile_message(tag: &str) -> String {
    let reason = match tag {
        "script" | "style" => {
            "the browser reads the contents of `<script>` and `<style>` as literal text, so the \
             `<!>` marker the island needs to address this binding is swallowed as text"
        }
        "title" | "textarea" => {
            "`<title>` and `<textarea>` read their contents as RCDATA, so the `<!>` marker the \
             island needs to address this binding is swallowed as text"
        }
        _ => {
            "the browser parses this element's contents into a fragment the island's marker \
             walker does not reach"
        }
    };
    format!(
        "`<{tag}>` cannot contain a dynamic binding here: {reason}, so the island would wire the \
         wrong node. Write the value as static text, or bind it on a sibling element that holds \
         normal content — not on `<{tag}>`, because the binding is what the element cannot keep."
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

    // --- The emit-time refusals (Task 10) ------------------------------------

    /// A void element cannot have children: the HTML parser closes it at the
    /// first `>`, so the children land *outside* it. A static template would
    /// therefore render `<br>` and then the children as siblings, while an
    /// island's `_setAttr`-built DOM would put them inside. Refusing it is the
    /// only honest answer, and it is `RA014` because the file parses.
    #[test]
    fn a_void_element_with_children_is_ra014() {
        let src = "\"use static\"\nexport default function P(props: Record<never, never>) { return <p><br>x</br></p> }\n";
        let d = check_jsx(
            crate::source::parse_tsx(&oxc_allocator::Allocator::default(), src).unwrap(),
            src,
        );
        assert_eq!(d.len(), 1, "{d:?}");
        assert_eq!(d[0].code, Code::RA014);
        assert!(d[0].message.contains("void"), "{}", d[0].message);
    }

    /// `<div class="a" class="b">` is a duplicate attribute. A static template
    /// keeps the first (it writes both, and the browser takes the first), while
    /// an island's `_setAttr` applies the last — so the same source renders two
    /// different attributes in the two modes. Refused rather than guessed at, and
    /// the message names the duplicated attribute so the agent can find it.
    #[test]
    fn a_duplicate_attribute_is_ra014() {
        let src = "\"use static\"\nexport default function P(props: Record<never, never>) { return <div class=\"a\" class=\"b\">x</div> }\n";
        let d = check_jsx(
            crate::source::parse_tsx(&oxc_allocator::Allocator::default(), src).unwrap(),
            src,
        );
        assert_eq!(d.len(), 1, "{d:?}");
        assert_eq!(d[0].code, Code::RA014);
        assert!(
            d[0].message.contains("`class`"),
            "the message names the duplicated attribute: {}",
            d[0].message
        );
    }

    /// `render()` returns one string, so an early `return` before the root JSX
    /// return returns something else. Lowering copies every body statement before
    /// the root return verbatim — including `if (on) return null` — into a
    /// function whose job is to return one template literal. Every `return`
    /// before the root is refused, JSX or not.
    #[test]
    fn an_early_non_jsx_return_is_ra014() {
        let src = "\"use static\"\nexport default function P(props: Record<never, never>) { const on = false; if (on) return null; return <p>hi</p> }\n";
        let d = check_jsx(
            crate::source::parse_tsx(&oxc_allocator::Allocator::default(), src).unwrap(),
            src,
        );
        assert_eq!(d.len(), 1, "{d:?}");
        assert_eq!(d[0].code, Code::RA014);
        assert!(
            d[0].message.contains("one string"),
            "the message says render() returns one string: {}",
            d[0].message
        );
    }

    // --- The reparse refusals (Task 11, Step 0) ------------------------------
    //
    // The emitted template is HTML text, so the browser's parser rebuilds the
    // tree. For these shapes it does not rebuild the tree the author wrote: it
    // closes a `<p>` at a block child, moves a `<div>` out of a `<table>`, splits
    // nested anchors, and so on. The island emitter cannot fix a shape the
    // browser rewrites, so the analyzer refuses it. Every one of these is
    // `check []` before Task 11 — the RED this group is written against.

    /// A block element inside `<p>`: the parser closes the paragraph at the block
    /// start tag, so the div becomes a sibling and the page renders a different
    /// shape. The message must name both tags and say "sibling".
    #[test]
    fn a_block_element_inside_a_paragraph_is_ra014() {
        let src = "\"use island\"\nexport default function P(props: Record<never, never>) { return <p>a<div>b</div></p> }\n";
        let d = check(src);
        assert_eq!(d.len(), 1, "{d:?}");
        assert_eq!(d[0].code, Code::RA014);
        assert!(
            d[0].message.contains("`<p>`"),
            "names the parent: {}",
            d[0].message
        );
        assert!(
            d[0].message.contains("`<div>`"),
            "names the child: {}",
            d[0].message
        );
        assert!(
            d[0].message.contains("sibling"),
            "says sibling: {}",
            d[0].message
        );
    }

    /// A `<div>` inside `<table>` is fostered out of the table entirely: it lands
    /// *before* the table, not inside it.
    #[test]
    fn a_table_may_contain_rows_but_not_a_div() {
        let src = "\"use island\"\nexport default function P(props: Record<never, never>) { return <table><div>x</div><tr><td>y</td></tr></table> }\n";
        let d = check(src);
        assert_eq!(d.len(), 1, "{d:?}");
        assert_eq!(d[0].code, Code::RA014);
        assert!(
            d[0].message.contains("`<table>`"),
            "names the parent: {}",
            d[0].message
        );
        assert!(
            d[0].message.contains("`<div>`"),
            "names the child: {}",
            d[0].message
        );
    }

    /// Bare text directly in a `<table>` is fostered before the table.
    #[test]
    fn bare_text_inside_a_table_is_ra014() {
        let src = "\"use island\"\nexport default function P(props: Record<never, never>) { return <table>text</table> }\n";
        let d = check(src);
        assert_eq!(d.len(), 1, "{d:?}");
        assert_eq!(d[0].code, Code::RA014);
        assert!(
            d[0].message.contains("foster"),
            "the message says the parser fosters text: {}",
            d[0].message
        );
    }

    /// The parser closes the open anchor at a nested `<a>`, so the inner one
    /// becomes a sibling.
    #[test]
    fn an_anchor_inside_an_anchor_is_ra014() {
        let src = "\"use island\"\nexport default function P(props: Record<never, never>) { return <a href=\"/x\"><a href=\"/y\">z</a></a> }\n";
        let d = check(src);
        assert_eq!(d.len(), 1, "{d:?}");
        assert_eq!(d[0].code, Code::RA014);
        assert!(
            d[0].message.contains("`<a>`"),
            "names the tag: {}",
            d[0].message
        );
    }

    /// A nested `<p>` also splits: the parser closes the first paragraph.
    #[test]
    fn a_nested_paragraph_is_ra014() {
        let src = "\"use island\"\nexport default function P(props: Record<never, never>) { return <p><p>z</p></p> }\n";
        let d = check(src);
        assert_eq!(d.len(), 1, "{d:?}");
        assert_eq!(d[0].code, Code::RA014);
        assert!(
            d[0].message.contains("`<p>`"),
            "names the tags: {}",
            d[0].message
        );
    }

    /// A `<li>` outside `<ul>`/`<ol>` has no list to belong to.
    #[test]
    fn a_list_item_must_have_a_list_parent() {
        let src = "\"use island\"\nexport default function P(props: Record<never, never>) { return <div><li>x</li></div> }\n";
        let d = check(src);
        assert_eq!(d.len(), 1, "{d:?}");
        assert_eq!(d[0].code, Code::RA014);
        assert!(
            d[0].message.contains("`<li>`"),
            "names the tag: {}",
            d[0].message
        );
        assert!(
            d[0].message.contains("`<ul>`"),
            "names the list: {}",
            d[0].message
        );
    }

    /// The shapes the parser rebuilds faithfully stay clean. A route root may be
    /// `p` — the rule is about CHILDREN, not the root tag.
    #[test]
    fn the_legal_shapes_stay_clean() {
        for src in [
            "\"use island\"\nexport default function P(props: Record<never, never>) { return <ul><li>a</li><li>b</li></ul> }\n",
            "\"use island\"\nexport default function P(props: Record<never, never>) { return <table><tbody><tr><td>x</td></tr></tbody></table> }\n",
            "\"use island\"\nexport default function P(props: Record<never, never>) { return <p><span>a</span> and <b>b</b></p> }\n",
            "\"use island\"\nexport default function P(props: Record<never, never>) { return <dl><dt>a</dt><dd>b</dd></dl> }\n",
            "\"use island\"\nexport default function P(props: Record<never, never>) { return <select><option>a</option></select> }\n",
            // A route root may be `p`: the rule is about CHILDREN, not the root tag.
            "\"use island\"\nexport default function P(props: Record<never, never>) { return <p>plain</p> }\n",
        ] {
            let alloc = oxc_allocator::Allocator::default();
            let program = crate::source::parse_tsx(&alloc, src).unwrap();
            assert!(check_jsx(program, src).is_empty(), "{src}");
        }
    }

    /// A **binding inside a text-only element** is `RA014` in an island: `script`
    /// and `style` read the `<!>` marker as literal text, `title` and `textarea`
    /// read it as RCDATA, and `template` puts it in a fragment the island's
    /// TreeWalker never descends into — so the marker count or its order differs
    /// from what the emitter wrote and `_a[k]` addresses the wrong site.
    ///
    /// A static route is unaffected (its template interpolates a string, never a
    /// marker), so the rule is island-only. The control below is that same binding
    /// in a `"use static"` route, which stays clean.
    #[test]
    fn a_binding_inside_a_text_only_element_is_ra014_in_an_island() {
        for src in [
            "\"use island\"\nimport { signal } from \"runai\"\nexport default function P(props: Record<never, never>) { const c = signal(\"x\"); return <style>{c()}</style> }\n",
            "\"use island\"\nimport { signal } from \"runai\"\nexport default function P(props: Record<never, never>) { const c = signal(\"x\"); return <script>{c()}</script> }\n",
            "\"use island\"\nimport { signal } from \"runai\"\nexport default function P(props: Record<never, never>) { const c = signal(\"x\"); return <textarea>{c()}</textarea> }\n",
            "\"use island\"\nimport { signal } from \"runai\"\nexport default function P(props: Record<never, never>) { const c = signal(\"x\"); return <div><template>{c()}</template></div> }\n",
        ] {
            let d = check(src);
            assert_eq!(d.len(), 1, "{src}: {d:?}");
            assert_eq!(d[0].code, Code::RA014, "{src}");
            assert!(
                d[0].message.contains("marker") || d[0].message.contains("text"),
                "{src}: the message says why: {}",
                d[0].message
            );
        }

        // The control: the same binding in a static route is clean — a static
        // template interpolates a string and never places a marker.
        let static_src = "\"use static\"\nexport default function P(props: Record<never, never>) { const c = \"x\"; return <style>{c}</style> }\n";
        let alloc = oxc_allocator::Allocator::default();
        let program = crate::source::parse_tsx(&alloc, static_src).unwrap();
        assert!(
            check_jsx(program, static_src).is_empty(),
            "a static template may hold a binding in a text-only element"
        );
    }

    /// The marker-hostile fix must be **reachable**: applying it verbatim may not
    /// land in another refusal. Moving the binding into an attribute is not a
    /// repair — [`element_has_binding`] counts attribute bindings, so the whole
    /// element is refused again with the same words. An agent follows that fix
    /// forever. The only honest repairs are static text, or a sibling element.
    #[test]
    fn the_marker_hostile_fix_names_a_reachable_repair() {
        let src = "\"use island\"\nimport { signal } from \"runai\"\nexport default function P(props: Record<never, never>) { const u = signal(\"x\"); return <textarea value={u()}>static</textarea> }\n";
        let d = check(src);
        assert_eq!(d.len(), 1, "{d:?}");
        assert_eq!(d[0].code, Code::RA014);
        assert!(
            !d[0].message.contains("into an attribute"),
            "the fix sends the agent into a repair loop: {}",
            d[0].message
        );
        assert!(
            !d[0].message.contains("plain attribute"),
            "the fix promises an attribute escape hatch that does not exist: {}",
            d[0].message
        );
        assert!(
            d[0].message.contains("sibling"),
            "the fix does not name a reachable repair: {}",
            d[0].message
        );
    }

    /// Every tag the HTML parser only accepts under one parent, outside it. The
    /// parser drops a table part with its content, keeps a `<td>`'s content in the
    /// enclosing element, and keeps an `<option>`/`<figcaption>`/`<legend>`/
    /// `<summary>` where it stands but not as the control its tag names — either
    /// way the emitted `_a[k]` addresses a node the author did not write, so the
    /// analysis must refuse the shape.
    ///
    /// Verified against parse5 8.0.1: `<td>`/`<th>` lose the cell but keep their
    /// content in the enclosing element (so `_setAttr(_a[k].parentNode, …)` sets
    /// the attribute on the *outer* element); `<tr>`, `<tbody>`, `<thead>`,
    /// `<tfoot>`, `<caption>`, `<colgroup>`, and `<col>` are dropped with their
    /// content; `<option>`/`<optgroup>`/`<figcaption>`/`<legend>`/`<summary>` are
    /// kept but inert, so the page is not the markup the author wrote.
    #[test]
    fn a_tag_outside_its_required_ancestor_is_ra014() {
        // (body, tag, required ancestor, the consequence in the user's terms)
        let cases: &[(&str, &str, &str, &str)] = &[
            ("<div><td>x</td></div>", "td", "tr", "enclosing element"),
            ("<div><th>x</th></div>", "th", "tr", "enclosing element"),
            (
                "<div><tr><td>x</td></tr></div>",
                "tr",
                "table",
                "drops the element",
            ),
            (
                "<div><tbody></tbody></div>",
                "tbody",
                "table",
                "drops the element",
            ),
            (
                "<div><thead></thead></div>",
                "thead",
                "table",
                "drops the element",
            ),
            (
                "<div><tfoot></tfoot></div>",
                "tfoot",
                "table",
                "drops the element",
            ),
            (
                "<div><caption>c</caption></div>",
                "caption",
                "table",
                "drops the element",
            ),
            (
                "<div><colgroup></colgroup></div>",
                "colgroup",
                "table",
                "drops the element",
            ),
            ("<div><col/></div>", "col", "table", "drops the element"),
            (
                "<div><option>a</option></div>",
                "option",
                "select",
                "browsers render",
            ),
            (
                "<div><optgroup></optgroup></div>",
                "optgroup",
                "select",
                "browsers render",
            ),
            (
                "<div><figcaption>c</figcaption></div>",
                "figcaption",
                "figure",
                "browsers render",
            ),
            (
                "<div><legend>l</legend></div>",
                "legend",
                "fieldset",
                "browsers render",
            ),
            (
                "<div><summary>s</summary></div>",
                "summary",
                "details",
                "browsers render",
            ),
            // The route root has no ancestor at all, which is not a legal parent
            // either: a root `<td>` leaves the marker's `parentNode` a document
            // fragment, where `setAttribute` does not exist.
            ("<td>x</td>", "td", "tr", "enclosing element"),
            ("<tr><td>x</td></tr>", "tr", "table", "drops the element"),
            ("<option>a</option>", "option", "select", "browsers render"),
        ];

        for (body, tag, required, consequence) in cases {
            let src = format!(
                "\"use island\"\nexport default function P(props: Record<never, never>) {{ return {body} }}\n"
            );
            let d = check(&src);
            assert_eq!(d.len(), 1, "{body}: {d:?}");
            assert_eq!(d[0].code, Code::RA014, "{body}");
            assert!(
                d[0].message.contains(&format!("`<{tag}>`")),
                "{body}: names the tag: {}",
                d[0].message
            );
            assert!(
                d[0].message.contains(&format!("`<{required}>`")),
                "{body}: names the required ancestor: {}",
                d[0].message
            );
            assert!(
                d[0].message.contains(consequence),
                "{body}: states the consequence in the user's terms: {}",
                d[0].message
            );
            assert!(
                d[0].message.contains("Wrap the"),
                "{body}: gives a reachable repair: {}",
                d[0].message
            );
        }
    }

    /// A tag that **closes an open element of its own kind** is the wrong-SHAPE
    /// class: the author wrote nesting and the page gets siblings. `<li>a<li>b`
    /// inside one `<ul>` parses as two sibling items, not one item containing
    /// another. The same holds for the description list, the headings (any level
    /// closes any level), buttons, options and optgroups, table cells and rows,
    /// and `<p>`/`<a>`; `<nobr>` is the obsolete member of the family.
    ///
    /// `<form>` is deliberately **not** here: parse5 *drops* a nested form
    /// (`<form><form>x</form></form>` → `(form "x")`), which is the DROP class,
    /// not the sibling class. It is refused by [`ancestor_refusal`]'s
    /// `<form>`-in-`<form>` arm with a message that says so; including it in this
    /// table would replace that true message with a false "becomes a sibling".
    ///
    /// Every row was verified against parse5 8.0.1 in a standards-mode document;
    /// see the module report for the printed parses. Each asserts `RA014`, both
    /// tag names in the message, and the word "sibling".
    #[test]
    fn an_element_that_closes_its_own_kind_is_ra014() {
        // (body, outer tag, inner tag)
        let cases: &[(&str, &str, &str)] = &[
            ("<ul><li>a<li>b</li></li></ul>", "li", "li"),
            ("<ol><li>a<li>b</li></li></ol>", "li", "li"),
            ("<dl><dt>a<dt>b</dt></dt></dl>", "dt", "dt"),
            ("<dl><dd>a<dd>b</dd></dd></dl>", "dd", "dd"),
            // `dt` and `dd` close each other, in both directions.
            ("<dl><dt>a<dd>b</dd></dt></dl>", "dt", "dd"),
            ("<dl><dd>a<dt>b</dt></dd></dl>", "dd", "dt"),
            ("<div><h1>a<h1>b</h1></h1></div>", "h1", "h1"),
            ("<div><h6>a<h6>b</h6></h6></div>", "h6", "h6"),
            // Any heading closes any heading, across levels.
            ("<div><h1>a<h3>b</h3></h1></div>", "h1", "h3"),
            ("<div><h6>a<h1>b</h1></h6></div>", "h6", "h1"),
            (
                "<div><button>a<button>b</button></button></div>",
                "button",
                "button",
            ),
            (
                "<select><option>a<option>b</option></option></select>",
                "option",
                "option",
            ),
            (
                "<select><optgroup label=\"x\"><optgroup label=\"y\"></optgroup></optgroup></select>",
                "optgroup",
                "optgroup",
            ),
            ("<div><nobr>a<nobr>b</nobr></nobr></div>", "nobr", "nobr"),
            (
                "<table><tbody><tr><td>a<td>b</td></td></tr></tbody></table>",
                "td",
                "td",
            ),
            (
                "<table><tbody><tr><th>a<th>b</th></th></tr></tbody></table>",
                "th",
                "th",
            ),
            // `td` and `th` close each other, in both directions.
            (
                "<table><tbody><tr><td>a<th>b</th></td></tr></tbody></table>",
                "td",
                "th",
            ),
            (
                "<table><tbody><tr><th>a<td>b</td></th></tr></tbody></table>",
                "th",
                "td",
            ),
            // A row closes an open row: the second becomes a sibling of the first.
            (
                "<table><tbody><tr><td>a</td><tr><td>b</td></tr></tr></tbody></table>",
                "tr",
                "tr",
            ),
            // The outer element closes at the inner one *through* a wrapper that
            // does not stop the search (`span` is not a scope boundary here).
            ("<p>a<span><p>b</p></span></p>", "p", "p"),
            ("<ul><li>a<div><li>b</li></div></li></ul>", "li", "li"),
        ];

        for (body, outer, inner) in cases {
            let src = format!(
                "\"use island\"\nexport default function P(props: Record<never, never>) {{ return {body} }}\n"
            );
            let d = check(&src);
            assert_eq!(d.len(), 1, "{body}: {d:?}");
            assert_eq!(d[0].code, Code::RA014, "{body}");
            assert!(
                d[0].message.contains(&format!("`<{outer}>`")),
                "{body}: names the outer tag: {}",
                d[0].message
            );
            assert!(
                d[0].message.contains(&format!("`<{inner}>`")),
                "{body}: names the inner tag: {}",
                d[0].message
            );
            assert!(
                d[0].message.contains("sibling"),
                "{body}: says the inner becomes a sibling: {}",
                d[0].message
            );
        }
    }

    /// `SPECIAL` is an **exact** mirror of parse5's special-element set.
    ///
    /// The `Scan::Special` walk (the `li`/`dt`/`dd` rows) stops at the first
    /// special element that is not `address`/`div`/`p`. One extra tag makes it
    /// stop early and silently accept a nesting parse5 splits (`<li>a<search><li>b`
    /// → two items), one missing tag makes it run past a boundary and refuse legal
    /// markup. So the list is pinned member-for-member against the real set,
    /// transcribed from **parse5 8.0.1**, `dist/common/html.js`,
    /// `SPECIAL_ELEMENTS[NS.HTML]` (81 tags). A hand-edit that adds or removes a
    /// tag fails here and must be justified against parse5, not by taste.
    #[test]
    fn the_special_list_is_exactly_parse5s() {
        // `[...SPECIAL_ELEMENTS[NS.HTML]]` mapped through `TAG_ID` → name, from
        // parse5 8.0.1; dumped and diffed against `SPECIAL` in the task report.
        // Sorted; 81 members.
        let parse5_8_0_1: &[&str] = &[
            "address",
            "applet",
            "area",
            "article",
            "aside",
            "base",
            "basefont",
            "bgsound",
            "blockquote",
            "body",
            "br",
            "button",
            "caption",
            "center",
            "col",
            "colgroup",
            "dd",
            "details",
            "dir",
            "div",
            "dl",
            "dt",
            "embed",
            "fieldset",
            "figcaption",
            "figure",
            "footer",
            "form",
            "frame",
            "frameset",
            "h1",
            "h2",
            "h3",
            "h4",
            "h5",
            "h6",
            "head",
            "header",
            "hgroup",
            "hr",
            "html",
            "iframe",
            "img",
            "input",
            "li",
            "link",
            "listing",
            "main",
            "marquee",
            "menu",
            "meta",
            "nav",
            "noembed",
            "noframes",
            "noscript",
            "object",
            "ol",
            "p",
            "param",
            "plaintext",
            "pre",
            "script",
            "section",
            "select",
            "source",
            "style",
            "summary",
            "table",
            "tbody",
            "td",
            "template",
            "textarea",
            "tfoot",
            "th",
            "thead",
            "title",
            "tr",
            "track",
            "ul",
            "wbr",
            "xmp",
        ];

        let mut ours = SPECIAL.to_vec();
        let mut theirs = parse5_8_0_1.to_vec();
        ours.sort_unstable();
        theirs.sort_unstable();
        assert_eq!(
            ours, theirs,
            "SPECIAL must equal parse5 8.0.1's set exactly"
        );
        assert_eq!(ours.len(), 81, "parse5 8.0.1 has 81 special tags");

        // The two absences the round-3 ruling called out, asserted explicitly so a
        // reader sees them as decisions, not oversights.
        assert!(
            !SPECIAL.contains(&"search"),
            "`search` is NOT special in parse5; adding it loses a real nesting"
        );
        assert!(
            !SPECIAL.contains(&"dialog"),
            "`dialog` is NOT special in parse5; the li split is refused by the `li` row"
        );
    }

    /// `search` is not special, so a `<li>` inside one is **not** protected by the
    /// special-element walk: the parser closes the outer `<li>` at it. Before the
    /// round-3 fix `SPECIAL` contained `search`, the walk stopped there, and this
    /// markup was reported CLEAN while parse5 split it.
    ///
    /// RED (with `search` in `SPECIAL`): `check` returned `[]`.
    /// GREEN: `RA014`, naming both `<li>`s and "sibling".
    #[test]
    fn a_list_item_inside_search_still_closes_the_outer_item() {
        let src = "\"use island\"\nexport default function P(props: Record<never, never>) { return <ul><li>outer<search><li>x</li></search></li><li>tail</li></ul> }\n";
        let d = check(src);
        assert_eq!(d.len(), 1, "{d:?}");
        assert_eq!(d[0].code, Code::RA014);
        assert!(
            d[0].message.contains("`<li>`") && d[0].message.contains("sibling"),
            "names the closed `<li>` and says it becomes a sibling: {}",
            d[0].message
        );
    }

    /// The shapes that look like the table's rows but are **legal nesting**. The
    /// parser keeps every one of these exactly as written, so refusing any of them
    /// would be a false positive on ordinary markup.
    ///
    /// `<label>` inside `<label>` is the one the table must not gain by pattern
    /// matching: HTML explicitly allows a label to hold a label for a control
    /// inside it, and parse5 keeps the nesting.
    #[test]
    fn the_shapes_that_nest_legally_stay_clean() {
        for body in [
            "<label><label>x</label></label>",
            "<label>outer<input/><label>inner</label></label>",
            // Separate lists: two sibling lists, not a nested item. (A route
            // returns one root, so the page wraps them; the shapes under test are
            // the two lists and the two headings.)
            "<div><ul><li>a</li></ul><ul><li>b</li></ul></div>",
            // Sibling headings are siblings by construction, not by a split.
            "<div><h1>a</h1><h2>b</h2></div>",
            // Sibling cells in one row.
            "<table><tbody><tr><td>a</td><td>b</td></tr></tbody></table>",
            "<table><tbody><tr><th>a</th><th>b</th></tr></tbody></table>",
            "<dl><dt>a</dt><dd>b</dd></dl>",
            "<select><option>a</option></select>",
            "<select><option>a</option><option>b</option></select>",
            "<select><optgroup label=\"g\"><option>a</option></optgroup></select>",
            // A nested list inside an item is legal, and parse5 keeps it.
            "<ul><li>a<ul><li>b</li></ul></li></ul>",
            "<ol><li><ol><li>x</li></ol></li></ol>",
            // A nested description list inside a value is legal.
            "<dl><dd>a<dl><dt>x</dt><dd>y</dd></dl></dd></dl>",
            // A nested table inside a cell is legal.
            "<table><tbody><tr><td>a<table><tbody><tr><td>b</td></tr></tbody></table></td></tr></tbody></table>",
            // A heading inside a wrapper is not closed by a heading inside a
            // further wrapper: the wrapper bounds the search.
            "<h1>a<span><h2>b</h2></span></h1>",
            "<h1>a<div><h2>b</h2></div></h1>",
            // A list item separated from its list by a table cell is not closed
            // by an outer item: the cell bounds the search.
            "<ul><li>a<table><tbody><tr><td><li>b</li></td></tr></tbody></table></li></ul>",
        ] {
            let src = format!(
                "\"use island\"\nexport default function P(props: Record<never, never>) {{ return {body} }}\n"
            );
            let d = check(&src);
            assert!(d.is_empty(), "{body}: {d:?}");
        }
    }

    /// The `<select>` boundary keeps the **ancestor** rule silent, and the one
    /// diagnostic that remains tells the truth.
    ///
    /// Round 2 put `select` in `SCOPE_HTML`/`SCOPE_BUTTON` so the self-closing
    /// walk stops there, but `ancestor_refusal`'s `<a>`-in-`<a>` arm did not know
    /// about it, so `<a href="/1">x<select><a href="/2">y</a></select></a>` was
    /// refused **twice** — the second with "the inner `<a>` becomes a sibling",
    /// which parse5 contradicts: it parses as `(a href=/1 "x" (select "y"))`, the
    /// inner anchor is *discarded*.
    ///
    /// Each fixture below places the same tag inside the `<select>`, so it fails
    /// two ways if the boundary regresses: a second diagnostic appears when the
    /// ancestor arm fires, and the word "sibling" reappears when the false
    /// sentence does. The remaining `<select>` content-model message is honest —
    /// every one of these inner tags is in parse5's *discard* arm
    /// (`startTagInSelect`'s `default`), so it says the element is discarded and
    /// only its text kept, never that it becomes a sibling.
    #[test]
    fn the_select_boundary_keeps_the_ancestor_rule_silent() {
        for tag in ["p", "a", "button", "nobr"] {
            let open = if tag == "a" {
                "<a href=\"/1\">".to_string()
            } else {
                format!("<{tag}>")
            };
            let inner = if tag == "a" {
                "<a href=\"/2\">y</a>".to_string()
            } else {
                format!("<{tag}>b</{tag}>")
            };
            let body = format!("{open}a<select>{inner}</select></{tag}>");
            let src = format!(
                "\"use island\"\nexport default function P(props: Record<never, never>) {{ return {body} }}\n"
            );
            let d = check(&src);
            assert_eq!(d.len(), 1, "{body}: one element, one diagnostic: {d:?}");
            assert_eq!(d[0].code, Code::RA014, "{body}");
            assert!(
                d[0].message.contains("`<select>`"),
                "{body}: the select content model is what refuses it: {}",
                d[0].message
            );
            assert!(
                d[0].message.contains("discards"),
                "{body}: states parse5's real outcome (the element is discarded): {}",
                d[0].message
            );
            assert!(
                !d[0].message.contains("sibling"),
                "{body}: must NOT claim the discarded element becomes a sibling: {}",
                d[0].message
            );
        }

        // The `<optgroup>` arm had the same false "moves it out" sentence for the
        // discard class; parse5 keeps only the text (`(select (optgroup "x"))`).
        let src = "\"use island\"\nexport default function P(props: Record<never, never>) { return <select><optgroup><div>x</div></optgroup></select> }\n";
        let d = check(src);
        assert_eq!(d.len(), 1, "{d:?}");
        assert!(
            d[0].message.contains("`<optgroup>`") && d[0].message.contains("discards"),
            "the optgroup discard class is stated as a discard: {}",
            d[0].message
        );
        assert!(
            !d[0].message.contains("moves `<div>` out"),
            "must not claim a discarded child is moved out: {}",
            d[0].message
        );
    }

    /// Every legal nesting of the tags the required-ancestor rule governs stays
    /// clean — the rule refuses a position, not a tag.
    #[test]
    fn the_legal_ancestors_stay_clean() {
        for body in [
            "<table><tbody><tr><td>x</td></tr></tbody></table>",
            "<table><thead><tr><th>h</th></tr></thead></table>",
            "<table><tfoot><tr><td>f</td></tr></tfoot></table>",
            "<table><caption>c</caption></table>",
            "<table><colgroup><col/></colgroup></table>",
            "<select><option>a</option></select>",
            "<select><optgroup><option>a</option></optgroup></select>",
            "<figure><figcaption>c</figcaption></figure>",
            "<details><summary>s</summary></details>",
            "<fieldset><legend>l</legend></fieldset>",
        ] {
            let src = format!(
                "\"use island\"\nexport default function P(props: Record<never, never>) {{ return {body} }}\n"
            );
            let d = check(&src);
            assert!(d.is_empty(), "{body}: {d:?}");
        }
    }
}
