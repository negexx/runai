// runai runtime — the signal graph an island runs on.
//
// Reads are calls: `count()` reads, `count(next)` writes. `derived` recomputes
// lazily, only when read after a dependency changed. `effect` subscribes and is
// rerun by writes to what it read; writes flush synchronously at the outermost
// write, and effects enqueued during a flush (an effect writing a signal another
// effect reads) run in that same flush, each at most once per wave.
//
// Dev-only checks are written as inline
//   if (typeof process !== "undefined" && process.env.NODE_ENV !== "production")
// guards, deliberately not a hoisted `const DEV`. esbuild (Task 16's pinned
// `bunx esbuild runtime.js --minify --format=esm`) folds that exact inline
// expression to a constant, deletes the guarded body, and so the dev
// instrumentation contributes nothing to the ≤6 KB measured bundle. A hoisted
// `const DEV` is only folded to `const DEV=false` and leaves the body in place.

// Owner frames. Every effect and owner scope gives its disposers to the nearest
// enclosing frame; disposing a frame disposes its children as well.
const root = { stops: [], children: [], parent: null }
let frame = root
let tracker = null
let flushing = false
const queue = new Set()

function openScope() {
  const scope = { stops: [], children: [], parent: frame }
  frame.children.push(scope)
  return scope
}

function disposeScope(scope) {
  if (scope.dead) return
  scope.dead = true
  const parent = scope.parent
  if (parent) {
    const i = parent.children.indexOf(scope)
    if (i >= 0) parent.children.splice(i, 1)
  }
  // Detach first: a child disposing itself must not splice the array we walk.
  const kids = scope.children
  const stops = scope.stops
  scope.children = []
  scope.stops = []
  for (const child of kids) disposeScope(child)
  for (const stop of stops) stop()
}

function track(node) {
  const t = tracker
  if (!t || t.deps.includes(node)) return
  t.deps.push(node)
  ;(node.subs || (node.subs = new Set())).add(t)
}

function untrack(node) {
  for (const dep of node.deps) if (dep.subs) dep.subs.delete(node)
  node.deps.length = 0
}

function recompute(d) {
  // Unconditional, not dev-gated: this guard is what makes a dependency cycle
  // unrepresentable, so a production bundle must not recurse forever instead.
  if (d.busy) {
    throw new Error(
      "derived cycle: this derived reads itself. Give the computation a signal to read instead.",
    )
  }
  d.busy = true
  untrack(d)
  const prev = tracker
  tracker = d
  try {
    d.cached = d.fn()
    // Only a successful compute is cacheable: a throwing derived must retry.
    d.dirty = false
  } finally {
    tracker = prev
    d.busy = false
  }
}

function propagate(node) {
  if (!node.subs) return
  for (const sub of node.subs) {
    sub.dirty = true
    // No write can reach an effect without marking every derived on the path
    // dirty, so a derived is notified conservatively and recomputes on read.
    if (sub.runs) queue.add(sub)
    else propagate(sub)
  }
}

function execute(e) {
  if (e.dead) return
  if (e.running) {
    throw new Error(
      "effect cycle: an effect wrote a signal it reads. Derive it instead, or move the write to an event handler (RA002).",
    )
  }
  if (e.scope) disposeScope(e.scope)
  untrack(e)
  const prevTracker = tracker
  const prevFrame = frame
  frame = e.scope = openScope()
  tracker = e
  e.running = true
  try {
    e.fn()
  } finally {
    e.running = false
    tracker = prevTracker
    frame = prevFrame
  }
}

function flush() {
  if (flushing) return
  flushing = true
  let error = null
  let waves = 0
  try {
    while (queue.size) {
      if (++waves > 100) {
        queue.clear()
        throw new Error(
          "effect cycle: effects keep invalidating each other. Derive it instead, or move the write to an event handler (RA002).",
        )
      }
      const wave = [...queue]
      queue.clear()
      for (const e of wave) {
        // One failing effect must not starve its siblings in the same wave.
        if (!e.dead) {
          try {
            execute(e)
          } catch (err) {
            error ||= err
          }
        }
      }
    }
  } finally {
    flushing = false
  }
  if (error) throw error
}

export function signal(initial) {
  const node = { value: initial, subs: null }
  return (...args) => {
    if (args.length === 0) {
      track(node)
      return node.value
    }
    const next = args[0]
    if (Object.is(next, node.value)) return
    node.value = next
    propagate(node)
    flush()
  }
}

export function derived(fn) {
  const node = { fn, deps: [], subs: null, cached: undefined, dirty: true }
  return (...args) => {
    if (
      typeof process !== "undefined" &&
      process.env.NODE_ENV !== "production" &&
      args.length
    ) {
      throw new Error(
        "RUNAI_DEV derived is read-only: call the source signal's setter instead.",
      )
    }
    if (node.dirty) recompute(node)
    track(node)
    return node.cached
  }
}

export function effect(fn) {
  const e = { fn, deps: [], runs: true, scope: null }
  const stop = () => {
    if (e.dead) return
    e.dead = true
    untrack(e)
    if (e.scope) disposeScope(e.scope)
  }
  frame.stops.push(stop)
  try {
    execute(e)
  } catch (err) {
    // A first run that throws must not leave an unreturnable live subscription.
    stop()
    throw err
  }
  return stop
}

export function owner(fn) {
  const prev = frame
  const scope = openScope()
  frame = scope
  try {
    fn()
  } catch (err) {
    // A failed scope must not leak its effects: dispose, then surface the error.
    disposeScope(scope)
    throw err
  } finally {
    frame = prev
  }
  return () => disposeScope(scope)
}

// --- internals the emitter calls -------------------------------------------

// _setText(node, value) — value is a raw value or a thunk, read reactively.
export function _setText(node, value) {
  return effect(() => {
    const v = typeof value === "function" ? value() : value
    node.textContent = v == null ? "" : String(v)
  })
}

// _setAttr(node, name, value) — false/null/undefined removes the attribute.
export function _setAttr(node, name, value) {
  return effect(() => {
    const v = typeof value === "function" ? value() : value
    if (v == null || v === false) node.removeAttribute(name)
    else node.setAttribute(name, v === true ? "" : String(v))
  })
}

// _on(node, name, handler) — listener lives until its owner is disposed.
export function _on(node, name, handler) {
  node.addEventListener(name, handler)
  const stop = () => node.removeEventListener(name, handler)
  frame.stops.push(stop)
  return stop
}
