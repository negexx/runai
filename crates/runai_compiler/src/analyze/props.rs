//! `RA006_UNANNOTATED_PROPS`: every component writes its props type down.
//!
//! The framework's guarantee is that the question "where does this value come
//! from?" always has a written answer. A component whose parameter has no type
//! annotation answers "somewhere" — and a component with no parameter at all
//! answers "nowhere" only in prose, not in types. So both cases are `RA006`:
//! propless components declare `type Props = Record<never, never>` and take
//! `props: Props`.

use crate::analyze::component::ComponentInfo;
use crate::diagnostic::{Code, Diagnostic, Severity};

/// Check that `info`'s component declares its props type.
///
/// The answer is [`ComponentInfo::has_props_type`], which is "exactly one
/// parameter, and that parameter is annotated". A type alias named `Props`
/// elsewhere in the file does not satisfy this: the annotation belongs on the
/// parameter, where the reader can see which type a given component takes.
pub fn check_props(info: &ComponentInfo) -> Vec<Diagnostic> {
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
    let fix = if info.params == 0 {
        // Propless. The type still has to be written down, so name the exact
        // type to declare — `Record<never, never>` is the "no props" object,
        // and an empty object type (`{}`) would accept anything.
        format!(
            "Declare `type Props = Record<never, never>` and take it as a parameter: \
             `export default function {name}(props: Props) {{ ... }}`.",
            name = info.name,
        )
    } else {
        // Props exist but their shape is unstated.
        format!(
            "Add a type for `{name}`'s props — e.g. `type {name}Props = {{ id: string }}` — and \
             annotate the parameter: `function {name}(props: {name}Props)`.",
            name = info.name,
        )
    };

    Diagnostic {
        code: Code::RA006,
        severity: Severity::Error,
        span: info.body_span,
        message: if info.params == 0 {
            format!(
                "component `{name}` takes no props, but does not declare that in a type",
                name = info.name,
            )
        } else {
            format!("component `{name}` has untyped props", name = info.name)
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
