//! The invariant that every instruction an agent is told to follow is **typeable**.
//!
//! Two defect classes this repository treats as its worst live here, and both
//! shipped once:
//!
//! 1. A repair that contains a `<…>` metavariable (`(props: <Type>)`). Applied
//!    verbatim it is a syntax error, so the agent's next run reports a new error
//!    at the same span.
//! 2. A repair that names a construct this milestone refuses (`<For each={items}>`
//!    for a spread child, `<Show>` for a JSX conditional). Applied verbatim it
//!    produces two more diagnostics, forever.
//!
//! The predicate that catches both is [`crate::diagnostic::instruction_bracket`]:
//! a `<` followed by an ASCII uppercase letter, where a lowercase DOM tag
//! (`<div>`, `<td>`, `<my-element>`) is an allowance because it *is* what to
//! write. This module applies it to the corpus of user-facing text the compiler
//! actually produces.
//!
//! **What is covered** (stated exactly, because a predicate applied to the wrong
//! set cannot catch its own defect):
//!
//! - every [`crate::diagnostic::Code::fix_template`], whole and strict: a fix is
//!   pure instruction, with no naming half to allow;
//! - the `## Fix` section of every generated skill, which is that same string
//!   verbatim (`skill_body` cannot drift from it — pinned elsewhere);
//! - the **instruction half** of every per-site `message` the rules compose
//!   (everything after the first `": "`, or the whole message when there is
//!   none), driven through [`crate::analyze::analyze`] *and* through the shared
//!   refusal composers that lower-level callers use;
//! - the per-site directive repairs, which must name no line at all.
//!
//! Not covered, deliberately: prose (`explain`, a skill's `## Why it is an
//! error`) names constructs as *things the file contains* — "a `<p>` holding a
//! block element", "`` `<For>` is a runtime primitive ''" — so a bracketed
//! uppercase name there is a symbol, not an instruction. `codegen`'s two
//! emitter-mode refusals and `build.rs`'s JSON-schema error are outside the
//! analysis pipeline; they are asserted directly where they live.

#[cfg(test)]
mod tests {
    use crate::analyze;
    use crate::analyze::jsx::{
        BODY_JSX_REPAIR, JSX_IN_EXPRESSION_REPAIR, MODULE_JSX_REPAIR, SPREAD_CHILD_REPAIR,
        ancestor_refusal, marker_hostile_message, parent_child_refusal, self_closing_refusal,
        text_refusal,
    };
    use crate::diagnostic::{
        Code, angle_bracket_placeholder, instruction_bracket, instruction_text,
    };
    use crate::ir::VOID_ELEMENTS;

    const PAGE: &str = "src/routes/index.page.tsx";

    /// Every message a fixture produces, checked. The path matters: the page-only
    /// rules (`RA010`, reserved names, reachability) key off it.
    fn check_all(fixtures: &[(&str, &str)]) {
        let mut checked = 0;
        for (what, source) in fixtures {
            for diagnostic in analyze::analyze(source, PAGE) {
                checked += 1;
                assert_instruction_clean(what, &diagnostic.code, &diagnostic.message);
            }
        }
        assert!(
            checked > 0,
            "the corpus produced no diagnostics, so it proves nothing"
        );
    }

    /// The one assertion. `what` names the fixture so a failure says which rule
    /// regressed.
    fn assert_instruction_clean(what: &str, code: &Code, message: &str) {
        let instruction = instruction_text(message);
        if let Some((at, token)) = instruction_bracket(instruction) {
            panic!(
                "{what} ({}): the instruction half of the message asks the agent to type \
                 `{token}` at byte {at}, which this milestone refuses or cannot compile: \
                 {message}",
                code.as_str()
            );
        }
    }

    /// A fix is a pure instruction, so the strict predicate applies: **no** `<…>`
    /// at all, not even a lowercase tag-as-metavariable. This is the test that
    /// would have caught `(props: <Type>)` in RA014's fix and in RA006's message.
    #[test]
    fn no_fix_or_skill_fix_section_carries_any_placeholder() {
        for code in Code::all() {
            let fix = code.fix_template();
            if let Some((at, token)) = angle_bracket_placeholder(fix) {
                panic!(
                    "{}: a fix is an instruction and must be typeable, but it carries `{token}` at \
                     byte {at}: {fix}",
                    code.as_str()
                );
            }
            // The skill's `## Fix` is `fix_template` verbatim; assert the section
            // itself rather than trusting the generator, so a future skill that
            // composes its own fix is still held to the rule.
            let body = crate::skills::skill_body(*code);
            let section = body
                .split_once("\n## Fix\n\n")
                .map(|(_, section)| section)
                .unwrap_or_else(|| panic!("{}: no `## Fix` section: {body}", code.as_str()));
            if let Some((_, token)) = angle_bracket_placeholder(section) {
                panic!(
                    "{}: the skill's `## Fix` section carries `{token}`: {section}",
                    code.as_str()
                );
            }
        }
    }

    /// The per-site messages, driven through the real pipeline: one fixture per
    /// message-composing rule, chosen so **every** distinct message text is
    /// produced at least once. A rule whose fixture is missing here cannot be
    /// caught by this test — which is why the table is grouped by module and each
    /// entry names what it exercises.
    #[test]
    fn no_per_site_message_instruction_carries_a_refused_construct() {
        let fixtures: &[(&str, &str)] = &[
            // --- analyze::component: the four RA011 shapes and RA014's async ---
            ("RA011 no default export", "\"use island\"\nconst x = 1\n"),
            (
                "RA011 not a component",
                "\"use island\"\nexport default 1\n",
            ),
            (
                "RA011 too many params",
                "\"use island\"\nexport default function P(a, b) { return <p/> }\n",
            ),
            (
                "RA011 no direct JSX return",
                "\"use island\"\nexport default function P(props: Record<never, never>) { return null }\n",
            ),
            (
                "RA014 async route component",
                "\"use island\"\nexport default async function P(props: Record<never, never>) { return <p/> }\n",
            ),
            // --- analyze::props: the two RA006 shapes ---
            (
                "RA006 propless",
                "\"use static\"\nexport default function P() { return <p/> }\n",
            ),
            (
                "RA006 untyped props",
                "\"use static\"\nexport default function P(props) { return <p/> }\n",
            ),
            // --- analyze::entry: declare, imports, re-exports, import-equals ---
            (
                "RA014 declare",
                "\"use static\"\ndeclare const x: number\nexport default function P(props: Record<never, never>) { return <p>{x}</p> }\n",
            ),
            (
                "RA014 foreign import",
                "\"use island\"\nimport { y } from \"./y\"\nexport default function P(props: Record<never, never>) { return <p>{y}</p> }\n",
            ),
            (
                "RA014 runai re-export",
                "\"use island\"\nexport { owner } from \"runai\"\nexport default function P(props: Record<never, never>) { return <p/> }\n",
            ),
            (
                "RA014 foreign re-export",
                "\"use island\"\nexport * from \"./y\"\nexport default function P(props: Record<never, never>) { return <p/> }\n",
            ),
            (
                "RA014 import equals",
                "\"use island\"\nimport y = require(\"./y\")\nexport default function P(props: Record<never, never>) { return <p/> }\n",
            ),
            (
                "RA014 export assignment",
                "\"use island\"\nconst y = 1\nexport = y\n",
            ),
            (
                "RA014 default runai import",
                "\"use island\"\nimport runai from \"runai\"\nexport default function P(props: Record<never, never>) { return <p/> }\n",
            ),
            (
                "RA014 namespace runai import",
                "\"use island\"\nimport * as runai from \"runai\"\nexport default function P(props: Record<never, never>) { return <p/> }\n",
            ),
            // --- analyze::reachable: signature + the three unreachable values ---
            (
                "RA014 route signature",
                "\"use island\"\nexport default function P({ a }: { a: string }) { return <p/> }\n",
            ),
            (
                "RA014 runai value in a static route",
                "\"use static\"\nimport { signal } from \"runai\"\nexport default function P(props: Record<never, never>) { const n = signal(0); return <p>{n()}</p> }\n",
            ),
            (
                "RA014 route parameter in static markup",
                "\"use static\"\nexport default function P(props: Record<never, never>) { return <p>{props.x}</p> }\n",
            ),
            (
                "RA014 route parameter at module scope",
                "\"use island\"\nconst x = props.x\nexport default function P(props: Record<never, never>) { return <p/> }\n",
            ),
            (
                "RA014 route component reference",
                "\"use static\"\nexport default function P(props: Record<never, never>) { return <p>{P}</p> }\n",
            ),
            // --- analyze::emitted_names: the reserved-name rows ---
            (
                "RA014 reserved declaration",
                "\"use island\"\nexport default function P(props: Record<never, never>) { const render = 1; return <p>{render}</p> }\n",
            ),
            (
                "RA014 reserved module binding",
                "\"use island\"\nimport { signal } from \"runai\"\nconst _esc = signal(0)\nexport default function P(props: Record<never, never>) { return <p>{_esc()}</p> }\n",
            ),
            (
                "RA014 reserved export name",
                "\"use island\"\nconst render = 1\nexport { render }\nexport default function P(props: Record<never, never>) { return <p/> }\n",
            ),
            // --- analyze::rules: the four signal rules ---
            (
                "RA001 untracked read",
                "\"use island\"\nimport { signal } from \"runai\"\nexport default function P(props: Record<never, never>) { const n = signal(0); return <p>{n()}</p> }\n",
            ),
            (
                "RA002 effect write",
                "\"use island\"\nimport { signal, effect } from \"runai\"\nexport default function P(props: Record<never, never>) { const n = signal(0); effect(() => { n(1) }); return <p/> }\n",
            ),
            (
                "RA009 render write",
                "\"use island\"\nimport { signal } from \"runai\"\nexport default function P(props: Record<never, never>) { const n = signal(0); n(1); return <p/> }\n",
            ),
            (
                "RA010 primitive in a static route",
                "\"use static\"\nimport { signal } from \"runai\"\nexport default function P(props: Record<never, never>) { return <p/> }\n",
            ),
            (
                "RA010 write in a static route",
                "\"use static\"\nimport { signal } from \"runai\"\nconst n = signal(0)\nn(1)\nexport default function P(props: Record<never, never>) { return <p/> }\n",
            ),
            (
                "RA010 handler in a static route",
                "\"use static\"\nexport default function P(props: Record<never, never>) { return <button onClick={() => {}}>x</button> }\n",
            ),
            // --- analyze::jsx: every construct whose message composes a repair ---
            (
                "RA014 capitalized tag",
                "\"use island\"\nexport default function P(props: Record<never, never>) { return <div><Greeting/></div> }\n",
            ),
            (
                "RA014 member-expression tag",
                "\"use island\"\nexport default function P(props: Record<never, never>) { return <div><Shared.Header/></div> }\n",
            ),
            (
                "RA014 namespaced tag",
                "\"use island\"\nexport default function P(props: Record<never, never>) { return <div><svg:rect/></div> }\n",
            ),
            (
                "RA014 this tag",
                "\"use island\"\nexport default function P(props: Record<never, never>) { return <div><this/></div> }\n",
            ),
            (
                "RA014 For primitive",
                "\"use island\"\nexport default function P(props: Record<never, never>) { return <div><For each={[]}/></div> }\n",
            ),
            (
                "RA014 Show primitive",
                "\"use island\"\nexport default function P(props: Record<never, never>) { return <div><Show when={true}/></div> }\n",
            ),
            (
                "RA014 When primitive",
                "\"use island\"\nexport default function P(props: Record<never, never>) { return <div><When value={1}/></div> }\n",
            ),
            (
                "RA014 JSX in an expression",
                "\"use island\"\nexport default function P(props: Record<never, never>) { return <div>{true ? <b/> : null}</div> }\n",
            ),
            (
                "RA014 spread attribute",
                "\"use island\"\nexport default function P(props: Record<never, never>) { return <div {...{}}/> }\n",
            ),
            (
                "RA014 spread child",
                "\"use island\"\nexport default function P(props: Record<never, never>) { return <div>{...[]}</div> }\n",
            ),
            (
                "RA014 JSX-valued attribute",
                "\"use island\"\nexport default function P(props: Record<never, never>) { return <div title=<b/>/> }\n",
            ),
            (
                "RA014 fragment root",
                "\"use island\"\nexport default function P(props: Record<never, never>) { return <><b/></> }\n",
            ),
            (
                "RA014 second JSX return",
                "\"use island\"\nexport default function P(props: Record<never, never>) { if (a) return <b/>; return <i/> }\n",
            ),
            (
                "RA014 early return",
                "\"use island\"\nexport default function P(props: Record<never, never>) { const a = 1; if (!a) return null; return <p/> }\n",
            ),
            (
                "RA014 void element with children",
                "\"use island\"\nexport default function P(props: Record<never, never>) { return <div><br>x</br></div> }\n",
            ),
            (
                "RA014 duplicate attribute",
                "\"use island\"\nexport default function P(props: Record<never, never>) { return <div class=\"a\" class=\"b\"/> }\n",
            ),
            (
                "RA014 body JSX",
                "\"use island\"\nexport default function P(props: Record<never, never>) { const h = <b/>; return <p/> }\n",
            ),
            (
                "RA014 module JSX",
                "\"use island\"\nconst h = <b/>\nexport default function P(props: Record<never, never>) { return <p/> }\n",
            ),
            (
                "RA014 marker-hostile binding",
                "\"use island\"\nimport { signal } from \"runai\"\nexport default function P(props: Record<never, never>) { const n = signal(0); return <textarea>{n()}</textarea> }\n",
            ),
            // The reparse family: one fixture per refusal composer.
            (
                "RA014 p holding a block child",
                "\"use island\"\nexport default function P(props: Record<never, never>) { return <div><p><div>x</div></p></div> }\n",
            ),
            (
                "RA014 table holding a foreign child",
                "\"use island\"\nexport default function P(props: Record<never, never>) { return <div><table><div>x</div></table></div> }\n",
            ),
            (
                "RA014 table holding a bare cell",
                "\"use island\"\nexport default function P(props: Record<never, never>) { return <div><table><td>x</td></table></div> }\n",
            ),
            (
                "RA014 section holding a foreign child",
                "\"use island\"\nexport default function P(props: Record<never, never>) { return <div><table><tbody><div>x</div></tbody></table></div> }\n",
            ),
            (
                "RA014 row holding a foreign child",
                "\"use island\"\nexport default function P(props: Record<never, never>) { return <div><table><tbody><tr><div>x</div></tr></tbody></table></div> }\n",
            ),
            (
                "RA014 colgroup holding a foreign child",
                "\"use island\"\nexport default function P(props: Record<never, never>) { return <div><table><colgroup><div>x</div></colgroup></table></div> }\n",
            ),
            (
                "RA014 select holding a foreign child",
                "\"use island\"\nexport default function P(props: Record<never, never>) { return <div><select><div>x</div></select></div> }\n",
            ),
            (
                "RA014 optgroup holding a foreign child",
                "\"use island\"\nexport default function P(props: Record<never, never>) { return <div><select><optgroup><div>x</div></optgroup></select></div> }\n",
            ),
            (
                "RA014 list holding a foreign child",
                "\"use island\"\nexport default function P(props: Record<never, never>) { return <div><ul><div>x</div></ul></div> }\n",
            ),
            (
                "RA014 description list holding a foreign child",
                "\"use island\"\nexport default function P(props: Record<never, never>) { return <div><dl><div>x</div></dl></div> }\n",
            ),
            (
                "RA014 bare text in a table",
                "\"use island\"\nexport default function P(props: Record<never, never>) { return <div><table>text</table></div> }\n",
            ),
            (
                "RA014 bare text in a row",
                "\"use island\"\nexport default function P(props: Record<never, never>) { return <div><table><tbody><tr>text</tr></tbody></table></div> }\n",
            ),
            (
                "RA014 nested anchor",
                "\"use island\"\nexport default function P(props: Record<never, never>) { return <div><a href=\"/1\"><a href=\"/2\">x</a></a></div> }\n",
            ),
            (
                "RA014 nested form",
                "\"use island\"\nexport default function P(props: Record<never, never>) { return <div><form><form>x</form></form></div> }\n",
            ),
            (
                "RA014 stray list item",
                "\"use island\"\nexport default function P(props: Record<never, never>) { return <div><li>x</li></div> }\n",
            ),
            (
                "RA014 stray description entry",
                "\"use island\"\nexport default function P(props: Record<never, never>) { return <div><dt>x</dt></div> }\n",
            ),
            (
                "RA014 cell outside a row",
                "\"use island\"\nexport default function P(props: Record<never, never>) { return <div><td>x</td></div> }\n",
            ),
            (
                "RA014 option outside a select",
                "\"use island\"\nexport default function P(props: Record<never, never>) { return <div><option>x</option></div> }\n",
            ),
            (
                "RA014 nested list item",
                "\"use island\"\nexport default function P(props: Record<never, never>) { return <div><ul><li>a<li>b</li></li></ul></div> }\n",
            ),
            (
                "RA014 nested row",
                "\"use island\"\nexport default function P(props: Record<never, never>) { return <div><table><tbody><tr><td>a<tr><td>b</td></tr></td></tr></tbody></table></div> }\n",
            ),
            (
                "RA014 nested option",
                "\"use island\"\nexport default function P(props: Record<never, never>) { return <div><select><option>a<option>b</option></option></select></div> }\n",
            ),
            // --- source: the two file-level codes ---
            (
                "RA012 missing directive",
                "export default function P(props: Record<never, never>) { return <p/> }\n",
            ),
            (
                "RA007 parse error",
                "\"use island\"\nexport default function P(props: Record<never, never>) { return <p> }\n",
            ),
        ];

        check_all(fixtures);
    }

    /// The shared refusal composers, called directly with every tag their tables
    /// list. A fixture-driven corpus only reaches the arms its fixtures happen to
    /// select; these loops are exhaustive over the tag lists, so a new tag with a
    /// new interpolated sentence is covered the day it is added.
    #[test]
    fn every_shared_refusal_composer_is_instruction_clean() {
        // Every tag either side of a nesting rule can name, plus every void and
        // marker-hostile element.
        let tags: &[&str] = &[
            "p",
            "div",
            "span",
            "a",
            "form",
            "li",
            "dt",
            "dd",
            "ul",
            "ol",
            "dl",
            "table",
            "thead",
            "tbody",
            "tfoot",
            "tr",
            "td",
            "th",
            "caption",
            "colgroup",
            "col",
            "select",
            "option",
            "optgroup",
            "figure",
            "figcaption",
            "fieldset",
            "legend",
            "details",
            "summary",
            "button",
            "nobr",
            "h1",
            "h2",
            "h6",
            "script",
            "style",
            "title",
            "textarea",
            "noscript",
            "template",
        ];
        let parents: &[&str] = &[
            "p", "table", "thead", "tbody", "tfoot", "tr", "colgroup", "select", "optgroup", "ul",
            "ol", "menu", "dl",
        ];
        let ancestors = [
            "div".to_string(),
            "ul".to_string(),
            "a".to_string(),
            "form".to_string(),
            "select".to_string(),
            "dl".to_string(),
            "table".to_string(),
            "tbody".to_string(),
        ];

        let mut checked = 0;
        let mut check = |what: &str, message: &str| {
            let instruction = instruction_text(message);
            if let Some((at, token)) = instruction_bracket(instruction) {
                panic!(
                    "{what}: the instruction half asks the agent to type `{token}` at byte {at}: \
                     {message}"
                );
            }
            checked += 1;
        };

        for parent in parents {
            for child in tags {
                if let Some(message) = parent_child_refusal(parent, child) {
                    check("parent/child refusal", &message);
                }
                if let Some(message) = text_refusal(parent) {
                    check("text refusal", &message);
                }
            }
        }
        for tag in tags {
            for parent in parents {
                let path = vec![parent.to_string()];
                if let Some(message) = self_closing_refusal(tag, &path) {
                    check("self-closing refusal", &message);
                }
            }
            for tag in tags {
                if let Some(message) = ancestor_refusal(tag, &ancestors) {
                    check("ancestor refusal", &message);
                }
            }
            if crate::ir::VOID_ELEMENTS.contains(tag) {
                let path = vec!["div".to_string()];
                if let Some(message) = parent_child_refusal("div", tag) {
                    check("void element", &message);
                }
                let _ = path;
            }
            if crate::analyze::jsx::MARKER_HOSTILE.contains(tag) {
                check("marker-hostile refusal", &marker_hostile_message(tag));
            }
        }
        for message in [
            JSX_IN_EXPRESSION_REPAIR,
            SPREAD_CHILD_REPAIR,
            BODY_JSX_REPAIR,
            MODULE_JSX_REPAIR,
            crate::analyze::jsx::SPACE_FIX,
        ] {
            check("shared repair constant", message);
        }

        // `VOID_ELEMENTS` is a real list; assert it was actually walked so the
        // loop above cannot become vacuous.
        assert!(
            VOID_ELEMENTS.iter().any(|tag| tags.contains(tag)),
            "the tag list no longer overlaps VOID_ELEMENTS"
        );
        assert!(checked > 100, "the corpus checked only {checked} messages");
    }

    /// The highest-priority invariant of this module: **every repair a refusal
    /// offers compiles.** This is stricter than "contains no placeholder" — a
    /// repair can be placeholder-free and still name a construct the compiler
    /// refuses (`<For each={items}>`), which sends an agent from a refusal into
    /// more refusals forever.
    ///
    /// Each case is a source that hits the refusal, plus the same file with the
    /// repair applied by hand. The applied form must carry no `RA014` and no
    /// `RA011`: a repair that trades one refusal for another has not repaired
    /// anything.
    #[test]
    fn every_repair_applied_by_hand_is_accepted() {
        // (what the refusal is for, source that triggers it, source with the repair applied)
        let cases: &[(&str, &str, &str)] = &[
            (
                "spread child",
                "\"use island\"\nexport default function P(props: Record<never, never>) { return <ul>{...items}</ul> }\n",
                "\"use island\"\nexport default function P(props: Record<never, never>) { return <ul><li>a</li></ul> }\n",
            ),
            (
                "spread attribute",
                "\"use island\"\nexport default function P(props: Record<never, never>) { return <div {...rest}/> }\n",
                "\"use island\"\nexport default function P(props: Record<never, never>) { return <div id=\"a\"/ > }\n",
            ),
            (
                "JSX in a `{…}` expression",
                "\"use island\"\nexport default function P(props: Record<never, never>) { return <div>{ok ? <b/> : null}</div> }\n",
                "\"use island\"\nexport default function P(props: Record<never, never>) { return <div>{ok ? \"yes\" : \"no\"}</div> }\n",
            ),
            (
                "JSX-valued attribute",
                "\"use island\"\nexport default function P(props: Record<never, never>) { return <div title=<b/>/> }\n",
                "\"use island\"\nexport default function P(props: Record<never, never>) { return <div><b/></div> }\n",
            ),
            (
                "runtime primitive <Show>",
                "\"use island\"\nexport default function P(props: Record<never, never>) { return <div><Show when={ok}/></div> }\n",
                "\"use island\"\nexport default function P(props: Record<never, never>) { return <div><b/></div> }\n",
            ),
            (
                "second JSX return",
                "\"use island\"\nexport default function P(props: Record<never, never>) { if (a) return <b/>; return <i/> }\n",
                "\"use island\"\nexport default function P(props: Record<never, never>) { return <div><b/></div> }\n",
            ),
            (
                "early return",
                "\"use island\"\nexport default function P(props: Record<never, never>) { const a = 1; if (!a) return null; return <p/> }\n",
                "\"use island\"\nexport default function P(props: Record<never, never>) { const a = 1; return <p>{a}</p> }\n",
            ),
            (
                "route signature",
                "\"use island\"\nexport default function P({ a }: { a: string }) { return <p/> }\n",
                "\"use island\"\nexport default function P(props: Record<never, never>) { return <p/> }\n",
            ),
            (
                "fragment root",
                "\"use island\"\nexport default function P(props: Record<never, never>) { return <><b/></> }\n",
                "\"use island\"\nexport default function P(props: Record<never, never>) { return <main><b/></main> }\n",
            ),
            (
                // The repair's half-sentence used to be "use or export the written
                // binding". The "export" half lands in a second RA014, because the
                // island module already exports every import specifier it writes, so
                // re-exporting one is a duplicate export. "Use it" compiles.
                "runai re-export",
                "\"use island\"\nexport { owner } from \"runai\"\nexport default function P(props: Record<never, never>) { return <p/> }\n",
                "\"use island\"\nimport { owner } from \"runai\"\nexport default function P(props: Record<never, never>) { return <p>{String(owner)}</p> }\n",
            ),
        ];

        for (what, before, after) in cases {
            // The fixture must actually hit the refusal, or "the repair compiles"
            // is a claim about a file that was never refused.
            let refused = analyze::analyze(before, PAGE);
            assert!(
                refused.iter().any(|d| d.code == Code::RA014),
                "{what}: the fixture does not trigger RA014: {refused:?}"
            );

            let repaired: Vec<String> = analyze::analyze(after, PAGE)
                .iter()
                .filter(|d| matches!(d.code, Code::RA014 | Code::RA011))
                .map(|d| d.message.clone())
                .collect();
            assert!(
                repaired.is_empty(),
                "{what}: applying the repair verbatim lands in another refusal: {repaired:?}"
            );
        }
    }

    /// The same rule applied to the spread-child repair specifically, because it
    /// is the one that shipped wrong: the repair must not name `<For>` at all.
    #[test]
    fn the_spread_child_repair_does_not_name_a_runtime_primitive() {
        let src = "\"use island\"\nexport default function P(props: Record<never, never>) { return <div>{...items}</div> }\n";
        let diagnostics = analyze::analyze(src, PAGE);
        assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
        let message = &diagnostics[0].message;
        for refused in ["<For", "<Show", "<When"] {
            assert!(
                !message.contains(refused),
                "the spread-child repair names `{refused}`, which is refused itself: {message}"
            );
        }
        assert!(
            message.contains("ordinary children") || message.contains("remove the spread"),
            "the repair must name a compilable edit: {message}"
        );
    }

    /// A directive repair must not name a **line**. `detect_mode` accepts a
    /// comment above the directive, so "Change line 1" edits the comment and the
    /// agent's next run reports the same thing: the repair-loop class, in text.
    ///
    /// Whole-word match, because "inline the value" contains the letters `line`
    /// and is not a line number.
    fn mentions_a_line(text: &str) -> Option<&str> {
        text.split(|c: char| !c.is_ascii_alphabetic())
            .find(|word| matches!(*word, "line" | "lines" | "Line" | "Lines"))
            .map(|_| text)
    }

    /// Every directive-change repair, asserted against the line rule: the
    /// canonical RA010 fix, the six per-site messages that tell an agent to change
    /// the directive, and RA012's add-a-directive fix.
    #[test]
    fn every_directive_repair_names_no_line() {
        let mut checked: Vec<String> = Vec::new();

        // The canonical fixes.
        for code in [Code::RA010, Code::RA012] {
            checked.push(code.fix_template().to_string());
        }

        // Every per-site message that names the directive, driven end to end.
        let fixtures: &[&str] = &[
            // reachable::runtime_import
            "\"use static\"\nimport { owner } from \"runai\"\nexport default function P(props: Record<never, never>) { return <p>{owner}</p> }\n",
            // reachable::route_parameter (component and module scope)
            "\"use static\"\nexport default function P(props: Record<never, never>) { return <p>{props.x}</p> }\n",
            "\"use island\"\nconst x = props.x\nexport default function P(props: Record<never, never>) { return <p/> }\n",
            // reachable::route_component
            "\"use static\"\nexport default function P(props: Record<never, never>) { return <p>{P}</p> }\n",
        ];
        for source in fixtures {
            for diagnostic in analyze::analyze(source, PAGE) {
                if diagnostic.message.contains("directive") {
                    checked.push(diagnostic.message.clone());
                }
            }
        }

        // The two emitter-mode refusals, which live outside `analyze`.
        let island_route = "\"use island\"\nexport default function P(props: Record<never, never>) { return <p/> }\n";
        let static_route = "\"use static\"\nexport default function P(props: Record<never, never>) { return <p/> }\n";
        if let Err(diagnostics) =
            crate::codegen::emit_prerender_checked(island_route, "src/routes/a.page.tsx")
        {
            for diagnostic in diagnostics {
                if diagnostic.message.contains("directive") {
                    checked.push(diagnostic.message);
                }
            }
        }
        if let Err(diagnostics) =
            crate::codegen::emit_client_checked(static_route, "src/routes/a.page.tsx", "/runai.js")
        {
            for diagnostic in diagnostics {
                if diagnostic.message.contains("directive") {
                    checked.push(diagnostic.message);
                }
            }
        }

        assert!(
            checked.len() >= 7,
            "expected at least 7 directive repairs, collected {}",
            checked.len()
        );
        for text in &checked {
            assert!(
                !text.contains("line 1"),
                "a directive repair still names line 1: {text}"
            );
            if let Some(offending) = mentions_a_line(text) {
                panic!("a directive repair names a line: {offending}");
            }
        }
    }
}
