"use island"
import { derived, signal } from "runai"

// The island: this route ships JavaScript. The heading is static text and the
// button's label is two text bindings — the count and its parity — so a click has
// to update two nodes, not one, and the e2e script proves both tracked.
export default function Counter(props: Record<never, never>) {
  const count = signal(0)
  const parity = derived(() => (count() % 2 === 0 ? "even" : "odd"))
  return (
    <main>
      <h1>Counter</h1>
      <button onClick={() => count(count() + 1)}>
        {count()} — {parity()}
      </button>
    </main>
  )
}
