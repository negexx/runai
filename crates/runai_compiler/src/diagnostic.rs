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
                "A signal is written inside an `effect(...)` argument. An effect re-runs when \
                 the signals it reads change, so writing one makes that write a dependency of \
                 the run that performed it — a loop."
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
                "A signal is written during render: in the component body or in a JSX expression \
                 container. Rendering must not have side effects — the write either runs once \
                 and leaves readers stale, or re-triggers the render that performed it."
            }
            Code::RA010 => {
                "A `\"use static\"` route uses a signal, a derived value, an effect, a resource, \
                 a write, or an event handler. Static routes ship zero bytes of JavaScript, so \
                 there is no runtime on the page to run the feature; it would silently do \
                 nothing."
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
                 syntax error. Lowering reports the construct instead of dropping it, because \
                 markup emitted without part of a node is wrong in a way nothing reports."
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
            Code::RA009 => {
                "Move the write into an `effect(...)` or the event handler that causes the change, \
                 so it runs in response to something rather than while markup is produced."
            }
            Code::RA010 => "Change line 1 to \"use island\" to ship JavaScript for this route.",
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
                "Do what the message names for this construct: write the spread out, move the JSX \
                 into the children or the returned tree, give the attribute or prop a value, wrap \
                 a root fragment in one parent element, render a runtime primitive's markup \
                 directly, or keep exactly one JSX return."
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
            "Change line 1 to \"use island\" to ship JavaScript for this route."
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
}
