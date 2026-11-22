//! The Markdown body of one diagnostic's skill file.
//!
//! `runai skills` (Task 15) writes one file per [`Code`], and the body comes
//! from here so the diagnostic and its skill cannot drift: both describe the
//! same rule, and the drift test below refuses an empty arm.
//!
//! `## What it means` is [`Code::explain`] and `## Fix` is [`Code::fix_template`]
//! verbatim — the same two functions [`crate::diagnostic::Diagnostic::new`] uses,
//! so a generated skill cannot disagree with what the compiler reports. Only
//! `## Why it is an error` is written per code here, because it is the prose that
//! explains the cost of the mistake rather than the repair.

use crate::diagnostic::Code;

/// Why a violation of `code` costs the user something. Paired with
/// [`Code::explain`] and [`Code::fix_template`] to build the skill body.
fn why(code: Code) -> &'static str {
    match code {
        Code::RA001 => {
            "A read there runs once and never again: no effect owns it, so nothing re-runs when \
             the signal changes. The value on screen is the value at first render. This is why \
             `RA001` only fires when the read's *nearest enclosing function* is the component \
             body itself: a read inside an event handler (`onClick={() => count()}`) or any other \
             nested callback (`setTimeout(() => count(), 10)`) runs when that callback runs, so it \
             sees the current value and nothing is frozen. Those reads are correct and are never \
             flagged. A read inside `derived(...)`, `effect(...)`, `resource(...)`, or a JSX \
             expression slot is likewise owned by that tracking scope, not by the render."
        }
        Code::RA002 => {
            "An effect re-runs when the signals it reads change. Writing a signal it reads makes \
             that write a new dependency of the run that performed it, which is a loop."
        }
        Code::RA006 => {
            "An unannotated parameter leaves the answer unstated, and a propless component leaves \
             it to prose. The next reader — human or agent — has to guess what may be passed."
        }
        Code::RA007 => {
            "Nothing downstream — lowering, analysis, codegen — can run on a file with a syntax \
             error. The diagnostic carries the parser's own message and location so the repair \
             lands where the error actually is. The same code covers a line break between two \
             inline elements, because that is the other way the file's visible text and its \
             rendered text disagree: JSX deletes whitespace-only text containing a newline."
        }
        Code::RA009 => {
            concat!(
                "Rendering must not have side effects. ",
                crate::diagnostic::ra009_consequence!()
            )
        }
        Code::RA010 => {
            "Static routes ship zero bytes of JavaScript. There is no runtime on the page to run a \
             signal or attach a listener, so the feature would silently do nothing."
        }
        Code::RA011 => {
            "The router renders the default export as the route. An export it cannot call, cannot \
             fill, or cannot get markup from leaves the route with nothing to render. A second \
             default export is a parse error."
        }
        Code::RA012 => {
            "The directive decides whether the route ships JavaScript. Inferring it would hide the \
             decision that controls the 0 KB guarantee."
        }
        Code::RA014 => {
            "The code covers three families, and each would be wrong silently rather than loudly. \
             \
             The JSX constructs it refuses are a capitalized tag or member-expression tag \
             (component inlining comes with a later plan), the runtime's own \
             `<For>`/`<Show>`/`<When>` primitives (their lowering comes with a later plan and is \
             not component inlining), a namespaced tag, a `<this>` tag, JSX inside a `{…}` \
             expression, a JSX-valued attribute, a spread attribute or child, a fragment as the \
             returned root, a second JSX return, JSX in any other route body statement, and JSX in \
             a module-level statement of a route file. Each would either drop part of a node or \
             copy JSX into a JavaScript module where it cannot parse. Module-level JSX is refused \
             only in a file that has a route: a `.tsx` file with no default export is a component \
             module, and a helper there may legitimately hold module-level JSX. The plan's later \
             milestones lift these one slice at a time. \
             \
             An import the emitted module cannot write is refused for one of two reasons. A \
             module other than `\"runai\"` has no bundler yet: each route is emitted from its own \
             file's text, so a second module cannot be resolved or inlined, and the emitted module \
             would not load. This lifts when bundling lands. A default (`import runai from \
             \"runai\"`) or namespace (`import * as runai from \"runai\"`) import of `\"runai\"` is \
             refused because the island emitter writes exactly one import, merging the route's \
             named specifiers only; the repair is to rewrite the declaration as named imports. \
             `import type` — and the inline `import { type X }` form — is exempt, because types are \
             stripped and leave no runtime import behind. \
             \
             The third family is a value the generated module genuinely cannot see, and it has \
             two members only. A `\"runai\"` import used in a `\"use static\"` route's markup is \
             refused because the prerender module contains no imports at all, so the repair is the \
             directive, `\"use island\"`. The route's own parameter in a `\"use static\"` route is \
             refused because `render()` takes no arguments, so `{props}` would be `undefined` at \
             build time. A module-level declaration is deliberately **not** in this family: \
             module-scope statements are copied into the emitted module above the component (design \
             §7.6 expects module-level state — \"module-scope signals are per-island too, since \
             each island is its own module instance\"), so a module-level `const` the markup names \
             is reachable. \
             \
             An attribute with an empty expression (`name={}`) is deliberately **not** on this \
             list: that is a parse error, reported as `RA007`, not valid syntax waiting for a later \
             milestone."
        }
    }
}

/// The Markdown body for one code: what it means, why it is an error, and how to
/// fix it.
pub fn skill_body(code: Code) -> String {
    format!(
        "## What it means\n\n{means}\n\n## Why it is an error\n\n{why}\n\n## Fix\n\n{fix}\n",
        means = code.explain(),
        why = why(code),
        fix = code.fix_template(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostic::{RA009_CONSEQUENCE, RA009_FALSE_CLAIMS};

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

    /// The diagnostic and the skill cannot drift: the section an agent copies is
    /// the exact string [`Diagnostic::new`] puts in `fix`.
    #[test]
    fn every_code_has_a_skill_whose_fix_matches_the_diagnostic() {
        for code in Code::all() {
            let body = skill_body(*code);
            assert!(
                body.contains(code.fix_template()),
                "fix drifted for {}",
                code.as_str()
            );
            assert!(
                body.contains(code.explain()),
                "explain drifted for {}",
                code.as_str()
            );
        }
    }

    /// Task 15 ships this body to users, and the RA001 skill must document the
    /// exemption that makes the rule livable: a read inside an event handler or any
    /// other nested callback runs at call time, sees the current value, and is never
    /// flagged. That is a stated deliverable of the task, so a reword cannot silently
    /// drop it.
    #[test]
    fn ra001_documents_the_handler_and_nested_callback_exemption() {
        let body = skill_body(Code::RA001);
        for phrase in [
            "`onClick={() => count()}`",
            "`setTimeout(() => count(), 10)`",
            "nested callback",
            "sees the current value",
            "never flagged",
        ] {
            assert!(body.contains(phrase), "missing {phrase}: {body}");
        }
    }

    /// The RA009 repair must be *legal on its own*: `RA002` fires unconditionally for a
    /// write whose nearest function is an `effect` body, so a fix that says "move it into
    /// an `effect`" lands the agent in `RA002` at the same span on the next run.
    ///
    /// The text must also state only what the runtime does. It flushes writes
    /// synchronously, so an already-live subscriber — an effect or a `_setText` node —
    /// *does* see the new value (`effect(() => seen.push(n()))` then `n(1)` gives
    /// `seen === [0, 1]`). The one unconditionally true consequence is that the component
    /// body runs once, so the write cannot re-run the component. Every earlier wording
    /// asserted a reader consequence the runtime denies, so the false-claim family is
    /// banned here and in `render_write`'s message (pinned in `analyze::rules`).
    #[test]
    fn ra009_offers_a_legal_repair_and_states_only_the_true_consequence() {
        let assert_clean = |what: &str, text: &str| {
            for false_claim in RA009_FALSE_CLAIMS {
                assert!(
                    !text.to_ascii_lowercase().contains(false_claim),
                    "false claim `{false_claim}` in the {what}: {text}"
                );
            }
        };

        let fix = Code::RA009.fix_template();
        // Any casing of `effect` — `effect(`, `` `effect` ``, "an effect" — names a
        // repair that is `RA002` at the same span, so a bare `effect(` check is not
        // enough. `to_ascii_lowercase` also covers `Effect`.
        assert!(
            !fix.to_ascii_lowercase().contains("effect"),
            "illegal repair, names `effect` in some casing: {fix}"
        );
        assert!(fix.contains("Derive the value"), "{fix}");
        assert!(fix.contains("event handler"), "{fix}");
        assert_clean("fix", fix);
        assert!(
            fix.contains(RA009_CONSEQUENCE),
            "the shared sentence is missing: {fix}"
        );

        let explain = Code::RA009.explain();
        assert_clean("explain", explain);
        assert!(
            explain.contains(RA009_CONSEQUENCE),
            "the shared sentence is missing: {explain}"
        );

        let why = skill_body(Code::RA009);
        assert_clean("skill body", &why);
        assert!(
            why.contains(RA009_CONSEQUENCE),
            "the shared sentence is missing from the skill: {why}"
        );
    }

    /// Task 15 ships this body to users, and the RA014 skill must say which
    /// constructs the milestone refuses rather than leaving an agent to discover
    /// them one cold run at a time. Every construct `analyze::jsx::check_jsx`
    /// emits must appear here, and the empty-attribute case must be described as
    /// a parse error rather than listed as valid syntax.
    ///
    /// The import and unreachable-value families came second, and each needs its
    /// own sentence: an agent that reads "a capitalized tag" for an import
    /// diagnostic is told about a construct it did not write. The repair text for
    /// each family is asserted too, because a skill whose fix does not apply is
    /// the worst kind of defect in this repository.
    #[test]
    fn ra014_explains_that_the_syntax_is_valid() {
        let body = skill_body(Code::RA014);
        assert!(body.contains("limitation of the compiler"), "{body}");
        for construct in [
            "capitalized tag or member-expression tag",
            "namespaced tag",
            "`<this>` tag",
            "JSX inside a `{…}` expression",
            "JSX-valued attribute",
            "spread attribute or child",
            "returned root",
            "second JSX return",
            "JSX in any other route body statement",
            "module-level statement of a route file",
            "`<For>`/`<Show>`/`<When>`",
        ] {
            assert!(body.contains(construct), "missing {construct}: {body}");
        }
        // The scope boundary, because it is what keeps a component module legal.
        assert!(
            body.contains("a `.tsx` file with no default export is a component module"),
            "the skill must state where the module-level refusal stops: {body}"
        );
        // The runtime primitives are not components: a skill that sent an agent to
        // component inlining for them would describe a mechanism that does not
        // apply to the tag it wrote.
        assert!(
            body.contains("not component inlining"),
            "the skill must not call the runtime primitives components: {body}"
        );
        // The carried defect: an empty attribute expression is `RA007`, so
        // listing it as "valid syntax this milestone lacks" sends an agent
        // looking for a later milestone instead of at the parse error.
        assert!(
            body.contains("that is a parse error, reported as `RA007`"),
            "the empty-attribute case must be described as a parse error: {body}"
        );
    }

    /// `entry.rs` reports `RA014` for `import x from "./data"` and for a default
    /// or namespace `"runai"` import, and `reachable` reports it for a `"runai"`
    /// name in a static route and for the route's parameter. The skill and the
    /// fix are what an agent reads first, so both must describe those cases and
    /// give a repair that is legal when applied verbatim.
    ///
    /// The module-level-value case used to be on this list and is deliberately
    /// gone: module statements are emitted now, so a skill still claiming "move
    /// it into the body" would prescribe a rewrite for a file that already works.
    #[test]
    fn ra014_explains_the_import_and_unreachable_value_cases() {
        let body = skill_body(Code::RA014);
        for phrase in [
            "An import the emitted module cannot write",
            "no bundler yet",
            "`import { type X }`",
            "namespace (`import * as runai from \"runai\"`)",
            "rewrite the declaration as named imports",
            "a value the generated module genuinely cannot see",
            "no imports at all",
            "route's own parameter",
        ] {
            assert!(
                body.contains(phrase),
                "the import/value families are missing `{phrase}`: {body}"
            );
        }
        // The reversal, stated where an agent reads it: a module-level value is
        // reachable, so the skill must not send one into the component body.
        assert!(
            body.contains("A module-level declaration is deliberately **not** in this family"),
            "the skill must say a module-level value is reachable: {body}"
        );

        let fix = Code::RA014.fix_template();
        for repair in [
            "inline the value",
            "import it from `\"runai\"`",
            "rewrite it as named imports",
            "`\"use island\"`",
            "write the value in the component body",
        ] {
            assert!(fix.contains(repair), "missing repair `{repair}`: {fix}");
        }
        // The import clause comes first: an agent reading only `fix` on an import
        // diagnostic must not be led to the JSX repairs.
        assert!(
            fix.find("For an import").unwrap() < fix.find("For a JSX construct").unwrap(),
            "the import repair must precede the JSX repairs: {fix}"
        );

        // The explain carries all three families, so `runai check` on an import
        // diagnostic does not explain a JSX construct the file does not contain.
        let explain = Code::RA014.explain();
        for family in [
            "JSX construct",
            "An `import`",
            "a value the generated module",
        ] {
            assert!(explain.contains(family), "missing `{family}`: {explain}");
        }
    }
}
