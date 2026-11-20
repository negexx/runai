import { test, expect } from "bun:test"
import { signal, derived, effect, owner, _setText, _setAttr, _on } from "../runtime.js"

test("signal reads and writes", () => {
  const count = signal(0)
  expect(count()).toBe(0)
  count(1)
  expect(count()).toBe(1)
})

test("derived recomputes when a dependency changes", () => {
  const n = signal(1)
  const double = derived(() => n() * 2)
  expect(double()).toBe(2)
  n(5)
  expect(double()).toBe(10)
})

test("effect reruns on dependency change", () => {
  const n = signal(0)
  const seen = []
  const stop = effect(() => seen.push(n()))
  n(1); n(2)
  expect(seen).toEqual([0, 1, 2])
  stop()
})

test("disposed effect stops observing", () => {
  const n = signal(0)
  let runs = 0
  const stop = effect(() => { n(); runs++ })
  stop()
  n(1)
  expect(runs).toBe(1)
})

test("owner disposes nested effects", () => {
  const n = signal(0)
  let runs = 0
  owner(() => { effect(() => { n(); runs++ }) })()
  n(1)
  expect(runs).toBe(1)
})

test("diamond dependency runs each effect once per write", () => {
  const n = signal(1)
  const a = derived(() => n() + 1)
  const b = derived(() => n() + 2)
  let runs = 0
  effect(() => { a(); b(); runs++ })
  const before = runs
  n(2)
  expect(runs - before).toBe(1)
})

// --- tests added beyond the brief: one per real hazard the brief leaves unproven ---

test("derived is cached and recomputes lazily, not on write", () => {
  const n = signal(1)
  let computes = 0
  const double = derived(() => { computes++; return n() * 2 })
  expect(double()).toBe(2)
  expect(computes).toBe(1)
  // a second read with no change must not recompute
  expect(double()).toBe(2)
  expect(computes).toBe(1)
  n(2)
  // a write must not push a recompute; the read pulls it
  expect(computes).toBe(1)
  expect(double()).toBe(4)
  expect(computes).toBe(2)
})

test("dynamic dependency edges are released on recompute", () => {
  const flag = signal(true)
  const a = signal(0)
  const b = signal(0)
  const seen = []
  const stop = effect(() => seen.push(flag() ? a() : b()))
  expect(seen).toEqual([0])
  b(1) // inactive branch: must not rerun
  expect(seen).toEqual([0])
  flag(false) // switch branch: reads b, which is now 1
  expect(seen).toEqual([0, 1])
  a(5) // a is no longer read: must not rerun
  expect(seen).toEqual([0, 1])
  stop()
})

test("writing an equal value does not rerun effects", () => {
  const n = signal(1)
  let runs = 0
  effect(() => { n(); runs++ })
  n(1)
  expect(runs).toBe(1)
})

test("disposing an outer owner reaches nested owner scopes", () => {
  const n = signal(0)
  let outer = 0
  let inner = 0
  const stop = owner(() => {
    effect(() => { n(); outer++ })
    owner(() => { effect(() => { n(); inner++ }) })
  })
  outer = 0; inner = 0
  n(1)
  expect([outer, inner]).toEqual([1, 1])
  stop()
  n(2)
  expect([outer, inner]).toEqual([1, 1])
})

test("child effects are disposed before each parent rerun", () => {
  const n = signal(0)
  const m = signal(0)
  let childRuns = 0
  const stop = effect(() => {
    n()
    effect(() => { m(); childRuns++ })
  })
  childRuns = 0
  n(1) // parent reruns: old child disposed, exactly one new child runs
  expect(childRuns).toBe(1)
  m(1) // only the live child observes m
  expect(childRuns).toBe(2)
  stop()
  n(2); m(2)
  expect(childRuns).toBe(2)
})

test("an effect writing a signal another effect reads settles without a lost update", () => {
  const a = signal(0)
  const b = signal(0)
  const seen = []
  effect(() => b(a()))
  effect(() => seen.push(b()))
  seen.length = 0
  a(1)
  expect(seen).toEqual([1])
})

test("a throwing effect does not starve its sibling effects", () => {
  const n = signal(0)
  const seen = []
  effect(() => { if (n() === 1) throw new Error("boom") })
  effect(() => seen.push(n()))
  seen.length = 0
  expect(() => n(1)).toThrow("boom")
  expect(seen).toEqual([1])
})

test("two effects that keep invalidating each other throw instead of hanging", () => {
  const on = signal(false)
  const a = signal(1)
  const b = signal(1)
  // Gated so construction settles: the ping-pong starts on the write, not now.
  effect(() => { if (on()) b(a()) })
  effect(() => { if (on()) a(b() + 1) })
  expect(() => on(true)).toThrow(/effect cycle/)
})

test("a throwing derived recomputes on the next read instead of caching", () => {
  const n = signal(1)
  let computes = 0
  const d = derived(() => {
    computes++
    if (n() === 1) throw new Error("boom")
    return n()
  })
  expect(() => d()).toThrow("boom")
  n(2)
  expect(d()).toBe(2)
  expect(computes).toBe(2)
})

test("writing to a derived throws instead of silently doing nothing", () => {
  const n = signal(1)
  const d = derived(() => n() * 2)
  expect(() => d(99)).toThrow(/read-only/)
  expect(d()).toBe(2)
})

test("a derived that reads itself throws instead of recursing forever", () => {
  const d = derived(() => d())
  expect(() => d()).toThrow(/derived cycle/)
})

test("a read inside an event handler is untracked and returns the value at call time", () => {
  const n = signal(0)
  const seen = []
  // No effect, no owner: exactly the situation of a click handler. A tracker
  // left behind by a previous flush would subscribe this read to nothing useful.
  const onClick = () => seen.push(n())
  onClick()
  n(1)
  onClick()
  expect(seen).toEqual([0, 1])
  // And the read must not have been replayed by the write.
  expect(seen.length).toBe(2)
})

test("an owner whose body throws still disposes what it created", () => {
  const n = signal(0)
  let runs = 0
  expect(() =>
    owner(() => {
      effect(() => { n(); runs++ })
      throw new Error("boom")
    }),
  ).toThrow("boom")
  runs = 0
  n(1)
  expect(runs).toBe(0)
})

test("an effect whose first run throws does not stay subscribed", () => {
  const n = signal(0)
  let runs = 0
  expect(() => effect(() => { n(); runs++; throw new Error("boom") })).toThrow("boom")
  runs = 0
  n(1)
  expect(runs).toBe(0)
})

test("a self-writing effect throws a cycle error instead of hanging", () => {
  const s = signal(0)
  expect(() => effect(() => s(s() + 1))).toThrow(/effect cycle/)
})

test("a throwing effect does not wedge the queue", () => {
  const n = signal(0)
  const seen = []
  effect(() => {
    const v = n()
    if (v === 1) throw new Error("boom")
    seen.push(v)
  })
  expect(() => n(1)).toThrow("boom")
  n(2)
  expect(seen).toEqual([0, 2])
})

// --- codegen internals Task 11 emits ---

test("_setText is a reactive text binding with a disposer", () => {
  const n = signal("a")
  const node = { textContent: "" }
  const stop = _setText(node, () => n())
  expect(node.textContent).toBe("a")
  n("b")
  expect(node.textContent).toBe("b")
  stop()
  n("c")
  expect(node.textContent).toBe("b")
})

test("_setAttr applies reactively and removes a falsy attribute", () => {
  const attrs = new Map()
  const node = {
    setAttribute: (name, value) => attrs.set(name, String(value)),
    removeAttribute: (name) => attrs.delete(name),
  }
  const disabled = signal(true)
  const stop = _setAttr(node, "disabled", () => disabled())
  expect(attrs.has("disabled")).toBe(true)
  disabled(false)
  expect(attrs.has("disabled")).toBe(false)
  stop()
})

test("_on binds a listener and is disposed by its owner", () => {
  const handlers = new Map()
  const node = {
    addEventListener: (name, handler) => handlers.set(name, handler),
    removeEventListener: (name) => handlers.delete(name),
  }
  const seen = []
  const stop = owner(() => _on(node, "click", (event) => seen.push(event)))
  handlers.get("click")(1)
  expect(seen).toEqual([1])
  stop()
  expect(handlers.size).toBe(0)
})
