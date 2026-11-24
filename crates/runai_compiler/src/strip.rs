//! TypeScript removal, with JSX preserved.
//!
//! Every route file is typed TSX, so whatever runs in the browser and whatever
//! `oxc_codegen` emits must be plain JavaScript. This module is that one step:
//! it parses the original source, runs `oxc_transformer` with the **TypeScript**
//! transform on and the **JSX** transform off, and prints the result.
//!
//! Two properties are load-bearing:
//!
//! - **JSX survives.** The transform is a type eraser, not a renderer: the
//!   output still contains tags, so the caller can re-parse it *as JSX* and lower
//!   it. `JsxOptions::disable()` is what keeps the JSX plugin from rewriting
//!   `<p>` into a function call.
//! - **The output is a whole module, not a fragment.** Directives, imports and
//!   `export default` are preserved (Oxc prints them), so the caller re-parses
//!   one module and finds the same route component it found before.
//!
//! The output is re-printed, so it is normalized rather than byte-identical to
//! the input: indentation is Oxc's, `'…'` becomes `"…"` and semicolons are
//! inserted. It is JavaScript, which is exactly what the emitters need — the
//! *original* source stays the only thing diagnostics quote, which is why
//! [`crate::analyze::jsx::check_jsx`] runs before this does.

use std::path::Path;

use oxc_allocator::Allocator;
use oxc_codegen::{Codegen, CodegenOptions};
use oxc_semantic::SemanticBuilder;
use oxc_transformer::{JsxOptions, TransformOptions, Transformer};

use crate::diagnostic::{Code, Diagnostic};
use crate::source::parse_program;

/// Strip TypeScript syntax from `source`, keeping JSX intact.
///
/// The pipeline is the pinned 0.159 one: parse, build semantic scoping, run the
/// transformer, print with codegen. Source is parsed with [`SourceType::tsx`]
/// via [`parse_program`], so a syntax error is `RA007` here exactly as it is
/// everywhere else.
///
/// `jsx: JsxOptions::disable()` is the whole point: it sets `jsx_plugin: false`
/// (and the display-name/self/source plugins with it) so the JSX tree is left
/// alone. Oxc 0.159's Rust API has no `"preserve"` string — that is the NAPI
/// spelling — and `JsxOptions::disable()` is its Rust equivalent.
///
/// # Errors
///
/// `RA007` when the file does not parse. `RA014` when the parser accepted the
/// file but the transformer could not finish, which is a compiler limitation
/// rather than a syntax error, with the transformer's own message attached.
pub fn strip_types(source: &str) -> Result<String, Vec<Diagnostic>> {
    let alloc = Allocator::default();
    let parsed = parse_program(&alloc, source)?;
    let mut program = parsed.program;

    // The transformer expects scoping that was built for its own tree, so the
    // semantic pass is not optional: without it, symbol-heavy transforms such as
    // the TypeScript one would have nothing to update.
    //
    // `with_enum_eval(true)` is load-bearing for a TypeScript `enum`. The
    // TypeScript transform **evaluates** enum member values and reads them from
    // `Scoping`, so without this flag it panics
    // (`Transformer requires Scoping produced with
    // SemanticBuilder::with_enum_eval(true)`) instead of returning a diagnostic.
    // A panic inside `strip_types` breaks its `Result<_, Vec<Diagnostic>>`
    // contract: `analyze` reports nothing for the file, so `runai check` says
    // clean and the emitter aborts the process.
    let scoping = SemanticBuilder::new()
        .with_enum_eval(true)
        .with_excess_capacity(2.0)
        .build(&program)
        .semantic
        .into_scoping();

    let options = TransformOptions {
        jsx: JsxOptions::disable(),
        ..TransformOptions::default()
    };

    let transformed = Transformer::new(&alloc, Path::new("route.tsx"), &options)
        .build_with_scoping(scoping, &mut program);

    if transformed.diagnostics.has_errors() {
        return Err(transformed
            .diagnostics
            .errors()
            .map(|diagnostic| {
                let span = diagnostic
                    .labels
                    .iter()
                    .find(|label| label.primary())
                    .or_else(|| diagnostic.labels.first())
                    .map_or((0, 0), |label| {
                        let span = label.span();
                        (span.start, span.end)
                    });
                Diagnostic::new(
                    Code::RA014,
                    span,
                    format!(
                        "the compiler could not remove types from this file: {message}",
                        message = diagnostic.message
                    ),
                )
            })
            .collect());
    }

    // Default options: Oxc's own formatting, and the leading comments it chooses
    // to keep. Nothing downstream depends on the exact whitespace — the IR slices
    // text and expressions out of *this* string, so it is only required to be
    // valid JavaScript that still holds JSX.
    Ok(Codegen::new()
        .with_options(CodegenOptions::default())
        .build(&program)
        .code)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The brief's typed fixture: `props: Record<never, never>` and `n: number`
    /// are gone, `n as number` is `n`, and the returned JSX is still JSX.
    #[test]
    fn types_are_removed_and_jsx_survives() {
        let src = "\"use island\"\nexport default function P(props: Record<never, never>) { const n: number = 1; return <p>{n as number}</p> }\n";
        let stripped = strip_types(src).unwrap();
        assert!(!stripped.contains(": number"), "{stripped}");
        assert!(!stripped.contains("Record<never, never>"), "{stripped}");
        assert!(!stripped.contains(" as number"), "{stripped}");
        assert!(
            stripped.contains("<p>"),
            "JSX was transformed away: {stripped}"
        );
        assert!(stripped.contains("</p>"), "{stripped}");
        assert!(
            stripped.contains("\"use island\""),
            "the directive must survive: {stripped}"
        );
        assert!(
            stripped.contains("export default function P"),
            "the export must survive: {stripped}"
        );
    }

    /// The stripped module must parse as JSX on its own: the caller re-parses it,
    /// so a transform that emitted JSX-but-not-parsable would fail there in a
    /// place with no useful span. Also the real hazard behind this task: Oxc's
    /// JSX plugin would have replaced the tag with a call, and the re-parse would
    /// find no JSX to lower at all.
    #[test]
    fn the_stripped_module_still_parses_as_tsx() {
        let src =
            "\"use static\"\nexport default function P() { return <div class=\"a\">hi</div> }\n";
        let stripped = strip_types(src).unwrap();
        let alloc = Allocator::default();
        let program =
            crate::source::parse_tsx(&alloc, &stripped).expect("stripped output must parse as TSX");
        assert!(
            crate::analyze::find_route_component(program).is_some(),
            "the route component must still be found in {stripped}"
        );
    }

    /// Types inside a JSX expression are removed too — that text becomes
    /// JavaScript in the emitted module, so `{n as number}` would be a syntax
    /// error there.
    #[test]
    fn an_expression_is_stripped_of_types() {
        let src = "\"use island\"\nexport default function P() { const n = 1; return <p>{n as number}</p> }\n";
        let stripped = strip_types(src).unwrap();
        assert!(stripped.contains("{n}"), "{stripped}");
    }

    /// A syntax error is the parser's `RA007`, from the same parse path as
    /// everywhere else — stripping must not invent a second error shape.
    #[test]
    fn a_syntax_error_is_ra007() {
        let errs = strip_types("\"use static\"\nexport default function P() { return <div> }\n")
            .unwrap_err();
        assert_eq!(errs[0].code, Code::RA007);
    }

    /// A TypeScript `enum` must be **erased**, not transformed by a transformer
    /// that was not given the scoping it needs. `oxc_transformer` 0.159 panics
    /// (`Transformer requires Scoping produced with
    /// SemanticBuilder::with_enum_eval(true)`) rather than returning a diagnostic,
    /// and `analyze` reports nothing for the file — so `runai check` would say
    /// clean and the emitter would abort the process. `strip_types`'s contract is
    /// `Result<_, Vec<Diagnostic>>`, and a panic breaks it.
    #[test]
    fn an_enum_is_stripped_instead_of_panicking() {
        let stripped = strip_types(
            "\"use island\"\nenum E { A }\nexport default function P() { return <p>{E.A}</p> }\n",
        )
        .unwrap();
        assert!(!stripped.contains("enum E"), "{stripped}");
        assert!(stripped.contains("E.A"), "{stripped}");
    }

    /// `import type` is a type-only import and disappears; a value import that
    /// the module uses stays.
    #[test]
    fn type_only_imports_disappear_and_value_imports_stay() {
        let src = "\"use island\"\nimport type { Props } from \"./types\"\nimport { signal } from \"runai\"\nexport default function P(props: Record<never, never>) { const n: number = signal(0); return <p>{n}</p> }\n";
        let stripped = strip_types(src).unwrap();
        assert!(!stripped.contains("./types"), "{stripped}");
        assert!(stripped.contains("\"runai\""), "{stripped}");
    }

    /// The output is JavaScript: an interface or a type alias is gone entirely.
    #[test]
    fn declarations_that_only_exist_in_the_type_system_disappear() {
        let src = "\"use static\"\ntype Tone = \"a\" | \"b\"\ninterface Props { tone: Tone }\nexport default function P(props: Props) { const tone: Tone = \"a\"; return <p class={tone}>hi</p> }\n";
        let stripped = strip_types(src).unwrap();
        assert!(!stripped.contains("type Tone"), "{stripped}");
        assert!(!stripped.contains("interface Props"), "{stripped}");
        assert!(stripped.contains("class={tone}"), "{stripped}");
    }
}
