//! The Markdown body of one diagnostic's skill file.
//!
//! `runai skills` (Task 15) writes one file per [`Code`], and the body comes
//! from here so the diagnostic and its skill cannot drift: both describe the
//! same rule, and the drift test below refuses an empty arm.
//!
//! The bodies describe the *pattern* of the fix rather than quoting a diagnostic
//! verbatim. `RA006` and `RA011` interpolate the component's name, so a single
//! static body cannot mirror them literally — it says what the repair is, and
//! the diagnostic carries the file-specific spelling.

use crate::diagnostic::Code;

/// The Markdown body for one code: what it means, why it is an error, and how to
/// fix it.
pub fn skill_body(code: Code) -> String {
    let (means, why, fix) = match code {
        Code::RA001 => (
            "A signal is read in the component body, outside any tracking scope.",
            "A read there runs once and never again: no effect owns it, so nothing re-runs \
             when the signal changes. The value on screen is the value at first render.",
            "Wrap the read in a tracking scope — `derived(() => count() * 2)`, an `effect(...)`, \
             a `resource(...)`, or a JSX slot — or call it inside an event handler.",
        ),
        Code::RA002 => (
            "A signal is written inside an `effect(...)` argument.",
            "An effect re-runs when the signals it reads change. Writing a signal it reads makes \
             that write a new dependency of the run that performed it, which is a loop.",
            "Write the signal from the event handler or the body that causes the change, and let \
             the effect only read what it needs.",
        ),
        Code::RA006 => (
            "A component's props parameter is missing an explicit type annotation.",
            "Props are the only way data enters a component, so their type is the written answer \
             to \"where does this value come from?\". A propless component has an answer too, and \
             it has to be written down.",
            "Annotate the parameter — `function Greeting(props: GreetingProps)` — or, for no \
             props, declare `type Props = Record<never, never>` and take it as a parameter.",
        ),
        Code::RA007 => (
            "The parser could not read this file as TSX.",
            "Nothing downstream can run on a file with a syntax error: lowering, analysis, and \
             codegen all need the tree the parser failed to produce. The diagnostic carries the \
             parser's own message and location.",
            "Repair the syntax at the reported span — an unclosed tag, an unbalanced brace, or the \
             parser's specific complaint in the message.",
        ),
        Code::RA009 => (
            "A signal is written during render: in the component body or in a JSX expression \
             container.",
            "Rendering must not have side effects. A write during render either runs once and \
             leaves readers stale, or re-triggers the render that performed it.",
            "Move the write into an event handler or an `effect(...)` so it runs in response to \
             something, not while markup is produced.",
        ),
        Code::RA010 => (
            "A `\"use static\"` route uses a signal, a derived value, an effect, a resource, a write, \
             or an event handler.",
            "Static routes ship zero bytes of JavaScript. There is no runtime on the page to run a \
             signal or attach a listener, so the feature would silently do nothing.",
            "Change line 1 to `\"use island\"` to ship JavaScript for this route, or remove the \
             dynamic feature so the page stays static.",
        ),
        Code::RA011 => (
            "A page file's default export is missing, takes parameters a route cannot be given, or \
             does not return JSX directly.",
            "The router renders the default export as the route. An export it cannot call, cannot \
             fill, or cannot get markup from leaves the route with nothing to render.",
            "Make the default export a component that returns JSX directly from its body, taking \
             at most one annotated `props` parameter. Replace an existing export rather than \
             declaring a second default export.",
        ),
        Code::RA012 => (
            "The file does not open with a `\"use static\"` or `\"use island\"` directive.",
            "The directive decides whether the route ships JavaScript. Inferring it would hide \
             the decision that controls the 0 KB guarantee, so it is never inferred.",
            "Make line 1 `\"use static\"` or `\"use island\"`. Single and double quotes are both \
             accepted; escapes such as `\"use\\x20static\"` are not.",
        ),
        Code::RA014 => (
            "The file is valid syntax, but it uses a construct this milestone's lowering does not \
             support: a spread (`{...rest}`), JSX inside a `{…}` expression, a JSX-valued \
             attribute, an empty attribute expression, a fragment as the returned root, or a \
             second JSX return.",
            "Lowering emits markup, and each of these would either drop part of a node or copy \
             JSX into a JavaScript expression where it cannot parse. Emitting without it is wrong \
             in a way nothing reports, so the construct is refused instead.",
            "Do what the `fix` names for this construct: write the spread out, move the JSX into \
             the children or into its own `return`, give the attribute a value, wrap a root \
             fragment in one parent element, or return exactly one element from the component \
             body.",
        ),
    };

    format!("## What it means\n\n{means}\n\n## Why it is an error\n\n{why}\n\n## Fix\n\n{fix}\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The drift test Task 15 depends on: a code with no body is a skill file
    /// that tells an agent nothing, which is worse than no file at all.
    #[test]
    fn every_code_has_a_skill_body() {
        for code in Code::all() {
            let body = skill_body(*code);
            assert!(
                !body.trim().is_empty(),
                "missing skill for {}",
                code.as_str()
            );
            for section in ["## What it means", "## Why it is an error", "## Fix"] {
                assert!(body.contains(section), "{}: {body}", code.as_str());
            }
        }
    }

    /// The newest code's body must describe what it actually refuses, not the
    /// parser: an agent reading "syntax error" would repair nothing.
    #[test]
    fn ra014_explains_that_the_syntax_is_valid() {
        let body = skill_body(Code::RA014);
        assert!(body.contains("valid syntax"), "{body}");
        assert!(body.contains("second JSX return"), "{body}");
        assert!(body.contains("JSX inside a `{…}` expression"), "{body}");
    }
}
