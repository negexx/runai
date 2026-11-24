//! Structured diagnostics.
//!
//! Every violation the compiler reports carries a stable [`Code`], a byte span,
//! a located `file:line:col`, an explanation, a fix, and the path to the matching
//! skill file. The JSON shape here is the contract consumed by `runai check
//! --json` and by agents, so field names and value encodings must not drift.
//!
//! The [`Code`] owns the text of `explain` and of `fix`. Rules choose a code and
//! a message; [`Diagnostic::new`] fills the rest. The file-specific part of a
//! repair therefore lives in `message`, and the canonical part lives in
//! [`Code::fix_template`], which is also what `runai skills` prints under
//! `## Fix` — so the diagnostic and its skill cannot drift.

use serde::Serialize;

/// The one claim every `RA009` text site makes, and the only thing the runtime
/// licenses: [`crate::analyze::rules`]'s `render_write` message, `Code::explain`,
/// `Code::fix_template`, the `RA009` skill's `## Why it is an error`, and the module
/// docs of [`crate::analyze::rules`] all take this sentence and no other.
///
/// A macro rather than a plain constant because the sites cannot all interpolate:
/// `explain` and `fix_template` are `&'static str` built by `concat!` (which takes
/// literals, not a `const`), and the module docs are `#![doc = …]`, whose value must
/// be a literal too. This expansion is the single literal they share.
macro_rules! ra009_consequence {
    () => {
        "The component body runs once, so this write cannot re-run the component."
    };
}
pub(crate) use ra009_consequence;

/// The claim every `RA009` text site must make and no other: the one sentence
/// [`crate::analyze::rules`]'s `render_write`, `Code::explain`, `Code::fix_template`,
/// the `RA009` skill's `## Why it is an error`, and the module docs of
/// [`crate::analyze::rules`] all carry verbatim.
pub(crate) const RA009_CONSEQUENCE: &str = ra009_consequence!();

/// Every claim about a render-time write that `packages/runai/runtime.js` disproves,
/// matched as a case-insensitive substring. Kept here, once, because two test modules
/// assert it — [`crate::skills`] against the `RA009` skill body and
/// [`crate::analyze::rules`] against the rendered message — and two copies would let
/// one be reworded while the other kept agreeing with itself.
///
/// Each entry was a sentence this task's text actually carried:
///
/// - `stale`: "every such reader stays stale" — the runtime flushes writes
///   synchronously, so a live subscriber *does* see the new value.
/// - `re-trigger`, `re-run the render`: a component body is never invoked again
///   (`mount` calls `component({})` once), so no write can re-run it.
/// - `updates no reader`: the `stale` claim from the other direction.
/// - `already rendered`: the premise of the stale claim; a reader that already
///   rendered is exactly the one that updates.
///
/// `cfg(test)`: every user is a test module (this file's, [`crate::skills`]'s, and
/// [`crate::analyze::rules`]'s), so a non-test build would have no referent for it.
#[cfg(test)]
pub(crate) const RA009_FALSE_CLAIMS: [&str; 5] = [
    "stale",
    "re-trigger",
    "re-run the render",
    "updates no reader",
    "already rendered",
];

/// Every claim about a self-writing effect that `packages/runai/runtime.js`
/// disproves, matched as a case-insensitive substring.
///
/// The runtime does not loop: `execute` throws `effect cycle: an effect wrote a
/// signal it reads` when it is asked to run an effect that is already running
/// (`runtime.js:167-171`), and `flush` throws the same error when effects keep
/// invalidating each other past the wave limit (`:195-200`). A same-value write is
/// an early return in the setter (`:246`), so it does not notify at all.
///
/// Each entry is a sentence this project's RA002 text actually carried:
///
/// - `causes the next run in turn` — the explain's round-1 wording.
/// - `schedules the next one` / `which is a loop` — the skill's round-1 wording.
/// - `each run schedules` — the `rules.rs` module doc's version.
#[cfg(test)]
pub(crate) const RA002_FALSE_CLAIMS: [&str; 4] = [
    "causes the next run in turn",
    "schedules the next one",
    "each run schedules",
    "which is a loop",
];

/// The sentence every `RA002` text site must carry: the mechanism is a throw.
#[cfg(test)]
pub(crate) const RA002_THROWS: &str = "effect cycle";

/// A stable diagnostic code. Codes are never reused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Code {
    RA001,
    RA002,
    RA006,
    RA007,
    RA009,
    RA010,
    RA011,
    RA012,
    RA014,
}

impl Code {
    /// Every code, in code order.
    pub fn all() -> &'static [Code] {
        &ALL
    }

    /// The canonical, stable string form, e.g. `"RA001_UNTRACKED_READ"`.
    pub fn as_str(&self) -> &'static str {
        match self {
            Code::RA001 => "RA001_UNTRACKED_READ",
            Code::RA002 => "RA002_EFFECT_WRITE",
            Code::RA006 => "RA006_UNANNOTATED_PROPS",
            Code::RA007 => "RA007_AMBIGUOUS_JSX",
            Code::RA009 => "RA009_IMPLICIT_EFFECT",
            Code::RA010 => "RA010_STATIC_VIOLATION",
            Code::RA011 => "RA011_BAD_ROUTE_EXPORT",
            Code::RA012 => "RA012_MISSING_MODE",
            Code::RA014 => "RA014_NOT_SUPPORTED_YET",
        }
    }

    /// The skill file for this code, e.g. `RA001_UNTRACKED_READ.md`.
    ///
    /// A bare file name, because the directory it is written to is the caller's
    /// choice: `runai skills --out <dir>`, and [`Code::doc_path`] for the shipped
    /// location.
    pub fn skill_file(&self) -> String {
        format!("{}.md", self.as_str())
    }

    /// Where an installed project finds this code's skill, e.g.
    /// `node_modules/runai/skills/RA001_UNTRACKED_READ.md`. This is the `doc`
    /// every diagnostic carries.
    pub fn doc_path(&self) -> String {
        format!("node_modules/runai/skills/{}", self.skill_file())
    }

    /// What the code means, in the words an agent triages on. `## What it means`
    /// in the generated skill is this text.
    pub fn explain(&self) -> &'static str {
        match self {
            Code::RA001 => {
                "A signal is read directly in the component body, outside any tracking scope. The \
                 read runs once and never again: no effect owns it, so nothing re-runs when the \
                 signal changes, and the value on screen is the value at first render. A read \
                 whose nearest enclosing function is *not* the component body — an event handler, \
                 or any other nested callback such as `setTimeout(() => n())` — is not this rule: \
                 it runs when that callback runs."
            }
            Code::RA002 => {
                "A signal is written inside an `effect(...)` argument. An effect re-runs for the \
                 signals it reads, and a write in its body executes as part of that run, so the \
                 write is a side effect of the effect rather than a derived value. When the effect \
                 also reads the signal it writes, the write during a run queues that same effect, \
                 and asking it to run while it is already running does not loop: the runtime \
                 throws `effect cycle: an effect wrote a signal it reads` (and a wave limit throws \
                 when effects keep invalidating each other). A write of the value the signal \
                 already holds never notifies anyone — the setter bails out first — so it neither \
                 re-runs nor throws."
            }
            Code::RA006 => {
                "A component's props parameter is missing an explicit type annotation, or the \
                 component takes no parameter at all. Props are the only way data enters a \
                 component, so their type is the written answer to \"where does this value come \
                 from?\", and a propless component has to write that answer down too."
            }
            Code::RA007 => {
                "Either the parser rejected this file before it could be compiled, or a line \
                 break sits between two inline elements. Nothing downstream — lowering, analysis, \
                 codegen — can run on a tree the parser failed to produce, so the parser's own \
                 message and location are reported instead of a guess at what was meant. A line \
                 break between two inline elements is the other case: JSX deletes whitespace-only \
                 text that contains a newline, so the space the author saw in the editor is not \
                 the space the page shows."
            }
            Code::RA009 => {
                concat!(
                    "A signal is written during render: in the component body or in a JSX \
                     expression container. ",
                    ra009_consequence!(),
                    " Rendering must not have side effects."
                )
            }
            Code::RA010 => {
                "A `\"use static\"` route ships a construct that needs JavaScript a prerendered \
                 page does not have: a signal, a derived value, an effect, a resource, a write, a \
                 runtime primitive, or code the markup itself carries — an inline `on…` handler, \
                 a literal `<script>` element. The page ships zero bytes of JavaScript, so a \
                 runtime feature would silently do nothing, while an inline handler or a script \
                 element is code the served page would run — neither can be on a page that makes \
                 the 0 KB claim."
            }
            Code::RA011 => {
                "A page file's default export is missing, is not a function this milestone can \
                 analyze, takes parameters a route cannot be given, or does not return JSX from \
                 the body. The router renders the default export as the route, and only a direct \
                 `return <…/>` is recognized, because resolving what a returned value renders \
                 needs type information this compiler does not have yet."
            }
            Code::RA012 => {
                "The file does not open with a `\"use static\"` or `\"use island\"` directive. The \
                 directive decides whether the route ships JavaScript, and inferring it would \
                 hide the decision that controls the 0 KB guarantee, so it is never inferred."
            }
            Code::RA014 => {
                "The parser accepted this file: this is a limitation of the compiler, not a \
                 syntax error. It covers five families. A first is a JSX construct this milestone \
                 cannot lower (a component tag, a spread, a fragment root, and so on), where \
                 lowering reports the construct instead of dropping it, because markup emitted \
                 without part of a node is wrong in a way nothing reports. That first family also \
                 covers four emit-time shapes whose output would differ by mode or not parse: \
                 children inside a void element, a duplicate attribute, an early `return` before \
                 the route's JSX return, and an `async` or generator route component. It also \
                 covers markup the HTML parser would reshape, so the emitted HTML text re-parses \
                 into a different tree than was written: a `<p>` holding a block-level element, a \
                 tag that closes an open element of its own kind, a child outside its parent's \
                 content model or bare text outside a table cell, an `<a>` inside an `<a>` or a \
                 `<form>` inside a `<form>`, a list item with no list, a tag the parser only builds \
                 under one parent, and — in an island — a dynamic binding inside a text-only \
                 element, where the marker the island needs is swallowed as text. A second is \
                 a module the emitted file cannot resolve or write: an `import` or re-export of a \
                 module other than `\"runai\"` has no bundler, a default or namespace import of \
                 `\"runai\"` is not a form the client module emits, and `import x = require(…)` or \
                 `export =` is rewritten by type stripping into `require`/`module.exports`, which \
                 an ES module does not have — each route is emitted from its own file, so a second \
                 module cannot be resolved or inlined. An island's import list is built from the \
                 runtime's actual exports, so a named specifier the runtime does not provide — \
                 `import { _a } from \"runai\"`, where `_a` is one of the emitter's own locals — is \
                 refused too: the artifact would ask the engine for a binding that does not exist \
                 and fail to load. A re-export is refused even when its source \
                 is `\"runai\"`, because the emitted module never carries one, so accepting it would \
                 drop the statement silently. The file's own export list is part of this family too: \
                 a name exported twice — `export const s` beside `export { s }`, or two \
                 `export { x }` statements — is refused because an ES module may export a name \
                 once, so the module would be a syntax error rather than a page. A third is a \
                 value the generated module cannot see — \
                 a `\"runai\"` name or the route component's own name at module scope, in a \
                 component-body statement, or in a \
                 `\"use static\"` route's markup, whose prerender module has no imports and does \
                 not declare the component, or the route's own parameter there, which the \
                 prerender call supplies with nothing, or at module scope, where no emitter \
                 declares it. A module-level declaration is *not* in this family: module-scope \
                 statements are emitted above the component. Neither is a component-body \
                 statement's own local, which is emitted inside the component. A `declare` \
                 statement is refused: it is the fourth family, because type stripping erases it \
                 before the route is emitted, so the binding it introduces never exists in the \
                 generated module — a route file defines the values it names. A fifth is the \
                 emitted module's own names: a declaration, export name, or reference to `_esc`, \
                 `_attr`, or `render` is refused because the generated module declares those \
                 itself."
            }
        }
    }

    /// The canonical fix for this code — the single source for both a
    /// diagnostic's `fix` and the skill's `## Fix` section.
    ///
    /// It must be legal advice on its own. The per-file specifics (the component
    /// name, the construct, the line to edit) are carried by the diagnostic's
    /// `message`, not by this text.
    pub fn fix_template(&self) -> &'static str {
        match self {
            Code::RA001 => {
                "Wrap the read in a tracking scope — `derived(() => count() * 2)`, an \
                 `effect(...)`, a `resource(...)`, or a JSX slot — or call it inside an event \
                 handler."
            }
            Code::RA002 => {
                "Derive the value instead of writing it, or move the write into the event handler \
                 that causes the change, and let the effect only read what it needs."
            }
            // Pinned verbatim: later tasks assert this exact text.
            Code::RA006 => {
                "Give the component exactly one typed parameter. With no props, write (props: Record<never, never>)."
            }
            Code::RA007 => {
                "Fix the syntax error the parser reported in `message`. In route files the usual \
                 causes are an unclosed element, an unterminated string inside an attribute, or \
                 adjacent sibling elements — add the missing `>` or closing element, close the \
                 attribute string, or wrap the siblings in one parent element. When `message` \
                 names a line break between inline elements instead, the repair is the one it \
                 carries. Put both on one line, or write {\" \"} where you want the space."
            }
            // Deliberately *not* "move it into an effect": `RA002` fires for every write whose
            // nearest function is the function passed to `effect(...)`, so that repair would
            // land the agent straight in `RA002` at the same span.
            Code::RA009 => {
                concat!(
                    "Derive the value instead of writing it, or move the write into the event \
                     handler that causes the change. ",
                    ra009_consequence!()
                )
            }
            Code::RA010 => {
                "Change this file's \"use static\" directive to \"use island\" to ship JavaScript \
                 for this route."
            }
            Code::RA011 => {
                "Add a default export if it is missing. Replace the existing default export if it \
                 is not a component, rather than declaring a second one. Either way it must take \
                 at most one annotated `props` parameter and return JSX directly from its body."
            }
            Code::RA012 => {
                "Add \"use static\" (prerendered to HTML, no JavaScript) or \"use island\" \
                 (hydrated on boot) as the first statement of the file. Single and double quotes \
                 are both accepted; escapes such as \"use\\x20static\" are not."
            }
            Code::RA014 => {
                "For an import or re-export, inline the value or import it from `\"runai\"`; a \
                 re-export whose source is `\"runai\"` becomes `import { owner } from \"runai\"` \
                 with the name used here; a \
                 default or namespace `\"runai\"` import is not a form the emitted module writes, \
                 so rewrite it as named imports; a name the runtime does not export is not imported \
                 by any `\"runai\"` specifier, so use one of the names the message lists, or import \
                 an `index.d.ts` type with `import { type … }`; write `import x = require(…)` as \
                 an `import`, and `export =` as `export default` or a named export. For a \
                 `declare` statement, replace it with a value this file defines, because it is \
                 erased before emission. \
                 For a route signature — a default, a destructuring pattern, a rest parameter, or \
                 a different name — the emitters rebuild the parameter list as one `props` \
                 parameter, so write the parameter as `(props: Record<never, never>)` when it \
                 carries no data, or with the type it actually needs, and read the fields from \
                 `props`. For a value \
                 the generated module cannot see — a `\"runai\"` name, or the route's own \
                 component, at module scope, in a component-body statement, or in a `\"use static\"` \
                 route's markup, or the route's own parameter in a \
                 `\"use static\"` route — do what the message names: change the directive to \
                 `\"use island\"` for the first two, and replace the parameter reference with a \
                 literal value. For a JSX construct, write the spread out, move the JSX into the \
                 children or the returned tree, give the attribute or prop a value, wrap a root \
                 fragment in one parent element, render a runtime primitive's markup directly, or \
                 keep exactly one JSX return. \
                 For children inside a void element, move them out of it or use an element that \
                 accepts children. For a duplicate attribute, keep exactly one of them, because a \
                 prerendered page keeps the first value and a hydrated island keeps the last. \
                 For an early `return` before the route's JSX return, return one element from \
                 the component: `render()` returns one string, so the body cannot exit early. \
                 For an `async` or generator route component, remove the modifier and return the \
                 markup directly, because both modes call the component synchronously. \
                 For a declaration, export name, or reference that uses a reserved generated \
                 name (`_esc`, `_attr`, or `render`), rename it, because the emitted module \
                 declares those names itself. For a name the file exports twice, delete one of \
                 the two exports or export one under a different name, because an ES module may \
                 export a name once."
            }
        }
    }
}

static ALL: [Code; 9] = [
    Code::RA001,
    Code::RA002,
    Code::RA006,
    Code::RA007,
    Code::RA009,
    Code::RA010,
    Code::RA011,
    Code::RA012,
    Code::RA014,
];

impl Serialize for Code {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

/// The first `<…>`-shaped placeholder in a user-facing string, if any.
///
/// The predicate is *a `<` immediately followed by an ASCII uppercase letter*:
/// `<Type>`, `<Name>`, `<For>`, `<Parent>`. Text an agent is meant to type must
/// never carry one, because `<Type>` is not TypeScript — applying it verbatim is
/// a syntax error, and `<For>` is a construct this milestone refuses.
///
/// It takes **any** user-facing string (a fix, an explain, a skill body, a
/// per-site message) so the same rule can be applied everywhere text is composed.
/// A message that legitimately *names* a symbol (`<Greeting>` names a component)
/// still contains one: that is why [`instruction_bracket`] exists.
pub fn angle_bracket_placeholder(text: &str) -> Option<(usize, &str)> {
    bracketed(text, |_| true)
}

/// The first `<…>` in **instruction** text that an agent cannot type.
///
/// Same scan, one allowance: a bracketed name that is a plain DOM tag — all
/// lowercase ASCII, optionally hyphenated (`<div>`, `<td>`, `<my-element>`) — is a
/// thing to write, not a placeholder. Everything else is a placeholder, including
/// the runtime's own primitives (`<For>`, `<Show>`, `<When>`), which this
/// milestone refuses, and a metavariable (`<Type>`, `<Name>`).
pub fn instruction_bracket(text: &str) -> Option<(usize, &str)> {
    bracketed(text, |token| {
        !token.chars().all(|c| c.is_ascii_lowercase() || c == '-')
    })
}

/// Scan for `<Name>`, keeping the hits `keep` names.
///
/// The returned slice runs from the `<` through the closing `>` when there is
/// one, so a failure quotes the exact text an agent would type.
fn bracketed(text: &str, keep: impl Fn(&str) -> bool) -> Option<(usize, &str)> {
    let bytes = text.as_bytes();
    for (i, window) in bytes.windows(2).enumerate() {
        if window[0] != b'<' || !window[1].is_ascii_uppercase() {
            continue;
        }
        // Start **after** the `<`, which is itself not a name character.
        let end = text[i + 1..]
            .find(|c: char| !(c.is_alphanumeric() || c == '_' || c == '-' || c == '.'))
            .map_or(text.len(), |offset| i + 1 + offset);
        let name = &text[i + 1..end];
        if keep(name) {
            let token_end = if text[end..].starts_with('>') {
                end + 1
            } else {
                end
            };
            return Some((i, &text[i..token_end]));
        }
    }
    None
}

/// The **instruction half** of a diagnostic message.
///
/// Every rule writes its message in two parts and joins them with `": "`: the
/// first names the construct, the second tells the agent what to write. A `<For>`
/// in the naming half names what the file contains; the same shape in the
/// instruction half is something the agent is asked to type. A message with no
/// `": "` is all instruction, so it is returned whole.
pub fn instruction_text(message: &str) -> &str {
    message
        .split_once(": ")
        .map_or(message, |(_, instruction)| instruction)
}

/// How loudly a diagnostic should be reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Error,
    Warning,
}

/// One compiler diagnostic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub code: Code,
    pub severity: Severity,
    /// The file the diagnostic belongs to, set by [`Diagnostic::locate`]. Empty
    /// until located, because the rules that build a diagnostic are given source
    /// text, not a path.
    pub file: String,
    /// 1-based line of the span start. `0` means "not located yet".
    pub line: u32,
    /// 1-based column of the span start, counted in characters.
    pub col: u32,
    pub span: (u32, u32),
    pub message: String,
    pub explain: String,
    pub fix: String,
    pub doc: String,
    pub patch: Option<String>,
}

impl Diagnostic {
    /// Build a diagnostic from the only two things a rule chooses: the code and
    /// the message.
    ///
    /// `severity`, `explain`, `fix`, and `doc` come from the code, so a rule
    /// cannot report a fix that disagrees with the code's skill. `file`, `line`,
    /// and `col` are filled by [`Diagnostic::locate`] once a path is known.
    pub fn new(code: Code, span: (u32, u32), message: impl Into<String>) -> Diagnostic {
        Diagnostic {
            code,
            severity: Severity::Error,
            file: String::new(),
            line: 0,
            col: 0,
            span,
            message: message.into(),
            explain: code.explain().to_string(),
            fix: code.fix_template().to_string(),
            doc: code.doc_path(),
            patch: None,
        }
    }

    /// Attach the file and the 1-based position of the span start.
    ///
    /// `analyze` calls this on every diagnostic before returning, so `check
    /// --json` over many files says where each error is. Line and column come
    /// from [`line_col`], which clamps a bad span instead of panicking.
    pub fn locate(&mut self, source: &str, path: &str) {
        let (line, col) = line_col(source, self.span);
        self.file = path.to_string();
        self.line = line as u32;
        self.col = col as u32;
    }

    /// Serialize as a single JSON object. `span` is `[start, end]`; `patch` is
    /// `null` when absent rather than omitted.
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("a diagnostic always serializes")
    }

    /// Serialize a list of diagnostics as one JSON array.
    pub fn render_all_json(diags: &[Diagnostic]) -> String {
        serde_json::to_string(diags).expect("diagnostics always serialize")
    }

    /// Render for a human: `file:line:col`, then message, explanation, fix, doc.
    ///
    /// A located diagnostic prints the position [`Diagnostic::locate`] stored, so
    /// the human and JSON forms can never disagree. An unlocated one derives it
    /// from `source` rather than printing a meaningless `:0:0`.
    pub fn render_human(&self, source: &str) -> String {
        let (line, col) = if self.line == 0 {
            line_col(source, self.span)
        } else {
            (self.line as usize, self.col as usize)
        };
        format!(
            "{file}:{line}:{col}: {severity}[{code}]: {message}\n  explain: {explain}\n  fix: {fix}\n  doc: {doc}\n",
            file = self.file,
            severity = match self.severity {
                Severity::Error => "error",
                Severity::Warning => "warning",
            },
            code = self.code.as_str(),
            message = self.message,
            explain = self.explain,
            fix = self.fix,
            doc = self.doc,
        )
    }
}

/// The 1-based `(line, column)` a diagnostic names, clamped to the source.
///
/// The position is the start of the span: `file:line:col` points at where the
/// problem is, not where the construct closes. Zero-width spans name their own
/// position, which is what `RA012_MISSING_MODE` (`(0, 0)`) needs.
///
/// The span is a byte offset, which may land inside a multi-byte UTF-8
/// character (transformed sources, `.tsx` text with non-ASCII JSX). Such an
/// offset is floored to the nearest character boundary rather than slicing
/// mid-character, which would panic. The column counts characters, not bytes,
/// so non-ASCII earlier on the line does not inflate it.
fn line_col(source: &str, span: (u32, u32)) -> (usize, usize) {
    let mut start = (span.0 as usize).min(source.len());
    while start > 0 && !source.is_char_boundary(start) {
        start -= 1;
    }
    let before = &source[..start];
    let line = before.matches('\n').count() + 1;
    let line_start = before.rfind('\n').map_or(0, |i| i + 1);
    (line, before[line_start..].chars().count() + 1)
}

impl Serialize for Diagnostic {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;

        let mut map = serializer.serialize_map(Some(11))?;
        map.serialize_entry("code", &self.code)?;
        map.serialize_entry("severity", &self.severity)?;
        map.serialize_entry("file", &self.file)?;
        map.serialize_entry("line", &self.line)?;
        map.serialize_entry("col", &self.col)?;
        map.serialize_entry("span", &[self.span.0, self.span.1])?;
        map.serialize_entry("message", &self.message)?;
        map.serialize_entry("explain", &self.explain)?;
        map.serialize_entry("fix", &self.fix)?;
        map.serialize_entry("doc", &self.doc)?;
        map.serialize_entry("patch", &self.patch)?;
        map.end()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Byte 27 is the `c` of `count()` on line 2: line 1 is 15 bytes including
    // its newline, and line 2 starts with `  const x = ` (12 bytes).
    const SRC: &str = "function A() {\n  const x = count()\n}\n";

    fn sample() -> Diagnostic {
        let mut d = Diagnostic::new(
            Code::RA001,
            (27, 34),
            "signal read outside a tracking scope",
        );
        d.locate(SRC, "src/routes/index.page.tsx");
        d
    }

    #[test]
    fn code_strings_are_stable() {
        assert_eq!(Code::RA001.as_str(), "RA001_UNTRACKED_READ");
        assert_eq!(Code::RA010.as_str(), "RA010_STATIC_VIOLATION");
        assert_eq!(Code::RA014.as_str(), "RA014_NOT_SUPPORTED_YET");
        assert_eq!(Code::all().len(), 9);
    }

    #[test]
    fn constructor_fills_fix_and_doc_from_the_code() {
        let d = sample();
        assert_eq!(d.fix, Code::RA001.fix_template());
        assert_eq!(d.explain, Code::RA001.explain());
        assert_eq!(d.doc, "node_modules/runai/skills/RA001_UNTRACKED_READ.md");
        assert_eq!(d.severity, Severity::Error);
        assert!(d.patch.is_none());
    }

    #[test]
    fn locate_computes_one_based_line_and_col() {
        let d = sample();
        assert_eq!((d.line, d.col), (2, 13));
        assert_eq!(d.file, "src/routes/index.page.tsx");
    }

    #[test]
    fn json_has_every_field_and_null_patch() {
        let json = sample().to_json();
        for key in [
            "code", "severity", "file", "line", "col", "span", "message", "explain", "fix", "doc",
            "patch",
        ] {
            assert!(
                json.contains(&format!("\"{key}\"")),
                "missing {key} in {json}"
            );
        }
        assert!(json.contains("\"patch\":null"));
    }

    #[test]
    fn human_output_names_the_line_and_the_fix() {
        let out = sample().render_human(SRC);
        assert!(out.contains("RA001_UNTRACKED_READ"));
        assert!(out.contains("src/routes/index.page.tsx:2:13"));
        assert!(out.contains(Code::RA001.fix_template()));
    }

    #[test]
    fn json_array_is_valid_json() {
        let out = Diagnostic::render_all_json(&[sample(), sample()]);
        let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed.as_array().unwrap().len(), 2);
    }

    #[test]
    fn json_encodings_match_the_agent_contract() {
        let parsed: serde_json::Value = serde_json::from_str(&sample().to_json()).unwrap();
        assert_eq!(parsed["code"], "RA001_UNTRACKED_READ");
        assert_eq!(parsed["severity"], "error");
        assert_eq!(parsed["file"], "src/routes/index.page.tsx");
        assert_eq!(parsed["line"], 2);
        assert_eq!(parsed["col"], 13);
        assert_eq!(parsed["span"], serde_json::json!([27, 34]));
        assert!(parsed["patch"].is_null());

        let mut with_patch = sample();
        with_patch.severity = Severity::Warning;
        with_patch.patch = Some("--- a\n+++ b\n".into());
        let parsed: serde_json::Value = serde_json::from_str(&with_patch.to_json()).unwrap();
        assert_eq!(parsed["severity"], "warning");
        assert_eq!(parsed["patch"], "--- a\n+++ b\n");
    }

    #[test]
    fn skill_file_follows_the_doc_path() {
        assert_eq!(Code::RA001.skill_file(), "RA001_UNTRACKED_READ.md");
        assert_eq!(Code::RA010.skill_file(), "RA010_STATIC_VIOLATION.md");
        assert_eq!(
            Code::RA001.doc_path(),
            "node_modules/runai/skills/RA001_UNTRACKED_READ.md"
        );
        assert_eq!(
            Code::RA014.doc_path(),
            "node_modules/runai/skills/RA014_NOT_SUPPORTED_YET.md"
        );
    }

    #[test]
    fn render_human_reports_path_line_and_column() {
        let src = "function A() {\n  const x = count()\n}\n";
        let mut diag = Diagnostic::new(Code::RA001, (27, 32), "signal read");
        // `count` begins at byte 27, which is line 2, column 13.
        diag.locate(src, "src/routes/index.page.tsx");
        let out = diag.render_human(src);
        assert!(out.starts_with("src/routes/index.page.tsx:2:13: error[RA001_UNTRACKED_READ]"));
    }

    /// A diagnostic that was never located still renders a real position, derived
    /// from the source the caller is rendering against.
    #[test]
    fn render_human_survives_spans_outside_the_source() {
        let mut diag = Diagnostic::new(Code::RA001, (10, 15), "signal read");
        diag.locate("short", "a.tsx");
        let out = diag.render_human("short");
        assert!(out.contains("a.tsx:1:6"));

        // Not located at all: `file` is empty but the position is still real.
        let unlocated = Diagnostic::new(Code::RA001, (10, 15), "signal read");
        let out = unlocated.render_human("short");
        assert!(
            out.starts_with(":1:6: error[RA001_UNTRACKED_READ]"),
            "{out}"
        );
    }

    /// `é` is two bytes, so every byte offset after it is shifted by one.
    const MULTIBYTE_SRC: &str = "const é = 1\n  const x = count()\n";

    #[test]
    fn render_human_counts_columns_in_characters() {
        let mut diag = Diagnostic::new(Code::RA001, (25, 30), "signal read");
        // Byte 13 starts line 2; `count` starts at byte 25, which is 12
        // characters into line 2, so the 1-based column is 13.
        diag.locate(MULTIBYTE_SRC, "src/multi.tsx");
        let out = diag.render_human(MULTIBYTE_SRC);
        assert!(out.starts_with("src/multi.tsx:2:13: error[RA001_UNTRACKED_READ]"));
    }

    #[test]
    fn render_human_floors_spans_inside_a_multibyte_char() {
        let mut diag = Diagnostic::new(Code::RA001, (7, 8), "signal read");
        // Byte 7 is the second byte of `é` (bytes 6..8): not a char boundary.
        // It floors to byte 6, so the column is that of `é`'s line, column 7.
        diag.locate(MULTIBYTE_SRC, "src/multi.tsx");
        let out = diag.render_human(MULTIBYTE_SRC);
        assert!(out.starts_with("src/multi.tsx:1:7: error[RA001_UNTRACKED_READ]"));
    }

    #[test]
    fn render_human_column_ignores_multibyte_width_on_the_same_line() {
        let src = "  const é = count()\n";
        let mut diag = Diagnostic::new(Code::RA001, (13, 18), "signal read");
        // `count` starts at byte 13; a byte column would say 14, a character
        // column says 13.
        diag.locate(src, "src/same_line.tsx");
        let out = diag.render_human(src);
        assert!(out.starts_with("src/same_line.tsx:1:13: error[RA001_UNTRACKED_READ]"));
    }

    #[test]
    fn all_codes_are_listed_once_in_order() {
        let codes: Vec<&str> = Code::all().iter().map(Code::as_str).collect();
        assert_eq!(
            codes,
            vec![
                "RA001_UNTRACKED_READ",
                "RA002_EFFECT_WRITE",
                "RA006_UNANNOTATED_PROPS",
                "RA007_AMBIGUOUS_JSX",
                "RA009_IMPLICIT_EFFECT",
                "RA010_STATIC_VIOLATION",
                "RA011_BAD_ROUTE_EXPORT",
                "RA012_MISSING_MODE",
                "RA014_NOT_SUPPORTED_YET",
            ]
        );
    }

    /// Two canonical fixes are quoted verbatim by later tasks and by the skills
    /// shipped to users. They are pinned here so a reword cannot drift silently.
    #[test]
    fn the_pinned_fix_texts_are_exact() {
        assert_eq!(
            Code::RA006.fix_template(),
            "Give the component exactly one typed parameter. With no props, write (props: Record<never, never>)."
        );
        assert_eq!(
            Code::RA010.fix_template(),
            "Change this file's \"use static\" directive to \"use island\" to ship JavaScript for this route."
        );
    }

    /// Every code owns both halves of its text; a blank one is a diagnostic and a
    /// skill file that tell an agent nothing.
    #[test]
    fn every_code_explains_itself_and_names_a_fix() {
        for code in Code::all() {
            assert!(!code.explain().is_empty(), "{}", code.as_str());
            assert!(code.fix_template().len() > 20, "{}", code.as_str());
        }
    }

    /// A `fix_template` is an instruction, so every character in it must be
    /// typeable. `<Name>` is fine in a *message*, where it names a symbol; in a
    /// fix, `<Type>` is a placeholder an agent cannot apply verbatim.
    ///
    /// This delegates to [`angle_bracket_placeholder`], the one predicate, so the
    /// rule here and the corpus-wide check in [`crate::instruction`] cannot
    /// disagree. The class-level coverage (every per-site message the rules
    /// compose, every shared refusal composer, every applied repair) lives there;
    /// this is the fix-only half, beside the text it constrains.
    #[test]
    fn no_fix_template_contains_an_angle_bracket_placeholder() {
        for code in Code::all() {
            let fix = code.fix_template();
            if let Some((at, token)) = angle_bracket_placeholder(fix) {
                panic!(
                    "{}: fix_template carries a `<…>` placeholder at byte {at}: {token} in {fix}",
                    code.as_str()
                );
            }
        }
    }

    /// `RA010` fires wherever the directive sits, not only on line 1: `detect_mode`
    /// accepts a comment above it. A fix that names a line tells the agent to edit
    /// whatever is on that line, which may be the comment — and the next run
    /// reports the same thing.
    ///
    /// The assertion is "mentions no line **at all**", not "does not say `line 1`":
    /// the earlier form let `the first line` and `line one` through. The six
    /// per-site directive repairs get the same treatment in
    /// [`crate::instruction`]'s `every_directive_repair_names_no_line`.
    #[test]
    fn ra010_fix_names_no_line() {
        let fix = Code::RA010.fix_template();
        for word in fix.split(|c: char| !c.is_ascii_alphabetic()) {
            assert!(
                !word.eq_ignore_ascii_case("line") && !word.eq_ignore_ascii_case("lines"),
                "RA010's fix must not mention a line at all: {fix}"
            );
        }
    }

    /// The shared sentence and the denied list are complements: the sentence must not
    /// itself contain a denied claim, or every site that uses it would fail the very
    /// assertion meant to catch a reword. This is the one test that reads both, and it
    /// is why the sentence cannot be swapped for the round-1 wording unnoticed.
    #[test]
    fn the_shared_consequence_is_not_itself_a_denied_claim() {
        for false_claim in RA009_FALSE_CLAIMS {
            assert!(
                !RA009_CONSEQUENCE.to_ascii_lowercase().contains(false_claim),
                "the shared sentence carries the denied claim `{false_claim}`: {RA009_CONSEQUENCE}"
            );
        }
    }

    /// The RA002 texts must describe what the runtime **does**, not what a loop
    /// would do. The message, `rules.rs`, and the fix were corrected in an earlier
    /// round; the explain and the skill still said "that write causes the next run
    /// in turn" / "each run schedules the next one, which is a loop", and those are
    /// the two sites an agent reads when the message is not enough.
    ///
    /// The runtime throws (`execute`'s `e.running` guard, and `flush`'s wave
    /// limit), and a same-value write does not even notify (`Object.is` early
    /// return in the setter). Both facts are asserted, so a rewording cannot drift
    /// back to the loop claim, and a version that omits the same-value case fails
    /// too.
    #[test]
    fn the_ra002_texts_state_the_throw_and_not_a_loop() {
        let sites: Vec<(&str, String)> = vec![
            ("explain", Code::RA002.explain().to_string()),
            ("skill body", crate::skills::skill_body(Code::RA002)),
            (
                "message",
                crate::analyze::rules::ra002_message_for_test("n"),
            ),
        ];

        for (what, text) in &sites {
            for false_claim in RA002_FALSE_CLAIMS {
                assert!(
                    !text.to_ascii_lowercase().contains(false_claim),
                    "the RA002 {what} still claims a loop (`{false_claim}`): {text}"
                );
            }
        }

        // The mechanism is a throw, and it must be named in both sites that
        // explain rather than instruct.
        for what in ["explain", "skill body"] {
            let text = &sites.iter().find(|(name, _)| *name == what).unwrap().1;
            assert!(
                text.contains(RA002_THROWS),
                "the RA002 {what} must name the throw (`{RA002_THROWS}`): {text}"
            );
            assert!(
                text.contains("returns early")
                    || text.contains("before notifying")
                    || text.contains("notifies nobody")
                    || text.contains("never notifies")
                    || text.contains("bails out"),
                "the RA002 {what} must name the same-value early return (the setter short-circuits \
                 before notifying), not merely contain a negation: {text}"
            );
        }
    }
}
