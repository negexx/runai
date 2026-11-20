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
//! at byte 0.

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

/// The two directives, including their quotes, exactly as they must appear in
/// the source text.
const USE_STATIC: &str = "\"use static\"";
const USE_ISLAND: &str = "\"use island\"";

/// Which mode the first statement declares, or `RA012_MISSING_MODE`.
///
/// Only a string literal statement in the first position counts. The comparison
/// is against the raw source text of the literal, quotes included, so an escape
/// sequence (`"use\x20static"`) cannot smuggle a directive past review: it is a
/// different string, and therefore not a mode.
// The `Err` type is `Diagnostic`, not `Box<Diagnostic>`, because the interface is
// fixed: every later consumer matches on `Result<_, Diagnostic>` directly.
#[allow(clippy::result_large_err)]
pub fn detect_mode(source: &str) -> Result<Mode, Diagnostic> {
    let alloc = Allocator::default();
    let ret = Parser::new(&alloc, source, SourceType::tsx()).parse();

    let directive = first_string_literal(&ret.program).and_then(|span| raw_text(source, span));

    match directive {
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
/// parser's own message and primary label span (or `(0, 0)` when the diagnostic
/// has no label).
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
            // Not every diagnostic carries a label, so `(0, 0)` is the fallback.
            let span = diagnostic.labels.first().map_or((0, 0), |label| {
                let span = label.span();
                (span.start, span.end)
            });

            Diagnostic {
                code: Code::RA007,
                severity: Severity::Error,
                span,
                message: format!("ambiguous JSX: {message}", message = diagnostic.message),
                explain: format!(
                    "The TSX parser rejected this file: {message}. An unclosed tag, an \
                     unterminated attribute, or two adjacent sibling elements with no separator \
                     makes the markup impossible to read, so the compiler refuses to guess.",
                    message = diagnostic.message
                ),
                fix: "Rewrite the JSX so every tag closes and every attribute terminates: look \
                      for a missing `>` or `</tag>`, an unterminated string inside an attribute, \
                      or two sibling elements running together."
                    .into(),
                doc: Code::RA007.skill_file(),
                patch: None,
            }
        })
        .collect())
}

/// The span of the string literal in the first statement position, if there is one.
///
/// Oxc lifts a leading string literal out of the statement list into
/// [`Program::directives`], so the directive prologue is checked first; a string
/// literal that was not in the prologue is still the first statement's expression
/// and is treated the same way.
fn first_string_literal<'a>(program: &'a Program<'a>) -> Option<Span> {
    if let Some(directive) = program.directives.first() {
        return Some(directive.expression.span);
    }

    match program.body.first() {
        Some(Statement::ExpressionStatement(statement)) => match &statement.expression {
            Expression::StringLiteral(literal) => Some(literal.span),
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

    #[test]
    fn missing_directive_is_ra012() {
        let src = "export default function A() { return <p/> }\n";
        let err = detect_mode(src).unwrap_err();
        assert_eq!(err.code, Code::RA012);
        assert!(err.fix.contains("\"use static\""));
    }

    /// The comparison is against the raw source text, so an escape sequence that
    /// cooks down to the same string is not a directive.
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
}
