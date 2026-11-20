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
// enclosing frame; disposing a frame disposes its children as well. `stops` is a
// Set so that an individually stopped disposer can delete itself: a stopped
// effect must not stay reachable from the root frame for the process lifetime.
const root = { stops: new Set(), children: [], parent: null }
let frame = root
let tracker = null
let flushing = false
let stamp = 0
// Dev-only: counts derived visits by propagation, proving the walk is linear.
let propagations = 0
const queue = new Set()

// Dev-only probes used by tests. Written inline so the measured build deletes
// them along with the other dev checks; see the header comment.
if (typeof process !== "undefined" && process.env.NODE_ENV !== "production") {
  globalThis.__runaiDevStops = () => root.stops.size
  globalThis.__runaiDevPropagate = () => propagations
  // Subscriber count of a signal/derived node. Disposal must not merely cancel
  // the edges it has now; a live reader must not be able to wire it back up.
  globalThis.__runaiDevSubs = (accessor) =>
    accessor.node && accessor.node.subs ? accessor.node.subs.size : 0
  // Disposer count of an owner scope, so the owner-scope branch of `stop()` can
  // be asserted directly instead of through the root tally.
  globalThis.__runaiDevScopeStops = (dispose) =>
    dispose.scope ? dispose.scope.stops.size : -1
  // Disposer count of the frame currently running, for assertions made *inside*
  // an owner body, where its dispose function does not exist yet.
  globalThis.__runaiDevFrameStops = () => frame.stops.size
}

function openScope() {
  const scope = { stops: new Set(), children: [], parent: frame }
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
  // Detach first: a child disposing itself must not splice the array we walk,
  // and a disposer deleting itself must not mutate the Set being walked.
  const kids = scope.children
  const stops = scope.stops
  scope.children = []
  scope.stops = new Set()
  for (const child of kids) disposeScope(child)
  for (const stop of [...stops]) stop()
}

function track(node) {
  const t = tracker
  // `node.dead` is what makes disposal terminal: a surviving reader may still
  // hold this node in its `deps`, and without this check its next run would
  // re-add the node to `subs`, resurrecting a disposed signal or derived.
  if (!t || node.dead || t.deps.includes(node)) return
  t.deps.push(node)
  ;(node.subs || (node.subs = new Set())).add(t)
}

function untrack(node) {
  for (const dep of node.deps) if (dep.subs) dep.subs.delete(node)
  node.deps.length = 0
}

// A disposed node forgets its own deps and stops retaining its subscribers, so
// a derived created inside a row scope cannot keep that row's state alive in a
// shared signal's `subs` after the row is gone.
//
// Disposal is terminal, not momentary: edges are dropped, but a surviving
// reader still holds the node in its own `deps` and would re-add it to `subs`
// on its next run via `track`. `dead` is what makes that impossible, and what
// stops a disposed derived from recomputing on a later read.
function disposeNode(node) {
  if (node.dead) return
  node.dead = true
  if (node.deps) untrack(node)
  if (node.subs) node.subs.clear()
}

// Register a node's release with the nearest frame, if that frame can ever be
// disposed. Registering with `root` would only pin the node for the process
// lifetime: root has no disposer, and a root-scope node already lives as long as
// the module that created it.
function onDispose(release) {
  if (frame !== root) frame.stops.add(release)
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

function propagate(node, s) {
  if (!node.subs) return
  // A fresh stamp per top-level write. A derived already stamped in this walk
  // has notified its own subscribers, so revisiting it is pure duplication; in a
  // diamond lattice that is one visit per inbound path, i.e. 2^n. The stamp is
  // read from the derived itself rather than from `dirty`, because a derived may
  // already be dirty from a compute that threw and still needs this write.
  s ||= ++stamp
  for (const sub of node.subs) {
    if (sub.runs) queue.add(sub)
    else if (sub.stamp !== s) {
      sub.stamp = s
      sub.dirty = true
      if (typeof process !== "undefined" && process.env.NODE_ENV !== "production") propagations++
      propagate(sub, s)
    }
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
  // Registering with the creation frame means a signal created inside a row
  // scope is released with that row: a disposed signal must stop retaining its
  // subscribers, and its closure must not outlive the frame.
  onDispose(() => disposeNode(node))
  const read = (...args) => {
    if (args.length === 0) {
      track(node)
      return node.value
    }
    // A disposed signal is inert: a surviving caller's stale reference must not
    // resurrect it after its frame released it.
    if (node.dead) return
    const next = args[0]
    if (Object.is(next, node.value)) return
    node.value = next
    propagate(node)
    flush()
  }
  // Dev-only handle for the tests' subscriber-count probe, folded out of the
  // measured bundle like every other dev check.
  if (typeof process !== "undefined" && process.env.NODE_ENV !== "production") read.node = node
  return read
}

export function derived(fn) {
  const node = { fn, deps: [], subs: null, cached: undefined, dirty: true }
  // Same rule as `signal`: a derived created inside a row scope keeps the row's
  // state alive in a shared signal's `subs` set unless the frame can dispose it.
  onDispose(() => disposeNode(node))
  const read = (...args) => {
    if (
      typeof process !== "undefined" &&
      process.env.NODE_ENV !== "production" &&
      args.length
    ) {
      throw new Error(
        "RUNAI_DEV derived is read-only: call the source signal's setter instead.",
      )
    }
    // Terminal disposal: a disposed derived serves its last cached value and
    // must neither recompute nor re-subscribe to the sources it was cut from.
    if (node.dead) return node.cached
    // Subscribe before computing: if `fn` throws, the edge the reader needs to
    // be woken again must already exist. The residual limit is that the derived
    // only knows the deps it read before the throw, so a retry has to come from
    // a write to one of those — strictly better than being inert forever.
    track(node)
    if (node.dirty) recompute(node)
    return node.cached
  }
  if (typeof process !== "undefined" && process.env.NODE_ENV !== "production") read.node = node
  return read
}

export function effect(fn) {
  const e = { fn, deps: [], runs: true, scope: null }
  const parent = frame
  const stop = () => {
    if (e.dead) return
    e.dead = true
    // Must remove itself: a repeated create/stop cycle at root scope would
    // otherwise retain every `stop` closure — and `fn` — for the process life.
    parent.stops.delete(stop)
    untrack(e)
    if (e.scope) disposeScope(e.scope)
  }
  parent.stops.add(stop)
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
  const dispose = () => disposeScope(scope)
  if (typeof process !== "undefined" && process.env.NODE_ENV !== "production")
    dispose.scope = scope
  return dispose
}

// --- internals the emitter calls -------------------------------------------

// _setText(node, value) — value is a raw value or a thunk, read reactively.
export function _setText(node, value) {
  return effect(() => {
    const v = typeof value === "function" ? value() : value
    node.textContent = v == null ? "" : String(v)
  })
}

// _setAttr(node, name, value) — the policy, pinned by a test:
//   false, null, undefined  -> removeAttribute (no attribute at all)
//   true                    -> setAttribute(name, "") (the bare HTML attribute)
//   anything else           -> String(value), so 0 and "" are set, not removed
// `0` is a real value (`tabindex={0}`, `value={0}`); removing it would be wrong.
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
  const parent = frame
  const stop = () => {
    if (stop.dead) return
    stop.dead = true
    parent.stops.delete(stop)
    node.removeEventListener(name, handler)
  }
  parent.stops.add(stop)
  return stop
}
