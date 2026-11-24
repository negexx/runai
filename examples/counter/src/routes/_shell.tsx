// A component module, not a route: the `_` prefix keeps the build from serving it.
// The build authors and checks it today but does not apply it — shell composition
// arrives with the router plan — so it exists to exercise `runai check` on a
// non-route component that takes children.
export default function Shell(props: { children: JSX.Element }) {
  return <main>{props.children}</main>
}
