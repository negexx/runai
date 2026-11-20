//! Structured diagnostics.
//!
//! Every violation the compiler reports carries a stable [`Code`], a byte span,
//! an explanation, a fix, and the path to the matching skill file. The JSON
//! shape here is the contract consumed by `runai check --json` and by agents,
//! so field names and value encodings must not drift.

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
        }
    }

    /// The skill file that explains this code, e.g. `skills/RA001_UNTRACKED_READ.md`.
    pub fn skill_file(&self) -> String {
        format!("skills/{}.md", self.as_str())
    }
}

static ALL: [Code; 8] = [
    Code::RA001,
    Code::RA002,
    Code::RA006,
    Code::RA007,
    Code::RA009,
    Code::RA010,
    Code::RA011,
    Code::RA012,
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
    pub span: (u32, u32),
    pub message: String,
    pub explain: String,
    pub fix: String,
    pub doc: String,
    pub patch: Option<String>,
}

impl Diagnostic {
    /// Serialize as a single JSON object. `span` is `[start, end]`; `patch` is
    /// `null` when absent rather than omitted.
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("a diagnostic always serializes")
    }

    /// Serialize a list of diagnostics as one JSON array.
    pub fn render_all_json(diags: &[Diagnostic]) -> String {
        serde_json::to_string(diags).expect("diagnostics always serialize")
    }

    /// Render for a human: `path:line:col`, then message, explanation, fix, doc.
    pub fn render_human(&self, source: &str, path: &str) -> String {
        let (line, col) = line_col(source, self.span);
        format!(
            "{path}:{line}:{col}: {severity}[{code}]: {message}\n  explain: {explain}\n  fix: {fix}\n  doc: {doc}\n",
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
/// The position is the start of the span: `path:line:col` points at where the
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

        let mut map = serializer.serialize_map(Some(8))?;
        map.serialize_entry("code", &self.code)?;
        map.serialize_entry("severity", &self.severity)?;
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

    fn sample() -> Diagnostic {
        Diagnostic {
            code: Code::RA001,
            severity: Severity::Error,
            span: (10, 15),
            message: "signal read outside a tracking scope".into(),
            explain: "A read in a component body does not re-run.".into(),
            fix: "Wrap it: const doubled = derived(() => count() * 2)".into(),
            doc: "skills/RA001_UNTRACKED_READ.md".into(),
            patch: None,
        }
    }

    #[test]
    fn code_strings_are_stable() {
        assert_eq!(Code::RA001.as_str(), "RA001_UNTRACKED_READ");
        assert_eq!(Code::RA010.as_str(), "RA010_STATIC_VIOLATION");
    }

    #[test]
    fn json_has_every_field_and_null_patch() {
        let json = sample().to_json();
        for key in [
            "code", "severity", "span", "message", "explain", "fix", "doc", "patch",
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
        let src = "function A() {\n  const x = count()\n}\n";
        let mut diag = sample();
        diag.span = (27, 32); // `count`, which starts on line 2
        let out = diag.render_human(src, "src/routes/index.page.tsx");
        assert!(out.contains("RA001_UNTRACKED_READ"));
        assert!(out.contains("src/routes/index.page.tsx:2"));
        assert!(out.contains("Wrap it: const doubled"));
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
        assert_eq!(parsed["span"], serde_json::json!([10, 15]));
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
        assert_eq!(Code::RA001.skill_file(), "skills/RA001_UNTRACKED_READ.md");
        assert_eq!(Code::RA010.skill_file(), "skills/RA010_STATIC_VIOLATION.md");
    }

    #[test]
    fn render_human_reports_path_line_and_column() {
        let src = "function A() {\n  const x = count()\n}\n";
        let mut diag = sample();
        // `count` begins at byte 27, which is line 2, column 13.
        diag.span = (27, 32);
        let out = diag.render_human(src, "src/routes/index.page.tsx");
        assert!(out.starts_with("src/routes/index.page.tsx:2:13: error[RA001_UNTRACKED_READ]"));
    }

    #[test]
    fn render_human_survives_spans_outside_the_source() {
        let out = sample().render_human("short", "a.tsx");
        assert!(out.contains("a.tsx:1:6"));
    }

    /// `é` is two bytes, so every byte offset after it is shifted by one.
    const MULTIBYTE_SRC: &str = "const é = 1\n  const x = count()\n";

    #[test]
    fn render_human_counts_columns_in_characters() {
        let mut diag = sample();
        // Byte 13 starts line 2; `count` starts at byte 25, which is 12
        // characters into line 2, so the 1-based column is 13.
        diag.span = (25, 30);
        let out = diag.render_human(MULTIBYTE_SRC, "src/multi.tsx");
        assert!(out.starts_with("src/multi.tsx:2:13: error[RA001_UNTRACKED_READ]"));
    }

    #[test]
    fn render_human_floors_spans_inside_a_multibyte_char() {
        let mut diag = sample();
        // Byte 7 is the second byte of `é` (bytes 6..8): not a char boundary.
        // It floors to byte 6, so the column is that of `é`'s line, column 7.
        diag.span = (7, 8);
        let out = diag.render_human(MULTIBYTE_SRC, "src/multi.tsx");
        assert!(out.starts_with("src/multi.tsx:1:7: error[RA001_UNTRACKED_READ]"));
    }

    #[test]
    fn render_human_column_ignores_multibyte_width_on_the_same_line() {
        let src = "  const é = count()\n";
        let mut diag = sample();
        // `count` starts at byte 13; a byte column would say 14, a character
        // column says 13.
        diag.span = (13, 18);
        let out = diag.render_human(src, "src/same_line.tsx");
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
            ]
        );
    }
}
