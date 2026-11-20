//! `RA006_UNANNOTATED_PROPS`: every component writes its props type down.
//!
//! The framework's guarantee is that the question "where does this value come
//! from?" always has a written answer. A component whose parameter has no type
//! annotation answers "somewhere" — and a component with no parameter at all
//! answers "nowhere" only in prose, not in types. So both cases are `RA006`:
//! propless components declare `type Props = Record<never, never>` and take
//! `props: Props`.
//!
//! The fix text is a snippet an agent copies, so it must be legal TypeScript and
//! must edit the declaration that exists. Three details follow from that: an
//! anonymous component is called `Page` rather than `default` (a reserved word),
//! the signature names the existing declaration, and the export form in the
//! snippet matches how the component is actually exported.

use crate::analyze::component::{ComponentInfo, ComponentKind};
use crate::diagnostic::{Code, Diagnostic, Severity};

/// Check that `info`'s component declares its props type.
///
/// The answer is [`ComponentInfo::has_props_type`], which is "exactly one
/// parameter, and that parameter is annotated". A type alias named `Props`
/// elsewhere in the file does not satisfy this: the annotation belongs on the
/// parameter, where the reader can see which type a given component takes.
pub fn check_props(info: &ComponentInfo) -> Vec<Diagnostic> {
    // A default export that is not a function has no props to type. `RA011`
    // already says to replace it, and a second diagnostic demanding a type for a
    // class or a literal would send the agent to edit the wrong thing.
    if info.kind == ComponentKind::NotAFunction {
        return Vec::new();
    }

    match info.params {
        // `function Page() {…}` — no parameter, so no type. This is *not* clean:
        // "it takes nothing" is a fact about the code that the type should state.
        0 => vec![unannotated_props(info)],
        // `function Page(props) {…}` — a parameter with no annotation.
        1 if !info.has_props_type => vec![unannotated_props(info)],
        // A typed parameter is clean. So is `params > 1`, which `RA011` already
        // rejects: the route component's problem is that it takes props at all,
        // not how they are typed, and two diagnostics for one mistake would
        // overstate it.
        _ => Vec::new(),
    }
}

/// `RA006_UNANNOTATED_PROPS`: the component's props have no written type.
fn unannotated_props(info: &ComponentInfo) -> Diagnostic {
    // `default` names the export, not a binding: `function default(props)` is a
    // parse error, so the text an agent copies must use a legal identifier.
    let name = info.display_name();

    // Where the fix names a signature, it must name the one that exists. When the
    // component is exported through a name (`export default Page`, `export { Page
    // as default }`), writing `export default function …` again is a duplicate
    // default export — a parse error for an agent following the fix verbatim.
    // There the repair is to annotate the existing declaration.
    let signature = if info.inline_default {
        format!("export default function {name}(props: Props)")
    } else {
        format!("function {name}(props: Props)")
    };

    let fix = if info.params == 0 {
        // Propless. The type still has to be written down, so name the exact
        // type to declare — `Record<never, never>` is the "no props" object,
        // and an empty object type (`{}`) would accept anything.
        format!(
            "Declare `type Props = Record<never, never>` and take it as a parameter: \
             `{signature} {{ ... }}`."
        )
    } else {
        // Props exist but their shape is unstated.
        format!(
            "Add a type for `{name}`'s props — e.g. `type {name}Props = {{ id: string }}` — and \
             annotate the parameter: `function {name}(props: {name}Props)`."
        )
    };

    Diagnostic {
        code: Code::RA006,
        severity: Severity::Error,
        span: info.body_span,
        message: if info.params == 0 {
            format!("component `{name}` takes no props, but does not declare that in a type")
        } else {
            format!("component `{name}` has untyped props")
        },
        explain: "Props are the only way data enters a component, so their type is the written \
                  answer to \"where does this value come from?\". An unannotated parameter leaves \
                  that answer unstated, and a propless component leaves it to prose."
            .into(),
        fix,
        doc: Code::RA006.skill_file(),
        patch: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyze::component::find_route_component;
    use crate::source::parse_tsx;

    use crate::analyze::component::validate_route_component;

    fn check(src: &str) -> Vec<crate::diagnostic::Diagnostic> {
        let alloc = oxc_allocator::Allocator::default();
        let program = parse_tsx(&alloc, src).unwrap();
        let info = find_route_component(program);
        let mut d = validate_route_component(info.as_ref());
        if let Some(i) = &info {
            d.extend(check_props(i));
        }
        d
    }

    #[test]
    fn untyped_props_is_ra006() {
        let src = "export default function Page(props) { return <p/> }\n";
        let d = check(src);
        assert_eq!(d[0].code, crate::diagnostic::Code::RA006);
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
        assert_eq!(d[0].code, crate::diagnostic::Code::RA006);
        assert!(d[0].fix.contains("Record<never, never>"));
    }

    /// The message is what an agent triages on; it must name the same mistake the
    /// fix repairs. It used to be asserted only through the fix.
    #[test]
    fn a_propless_message_says_the_props_are_undeclared() {
        let src = "export default function Page() { return <p/> }\n";
        let d = check(src);
        assert_eq!(
            d[0].message,
            "component `Page` takes no props, but does not declare that in a type"
        );
        assert!(d[0].explain.contains("type"), "{}", d[0].explain);
    }

    #[test]
    fn missing_default_export_is_ra011() {
        let src = "export function Page() { return <p/> }\n";
        let d = check(src);
        assert_eq!(d[0].code, crate::diagnostic::Code::RA011);
    }

    /// `RA006` is about the annotation being on the parameter, not a type alias
    /// existing somewhere in the file. A param that names an undeclared type is
    /// still annotated; an alias with no annotation on the param is still not.
    #[test]
    fn the_annotation_must_be_on_the_parameter() {
        let src =
            "type Props = { id: string }\nexport default function Page(props) { return <p/> }\n";
        assert_eq!(check(src)[0].code, crate::diagnostic::Code::RA006);
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
            vec![crate::diagnostic::Code::RA011]
        );
    }

    /// A rest parameter is a parameter: `function Page(...props)` is not
    /// propless, so it must not get the "declare `Record<never, never>`" fix.
    /// It is one untyped parameter, which is an untyped-props `RA006`.
    #[test]
    fn rest_parameter_is_not_propless() {
        let src = "export default function Page(...props) { return <p/> }\n";
        let d = check(src);
        assert_eq!(d[0].code, crate::diagnostic::Code::RA006);
        assert!(
            !d[0].fix.contains("Record<never, never>"),
            "rest-only props got the propless fix: {:?}",
            d[0].fix
        );

        let typed = "export default function Page(...props: Props[]) { return <p/> }\n";
        assert!(check(typed).is_empty());
    }

    /// An arrow function's props annotation is on the arrow's parameter too.
    #[test]
    fn arrow_function_props_are_checked_the_same_way() {
        let untyped = "export default (props) => <p/>\n";
        let d = check(untyped);
        assert_eq!(d[0].code, crate::diagnostic::Code::RA006);

        let typed = "export default (props: Props) => <p/>\n";
        assert!(check(typed).is_empty());
    }

    /// An anonymous component is named `"default"`, which is the export's name,
    /// not a binding. Emitting it verbatim gives `function default(props)` — a
    /// parse error that sends an agent following the fix into a loop it cannot
    /// diagnose. Both branches must use a legal identifier.
    #[test]
    fn an_anonymous_component_gets_a_legal_identifier_in_the_fix() {
        for src in [
            // concice arrow, block arrow, anonymous function declaration
            "export default () => <p/>\n",
            "export default () => { return <p/> }\n",
            "export default function () { return <p/> }\n",
        ] {
            let d = check(src);
            assert_eq!(d[0].code, crate::diagnostic::Code::RA006, "{src}");
            assert!(
                !d[0].fix.contains("function default"),
                "{src} emitted an illegal identifier: {:?}",
                d[0].fix
            );
            assert!(
                d[0].fix.contains("function Page(props: Props)"),
                "{src}: {:?}",
                d[0].fix
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
            assert_eq!(d[0].code, crate::diagnostic::Code::RA006, "{src}");
            assert!(
                !d[0].fix.contains("function default"),
                "{src} emitted an illegal identifier: {:?}",
                d[0].fix
            );
            assert!(!d[0].fix.contains("defaultProps"), "{src}: {:?}", d[0].fix);
            assert!(d[0].fix.contains("type PageProps"), "{src}: {:?}", d[0].fix);
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
            assert!(check_props(&info).is_empty(), "{src}");
        }
    }

    /// When the component reaches its default export through a name, the fix must
    /// not tell the agent to write `export default function Page(…)`: that is a
    /// second default export, i.e. a parse error for anyone who follows the text
    /// verbatim. The repair names the existing declaration instead.
    #[test]
    fn a_named_default_export_fix_does_not_add_a_second_export() {
        for src in [
            "function Page() { return <p/> }\nexport default Page\n",
            "function Page() { return <p/> }\nexport { Page as default }\n",
        ] {
            let d = check(src);
            assert!(
                !d[0].fix.contains("export default function"),
                "{src}: {:?}",
                d[0].fix
            );
            assert!(
                d[0].fix.contains("`function Page(props: Props)"),
                "{src}: {:?}",
                d[0].fix
            );
        }

        // An inline default export is the thing to edit, and says so.
        let d = check("export default function Page() { return <p/> }\n");
        assert!(
            d[0].fix
                .contains("`export default function Page(props: Props)"),
            "{:?}",
            d[0].fix
        );
    }

    /// The fix names the **binding**, not a conventional `Page`.
    ///
    /// `component_from_arrow` hardcoded `name: "default"`, and every earlier test
    /// happened to call its binding `Page`, which hid the difference. For a real
    /// file — `const AboutPage = (props) => <p/>; export default AboutPage` — the
    /// fix said `function Page(props: Props)`: applying that verbatim declares a
    /// new, unused `Page` and leaves `AboutPage` exactly as untyped as before, so
    /// the next run reports `RA006` again. The name in the text must be one the
    /// agent can find in this file.
    #[test]
    fn a_resolved_default_export_is_named_by_its_binding_not_page() {
        for (src, expected_fix) in [
            (
                "const AboutPage = (props) => <p/>\nexport default AboutPage\n",
                "function AboutPage(props: AboutPageProps)",
            ),
            (
                "const AboutPage = (props) => <p/>\nexport { AboutPage as default }\n",
                "function AboutPage(props: AboutPageProps)",
            ),
            (
                "const AboutPage = () => <p/>\nexport default AboutPage\n",
                "`function AboutPage(props: Props)",
            ),
            (
                "const AboutPage = () => <p/>\nexport { AboutPage as default }\n",
                "`function AboutPage(props: Props)",
            ),
            (
                // The name may also reach the export through parentheses.
                "const AboutPage = () => <p/>\nexport default (AboutPage)\n",
                "`function AboutPage(props: Props)",
            ),
            (
                // A named function expression is scoped to its own body: the
                // module binding the agent can edit is the declarator's.
                "const AboutPage = function Actual() { return <p/> }\nexport default AboutPage\n",
                "`function AboutPage(props: Props)",
            ),
            (
                // Following an alias lands on the declaration that has the
                // parameter, which is named `AboutPage`, not the alias.
                "const Alias = (AboutPage)\nconst AboutPage = () => <p/>\nexport default Alias\n",
                "`function AboutPage(props: Props)",
            ),
        ] {
            let d = check(src);
            assert_eq!(d[0].code, crate::diagnostic::Code::RA006, "{src}");
            assert!(
                d[0].fix.contains(expected_fix),
                "{src}: expected the binding's name in the fix: {:?}",
                d[0].fix
            );
            assert!(
                !d[0].fix.contains("function default"),
                "{src}: illegal identifier: {:?}",
                d[0].fix
            );
            assert!(
                !d[0].fix.contains("function Page("),
                "{src}: the fix names a binding this file does not declare: {:?}",
                d[0].fix
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
            assert_eq!(d[0].code, crate::diagnostic::Code::RA006, "{src}");
            assert!(d[0].message.contains("`Page`"), "{src}: {:?}", d[0].message);
            assert!(
                !d[0].message.contains("`default`"),
                "{src}: {:?}",
                d[0].message
            );
        }
    }

    /// Every `RA006` points at its own skill file, so Task 15's generated docs
    /// and these paths cannot drift apart.
    #[test]
    fn every_ra006_points_at_the_skill_file() {
        let alloc = oxc_allocator::Allocator::default();
        let src = "export default function Page(props) { return <p/> }\n";
        let program = parse_tsx(&alloc, src).unwrap();
        let info = find_route_component(program).unwrap();
        for diagnostic in check_props(&info) {
            assert_eq!(diagnostic.doc, crate::diagnostic::Code::RA006.skill_file());
        }
    }

    /// The propless fix names an exact type and signature. Applying it must be
    /// clean on the next run, or the agent is in a repair loop.
    #[test]
    fn following_the_propless_fix_is_clean() {
        let src = "type Props = Record<never, never>\nexport default function Page(props: Props) { return <p/> }\n";
        assert!(check(src).is_empty());
    }

    /// Destructuring the props object (`function Page({ id }: Props)`) is an
    /// annotated parameter, not an untyped one.
    #[test]
    fn a_destructured_parameter_counts_as_annotated() {
        let src = "type Props = { id: string }\nexport default function Page({ id }: Props) { return <p/> }\n";
        assert!(check(src).is_empty());
    }
}
