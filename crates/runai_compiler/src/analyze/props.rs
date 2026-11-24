//! `RA006_UNANNOTATED_PROPS`: every component writes its props type down.
//!
//! The framework's guarantee is that the question "where does this value come
//! from?" always has a written answer. A component whose parameter has no type
//! annotation answers "somewhere" — and a component with no parameter at all
//! answers "nowhere" only in prose, not in types. So both cases are `RA006`:
//! propless components declare `type Props = Record<never, never>` and take
//! `props: Props`.
//!
//! This runs on **every** component in every `.tsx` file — [`find_components`],
//! not only the route — because a `_shell.tsx` component owes the same answer.
//!
//! The fix is canonical per code (one sentence for every site), so the per-site
//! half lives in `message`: the exact signature line to write, sliced from the
//! declaration the file already has. That line must be legal TypeScript and must
//! edit the declaration that exists, so three details follow: an anonymous
//! component is called `Page` rather than `default` (a reserved word), the
//! signature names the existing binding, and the form in the snippet matches how
//! the component is actually exported.

use crate::analyze::component::{ComponentInfo, ComponentKind};
use crate::diagnostic::{Code, Diagnostic};

/// Check that `info`'s component declares its props type.
///
/// The answer is [`ComponentInfo::has_props_type`], which is "exactly one
/// parameter, and that parameter is annotated". A type alias named `Props`
/// elsewhere in the file does not satisfy this: the annotation belongs on the
/// parameter, where the reader can see which type a given component takes.
///
/// `source` is the file `info` was parsed from. It is only read to quote the
/// declaration's own signature line back at the agent; nothing about the rule
/// depends on it.
pub fn check_props(info: &ComponentInfo, source: &str) -> Vec<Diagnostic> {
    // Not a function: no props to type. `RA011` already says to replace it, and
    // a second diagnostic demanding a type for a class or a literal would send
    // the agent to edit the wrong thing.
    if info.kind == ComponentKind::NotAFunction {
        return Vec::new();
    }

    match info.params {
        // `function Page() {…}` — no parameter, so no type. This is *not* clean:
        // "it takes nothing" is a fact about the code that the type should state.
        0 => vec![unannotated_props(info, source)],
        // `function Page(props) {…}` — a parameter with no annotation.
        1 if !info.has_props_type => vec![unannotated_props(info, source)],
        // A typed parameter is clean. So is `params > 1`, which `RA011` already
        // rejects for a route: the component's problem is that it takes props at
        // all, not how they are typed, and two diagnostics for one mistake would
        // overstate it.
        _ => Vec::new(),
    }
}

/// `RA006_UNANNOTATED_PROPS`: the component's props have no written type.
fn unannotated_props(info: &ComponentInfo, source: &str) -> Diagnostic {
    // `default` names the export, not a binding: `function default(props)` is a
    // parse error, so the text an agent copies must use a legal identifier.
    let name = info.display_name();

    // The signature line to write, in the declaration form the file already
    // uses. Slicing the source is what makes `const Badge = …` stay a `const`
    // and `export default function Page` stay an `export default`: a synthesized
    // `function Page(…)` line would be a second declaration of the same binding,
    // i.e. a redeclaration error for anyone following it verbatim.
    let message = if info.params == 0 {
        // Propless. The type still has to be written down, so name the exact
        // type to declare — `Record<never, never>` is the "no props" object,
        // and an empty object type (`{}`) would accept anything.
        let signature = info.signature(source, "(props: Record<never, never>)");
        format!(
            "component `{name}` takes no props, but does not declare that in a type; write: \
             {signature}"
        )
    } else {
        // Props exist but their shape is unstated. The alias name is derived from
        // the component, so the text names a type the agent can add here.
        let signature = info.signature(source, &format!("(props: {name}Props)"));
        format!(
            "component `{name}` has untyped props; write: {signature} with a type for \
             `{name}`'s props, e.g. `type {name}Props = {{ id: string }}`"
        )
    };

    Diagnostic::new(Code::RA006, info.props_span(), message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyze::component::find_route_component;
    use crate::source::parse_tsx;

    use crate::analyze::component::{find_components, validate_route_component};
    use crate::diagnostic::Code;

    /// Both passes, in the order Task 9's `analyze` runs them: route validation
    /// for the default export, then props for every component in the file.
    fn check(src: &str) -> Vec<crate::diagnostic::Diagnostic> {
        let alloc = oxc_allocator::Allocator::default();
        let program = parse_tsx(&alloc, src).unwrap();
        let mut d = validate_route_component(find_route_component(program).as_ref());
        for c in find_components(program) {
            d.extend(check_props(&c, src));
        }
        d
    }

    #[test]
    fn untyped_props_is_ra006() {
        let src = "export default function Page(props) { return <p/> }\n";
        let d = check(src);
        assert_eq!(d[0].code, Code::RA006);
    }

    #[test]
    fn typed_props_is_clean() {
        let src = "type Props = { id: string }\nexport default function Page(props: Props) { return <p/> }\n";
        assert!(check(src).is_empty());
    }

    #[test]
    fn no_props_still_requires_a_type() {
        let src = "export default function Page() { return <p/> }\n";
        let d = check(src);
        assert_eq!(d[0].code, Code::RA006);
        assert!(d[0].fix.contains("Record<never, never>"));
    }

    /// The brief's own name for this test: the canonical `fix` is one sentence
    /// for every site, so the exact line to write must be in `message`.
    #[test]
    fn no_props_still_requires_a_type_and_shows_the_line() {
        let d = check("export default function Page() { return <p/> }\n");
        assert_eq!(d[0].code, Code::RA006);
        assert!(d[0].fix.contains("props: Record<never, never>"));
        assert!(
            d[0].message
                .contains("export default function Page(props: Record<never, never>) {"),
            "{:?}",
            d[0].message
        );
    }

    /// Every component owes a props type, not only the route. A page's `_shell`
    /// declares components too, and `RA006` is about the answer each of them has
    /// to give — so the file gets exactly one diagnostic, and it names `Badge`.
    #[test]
    fn non_route_component_needs_a_props_type_too() {
        let src = "export default function Page(props: Record<never, never>) { return <p/> }\nfunction Badge() { return <b/> }\n";
        let d = check(src);
        assert_eq!(d.len(), 1, "{d:?}");
        assert_eq!(d[0].code, Code::RA006);
        assert!(
            d[0].message
                .contains("function Badge(props: Record<never, never>) {"),
            "{:?}",
            d[0].message
        );
    }

    #[test]
    fn missing_default_export_is_ra011() {
        let src = "export function Page(props: Record<never, never>) { return <p/> }\n";
        let d = check(src);
        assert_eq!(d[0].code, Code::RA011);
    }

    /// The message is what an agent triages on; it must name the same mistake the
    /// fix repairs — and, since the canonical fix is per code, it must carry the
    /// exact signature line to write.
    #[test]
    fn a_propless_message_says_the_props_are_undeclared() {
        let src = "export default function Page() { return <p/> }\n";
        let d = check(src);
        assert!(
            d[0].message.starts_with(
                "component `Page` takes no props, but does not declare that in a type"
            ),
            "{:?}",
            d[0].message
        );
        assert!(
            d[0].message
                .contains("export default function Page(props: Record<never, never>) {"),
            "the message must carry the exact line to write: {:?}",
            d[0].message
        );
        assert!(d[0].explain.contains("type"), "{}", d[0].explain);
    }

    /// `RA006` is about the annotation being on the parameter, not a type alias
    /// existing somewhere in the file. A param that names an undeclared type is
    /// still annotated; an alias with no annotation on the param is still not.
    #[test]
    fn the_annotation_must_be_on_the_parameter() {
        let src =
            "type Props = { id: string }\nexport default function Page(props) { return <p/> }\n";
        assert_eq!(check(src)[0].code, Code::RA006);
    }

    /// Props are a single object argument, so more than one is not "untyped
    /// props" — it is a bad route export, and `RA011` already says so.
    #[test]
    fn multiple_params_is_ra011_not_ra006() {
        let src = "export default function Page(a: A, b: B) { return <p/> }\n";
        let d = check(src);
        assert_eq!(
            d.iter()
                .map(|diagnostic| diagnostic.code)
                .collect::<Vec<_>>(),
            vec![Code::RA011]
        );
    }

    /// A rest parameter is a parameter: `function Page(...props)` is not
    /// propless, so its message must say its props are untyped rather than that
    /// it takes none. The written line is still "one typed parameter", because
    /// that is the rule the code states.
    #[test]
    fn rest_parameter_is_not_propless() {
        let src = "export default function Page(...props) { return <p/> }\n";
        let d = check(src);
        assert_eq!(d[0].code, Code::RA006);
        assert!(
            d[0].message
                .starts_with("component `Page` has untyped props"),
            "rest-only props were reported as propless: {:?}",
            d[0].message
        );
        assert!(
            !d[0].message.contains("takes no props"),
            "rest-only props got the propless repair: {:?}",
            d[0].message
        );

        let typed = "export default function Page(...props: Props[]) { return <p/> }\n";
        assert!(check(typed).is_empty());
    }

    /// An arrow function's props annotation is on the arrow's parameter too.
    #[test]
    fn arrow_function_props_are_checked_the_same_way() {
        let untyped = "export default (props) => <p/>\n";
        let d = check(untyped);
        assert_eq!(d[0].code, Code::RA006);

        let typed = "export default (props: Props) => <p/>\n";
        assert!(check(typed).is_empty());
    }

    /// An anonymous component is named `"default"`, which is the export's name,
    /// not a binding. Emitting it verbatim gives `function default(props)` — a
    /// parse error that sends an agent following the message into a loop it
    /// cannot diagnose. Both branches must use a legal identifier, and the line
    /// they name is the arrow or the declaration the file actually has.
    #[test]
    fn an_anonymous_component_gets_a_legal_identifier_in_the_fix() {
        for (src, expected) in [
            (
                "export default () => <p/>\n",
                "export default (props: Record<never, never>) =>",
            ),
            (
                "export default () => { return <p/> }\n",
                "export default (props: Record<never, never>) => {",
            ),
            (
                "export default function () { return <p/> }\n",
                "export default function (props: Record<never, never>) {",
            ),
        ] {
            let d = check(src);
            assert_eq!(d[0].code, Code::RA006, "{src}");
            assert!(
                !d[0].message.contains("function default"),
                "{src} emitted an illegal identifier: {:?}",
                d[0].message
            );
            assert!(
                d[0].message.contains(expected),
                "{src}: expected {expected:?} in {:?}",
                d[0].message
            );
            assert!(
                !d[0].message.contains("`default`"),
                "{src}: {:?}",
                d[0].message
            );
        }
    }

    /// The untyped branch has the same defect and the same repair: a legal name
    /// for the alias and the annotation.
    #[test]
    fn an_anonymous_component_with_untyped_props_gets_a_legal_identifier() {
        for src in [
            "export default (props) => <p/>\n",
            "export default function (props) { return <p/> }\n",
        ] {
            let d = check(src);
            assert_eq!(d[0].code, Code::RA006, "{src}");
            assert!(
                !d[0].message.contains("function default"),
                "{src} emitted an illegal identifier: {:?}",
                d[0].message
            );
            assert!(
                !d[0].message.contains("defaultProps"),
                "{src}: {:?}",
                d[0].message
            );
            assert!(
                d[0].message.contains("type PageProps"),
                "{src}: {:?}",
                d[0].message
            );
        }
    }

    /// A default export that is not a function has no props to annotate.
    /// `RA011` already owns that repair; a second diagnostic demanding a type
    /// would send the agent to edit a class or a literal.
    #[test]
    fn a_non_function_default_export_gets_no_props_diagnostic() {
        for src in ["export default 42\n", "export default class Page {}\n"] {
            let alloc = oxc_allocator::Allocator::default();
            let program = parse_tsx(&alloc, src).unwrap();
            let info = find_route_component(program).unwrap();
            assert!(check_props(&info, src).is_empty(), "{src}");
        }
    }

    /// When the component reaches its default export through a name, the message
    /// must not tell the agent to write `export default function Page(…)`: that is
    /// a second default export, i.e. a parse error for anyone who follows the text
    /// verbatim. The repair names the existing declaration instead.
    #[test]
    fn a_named_default_export_fix_does_not_add_a_second_export() {
        for src in [
            "function Page() { return <p/> }\nexport default Page\n",
            "function Page() { return <p/> }\nexport { Page as default }\n",
        ] {
            let d = check(src);
            assert!(
                !d[0].message.contains("export default function"),
                "{src}: {:?}",
                d[0].message
            );
            assert!(
                d[0].message
                    .contains("function Page(props: Record<never, never>)"),
                "{src}: {:?}",
                d[0].message
            );
        }

        // An inline default export is the thing to edit, and says so.
        let d = check("export default function Page() { return <p/> }\n");
        assert!(
            d[0].message
                .contains("export default function Page(props: Record<never, never>)"),
            "{:?}",
            d[0].message
        );
    }

    /// The message names the **binding**, not a conventional `Page`.
    ///
    /// `component_from_arrow` hardcoded `name: "default"`, and every earlier test
    /// happened to call its binding `Page`, which hid the difference. For a real
    /// file — `const AboutPage = (props) => <p/>; export default AboutPage` — the
    /// text said `function Page(props: Props)`: applying that verbatim declares a
    /// new, unused `Page` and leaves `AboutPage` exactly as untyped as before, so
    /// the next run reports `RA006` again. The name in the text must be one the
    /// agent can find in this file.
    #[test]
    fn a_resolved_default_export_is_named_by_its_binding_not_page() {
        for (src, expected_text) in [
            (
                "const AboutPage = (props) => <p/>\nexport default AboutPage\n",
                "const AboutPage = (props: AboutPageProps) =>",
            ),
            (
                "const AboutPage = (props) => <p/>\nexport { AboutPage as default }\n",
                "const AboutPage = (props: AboutPageProps) =>",
            ),
            (
                "const AboutPage = () => <p/>\nexport default AboutPage\n",
                "const AboutPage = (props: Record<never, never>) =>",
            ),
            (
                "const AboutPage = () => <p/>\nexport { AboutPage as default }\n",
                "const AboutPage = (props: Record<never, never>) =>",
            ),
            (
                // The name may also reach the export through parentheses.
                "const AboutPage = () => <p/>\nexport default (AboutPage)\n",
                "const AboutPage = (props: Record<never, never>) =>",
            ),
            (
                // A named function expression is scoped to its own body, so the
                // signature is the declarator's, even though the literal's own
                // name appears in it.
                "const AboutPage = function Actual() { return <p/> }\nexport default AboutPage\n",
                "const AboutPage = function Actual(props: Record<never, never>) {",
            ),
            (
                // Following an alias lands on the declaration that has the
                // parameter, which is named `AboutPage`, not the alias.
                "const Alias = (AboutPage)\nconst AboutPage = () => <p/>\nexport default Alias\n",
                "const AboutPage = (props: Record<never, never>) =>",
            ),
            (
                // An exported declaration keeps its `export`: writing the line
                // without it would leave the component no longer exported.
                "export const AboutPage = () => <p/>\nexport default AboutPage\n",
                "export const AboutPage = (props: Record<never, never>) =>",
            ),
        ] {
            let d = check(src);
            assert_eq!(d[0].code, Code::RA006, "{src}");
            assert!(
                d[0].message.contains(expected_text),
                "{src}: expected the binding's name in the message: {:?}",
                d[0].message
            );
            assert!(
                !d[0].message.contains("function default"),
                "{src}: illegal identifier: {:?}",
                d[0].message
            );
            assert!(
                !d[0].message.contains("function Page("),
                "{src}: the message names a binding this file does not declare: {:?}",
                d[0].message
            );
            assert!(
                d[0].message.contains("`AboutPage`"),
                "{src}: the message must name the binding: {:?}",
                d[0].message
            );
        }
    }

    /// Applying the renamed fix is clean on the next run — the repair loop this
    /// is about. `AboutPage` is the binding the fix names, so annotating *it*
    /// clears `RA006` instead of leaving the original unannotated.
    #[test]
    fn the_renamed_fix_can_be_applied() {
        let src = "type Props = Record<never, never>\nconst AboutPage = (props: Props) => <p/>\nexport default AboutPage\n";
        assert!(check(src).is_empty(), "{:?}", check(src));

        let src = "type Props = Record<never, never>\nconst AboutPage = (props: Props) => <p/>\nexport { AboutPage as default }\n";
        assert!(check(src).is_empty(), "{:?}", check(src));
    }

    /// The other half of the rule: a genuinely anonymous component has no binding
    /// name, so `Page` remains the only honest stand-in. Renaming must not turn
    /// `export default (props) => <p/>` into something invented from thin air.
    #[test]
    fn an_anonymous_default_export_still_yields_page() {
        for src in [
            "export default (props) => <p/>\n",
            "export default (() => <p/>)\n",
        ] {
            let d = check(src);
            assert_eq!(d[0].code, Code::RA006, "{src}");
            assert!(d[0].message.contains("`Page`"), "{src}: {:?}", d[0].message);
            assert!(
                !d[0].message.contains("`default`"),
                "{src}: {:?}",
                d[0].message
            );
        }
    }

    /// Every `RA006` points at its own skill file in the installed location, so
    /// Task 15's generated docs and these paths cannot drift apart.
    #[test]
    fn every_ra006_points_at_the_skill_file() {
        let alloc = oxc_allocator::Allocator::default();
        let src = "export default function Page(props) { return <p/> }\n";
        let program = parse_tsx(&alloc, src).unwrap();
        let info = find_route_component(program).unwrap();
        for diagnostic in check_props(&info, src) {
            assert_eq!(diagnostic.doc, Code::RA006.doc_path());
        }
    }

    /// The propless repair names an exact type and signature. Applying it must be
    /// clean on the next run, or the agent is in a repair loop.
    #[test]
    fn following_the_propless_fix_is_clean() {
        // `Record<never, never>` as the annotation is what the message names, and
        // the type alias is the one it tells the agent to declare.
        let src = "type Props = Record<never, never>\nexport default function Page(props: Props) { return <p/> }\n";
        assert!(check(src).is_empty());

        let hint = "export default function Page() { return <p/> }\n";
        let d = check(hint);
        let line = d[0]
            .message
            .split(';')
            .find(|part| part.contains("write:"))
            .expect("the message names the line to write")
            .replace("write:", "")
            .replace('`', "")
            .trim()
            .to_string();
        let applied = format!("type Props = Record<never, never>\n{line} return <p/> }}\n");
        assert!(check(&applied).is_empty(), "{applied}");
    }

    /// The same repair-loop assertion for a non-route component: the line the
    /// message names edits `Badge` where it is declared, rather than adding a
    /// component nobody calls.
    #[test]
    fn following_a_non_default_component_fix_is_clean() {
        let src = "export default function Page(props: Record<never, never>) { return <p/> }\nfunction Badge() { return <b/> }\n";
        let d = check(src);
        assert_eq!(d.len(), 1, "{d:?}");
        assert!(
            d[0].message
                .contains("function Badge(props: Record<never, never>) {"),
            "{:?}",
            d[0].message
        );

        let applied = "export default function Page(props: Record<never, never>) { return <p/> }\n\
                       function Badge(props: Record<never, never>) { return <b/> }\n";
        assert!(check(applied).is_empty(), "{applied}");
    }

    /// Destructuring the props object (`function Page({ id }: Props)`) is an
    /// annotated parameter, not an untyped one.
    #[test]
    fn a_destructured_parameter_counts_as_annotated() {
        let src = "type Props = { id: string }\nexport default function Page({ id }: Props) { return <p/> }\n";
        assert!(check(src).is_empty());
    }

    /// `_shell.tsx` is not a route and gets no `RA011`, but every component in it
    /// still owes a props type. This is Task 9's split spelled out: props over
    /// `find_components`, route validation only for `.page.tsx`.
    #[test]
    fn a_shell_file_is_checked_for_props_but_not_route_shape() {
        let props_only = |src: &str| {
            let alloc = oxc_allocator::Allocator::default();
            let program = parse_tsx(&alloc, src).unwrap();
            find_components(program)
                .into_iter()
                .flat_map(|component| check_props(&component, src))
                .collect::<Vec<_>>()
        };

        // The plan's own `_shell.tsx` fixture: typed, and clean.
        let shell = "export default function Shell(props: { children: JSX.Element }) { return <main>{props.children}</main> }\n";
        assert!(props_only(shell).is_empty(), "{:?}", props_only(shell));

        // The same shell with an untyped helper: one `RA006`, no `RA011`.
        let src = "export default function Shell(props: { children: JSX.Element }) { return <main>{props.children}</main> }\n\
                   function Badge() { return <b/> }\n";
        let diagnostics = props_only(src);
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].code, Code::RA006);
        assert!(
            diagnostics[0]
                .message
                .contains("function Badge(props: Record<never, never>) {"),
            "{:?}",
            diagnostics[0].message
        );
    }
}
