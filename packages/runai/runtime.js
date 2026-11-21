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
  // The dependency nodes a live effect holds. Disposal must cut the *reverse*
  // edge as well as the forward one, or a reader that never reruns pins the
  // disposed node — and its `fn` — for its own lifetime.
  globalThis.__runaiDevDeps = (stop) => (stop.node && stop.node.deps) || []
}

function openScope(parent = frame) {
  const scope = { stops: new Set(), children: [], parent }
  parent.children.push(scope)
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
// Disposal is terminal, not momentary: edges are dropped in both directions, and
// a surviving reader cannot re-add the node to `subs` on its next run via
// `track`. `dead` is what makes that impossible, and what stops a disposed
// derived from recomputing on a later read.
function disposeNode(node) {
  if (node.dead) return
  node.dead = true
  if (node.deps) untrack(node)
  // Cut the reverse edge too. Clearing `subs` stops the node notifying anyone,
  // but a surviving reader still holds this node in its own `deps` — and if it
  // never reruns it holds the node's `fn` (a `<For>` row's closure) forever.
  // Disposal has to be terminal in both directions.
  if (node.subs) {
    for (const sub of node.subs) {
      const i = sub.deps.indexOf(node)
      if (i >= 0) sub.deps.splice(i, 1)
    }
    node.subs.clear()
  }
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
    // resurrect it after its frame released it. Defensible in production — a
    // write to a released node is a teardown artefact, not a value error — but
    // silently dropping it is undiagnosable, and this framework's product is
    // diagnostics. Dev-only warning, not a throw: a teardown race must not turn
    // into a crash.
    if (node.dead) {
      if (typeof process !== "undefined" && process.env.NODE_ENV !== "production") {
        console.warn(
          "RUNAI_DEV write to a disposed signal ignored: its owner was disposed, so this setter is stale. Move the write into a live scope's effect or handler.",
        )
      }
      return
    }
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
  // Dev-only handle for the tests' dependency probe, folded out of the measured
  // bundle like every other dev check.
  if (typeof process !== "undefined" && process.env.NODE_ENV !== "production") stop.node = e
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

// --- components ------------------------------------------------------------

// A component body is not a tracking scope, so a builder function's reads must
// not be attributed to the effect that happened to call it. Otherwise `<For>`
// would read its own row signal during a rebuild, subscribe to it, and the next
// reorder would write that signal from the effect that reads it, i.e. an
// effect cycle. `build` opens a scope under `parent` (the frame at the call
// site, not the transient effect run), clears the tracker for the duration of
// `fn`, and hangs `fn`'s result on the scope as `scope.node` so the caller can
// dispose the row/branch and still address its DOM node.
function build(parent, fn) {
  const scope = openScope(parent)
  const prevTracker = tracker
  const prevFrame = frame
  tracker = null
  frame = scope
  try {
    scope.node = fn(scope)
  } catch (err) {
    disposeScope(scope)
    throw err
  } finally {
    tracker = prevTracker
    frame = prevFrame
  }
  return scope
}

// template(html) -> <template> whose `.content` is the parsed fragment.
// cloneNode the fragment's first child, never assign innerHTML on a live node:
// the pinned emitter shape is `t.content.firstChild.cloneNode(true)`.
export function template(html) {
  const t = document.createElement("template")
  t.innerHTML = html
  // A fresh fragment every call: two mounts of one template must not share
  // nodes. Cloning the whole fragment (not just its first child) keeps a
  // multi-root template intact.
  t.clone = () => t.content.cloneNode(true)
  return t
}

// A builder may return a fragment — every `For`/`Show`/`When` does, so nesting
// them is the normal composition. Inserting a fragment moves its children into
// the parent and leaves the fragment itself empty and parentless. So a group is
// resolved at *use* time, never captured by the parent: a `Show`/`When`/`For`
// exposes a live `kids()` and its visible group can change long after whoever
// built it looked at it. An author-supplied plain fragment has no such group, so
// its child list is captured on first read — always before its first insert,
// since reading a group is what leads to inserting it — and then served through
// the same accessor. (Read `childNodes` twice and the second pass is empty.)
function nodes(node) {
  if (!node) return []
  if (node.kids) return node.kids()
  if (node.nodeType !== 11) return [node]
  const group = [...node.childNodes]
  node.kids = () => group
  return group
}

// For — keyed list. A row is built exactly once per key: surviving keys keep
// their node *and* their row scope, so effects created by `children` keep
// running; only the node's position changes. Keys that disappear have their row
// scope disposed, which stops the effects and releases the row's signals.
//
// Rows live in the returned fragment, before a comment anchor maintained as its
// last child. `anchor.parentNode` is therefore the fragment while the list is
// detached and the real parent after the fragment is appended (appending a
// fragment moves its children out), so one code path handles both, and no
// wrapper element is introduced.
export function For(props) {
  const range = document.createDocumentFragment()
  const anchor = range.appendChild(document.createComment(""))
  const parent = frame
  const rows = new Map()
  // The live group, resolved on every read: each row's builder may return a
  // nested `Show`/`When`/`For` whose own visible nodes change long after this
  // list was built, so a group captured by the parent would be dead the moment
  // a nested primitive changed branch.
  let order = []
  range.kids = () => {
    const group = []
    for (const key of order) {
      const row = rows.get(key)
      if (row) for (const n of nodes(row.node)) group.push(n)
    }
    group.push(anchor)
    return group
  }
  effect(() => {
    // Read the dependencies *before* asking whether the list is attached. The
    // anchor is inside a `Show`/`When` branch that is taken out of the document
    // while hidden, so a `if (!host) return` here would end the run with no
    // dependencies: `execute` untracks first, so the effect would be permanently
    // unsubscribed and the list dead for the island's life (and its rows never
    // disposed). Only the DOM insertion is guarded.
    const host = anchor.parentNode
    const items = props.each() || []
    // `by` is called exactly once per item: an impure `by` read twice would
    // yield a key the reorder cannot find, and the throw would name `rows`.
    const keys = items.map((i) => props.by(i))
    const live = new Set()
    order = []
    for (let i = 0; i < items.length; i++) {
      const item = items[i]
      const key = keys[i]
      // A duplicate key would overwrite the first row's item signal (one item of
      // data silently vanishing) and then reorder that row twice, which for a
      // multi-node row reverses the row's own nodes. Unique keys are RA003's job,
      // but RA003 is not implemented yet in this milestone, so the runtime must
      // not corrupt the DOM *and* must not drop a row without saying so. Dev
      // warning, not a throw: production keeps rendering whatever is renderable.
      // The message says "not yet implemented" because this is all an agent ever
      // sees: naming a diagnostic that does not exist sends it looking for one.
      if (live.has(key)) {
        if (typeof process !== "undefined" && process.env.NODE_ENV !== "production") {
          // A warning must never become a render abort: `key` is only *typed*
          // string | number, and `String()` throws for an object with a throwing
          // toString/Symbol.toPrimitive or a null-prototype object. Formatting
          // defensively keeps the throw out of the effect body, where `flush`
          // would collect it and leave `order`/`rows` half-applied.
          // `String(key)` is deliberate rather than a `${key}` template literal:
          // `String` accepts a Symbol, a template literal throws on one.
          let shown
          try {
            shown = String(key)
          } catch {
            shown = typeof key
          }
          console.warn(
            `RUNAI_DEV duplicate key in For ignored: the row with key ${shown} is not rendered. Give each item a stable, unique key (RA003, not yet implemented).`,
          )
        }
        continue
      }
      live.add(key)
      order.push(key)
      const row = rows.get(key)
      // A surviving row only gets its item signal written: its node and its
      // effects are reused, so `children` is not re-run. That is what makes a
      // row's local state survive a reorder.
      if (row) row.item(item)
      else {
        const created = build(parent, (row) => {
          row.item = signal(item)
          return props.children(row.item)
        })
        rows.set(key, created)
      }
    }
    for (const [key, row] of rows) {
      if (live.has(key)) continue
      rows.delete(key)
      // Take the row's *current* nodes out of the document, then dispose its
      // scope: its effects stop, and its signals/deriveds are released with it.
      // A surviving row is never touched this way.
      take(nodes(row.node))
      disposeScope(row)
    }
    // Walk backwards, inserting each row before the next one: only rows whose
    // position actually changed are moved, and moving preserves identity. `host`
    // is null while the list is detached (hidden by a `Show`); the rows were
    // still built and ordered above, and the live group serves them on re-show.
    let next = anchor
    for (let i = order.length - 1; i >= 0; i--) {
      const row = nodes(rows.get(order[i]).node)
      // A row that rendered nothing (a component returning null) occupies no
      // position; it must not become the reference node for the row below it.
      if (!row.length) continue
      const last = row[row.length - 1]
      if (host && last !== next && last.nextSibling !== next)
        for (const n of row) if (n !== next) host.insertBefore(n, next)
      next = row[0]
    }
  })
  return range
}

// Show — conditional branch. The branches are caller-built nodes, so Show has
// nothing of its own to dispose. It must NOT dispose the hidden branch either:
// Task 5 disposal is terminal, so a hidden-then-shown branch would come back
// dead. It only moves the active node(s) in and out of the document.
export function Show(props) {
  const range = document.createDocumentFragment()
  const anchor = range.appendChild(document.createComment(""))
  // The active branch is resolved on *every* read, not captured when `which`
  // changes: a branch may be a nested `Show`/`When`/`For` whose own visible
  // nodes change long after this swap, so a cached snapshot would leave a parent
  // inserting nodes that are no longer the branch's group.
  let which = -1
  const active = () => (which === 1 ? props.children : props.fallback)
  // The anchor travels with the group: it is the insertion point a later swap
  // uses, so leaving it behind in the emptied fragment would insert the new
  // branch into a detached node.
  range.kids = () => [...nodes(active()), anchor]
  effect(() => {
    const next = props.when() ? 1 : 0
    if (next === which) return
    // Remove the outgoing branch's *current* group, then flip, then insert the
    // incoming branch's *current* group. `host` is null while this `Show` itself
    // is hidden; the swap still happens so `kids()` serves the right branch.
    take(nodes(active()))
    which = next
    const host = anchor.parentNode
    if (host) for (const n of nodes(active())) host.insertBefore(n, anchor)
  })
  return range
}

// When — the only way to render an async value. Takes the accessor uncalled and
// reads it inside an effect, so the pending branch is on screen synchronously
// (before any promise can settle) and each status change swaps the branch.
// A branch built from data gets its own scope, disposed when that branch
// leaves, so its effects do not outlive the data they were built from.
export function When(props) {
  const range = document.createDocumentFragment()
  const anchor = range.appendChild(document.createComment(""))
  const parent = frame
  let scope = null
  // Resolved on every read, same rule as `Show`; `pending` is a caller-built
  // constant node and `nodes` serves its group from the same accessor.
  const active = () => (scope ? scope.node : props.pending)
  // Same rule as `Show`: the anchor is part of the group.
  range.kids = () => [...nodes(active()), anchor]
  effect(() => {
    const result = props.value()
    // A later pending state must not resurrect a disposed node, so `pending` is
    // never disposed here; only a built branch carries a scope to dispose.
    const next =
      result.status === "pending"
        ? null
        : build(parent, () =>
            result.status === "error" ? props.error(result.error) : props.children(result.data),
          )
    take(nodes(active()))
    if (scope) disposeScope(scope)
    scope = next
    const host = anchor.parentNode
    if (host) for (const n of nodes(active())) host.insertBefore(n, anchor)
  })
  return range
}

// Remove a group of nodes from wherever they are. A node already detached is
// skipped: detaching is idempotent, so disposal order cannot double-remove.
function take(list) {
  for (const n of list) if (n.parentNode) n.parentNode.removeChild(n)
}

// resource(fn) -> accessor to { status: "pending" | "error" | "success" }.
// The compute runs inside an effect, so a signal read before its first await
// makes it re-run; a re-run aborts the previous call through the AbortSignal
// handed to a one-argument `fn`. A synchronous throw and a rejected promise
// both land in the error state: `When` must never need a try/catch.
export function resource(fn) {
  const out = signal({ status: "pending" })
  effect(() => {
    const ctrl = new AbortController()
    // Registered in the effect's own scope: a re-run (or disposal of the
    // caller's owner) aborts the in-flight call, and the check below keeps a
    // late resolution from writing to a torn-down scope.
    onDispose(() => ctrl.abort())
    out({ status: "pending" })
    let pending
    try {
      pending = fn.length ? fn({ signal: ctrl.signal }) : fn()
    } catch (error) {
      out({ status: "error", error })
      return
    }
    Promise.resolve(pending).then(
      (data) => {
        if (!ctrl.signal.aborted) out({ status: "success", data })
      },
      (error) => {
        if (!ctrl.signal.aborted) out({ status: "error", error })
      },
    )
  })
  return out
}

// mount(component, root) — an island's entry point. Everything the component
// creates is owned by one scope, so the returned disposer tears down its
// effects, signals, and listeners and takes its DOM out of the page. `owner`
// already disposes a scope whose body throws, so a component that throws during
// mount leaves nothing registered behind.
export function mount(component, root) {
  let node
  const dispose = owner(() => {
    node = component({})
    if (!node) return
    // Seed a plain fragment's group while it still holds its children: appending
    // moves them into `root` and leaves the fragment empty. `nodes` caches that
    // group only for an author-supplied fragment; every primitive answers with
    // its live group, which is resolved again at dispose time below.
    nodes(node)
    root.appendChild(node)
  })
  return () => {
    dispose()
    // Resolved after disposal, not captured: a nested `Show`/`When`/`For` keeps
    // swapping which nodes *are* the component's group, so a build-time snapshot
    // would leave a live branch in the page and remove already-detached nodes.
    take(nodes(node))
  }
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
