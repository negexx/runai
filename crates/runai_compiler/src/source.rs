//! Oxc parsing and route-mode detection.
//!
//! Every route file is parsed here, and nothing else in the compiler touches the
//! parser directly. Two answers come out of this module:
//!
//! - what the file's directive says ([`detect_mode`]), and
//! - whether the file is syntactically valid at all ([`parse_tsx`]).
//!
//! A parser rejection is reported as `RA007_AMBIGUOUS_JSX`, quoting the parser's
//! own message so an agent sees the real syntax error rather than a generic one.
//! A file without a mode directive is `RA012_MISSING_MODE` with a zero-width span
//! at byte 0. The directive's quote style is irrelevant; its escapes are not.

use oxc_allocator::Allocator;
use oxc_ast::ast::{Expression, Program, Statement};
use oxc_parser::Parser;
use oxc_span::{SourceType, Span};

use crate::diagnostic::{Code, Diagnostic, Severity};

/// How a route file renders: prerendered with no JavaScript, or hydrated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// `"use static"`: prerendered to HTML at build time, ships no JavaScript.
    Static,
    /// `"use island"`: one island that mounts on boot and replaces the markup.
    Island,
}

/// The two directives as they read inside the quotes. Quote style is not part of
/// the directive: JavaScript's own directive semantics are quote-insensitive, and
/// accepting `'use static'` infers nothing about the mode.
const USE_STATIC: &str = "use static";
const USE_ISLAND: &str = "use island";

/// Which mode the first statement declares, or `RA012_MISSING_MODE`.
///
/// Only a string literal statement in the first position counts. The comparison
/// is against the directive's raw content, quotes stripped but escapes left
/// as-is, so an escape sequence (`"use\x20static"`) cannot smuggle a directive
/// past review: it is a different string, and therefore not a mode.
// The `Err` type is `Diagnostic`, not `Box<Diagnostic>`, because the interface is
// fixed: every later consumer matches on `Result<_, Diagnostic>` directly.
#[allow(clippy::result_large_err)]
pub fn detect_mode(source: &str) -> Result<Mode, Diagnostic> {
    let alloc = Allocator::default();
    let ret = Parser::new(&alloc, source, SourceType::tsx()).parse();

    match directive_content(source, &ret.program) {
        Some(USE_STATIC) => Ok(Mode::Static),
        Some(USE_ISLAND) => Ok(Mode::Island),
        _ => Err(missing_mode()),
    }
}

/// `RA012_MISSING_MODE`: the file never names a mode.
fn missing_mode() -> Diagnostic {
    Diagnostic {
        code: Code::RA012,
        severity: Severity::Error,
        // The directive belongs at the very top of the file; point there.
        span: (0, 0),
        message: "route file has no `\"use static\"` or `\"use island\"` directive".into(),
        explain: "Every route file must open with a string literal statement that names its \
                  rendering mode. The compiler never infers the mode, so an agent adding a route \
                  always gets one answer instead of a guess."
            .into(),
        fix: "Add `\"use static\"` (prerendered to HTML, no JavaScript) or `\"use island\"` \
              (hydrated on boot) as the first statement of the file."
            .into(),
        doc: Code::RA012.skill_file(),
        patch: None,
    }
}

/// Parse a route file as TSX.
///
/// Returns the program only when the parser reported no diagnostics at all. Any
/// diagnostic becomes one `RA007_AMBIGUOUS_JSX`, in source order, carrying the
/// parser's own message and its primary label span. A diagnostic with no label
/// says so in the message instead of claiming a location it does not have.
pub fn parse_tsx<'a>(
    alloc: &'a Allocator,
    source: &'a str,
) -> Result<&'a Program<'a>, Vec<Diagnostic>> {
    let ret = Parser::new(alloc, source, SourceType::tsx()).parse();

    if ret.diagnostics.is_empty() {
        return Ok(alloc.alloc(ret.program));
    }

    Err(ret
        .diagnostics
        .iter()
        .map(|diagnostic| {
            // Prefer the primary label: secondary labels often point at a context
            // span that is not where the error is.
            let label = diagnostic
                .labels
                .iter()
                .find(|label| label.primary())
                .or_else(|| diagnostic.labels.first());

            let message = match label {
                Some(_) => format!("ambiguous JSX: {message}", message = diagnostic.message),
                // `(0, 0)` renders as `path:1:1`, which claims a real location.
                None => format!(
                    "ambiguous JSX (parser gave no source location): {message}",
                    message = diagnostic.message
                ),
            };

            Diagnostic {
                code: Code::RA007,
                severity: Severity::Error,
                span: label.map_or((0, 0), |label| {
                    let span = label.span();
                    (span.start, span.end)
                }),
                message,
                // No parser text here: `message` already carries it, and repeating it
                // in the structured output would just say the same thing twice.
                explain: "The parser rejected this file before it could be compiled. The compiler \
                          reports the parser's own words in `message` instead of guessing at what \
                          was meant, because a wrong guess sends the repair into the wrong part of \
                          the file."
                    .into(),
                // The cause is not known: `parse_tsx` maps *every* parser diagnostic
                // here, including non-JSX ones (`Cannot assign to this expression`).
                // So the markup mistakes are offered as the usual causes in route
                // files, not as the diagnosis.
                fix: "Fix the syntax error the parser reported in `message`. In route files the \
                      usual causes are an unclosed element, an unterminated string inside an \
                      attribute, or adjacent sibling elements — add the missing `>` or closing \
                      element, close the attribute string, or wrap the siblings in one parent \
                      element."
                    .into(),
                doc: Code::RA007.skill_file(),
                patch: None,
            }
        })
        .collect())
}

/// The directive's content, quotes stripped but escapes left as they appear, or
/// `None` when the first statement position holds no string literal.
///
/// Oxc lifts a leading string literal out of the statement list into
/// [`Program::directives`], so the directive prologue is checked first and its
/// `directive` field is already quote-stripped. The `body` fallback is defence in
/// depth: for a well-formed prologue it is unreachable, because a leading
/// string-literal expression statement *is* the prologue and Oxc hoists it.
fn directive_content<'a>(source: &'a str, program: &'a Program<'a>) -> Option<&'a str> {
    if let Some(directive) = program.directives.first() {
        return Some(directive.directive.as_str());
    }

    match program.body.first() {
        Some(Statement::ExpressionStatement(statement)) => match &statement.expression {
            Expression::StringLiteral(literal) => raw_text(source, literal.span).map(strip_quotes),
            _ => None,
        },
        _ => None,
    }
}

/// The raw source text a span names, quotes included, or `None` if the span falls
/// outside the source or off a character boundary.
fn raw_text(source: &str, span: Span) -> Option<&str> {
    source.get(span.start as usize..span.end as usize)
}

/// Strip one pair of matching quote delimiters, if the text has one.
///
/// The `body` fallback reads source text rather than Oxc's hoisted `directive`
/// field, so it has to remove the delimiters itself to be quote-insensitive.
/// Two deliberate rules:
///
/// - A failed trailing-quote check returns the *prefix-stripped* string, never
///   `raw`. Returning `raw` would undo a successful prefix strip and hand back the
///   still-quoted text — exactly the quote sensitivity this is meant to remove.
/// - A mismatched pair (`"use static'`) is not a quoted string, so nothing is
///   stripped: one quote alone is not a delimiter.
fn strip_quotes(raw: &str) -> &str {
    let Some(open) = raw
        .as_bytes()
        .first()
        .copied()
        .filter(|byte| matches!(byte, b'"' | b'\''))
    else {
        return raw;
    };

    let inner = &raw[1..];
    match inner.as_bytes().last().copied() {
        Some(close) if close == b'"' || close == b'\'' => {
            if close == open {
                &inner[..inner.len() - 1]
            } else {
                // Mismatched pair: not a quoted string, so strip nothing.
                raw
            }
        }
        // A one-sided quote is not a literal, but a prefix strip that did happen
        // must not be undone.
        _ => inner,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_static() {
        let src = "\"use static\"\nimport x from 'y'\n";
        assert!(matches!(detect_mode(src), Ok(Mode::Static)));
    }

    #[test]
    fn detects_island() {
        let src = "\"use island\"\nexport default function A() { return <p/> }\n";
        assert!(matches!(detect_mode(src), Ok(Mode::Island)));
    }

    /// Quote style carries no information about the mode, so a single-quoted
    /// directive must count. Otherwise the `RA012` fix text would tell an agent to
    /// add a double-quoted line above the existing single-quoted one, and the file
    /// would stay `RA012` forever.
    #[test]
    fn single_quotes_are_accepted() {
        assert!(matches!(detect_mode("'use static'\n"), Ok(Mode::Static)));
        assert!(matches!(detect_mode("'use island'\n"), Ok(Mode::Island)));
    }

    /// The directive must be the first statement; later positions are ordinary
    /// string expressions and name no mode.
    #[test]
    fn directive_must_be_first() {
        let src = "import x from 'y'\n\"use static\"\n";
        assert_eq!(detect_mode(src).unwrap_err().code, Code::RA012);
    }

    #[test]
    fn comment_before_the_directive_still_counts() {
        let src = "// hi\n\"use static\"\n";
        assert!(matches!(detect_mode(src), Ok(Mode::Static)));
    }

    #[test]
    fn missing_directive_is_ra012() {
        let src = "export default function A() { return <p/> }\n";
        let err = detect_mode(src).unwrap_err();
        assert_eq!(err.code, Code::RA012);
        assert!(err.fix.contains("\"use static\""));
    }

    /// The comparison leaves escapes as-is, so an escape sequence that cooks down
    /// to the same string is not a directive.
    #[test]
    fn escapes_do_not_smuggle_a_directive() {
        let src = "\"use\\x20static\"\nexport default function A() { return <p/> }\n";
        assert_eq!(detect_mode(src).unwrap_err().code, Code::RA012);
    }

    #[test]
    fn unclosed_jsx_is_ra007_with_span() {
        let alloc = oxc_allocator::Allocator::default();
        let src = "const a = <div>;\n";
        let errs = parse_tsx(&alloc, src).unwrap_err();
        assert_eq!(errs[0].code, Code::RA007);
        assert!(errs[0].span.0 > 0, "span should point into the file");
    }

    /// `message` is where the parser's own words live. A non-JSX syntax error must
    /// still tell the agent what actually broke, and `explain` must not repeat the
    /// same sentence in the same JSON object. The expected text is read from a
    /// separate raw parse, not from our own output.
    #[test]
    fn ra007_message_carries_the_parser_message() {
        let alloc = oxc_allocator::Allocator::default();
        let src = "const a = <div>;\n";

        let ret = Parser::new(&alloc, src, SourceType::tsx()).parse();
        let parser_message = ret
            .diagnostics
            .first()
            .expect("fixture must not parse")
            .message
            .to_string();

        let errs = parse_tsx(&alloc, src).unwrap_err();
        assert!(
            errs[0].message.contains(&parser_message),
            "message did not carry the parser message {parser_message:?}: {:?}",
            errs[0].message
        );
        assert!(
            !errs[0].explain.contains(&parser_message),
            "explain duplicates the parser message inside one diagnostic: {:?}",
            errs[0].explain
        );
    }

    /// `parse_tsx` maps every parser diagnostic to `RA007`, so the advice must not
    /// assert a JSX cause. Oxc on `1 = 2` says `Cannot assign to this expression`
    /// (verified by running); an unclosed-tag hunt would be a wasted repair round.
    #[test]
    fn non_jsx_parser_error_gets_honest_advice() {
        let alloc = oxc_allocator::Allocator::default();
        // Verified: Oxc reports `Cannot assign to this expression` for this fixture.
        let src = "const a = 1 = 2;\n";
        let errs = parse_tsx(&alloc, src).unwrap_err();
        assert_eq!(errs.len(), 1);
        assert_eq!(errs[0].code, Code::RA007);
        assert!(
            errs[0].message.contains("Cannot assign to this expression"),
            "message did not carry the parser's own words: {:?}",
            errs[0].message
        );
        assert!(
            !errs[0].fix.contains("tag"),
            "fix asserts a JSX cause for a non-JSX error: {:?}",
            errs[0].fix
        );
    }

    /// The unlabelled branch is reachable: Oxc's nested-`ParserDiagnostic`
    /// constructors `file_appears_to_be_binary()` and `overlong_source()` attach no
    /// label at all. `U+FFFD` triggers the first one (verified by running). A NUL
    /// byte does *not* — it is `Invalid Character` with a label — so the fixture is
    /// the replacement character, not a NUL.
    #[test]
    fn unlabelled_diagnostic_says_it_has_no_location() {
        let alloc = oxc_allocator::Allocator::default();
        let src = "const a = \u{FFFD};\n";
        let errs = parse_tsx(&alloc, src).unwrap_err();
        assert_eq!(errs[0].code, Code::RA007);
        assert!(
            errs[0].message.contains("parser gave no source location"),
            "unlabelled diagnostic claimed a location: {:?}",
            errs[0].message
        );
        assert_eq!(errs[0].span, (0, 0));
    }

    /// When a diagnostic carries several labels, the span must be the primary one.
    /// Oxc's `'extends' clause must precede 'implements' clause` is such a case and
    /// does fire under `SourceType::tsx()`; the primary label is the *second* one
    /// and covers `extends` (bytes 21..28), not the earlier `implements`.
    #[test]
    fn ra007_prefers_the_primary_label() {
        let alloc = oxc_allocator::Allocator::default();
        let src = "class A implements B extends C {}\n";
        let errs = parse_tsx(&alloc, src).unwrap_err();
        assert!(
            errs[0].message.contains("must precede"),
            "unexpected parser message: {:?}",
            errs[0].message
        );
        assert_eq!(errs[0].span, (21, 28));
        assert_eq!(&src[21..28], "extends");
    }

    /// `strip_quotes` must not fall back to `raw` after a successful prefix strip,
    /// and must not accept mismatched delimiters.
    #[test]
    fn strip_quotes_is_symmetric_and_forgiving_of_one_sided_input() {
        assert_eq!(strip_quotes("\"use static\""), "use static");
        assert_eq!(strip_quotes("'use static'"), "use static");
        // Prefix strips, suffix fails: keep the stripped form, not the quoted raw.
        assert_eq!(strip_quotes("\"use static"), "use static");
        assert_eq!(strip_quotes("'use static"), "use static");
        // Mismatched delimiters are not a quoted string, so nothing is stripped.
        assert_eq!(strip_quotes("\"use static'"), "\"use static'");
        assert_eq!(strip_quotes("'use static\""), "'use static\"");
        // Degenerate inputs must not panic or slice off a char boundary.
        assert_eq!(strip_quotes(""), "");
        // A lone opening quote is still stripped by the prefix rule, as it was before.
        assert_eq!(strip_quotes("\""), "");
        assert_eq!(strip_quotes("x"), "x");
    }

    /// `Directive::directive` is documented as the raw content with escapes left
    /// as-is; both facts below are load-bearing for `detect_mode`.
    #[test]
    fn directive_field_is_quote_insensitive_and_escape_sensitive() {
        let alloc = oxc_allocator::Allocator::default();

        let ret = Parser::new(&alloc, "'use static'\n", SourceType::tsx()).parse();
        let single = ret.program.directives.first().expect("directive hoisted");
        assert_eq!(single.directive.as_str(), "use static");

        let ret = Parser::new(&alloc, "\"use\\x20static\"\n", SourceType::tsx()).parse();
        let escaped = ret.program.directives.first().expect("directive hoisted");
        assert_eq!(escaped.directive.as_str(), "use\\x20static");
    }
}
