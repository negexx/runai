//! The Markdown of one diagnostic's skill file.
//!
//! `runai skills` (Task 15) writes one file per [`Code`], and the text comes
//! from here so the diagnostic and its skill cannot drift: both describe the
//! same rule, and the drift test below refuses an empty arm.
//!
//! The file opens with frontmatter (`---`, `name`, `description`, `---`) because
//! a skill file is loaded by tooling that reads the name and the one-line
//! description before the prose. `## What it means` is [`Code::explain`] and
//! `## Fix` is [`Code::fix_template`] verbatim — the same two functions
//! [`crate::diagnostic::Diagnostic::new`] uses, so a generated skill cannot
//! disagree with what the compiler reports. Only `## Why it is an error` and the
//! description are written per code here, because they say what the mistake
//! costs, which the repair text does not.

use crate::diagnostic::Code;

/// The one-line `description:` in the frontmatter: what the rule rejects, in the
/// same words the design's strictness table uses.
///
/// One line, because YAML frontmatter has no continuation here and a wrapped
/// value would read as a new key. It names the rule rather than repeating
/// [`Code::explain`], which is the body's first section.
fn description(code: Code) -> &'static str {
    match code {
        Code::RA001 => {
            "A signal read in the component body outside any tracking scope freezes at the first \
             render."
        }
        Code::RA002 => {
            "A signal written inside an `effect` body makes the write its own dependency."
        }
        Code::RA006 => {
            "A component's props parameter has no explicit type, or there is no parameter at all."
        }
        Code::RA007 => {
            "The parser rejected the file, or a line break sits between two inline elements and \
             deletes a visible space."
        }
        Code::RA009 => "A signal is written during render, which is a side effect in rendering.",
        Code::RA010 => {
            "A `\"use static\"` route uses a signal, effect, resource, write, or handler it cannot run."
        }
        Code::RA011 => {
            "A page file's default export is missing, is not a component, or cannot render the \
             route."
        }
        Code::RA012 => "The file opens without a `\"use static\"` or `\"use island\"` directive.",
        Code::RA014 => {
            "Valid syntax this milestone cannot compile — an unsupported construct, import, \
             unreachable value, `declare`, or reserved name."
        }
    }
}

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
             it to prose. The next reader — human or agent — has to guess what may be passed. The \
             canonical fix is one sentence for every site, so the per-file half lives in the \
             diagnostic's `message`: it quotes the exact signature line to write, in the \
             declaration form the file already uses, naming the binding that exists rather than a \
             synthesized `function Page(…)`."
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
            "The router renders the default export as the route, and it has no other source. Three \
             shapes produce this code, and the `message` says which one the file has: there is no \
             default export at all; a default export exists but is not a function this milestone \
             can analyze (a class, a literal, a call, or an identifier that cannot be resolved in \
             this file); or it is a function whose body does not `return <…/>` directly, or it \
             takes more than one parameter. Only a direct `return` is recognized, because \
             resolving what a returned variable or conditional renders needs type information this \
             compiler does not have yet. An export the router cannot call, cannot fill, or cannot \
             get markup from leaves the route with nothing to render. The canonical fix is one \
             sentence for every site, so the per-file half is in the `message`: it names the \
             existing declaration and, for a named export, quotes the signature line to edit."
        }
        Code::RA012 => {
            "The directive decides whether the route ships JavaScript. Inferring it would hide the \
             decision that controls the 0 KB guarantee."
        }
        Code::RA014 => {
            "`RA014` is what the compiler reports when the file parses but this milestone cannot \
             compile it: each refusal is a limitation of the compiler, not a syntax error. The \
             code covers five families, and each would be wrong silently rather than loudly.\
             \n\n\
             The first is the JSX constructs it refuses: a capitalized tag or member-expression tag \
             (component inlining comes with a later plan), the runtime's own \
             `<For>`/`<Show>`/`<When>` primitives (their lowering comes with a later plan and is \
             not component inlining), a namespaced tag, a `<this>` tag, JSX inside a `{…}` \
             expression, a JSX-valued attribute, a spread attribute or child, a fragment as the \
             returned root, a second JSX return, JSX in any other route body statement, and JSX in \
             a module-level statement of a route file. Each would either drop part of a node or \
             copy JSX into a JavaScript module where it cannot parse. Module-level JSX is refused \
             only in a file that has a route: a `.tsx` file with no default export is a component \
             module, and a helper there may legitimately hold module-level JSX. The plan's later \
             milestones lift these one slice at a time. Four emit-time shapes join this same first \
             family because the artifact they would produce is wrong rather than merely \
             unsupported: children inside a void element, which the HTML parser would move outside \
             it; a duplicate attribute, which a prerendered page would resolve to the first value \
             and a hydrated island to the last; an early `return` before the route's JSX return, \
             which `render()` cannot carry because it returns one string; and an `async` or generator \
             route component: both modes call the component synchronously and use its return \
             value, so a generator route component's iterator and an `async` one's promise are not \
             the markup either mode inserts.\
             \n\n\
             A further group inside that first family is markup the HTML parser would **reshape**, so \
             an island's emitted HTML text re-parses into a different tree than the author wrote \
             and a marker would address the wrong node. Each is refused rather than emitted, and \
             the `message` names the parent, the child, what the parser does, and the repair: a \
             `<p>` containing a block-level element, which closes the paragraph so the child \
             becomes a sibling; a tag that **closes an open element of its own kind** (`<li>` in \
             `<li>`, `<p>` in `<p>`, `<tr>` in `<tr>`, `<td>`/`<th>` in either, any heading in any \
             other, `<button>`, `<option>`/`<optgroup>`, `<a>`, `<nobr>`); a child that violates its \
             parent's **content model** (`<table>` holding anything but its table parts, `<ul>`/\
             `<ol>`/`<menu>` anything but `<li>`, `<dl>` anything but `<dt>`/`<dd>`/`<div>`, and so \
             on), or bare text fostered out of a table family element; an `<a>` inside an `<a>` or \
             a `<form>` inside a `<form>`; an `<li>`/`<dt>`/`<dd>` with no list to belong to; a tag \
             the parser only builds under one parent (`<td>` outside `<tr>`, `<option>` outside \
             `<select>`, `<figcaption>` outside `<figure>`, …); and, in an island, a dynamic \
             binding inside a text-only element (`<script>`, `<style>`, `<title>`, `<textarea>`, \
             `<noscript>`, …), where the `<!>` marker the emitter needs is swallowed as text. The \
             repair is to move the child to where its content model allows it, or to write the \
             value as static text or on a sibling element that holds normal content.\
             \n\n\
             An import or re-export the emitted module cannot resolve or write is the second family, \
             refused for one of three reasons. A module other than `\"runai\"` has no bundler yet: \
             each route is emitted from its own file's text, so a second module can be neither \
             resolved nor inlined, and the emitted module would not load. The same \
             holds for a re-export (`export … from \"./x\"`, `export * from \"./x\"`): the emitted \
             file writes one import or none, so the statement names a module nothing resolves. \
             This lifts when bundling lands. A re-export whose source *is* `\"runai\"` is refused \
             for a different reason — the statement is never emitted, so accepting it would drop \
             the author's statement with no diagnostic — and the repair is `import { owner } from \
             \"runai\"`, then use or export the written binding. A default (`import runai from \
             \"runai\"`) or namespace (`import * as runai from \"runai\"`) import of `\"runai\"` is \
             refused because the island emitter writes exactly one import, merging the route's \
             named specifiers only; the repair is to rewrite the declaration as named imports. \
             And the CommonJS spellings TypeScript allows — `import x = require(\"./x\")` and \
             `export = x` — are refused because type stripping rewrites them to `require(…)` and \
             `module.exports = …`, which an ES module does not have. `import type` — and the \
             inline `import { type X }` form — is exempt, because types are stripped and leave no \
             runtime import behind.\
             \n\n\
             The third family is a value the generated module genuinely cannot see, and it has \
             two members only. A `\"runai\"` name, or the route component's own name, used at \
             module scope, in a component-body statement, or in a `\"use static\"` route's markup \
             is refused because the prerender module contains no imports and does not declare the \
             component, so the repair is the directive, `\"use island\"`. The route's own parameter \
             in a `\"use static\"` route is refused because `render()` takes no arguments, so \
             `{props}` or `const x = props` would be `undefined` at build time; at **module scope** \
             it is refused in an island too, because no emitter declares the parameter there \
             either. A module-level declaration is deliberately **not** in this family: \
             module-scope statements are copied into the emitted module above the component (design \
             §7.6 expects module-level state — \"module-scope signals are per-island too, since \
             each island is its own module instance\"), so a module-level `const` the markup names \
             is reachable. A component-body statement's own local is not in this family either: \
             the statement is copied into the component, so the local exists there.\
             \n\n\
             A `declare` statement is the fourth family: it is erased before emission, so the \
             binding it introduces never exists in the generated module, and a route file cannot \
             declare a binding it does not define.\
             \n\n\
             The parameter list itself is a **shape** the emitters rebuild rather than copy: both \
             write the component as `function <Name>(props)`, so a default, a destructuring \
             pattern, a rest parameter, or a different name is dropped, and everything it binds \
             does not exist in the artifact. That is why `props: P = owner` is refused in an \
             island where `owner` is otherwise a real name — the defect is the dropped signature, \
             not the reference.\
             \n\n\
             The fifth family is the generated module's own **reserved** names: `_esc`, `_attr`, \
             and `render`. A declaration, export name, or reference to one of them is refused \
             because the emitted module declares those names itself, so a route binding would \
             shadow a helper the template calls or export `render` twice. A `var` makes this \
             sharper than a name scan suggests: it hoists to the route body's own function scope, \
             which is the scope the emitter copies into `render()`, so a `var` in a nested block, \
             a `for` init, or a `try`/`catch` reaches the template's call site even though it is \
             not written at the top of the body.\
             \n\n\
             An attribute with an empty expression (`name={}`) is deliberately **not** on this \
             list: that is a parse error, reported as `RA007`, not valid syntax waiting for a later \
             milestone."
        }
    }
}

/// The Markdown of one code's skill file: frontmatter, then what it means, why it
/// is an error, and how to fix it.
///
/// `runtime.js` is not the only thing `npm install` has to put on disk — the
/// `doc` every diagnostic prints names a file in this same package — so the
/// frontmatter names the code the way [`Code::as_str`] does, and that string is
/// the one the file is named after.
pub fn skill_body(code: Code) -> String {
    format!(
        "---\nname: {name}\ndescription: {description}\n---\n\n\
         ## What it means\n\n{means}\n\n## Why it is an error\n\n{why}\n\n## Fix\n\n{fix}\n",
        name = code.as_str(),
        description = description(code),
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

    /// `runai skills` writes a file per code, and the file is a skill: it opens
    /// with YAML frontmatter naming the code and carrying a one-line description.
    /// Both are assertions rather than prose, because a generator that emitted a
    /// body with no frontmatter would still pass every section test above.
    ///
    /// The description must be **one line** and must not contain `: ` outside
    /// backticks — a colon in a plain YAML scalar starts a new mapping and makes
    /// the frontmatter unparseable, which fails silently in a loader that ignores
    /// the error.
    #[test]
    fn every_skill_has_frontmatter_naming_its_code() {
        for code in Code::all() {
            let body = skill_body(*code);
            assert!(
                body.starts_with("---\n"),
                "{}: no frontmatter: {body}",
                code.as_str()
            );
            let name = format!("name: {}\n", code.as_str());
            assert!(body.contains(&name), "{}: {body}", code.as_str());

            let rest = body.strip_prefix("---\n").unwrap();
            let front = &rest[..rest.find("\n---\n").expect("frontmatter must close")];
            let description = front
                .lines()
                .find_map(|line| line.strip_prefix("description: "))
                .unwrap_or_else(|| panic!("{}: no description: {front}", code.as_str()));
            assert!(
                !description.trim().is_empty(),
                "{}: empty description",
                code.as_str()
            );
            // A second `description:` line would mean one wrapped description was
            // read as two keys; a `: ` inside the value is the same YAML hazard.
            assert_eq!(
                front.matches("description:").count(),
                1,
                "{}: one description line: {front}",
                code.as_str()
            );
            assert!(
                !description.contains(": "),
                "{}: a plain YAML scalar cannot contain `: `: {description}",
                code.as_str()
            );
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

    /// `RA014`'s prose counts its families, and the count has to match the number
    /// it lists. Round 1 left it saying "four families" and then naming a
    /// "fifth", and the fifth clause came before the third and fourth — a linear
    /// reader met the ordinal out of order, which is the kind of prose an agent
    /// triages on. Both texts now say five and name the fifth last.
    ///
    /// The order assertion pins **one anchor phrase per family**, not the ordinal
    /// words: the loop above already proves "fifth" exists, so checking
    /// `third < fifth` would still pass if a family paragraph were deleted as long
    /// as the word "third" survived in a neighbouring clause. Each anchor below is
    /// a sentence that only that family's paragraph contains, so deleting a
    /// family removes its anchor and fails the test.
    #[test]
    fn ra014_counts_and_orders_its_five_families() {
        // One anchor per family, in order. `explain` and the skill's
        // `## Why it is an error` are separate prose, so each gets its own
        // phrasing of the same five anchors.
        let explain_anchors = [
            // (1) a JSX construct the milestone cannot lower
            "A first is a JSX construct this milestone",
            // (2) a module the emitted file cannot resolve or write
            "A second is a module the emitted file cannot resolve or write",
            // (3) a value the generated module cannot see
            "A third is a value the generated module cannot see",
            // (4) a `declare` statement
            "A `declare` statement is refused: it is the fourth family",
            // (5) the emitted module's own reserved names
            "A fifth is the emitted module's own names",
        ];
        let why_anchors = [
            "The first is the JSX constructs it refuses",
            "is the second family, refused for one of three reasons",
            "The third family is a value the generated module genuinely cannot see",
            "A `declare` statement is the fourth family",
            "The fifth family is the generated module's own **reserved** names",
        ];
        let skill = skill_body(Code::RA014);
        let why =
            &skill[skill.find("## Why it is an error").unwrap()..skill.find("## Fix").unwrap()];
        for (what, text, anchors) in [
            (
                "explain",
                Code::RA014.explain().to_string(),
                explain_anchors,
            ),
            ("skill", why.to_string(), why_anchors),
        ] {
            assert!(text.contains("five families"), "{what}: {text}");
            assert!(!text.contains("four families"), "{what}: {text}");
            let positions: Vec<usize> = anchors
                .iter()
                .map(|anchor| {
                    let count = text.matches(anchor).count();
                    assert_eq!(
                        count, 1,
                        "{what}: family anchor `{anchor}` must appear exactly once: {text}"
                    );
                    text.find(anchor).unwrap()
                })
                .collect();
            assert!(
                positions.windows(2).all(|pair| pair[0] < pair[1]),
                "{what}: the five families must appear in order: {positions:?}\n{text}"
            );
        }
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
            "children inside a void element",
            "a duplicate attribute",
            "an early `return` before the route's JSX return",
            "a generator route component",
            "reserved",
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
            "An import or re-export the emitted module cannot resolve or write",
            "no bundler yet",
            "`import { type X }`",
            "namespace (`import * as runai from \"runai\"`)",
            "rewrite the declaration as named imports",
            "`export … from \"./x\"`",
            "`import x = require(\"./x\")`",
            "`export = x`",
            "a value the generated module genuinely cannot see",
            "no imports and does not declare the component",
            "route component's own name",
            "route's own parameter",
            "A `declare` statement is the fourth family",
            "The parameter list itself is a **shape**",
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
            "replace the parameter reference with a literal value",
            "replace it with a value this file defines",
            "write the parameter as `(props: <Type>)`",
            "`export =` as `export default`",
        ] {
            assert!(fix.contains(repair), "missing repair `{repair}`: {fix}");
        }
        // The circular repair must be gone: a reference may already be in the
        // component body, so "write the value in the body" leaves nowhere to go.
        assert!(
            !fix.contains("write the value in the component body"),
            "the circular parameter repair is gone: {fix}"
        );
        // The import clause comes first: an agent reading only `fix` on an import
        // diagnostic must not be led to the JSX repairs.
        assert!(
            fix.find("For an import or re-export").unwrap()
                < fix.find("For a JSX construct").unwrap(),
            "the import repair must precede the JSX repairs: {fix}"
        );

        // The explain carries all five families, so `runai check` on an import
        // diagnostic does not explain a JSX construct the file does not contain.
        let explain = Code::RA014.explain();
        for family in [
            "JSX construct",
            "an `import` or re-export",
            "a value the generated module",
            "`import x = require(…)`",
            "A `declare` statement is refused",
        ] {
            assert!(explain.contains(family), "missing `{family}`: {explain}");
        }
    }

    /// Task 10 added five refusals whose defects are not "a JSX construct cannot
    /// be lowered" and whose repairs are not in the older clauses. Each must be
    /// named in the skill **and** in the canonical fix, and the canonical fix
    /// must name a repair that is legal when applied verbatim — a duplicate
    /// attribute cannot be fixed by "give the attribute a value", which would
    /// leave the duplicate in place and the next run would report the same row.
    ///
    /// The clauses are per-code ("For X, do Y"), not interpolated per-site text:
    /// the per-file specifics belong in the diagnostic's `message` (Task 2's
    /// ruling), so this asserts the code-wide sentence, not filenames or spans.
    #[test]
    fn ra014_names_the_emit_time_refusals_in_skill_and_fix() {
        let body = skill_body(Code::RA014);
        for (label, phrase) in [
            // (a) void element with children
            ("void with children", "children inside a void element"),
            // (b) duplicate attribute
            ("duplicate attribute", "a duplicate attribute"),
            // (c) early non-JSX return
            (
                "early return",
                "an early `return` before the route's JSX return",
            ),
            // (d) async or generator component
            ("async/generator", "a generator route component"),
            // (e) reserved generated name
            ("reserved name", "reserved"),
        ] {
            assert!(
                body.contains(phrase),
                "{label}: the skill must name the refusal `{phrase}`: {body}"
            );
        }

        let fix = Code::RA014.fix_template();
        for (label, repair) in [
            // (a) the legal repair moves the children out of the void element.
            (
                "void with children",
                "For children inside a void element, move them out of it or use an element that \
                 accepts children",
            ),
            // (b) keeping one is the only legal repair; "give it a value" cannot
            // remove a duplicate.
            (
                "duplicate attribute",
                "For a duplicate attribute, keep exactly one of them",
            ),
            // (c) the body must return one element.
            (
                "early return",
                "For an early `return` before the route's JSX return, return one element from \
                 the component",
            ),
            // (d) remove the modifier.
            (
                "async/generator",
                "For an `async` or generator route component, remove the modifier",
            ),
            // (e) rename.
            (
                "reserved name",
                "For a declaration, export name, or reference that uses a reserved generated \
                 name (`_esc`, `_attr`, or `render`), rename it",
            ),
        ] {
            assert!(
                fix.contains(repair),
                "{label}: the canonical fix must carry a legal repair, missing: {repair}\n{fix}"
            );
        }
    }

    /// The reparse family is the largest class of `RA014` a route can actually hit:
    /// every `<p>` holding a block element, every same-kind nesting, every content
    /// model violation, every nested `<a>`/`<form>`, every stray list item, every
    /// required-ancestor tag, and the island marker-hostile rule. Task 11/13 added
    /// all of them and they were named in **neither** the skill nor `explain`, so a
    /// diagnostic from that family pointed at a skill describing JSX constructs and
    /// imports the file did not contain. Each anchor below is the phrase the skill
    /// now gives; deleting a family removes its anchor and fails the test.
    #[test]
    fn ra014_names_the_reparse_family_in_skill_and_explain() {
        let body = skill_body(Code::RA014);
        for (label, phrase) in [
            ("heading", "markup the HTML parser would **reshape**"),
            ("block in p", "a `<p>` containing a block-level element"),
            ("same kind", "closes an open element of its own kind"),
            ("content model", "**content model**"),
            (
                "bare text",
                "bare text fostered out of a table family element",
            ),
            ("nested anchor", "an `<a>` inside an `<a>`"),
            ("nested form", "a `<form>` inside a `<form>`"),
            ("stray item", "with no list to belong to"),
            ("required parent", "only builds under one parent"),
            (
                "marker hostile",
                "dynamic binding inside a text-only element",
            ),
        ] {
            assert!(
                body.contains(phrase),
                "{label}: the reparse family must be named: `{phrase}`\n{body}"
            );
        }

        // `explain` is what `runai check` prints, so the same family has to be
        // reachable there: an agent reading a `<p><div>` diagnostic must not be
        // told only about component tags.
        let explain = Code::RA014.explain();
        for phrase in [
            "markup the HTML parser would reshape",
            "holding a block-level element",
            "closes an open element of its own kind",
            "content model",
            "inside an `<a>`",
            "inside a `<form>`",
            "with no list",
            "only builds under one parent",
            "text-only element",
        ] {
            assert!(
                explain.contains(phrase),
                "the reparse family must reach `explain` too: `{phrase}`\n{explain}"
            );
        }
    }

    /// Task 3 round 2 rewrote RA007's fix to be cause-agnostic: it no longer
    /// asserts JSX causes for a parser error that is not about JSX (`const a = 1 =
    /// 2` is a parse error with no tag in it). The skill quotes `fix_template`
    /// verbatim, so it inherits that wording — but the skill's own prose is a
    /// separate paragraph and could reintroduce a JSX-only claim. Both halves are
    /// asserted here, because the drift test for the fix cannot see the prose.
    ///
    /// `tag` is the proxy Task 3's own `source.rs` test uses; `unclosed` and
    /// `sibling` are the concrete causes. The honest text says *the usual causes*
    /// and points at `message`, which is what the agent's own file carries.
    #[test]
    fn ra007_skill_does_not_assert_a_jsx_cause_for_a_non_jsx_parse_error() {
        let body = skill_body(Code::RA007);
        let why = &body[body.find("## Why it is an error").unwrap()..body.find("## Fix").unwrap()];
        // The prose may name the line-break case (it *is* a JSX cause), but it must
        // not claim every RA007 is a JSX tag problem. `unclosed` is fine only in
        // the fix's "usual causes" clause, which is where it belongs — so the
        // assertion is scoped to the `why` section, which must stay cause-agnostic.
        assert!(
            !why.contains("unclosed tag"),
            "the why section must not claim a JSX cause for every RA007: {why}"
        );
        // The fix does name the usual causes, and it must frame them as usual, not
        // as the cause: "the usual causes are" is the load-bearing phrase.
        let fix = Code::RA007.fix_template();
        assert!(
            fix.contains("the usual causes are"),
            "the fix must offer JSX causes as usual, not certain: {fix}"
        );
        assert!(
            fix.starts_with("Fix the syntax error the parser reported in `message`"),
            "the fix must point at the parser's own message first: {fix}"
        );
        // And the skill carries the repair that names the parser, so an agent
        // reading the file has the same instruction the diagnostic printed.
        assert!(
            body.contains(fix),
            "the skill must quote the current fix: {body}"
        );
    }

    /// RA011 has three constructors with different wording — no default export, a
    /// default export that does not return JSX, and one that is not a function this
    /// milestone can analyze — plus the >1 parameter case. A skill that described
    /// only "add a default export" would send an agent with an existing export into
    /// a duplicate-export parse error, which is the repair loop this code exists to
    /// prevent. So all four shapes are named, and the skill says the `message`
    /// carries which one the file has.
    #[test]
    fn ra011_skill_names_every_constructor() {
        let body = skill_body(Code::RA011);
        for (label, phrase) in [
            ("missing", "there is no default export at all"),
            (
                "not a function",
                "a default export exists but is not a function this milestone can analyze",
            ),
            ("no JSX", "does not `return <…/>` directly"),
            ("too many params", "takes more than one parameter"),
            ("per site", "the `message` says which one the file has"),
        ] {
            assert!(
                body.contains(phrase),
                "{label}: RA011's skill must name this shape: `{phrase}`\n{body}"
            );
        }
        // The duplicate-export hazard is stated, because "add a default export" is
        // the wrong repair for two of the three shapes.
        assert!(
            body.contains("existing declaration"),
            "the skill must say the fix edits the existing declaration: {body}"
        );
    }

    /// RA006's and RA011's fixes are static per code: the per-file half (the exact
    /// signature line, the declaration's name) lives in the diagnostic's `message`,
    /// because a static skill cannot name a binding it has never seen. This pins
    /// that split, so a future "improvement" that interpolates `{name}` into
    /// `fix_template` fails here rather than shipping a skill that says
    /// `function {name}(…)`.
    #[test]
    fn per_site_fixes_are_described_not_quoted() {
        for code in Code::all() {
            let fix = code.fix_template();
            assert!(
                !fix.contains("{name}") && !fix.contains("{}"),
                "{}: fix_template must be static; per-site text belongs in `message`: {fix}",
                code.as_str()
            );
        }
        // And the two codes whose sites differ say where the per-site half is.
        for (code, what) in [
            (Code::RA006, "the diagnostic's `message`"),
            (Code::RA011, "the `message`"),
        ] {
            let body = skill_body(code);
            assert!(
                body.contains(what),
                "{}: the skill must point at the per-site text: {body}",
                code.as_str()
            );
        }
    }

    /// The committed copy under `packages/runai/skills/` is what `npm install`
    /// ships, and it is the file every diagnostic's `doc` names. If it drifts from
    /// the generator, an agent reads a skill that describes a rule the compiler no
    /// longer reports — and nothing else would notice.
    ///
    /// Both directions matter and both are asserted: a missing file is a `doc`
    /// path that 404s, and a stale file is worse, because it *looks* authoritative.
    #[test]
    fn committed_skills_match_the_generator() {
        let dir =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packages/runai/skills");
        for code in Code::all() {
            let path = dir.join(code.skill_file());
            let committed = std::fs::read_to_string(&path).unwrap_or_else(|_| {
                panic!(
                    "missing {}; run `cargo run -p runai_cli -- skills --out packages/runai/skills`",
                    path.display()
                )
            });
            assert_eq!(
                committed,
                skill_body(*code),
                "stale {}; run `cargo run -p runai_cli -- skills --out packages/runai/skills`",
                path.display()
            );
        }

        // A file for a code that no longer exists is dead documentation an agent
        // may still reach through an old link, and the count has to be exact: one
        // file per code in `Code::all()`.
        let mut on_disk: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|entry| {
                let name = entry.ok()?.file_name().to_string_lossy().into_owned();
                name.ends_with(".md").then_some(name)
            })
            .collect();
        let mut expected: Vec<String> = Code::all().iter().map(Code::skill_file).collect();
        on_disk.sort();
        expected.sort();
        assert_eq!(
            on_disk, expected,
            "packages/runai/skills must hold exactly one file per code"
        );
    }
}
