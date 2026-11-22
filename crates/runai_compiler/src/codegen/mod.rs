//! Code generation: from a lowered route to the module a build writes.
//!
//! Two emitters exist across this milestone's tasks: [`prerender`] for a
//! `"use static"` route, and the island emitter for `"use island"`. Only the
//! prerender one is here.
//!
//! # The checked wrappers are the public API, and that is a deliberate ruling
//!
//! [`crate::ir::lower_route`] is **not** a gate: it assumes every rule already
//! passed, and its spans index the type-stripped source rather than the agent's
//! file. So a caller must run [`crate::analyze::entry::analyze`] first, and the
//! only public way to reach an artifact is the `*_checked` function — it calls
//! `analyze`, refuses on any error, and only then lowers and emits. The unchecked
//! emitters are `pub(crate)`, which makes "emit without checking" unreachable at
//! compile time from outside this crate. The naming is the invariant: a `pub`
//! emitter name that does not end in `_checked` would be a path from a source
//! file to an artifact with no rule pass in between.

pub(crate) mod prerender;

use crate::analyze::entry::analyze;
use crate::codegen::prerender::emit_prerender;
use crate::diagnostic::{Code, Diagnostic, Severity};
use crate::ir::lower_route;
use crate::source::Mode;

pub use prerender::template_literal_body;

/// The prerender module for one route file, or the diagnostics that stop it.
///
/// This is the public entry point for a `"use static"` page. It runs
/// [`analyze`] first — the same list `runai check` reports, so the build and the
/// check cannot disagree — and returns it unchanged when any diagnostic is an
/// error. A file whose parse failed gets `RA007` and nothing else, because
/// `analyze` short-circuits there; a file with a signal, a handler, a refused JSX
/// construct, or an unreachable name gets that diagnostic rather than an artifact.
///
/// # Why the mode is checked here too
///
/// A `"use island"` page is `emit_client_checked`'s, and a prerender module for it
/// would carry a signal into a file with no runtime: `render()` returns one
/// string, so there is no mount point for it to write into. `analyze` would also
/// accept the island file (nothing about it is an error in island mode), so the
/// mode is the one refusal `analyze` cannot make. `RA014` is the right code: the
/// file parses, and this is the compiler declining a construct — here, the wrong
/// emitter for the declared mode — not a syntax error.
///
/// # Errors
///
/// The full diagnostic list from `analyze`, plus a `Mode` refusal when the route
/// is an island. `lower_route`'s own diagnostics can only appear for a file the
/// analysis accepted, so they are an internal invariant rather than the agent's
/// first signal.
pub fn emit_prerender_checked(source: &str, path: &str) -> Result<String, Vec<Diagnostic>> {
    let diagnostics = analyze(source, path);
    if diagnostics.iter().any(|d| d.severity == Severity::Error) {
        return Err(diagnostics);
    }
    let route = lower_route(source)?;
    if route.mode != Mode::Static {
        return Err(vec![Diagnostic::new(
            Code::RA014,
            (0, 0),
            format!(
                "`{path}` is a \"use island\" route: the prerender emitter writes static routes only. \
                 Change line 1 to \"use static\", or emit it with `emit_client_checked`."
            ),
        )]);
    }
    Ok(emit_prerender(&route))
}

#[cfg(test)]
mod tests {
    use super::*;

    const P: &str = "export default function P(props: Record<never, never>)";

    /// The whole point of the checked wrapper: a file the analysis refuses never
    /// becomes an artifact. A signal in a static route is `RA010`, and the
    /// returned list carries it *located*, exactly as `runai check` would print
    /// it — not a bare error string.
    #[test]
    fn a_refused_file_returns_the_diagnostics_instead_of_a_module() {
        let src = format!(
            "\"use static\"\nimport {{ signal }} from \"runai\"\n{P} {{ const n = signal(0); return <p>{{n()}}</p> }}\n"
        );
        let diagnostics = emit_prerender_checked(&src, "src/routes/index.page.tsx")
            .expect_err("a static route with a signal must not emit");
        assert!(
            diagnostics.iter().any(|d| d.code == Code::RA010),
            "{diagnostics:?}"
        );
        let first = &diagnostics[0];
        assert_eq!(first.file, "src/routes/index.page.tsx");
        assert!(first.line >= 1, "located: {first:?}");
    }

    /// A parse failure is `RA007` and nothing else, so the emitter never sees a
    /// tree the parser did not produce.
    #[test]
    fn a_syntax_error_is_ra007_and_never_emits() {
        let src = format!("\"use static\"\n{P} {{ return <div> }}\n");
        let diagnostics =
            emit_prerender_checked(&src, "src/routes/bad.page.tsx").expect_err("no artifact");
        assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
        assert_eq!(diagnostics[0].code, Code::RA007);
    }

    /// An island page is the client emitter's; a prerender module for it would
    /// carry a signal into a file with no runtime. The refusal is the mode, which
    /// is the one thing `analyze` cannot report for an island file.
    #[test]
    fn an_island_route_is_refused_and_the_repair_names_the_directive() {
        let src = format!("\"use island\"\n{P} {{ return <p>hi</p> }}\n");
        let diagnostics =
            emit_prerender_checked(&src, "src/routes/index.page.tsx").expect_err("island refused");
        assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
        assert_eq!(diagnostics[0].code, Code::RA014);
        assert!(
            diagnostics[0].message.contains("emit_client_checked"),
            "the repair names the emitter that owns it: {:?}",
            diagnostics[0].message
        );
        assert!(
            diagnostics[0].message.contains("\"use static\""),
            "the repair names the directive: {:?}",
            diagnostics[0].message
        );
    }

    /// A clean static route emits, and the artifact is what the emitter's own
    /// tests pin.
    #[test]
    fn a_clean_static_route_emits_a_module_with_no_imports() {
        let src = format!("\"use static\"\n{P} {{ return <p>hi</p> }}\n");
        let js = emit_prerender_checked(&src, "src/routes/about.page.tsx")
            .expect("a clean static route emits");
        assert!(!js.contains("import"), "{js}");
        assert!(js.contains("export function render()"), "{js}");
        assert!(js.contains("return `<p>hi</p>`"), "{js}");
    }
}
