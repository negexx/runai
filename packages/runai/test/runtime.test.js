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
  // A signal the effect does NOT read. If the handler's read leaked into the
  // effect's dependency list, writing `side` would rerun the effect and `runs`
  // would grow — that is the observable failure this test now really catches.
  const side = signal("s")
  const seen = []
  let runs = 0
  // A live effect runs first, so the tracking machinery is genuinely in use. With
  // no effect ever created, `tracker` is null whichever way the runtime is
  // written, so the assertion below could not fail for the reason it claims.
  const stop = effect(() => { n(); runs++ })
  const onClick = () => seen.push(n(), side())
  onClick()
  n(1)
  onClick()
  expect(seen).toEqual([0, "s", 1, "s"])
  expect(runs).toBe(2)
  const before = runs
  side("t")
  expect(runs).toBe(before) // the handler's read was not tracked
  expect(seen.length).toBe(4) // and the write did not replay the handler
  stop()
})

test("a throwing derived leaves its reader subscribed, so a later write retries it", () => {
  const n = signal(1)
  const d = derived(() => {
    if (n() === 1) throw new Error("boom")
    return n()
  })
  let runs = 0
  const stop = effect(() => {
    runs++
    try { d() } catch { /* the user sees one error, then a retry */ }
  })
  expect(() => n(2)).not.toThrow()
  // Subscribing before the throw is what keeps the effect reachable: without it
  // the effect is inert for the rest of its life and the UI freezes.
  expect(runs).toBe(2)
  expect(d()).toBe(2)
  stop()
})

test("a derived is untracked from its source when its owner is disposed", () => {
  const n = signal(1)
  let computes = 0
  let d
  const dispose = owner(() => {
    d = derived(() => { computes++; return n() })
  })
  expect(d()).toBe(1)
  expect(computes).toBe(1)
  dispose()
  // Disposal must drop the edge, not merely stop calling the derived: a write
  // to `n` would otherwise make the derived dirty and the read recompute.
  n(2)
  expect(computes).toBe(1)
  expect(d()).toBe(1)
  expect(computes).toBe(1)
})

test("a signal created in an owner stops retaining subscribers when disposed", () => {
  let inner
  let runs = 0
  const dispose = owner(() => { inner = signal(0) })
  // An effect outside the owner reads it, so the disposed signal holds a
  // subscriber that lives elsewhere. Disposal must make it release that edge.
  const stop = effect(() => { inner(); runs++ })
  expect(runs).toBe(1)
  // The write is deliberately a no-op; its dev warning is asserted by its own
  // test, so silence it here rather than printing it as suite output.
  const warn = console.warn
  console.warn = () => {}
  try {
    dispose()
    inner(1)
    expect(runs).toBe(1)
    inner(2)
    expect(runs).toBe(1)
  } finally {
    console.warn = warn
  }
  // A stale reference must not resurrect it either: the write is a no-op, so
  // the value it serves is still the one it had when its frame went away.
  expect(inner()).toBe(0)
  stop()
})

test("a disposed signal is dropped from a live reader's deps, not just from its own subs", () => {
  const deps = globalThis.__runaiDevDeps
  expect(typeof deps).toBe("function")
  const other = signal(0)
  let inner
  const dispose = owner(() => { inner = signal(0) })
  let runs = 0
  const stop = effect(() => { inner(); other(); runs++ })
  expect(runs).toBe(1)
  // The reader holds the signal in its own `deps` — the reverse edge. Clearing
  // `node.subs` alone leaves this in place, and if the effect never reruns (a
  // root-scope reader that only ever reads `other`) it pins `inner` — and, for a
  // `<For>` row, the row's closure — for the reader's whole lifetime.
  expect(deps(stop).includes(inner.node)).toBe(true)
  dispose()
  // No rerun in between: disposal itself must cut the reverse edge.
  expect(runs).toBe(1)
  expect(deps(stop).includes(inner.node)).toBe(false)
  // And the reader still works: cutting the dead edge must not have unsubscribed
  // it from the nodes that are still alive.
  other(1)
  expect(runs).toBe(2)
  stop()
})

test("a write to a disposed signal is a dev warning, not a silent drop", () => {
  let inner
  const dispose = owner(() => { inner = signal(0) })
  const stop = effect(() => { inner() })
  const warn = console.warn
  const seen = []
  console.warn = (message) => seen.push(message)
  try {
    dispose()
    inner(1)
    // A teardown race must not crash: the write is still a no-op.
    expect(inner()).toBe(0)
  } finally {
    console.warn = warn
  }
  // Silently dropping it would be undiagnosable, and this framework's product is
  // diagnostics. One warning, naming the dev gate, no throw.
  expect(seen.length).toBe(1)
  expect(seen[0]).toMatch(/RUNAI_DEV/)
  expect(seen[0]).toMatch(/disposed signal/)
  stop()
})

test("a disposed derived does not recompute or re-subscribe on a later read", () => {
  const n = signal(1)
  // A second signal the surviving reader also reads, so it can be woken without
  // going through the disposed derived — that is the read that leaks.
  const bump = signal(0)
  const subs = globalThis.__runaiDevSubs
  let computes = 0
  let runs = 0
  let d
  const dispose = owner(() => { d = derived(() => { computes++; return n() }) })
  const stop = effect(() => { bump(); runs++; d() })
  expect([computes, runs]).toEqual([1, 1])
  expect(subs(n)).toBe(1) // the derived subscribes to the source
  expect(subs(d)).toBe(1) // and the surviving reader subscribes to the derived
  dispose()
  // Disposal severs the source edge and clears the derived's own subscribers.
  expect(subs(n)).toBe(0)
  expect(subs(d)).toBe(0)
  // Wake the surviving reader. It still holds the disposed derived in `deps`,
  // so its next run calls the accessor again: that read must not recompute the
  // dead node and must not re-add it to the source's subscriber set.
  bump(1)
  expect(runs).toBe(2)
  expect(computes).toBe(1)
  expect(d()).toBe(1)
  expect(computes).toBe(1)
  expect(subs(n)).toBe(0)
  expect(subs(d)).toBe(0)
  stop()
})

test("a derived disposed while dirty serves its last value instead of recomputing", () => {
  const n = signal(1)
  let computes = 0
  let d
  const dispose = owner(() => { d = derived(() => { computes++; return n() }) })
  expect(d()).toBe(1)
  expect(computes).toBe(1)
  // Dirty at disposal — the case the clean-at-disposal test cannot see. A
  // momentary disposal would recompute here and serve 2.
  n(2)
  expect(computes).toBe(1)
  dispose()
  expect(d()).toBe(1)
  expect(computes).toBe(1)
})

test("a stopped effect is not retained by the root scope", () => {
  const count = globalThis.__runaiDevStops
  expect(typeof count).toBe("function")
  const base = count()
  const stops = []
  for (let i = 0; i < 50; i++) stops.push(effect(() => {}))
  expect(count()).toBe(base + 50)
  for (const stop of stops) stop()
  // Every stop must remove itself from the frame's set: an island mount/dispose
  // cycle repeats this indefinitely, so growth here is a process-lifetime leak.
  expect(count()).toBe(base)
})

test("a stopped _on binding is not retained by the root scope", () => {
  const count = globalThis.__runaiDevStops
  const base = count()
  const node = { addEventListener() {}, removeEventListener() {} }
  const offs = []
  for (let i = 0; i < 50; i++) offs.push(_on(node, "click", () => {}))
  expect(count()).toBe(base + 50)
  // `_on` returns a disposer the emitter is allowed to ignore. Ignoring it is
  // fine (the owner covers it), but calling it must not leave the closure in the
  // root frame — otherwise every island remount leaks 50 of them. Deleting an
  // already-absent entry is idempotent, so the tally is the only way to see the
  // difference between "removed" and "never registered".
  for (const off of offs) off()
  expect(count()).toBe(base)
})

test("a stopped effect is not retained by its owner scope", () => {
  const rootCount = globalThis.__runaiDevStops
  const scopeCount = globalThis.__runaiDevScopeStops
  const frameStops = globalThis.__runaiDevFrameStops
  // The non-root path is the one `<For>` rows take: a 50-row list inside one
  // island scope must not accumulate closures in that scope either.
  const n = signal(0)
  let outer
  let runs = 0
  const dispose = owner(() => {
    outer = effect(() => { n(); runs++ })
  })
  const rootBase = rootCount()
  expect(runs).toBe(1)
  // Stopping one effect while its owner is still alive must delete it from the
  // scope it registered with, and from nowhere else.
  outer()
  expect(scopeCount(dispose)).toBe(0)
  expect(rootCount()).toBe(rootBase)
  n(1)
  expect(runs).toBe(1)

  // The same loop as the root test, but inside a live owner scope: rows that
  // mount and unmount under one island must not accumulate in that scope, and
  // the root tally must stay untouched by work that never registered there.
  let live
  const dispose2 = owner(() => {
    const stops = []
    for (let i = 0; i < 50; i++) stops.push(effect(() => {}))
    expect(frameStops()).toBe(50)
    for (const stop of stops) stop()
    expect(frameStops()).toBe(0)
    const node = { addEventListener() {}, removeEventListener() {} }
    for (let i = 0; i < 50; i++) _on(node, "click", () => {})()
    expect(frameStops()).toBe(0)
    expect(rootCount()).toBe(rootBase)
    live = effect(() => { n(); runs++ })
  })
  expect(scopeCount(dispose2)).toBe(1) // only `live` is still registered
  // Disposing the owner disposes `live`, which deletes itself from the scope
  // being walked, and must not grow either tally.
  dispose2()
  expect(scopeCount(dispose2)).toBe(0)
  expect(rootCount()).toBe(rootBase)
  const before = runs
  n(2)
  expect(runs).toBe(before)
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

test("propagation through a deep diamond is linear, not exponential", () => {
  const n = signal(1)
  // 20 levels of two deriveds both reading the level above: naively the walk
  // visits the last level once per inbound path, 2^20 times per write.
  const visits = globalThis.__runaiDevPropagate
  expect(typeof visits).toBe("function")
  let level = null
  for (let i = 0; i < 20; i++) {
    const prev = level
    level = prev
      ? [derived(() => prev[0]() + prev[1]() + 1), derived(() => prev[0]() - prev[1]())]
      : [derived(() => n() + 1), derived(() => n() - 1)]
  }
  let runs = 0
  const stop = effect(() => { level[0](); level[1](); runs++ })
  const before = visits()
  n(2)
  expect(runs).toBe(2)
  // 40 derived nodes, each visited once: bounded well below 2^20.
  expect(visits() - before).toBeLessThanOrEqual(40)
  stop()
})

test("_setAttr policy: null/undefined/false remove, everything else sets", () => {
  const attrs = new Map()
  const node = {
    setAttribute: (name, value) => attrs.set(name, String(value)),
    removeAttribute: (name) => attrs.delete(name),
  }
  const v = signal(null)
  _setAttr(node, "data-x", () => v())
  expect(attrs.has("data-x")).toBe(false)
  // null and undefined both mean "no attribute", like false.
  v(undefined)
  expect(attrs.has("data-x")).toBe(false)
  v(false)
  expect(attrs.has("data-x")).toBe(false)
  // Strings are set verbatim, including a falsy-but-present "" and "0".
  v("a")
  expect(attrs.get("data-x")).toBe("a")
  v("")
  expect(attrs.has("data-x")).toBe(true)
  expect(attrs.get("data-x")).toBe("")
  v(0)
  expect(attrs.get("data-x")).toBe("0")
  // true is the bare attribute: present with an empty value.
  v(true)
  expect(attrs.get("data-x")).toBe("")
})

test("_on binds a listener and is disposed by its owner", () => {
  const handlers = new Set()
  const node = {
    addEventListener: (name, handler) => handlers.add(handler),
    removeEventListener: (name, handler) => handlers.delete(handler),
  }
  const seen = []
  const stop = owner(() => _on(node, "click", (event) => seen.push(event)))
  handlers.forEach((handler) => handler(1))
  expect(seen).toEqual([1])
  stop()
  expect(handlers.size).toBe(0)
})

test("_on's own disposer removes the listener and is idempotent", () => {
  const handlers = new Set()
  let removed = 0
  const node = {
    addEventListener: (name, handler) => handlers.add(handler),
    removeEventListener: (name, handler) => {
      removed++
      handlers.delete(handler)
    },
  }
  const handler = () => {}
  const off = _on(node, "click", handler)
  expect(handlers.size).toBe(1)
  off()
  expect(handlers.size).toBe(0)
  // Stopping twice must not throw or resurrect the listener, and must not run
  // the teardown a second time: `stop.dead` is what makes the second call a
  // no-op rather than a repeated `removeEventListener` + `stops.delete`.
  off()
  expect(handlers.size).toBe(0)
  expect(removed).toBe(1)
})
