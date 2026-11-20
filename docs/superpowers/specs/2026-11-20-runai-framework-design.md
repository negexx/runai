# runai — an agent-first web framework

**Status:** design, awaiting approval
**Date:** 2026-11-20
**Working name:** `runai` (taken from the project directory; my call, renameable)

---

## 1. Intent and why

Front-end frameworks are optimized for humans: forgiving syntax, implicit
behavior, silent recovery from mistakes. Those affordances exist because a
human gets stuck when a tool is pedantic, and a human can see when the output
is wrong.

An agent gets stuck when a tool is *silent*. It cannot see the page. When a
framework accepts a subtly wrong program, the agent gets no signal, the tests
pass, and the bug ships. The cost lands later, in review, or never.

So the framework is built on the opposite premise: **the compiler must be able
to see every mistake the agent can make, and must say so in a form the agent
can act on without guessing.** Performance is the second axis, not a decoration
on the first: an agent that waits on a slow build has a slow repair loop, so
compile speed is part of the agent's time-to-green.

### What this is not

Not a faster React, and not a new programming language. It is strict TSX
compiled to direct DOM mutations, with every ambiguity turned into a coded
compile error that names its own fix.

---

## 2. Goals

- An agent builds a working, idiomatic app of moderate size on the **first
  attempt**, and when it does not, needs **fewer repair rounds** and **fewer
  tokens** than the same agent using React.
- Runtime performance at or near hand-written DOM code: **target ≤ 1.25×
  vanilla** on the benchmark subset in §9.2, with fully static routes shipping
  **0 KB of JavaScript**.
- Sub-second incremental compile on a medium app; whole-project check in
  single-digit seconds.
- Every diagnostic is machine-actionable: stable code, source span, plain
  explanation, exact fix, and a local `SKILL.md` path.
- The agent can verify its own work headlessly, without a browser.

## 3. Non-goals (milestone 1)

- Server-side rendering, streaming, resumability, server functions, `use cache`.
- A custom authoring language. TSX only.
- WASM runtime for the render path. It is measurably slower than plain DOM
  calls for DOM-shaped work; it belongs in user compute, not in the framework.
- Native/GPU rendering. Browser is a hard requirement.
- Migration from React, a component library, an ecosystem.
- Winning a benchmark against vanilla. Vanilla is the floor, not the goal.

## 4. Anti-goals

These make the project a failure even if it works:

- A framework that accepts a program it cannot fully analyze.
- A diagnostic that says "invalid JSX" without saying which fix to apply.
- A dev server that an agent cannot start, query, and stop reliably.
- A performance claim without a reproducible command and a number.
- A custom syntax that buys strictness by throwing away TSX's training-data
  advantage, before measurement proves the trade is positive.

---

## 5. Constraints

- **Rust 1.99** for the compiler, on **Oxc** for parsing and semantic analysis.
- **Node 24 and Bun 1.4** both supported for the CLI and build. The CLI detects
  Bun and uses it for build-time module execution; otherwise it uses Node.
- Compiler output is plain ESM. No runtime-only magic.
- **Rolldown** (Vite 8's Rust bundler) produces browser bundles, via its
  programmatic API. If that API proves unstable, fall back to Vite's build API;
  no other bundler, so there is one module graph and one plugin model.
- We do **not** reimplement TypeScript's type checker. The compiler strips
  types and performs structural/reactivity analysis; `tsc --noEmit` runs
  alongside for types, and its output is normalized into our diagnostic format
  so the agent sees one error language.
- Runtime budget: **≤ 6 KB gzipped**, defined as minified + gzipped, including
  `signal`, `derived`, `effect`, `For`, `Show`, `When`, `resource`, and routing
  glue — excluding user code and the dev-only instrumentation build.
- Prebuilt native binaries for Linux x64, macOS arm64/x64, and Windows x64,
  delivered as npm optional dependencies. Linux arm64 is best-effort in M1.

---

## 6. Approaches considered

| | Approach | Why not / why |
|---|---|---|
| **A** | **Strict TSX → direct DOM.** Oxc compiler in Rust, signal runtime, no VDOM, coded diagnostics. | **Chosen.** Keeps TSX's in-distribution advantage for models, and buys reliability from strictness rather than from novel syntax. Fastest to a working app. |
| **B** | Purpose-built strict DSL, own grammar and compiler. | Rejected for v1. Strictness ceiling is higher, but zero-shot success drops because the language is out of distribution. Revisit as v2 only if the eval harness shows strict TSX leaving reliability on the table. |
| **C** | TSX → WASM reactive core, JS DOM bridge. | Rejected. DOM writes cross the WASM boundary; the boundary cost exceeds the savings for UI work, and it adds init-order and memory failure modes the agent must now debug. |
| **D** | A + typed WASM escape hatch for pure compute. | Deferred to v1.1. Same authoring surface, adds native speed only where compute dominates. |
| **E** | Client-first with build-time prerender. | **Chosen for milestone 1.** Static routes emit zero-JS HTML; interactive regions boot as islands. No hydration protocol, no streaming, no server process. |
| **F** | Full-stack from the first commit (SSR, streaming, resumability). | Rejected for v1. Highest ceiling, but months before anything runs, and each added layer multiplies first-attempt failure modes — directly against the metrics. |

### The decision that follows from E

Prerendered HTML is produced by a **separate string-emitting codegen path** in
the compiler, not by a DOM shim in Node. Interactive regions are **islands**:
each is marked in source, receives its own container, and replaces that
container on boot. There is no hydration.

Trade-off, stated plainly: an interactive island re-renders client-side, so it
shows a brief empty state until its script runs, and its prerendered markup is
discarded. In exchange there is no hydration mismatch — a failure class that
consumes a large share of real agent repair rounds — and no serialization
protocol to get wrong.

---

## 7. Design

### 7.1 Authoring model

Components are functions that run **once**. Props in, DOM out. Reactivity is
fine-grained: no component re-execution, therefore no hook-order rules, no
dependency arrays, no stale closures, no memoization annotations, and no
"why did this re-render" question. Those five failure classes disappear
structurally rather than by convention.

```tsx
import { signal, derived, effect, resource, For, When } from "runai"

export function Counter() {
  const count = signal(0)
  const parity = derived(() => (count() % 2 === 0 ? "even" : "odd"))

  return (
    <button onClick={() => count(count() + 1)}>
      {count()} — {parity()}
    </button>
  )
}
```

- Reactive reads are **calls**: `count()`, never a bare variable. Reading looks
  different from a plain value, which is the single most useful property for an
  agent reading its own code back.
- `signal(initial)` returns a single accessor that is also a setter:
  `count()` reads, `count(next)` writes. One name, one object, no tuple
  destructuring to get wrong and no `setCount`/`count` pairing to mismatch.
- `derived` replaces every `useMemo`. There is no `useMemo`.
- `effect` is for leaving the system (DOM APIs, subscriptions, logging), not for
  deriving state.
- `<For>` is keyed and requires a stable key. `<Show>` is conditional. Plain
  `map()` in JSX is a compile error.

**Tracking scopes, precisely.** These and only these are tracking scopes:
the expression slots of JSX children and attributes, `derived(fn)`,
`effect(fn)`, and the compute function passed to `resource(fn)`. A component
body is **not** a tracking scope. Event handlers are **not** tracking scopes:
a read inside `onClick` returns the value at click time, which is correct and
intentional. `RA001` fires only for reads in a component body that are not
inside one of those scopes.

**Async is explicit and total.** `resource(fn)` returns an accessor to a
discriminated union, and the only way to render it is `<When>`, which requires
all three branches:

```tsx
const user = resource(() => fetchUser(props.id))

<When value={user}
      pending={<Skeleton />}
      error={(e) => <ErrorView error={e} />}>
  {(data) => <h1>{data.name}</h1>}
</When>
```

`user()` returns `{ status: "pending" } | { status: "error", error } |
{ status: "success", data }`. `<When>` takes the accessor uncalled and narrows
it for each branch. This keeps one rule: **everything reactive is a call.**
Omitting `pending`, `error`, or children is a compile error. This is the
framework's signature guarantee: the agent cannot forget a loading state or an
error branch, because the program will not build.

`resource` re-runs when a signal read inside its compute function changes, and
cancels the previous in-flight call.

### 7.2 Components, props, and composition

- A **component** is any function that returns JSX. No decorator, no registry,
  no file convention is required to make one.
- Props are a single object argument with an **explicit type**, always:

  ```tsx
  type GreetingProps = { name: string; tone?: "warm" | "flat" }
  export function Greeting(props: GreetingProps) {
    return <p class={props.tone ?? "warm"}>Hello, {props.name}</p>
  }
  ```

  `RA006` fires on any component without an explicit props type, including
  components that take no props (`type Props = Record<never, never>`), so the
  question "where does this value come from" always has a written answer.
- **Nesting is allowed.** A component referenced statically is **inlined at
  compile time** into its parent's template, so `<Greeting name="Ada" />`
  contributes no runtime component boundary. Dynamic component selection
  (a variable holding a component) is not supported in M1 and is a compile
  error.
- **Children** arrive as `props.children`, typed as `JSX.Element`. Multiple
  named slots are not in M1; if a component needs two regions, it takes two
  props that are elements.
- **Identity and lifetime.** A component instance is owned by the island root
  (or by the parent `<For>` row). Signals created in a component body are
  disposed when that owner is disposed. There is no re-execution, so there is
  no "effect re-runs because a dependency changed" lifetime question.
- **Route entry point.** A page file's **default export** is its route
  component. It must be a component and must accept no props (`RA011`).

### 7.3 Static routes, islands, and the 0 KB guarantee

Every route file declares its rendering mode on the first line, above imports:

```tsx
"use static"
```

or

```tsx
"use island"
```

- **`"use static"`** — the route is prerendered to HTML at build time and ships
  **no JavaScript at all**. Any signal, event handler, `effect`, or `resource`
  in that file is `RA010_STATIC_VIOLATION` with the fix "change to
  `"use island"`". This makes the 0 KB claim a compiler-enforced invariant, not
  a build-output observation.
- **`"use island"`** — the route gets one container and one script. On boot the
  island mounts into its container, replacing the prerendered markup. One
  island per route in M1.
- A missing directive is `RA012_MISSING_MODE`. There is no inference: an agent
  never has to guess which mode a file is in, and review never has to trace it.

Granularity is **per route file** in M1. A static page that needs one
interactive widget becomes an island page. Component-level islands are v1.1;
this is the single largest scope cut in the design and it is deliberate.

### 7.4 Strictness rules (milestone 1)

Every violation is a stable code, a span, an explanation, a fix, and a
`node_modules/runai/skills/<code>.md` path.

| Code | Rejects | Fix it points to |
|---|---|---|
| `RA001_UNTRACKED_READ` | Signal read in a component body outside a tracking scope — the silently-frozen value | Wrap in `derived(...)` |
| `RA002_EFFECT_WRITE` | Writing a signal inside an `effect` | Derive it instead, or move the write to an event handler |
| `RA003_UNKEYED_LIST` | `.map()` in JSX, or `<For>` without a stable key | `<For each={...} key={...}>` |
| `RA004_INCOMPLETE_BRANCH` | `<When>` missing `pending`, `error`, or children | Add the missing branch |
| `RA005_MUTATED_STATE` | Mutating a value held in a signal | Call the setter with a new value |
| `RA006_UNANNOTATED_PROPS` | Component props without an explicit type | Add the type |
| `RA007_AMBIGUOUS_JSX` | Whitespace/sibling ambiguity, unclosed tags, unterminated attributes | Rewrite the JSX |
| `RA008_SERVER_IMPORT` | Importing `node:*` or a server-only module into client code | Remove it; M1 has no server runtime |
| `RA009_IMPLICIT_EFFECT` | State written during render (component body or JSX slot) | `effect`, or an event handler |
| `RA010_STATIC_VIOLATION` | Signal, event, `effect`, or `resource` in a `"use static"` route | Change the directive to `"use island"` |
| `RA011_BAD_ROUTE_EXPORT` | Page file without a default-exported, propless component | Add or fix the default export |
| `RA012_MISSING_MODE` | Route file without `"use static"` or `"use island"` | Add the directive |

`store`, `batch`, and `untrack` are **not** in M1. No diagnostic points at them.

Additional dev-only runtime enforcement: values held in signals are wrapped in
readonly proxies, so `todos.push(x)` throws with `RA005` and its fix instead of
silently failing to update.

### 7.5 Compiler

Single Rust binary on Oxc:

1. **Parse** — `oxc_parser` for TSX.
2. **Analyze** — `oxc_semantic` scopes/bindings/references, plus our passes:
   mode directive, reactivity scopes, component boundaries, static inlining,
   prop types, branch completeness, route export shape.
3. **Lower** — JSX to a template-and-bindings IR. Static structure becomes one
   template string; dynamic slots become bindings.
4. **Emit** — ESM. Client codegen clones the template once and wires bindings.
   Prerender codegen emits string concatenation from the same IR.

```tsx
// source
<button onClick={inc}>{count()}</button>
```

```js
// emitted (shape, not final output)
const _t = template("<button></button>")
const el = _t.cloneNode(true)
effect(() => (el.textContent = count()))
el.addEventListener("click", inc)
```

Static text and attributes never enter the runtime. That is where the
performance comes from: not a faster diff, but the absence of a diff.

**Prerender execution.** `runai build` emits prerender modules and executes
them in Bun (or Node) to produce HTML. The compiler strips `effect` bodies from
prerender output. A `resource` that cannot resolve at build time renders its
**pending** branch into the HTML; nothing awaits a network call during build.
Builds are therefore hermetic and deterministic, and a static route may
legitimately show its loading markup in the served HTML.

Diagnostics are structured: `code`, `severity`, `span`, `message`, `explain`,
`fix`, `doc`, and a reserved nullable `patch`. `fix` is **prose plus a small
code snippet**, not an applied patch: the agent reads it and edits. `--json`
emits one array; human output is the default. `tsc` errors are normalized into
the same schema with `fix: null`, so tooling has one shape to consume.

### 7.6 Runtime

≤ 6 KB gzipped:

- signal graph (`signal`, `derived`, `effect`, owner scope, disposal)
- template clone and binding application
- `For` (keyed, moves nodes rather than recreating), `Show`, `When`
- `resource` with cancellation
- event delegation for repeated handlers
- island mount
- dev-only instrumentation: tracking-scope assertions, readonly state,
  component tree for `inspect`

**One signal graph per island.** Two islands on the same page do not share
state; each mounts, owns, and disposes its own graph. Module-scope signals are
per-island too, since each island is its own module instance. Cross-island
state is v1.1. This keeps ownership and disposal trivially correct and is a
deliberate limitation, not an oversight.

### 7.7 Routing

- Routes live in `src/routes/`. `index.page.tsx` → `/`. `about.page.tsx` →
  `/about`. `blog/[slug].page.tsx` → `/blog/:slug`, with the segment passed as
  `props.params`.
- `404.page.tsx` handles unmatched paths.
- The document shell — `<html>`, `<head>`, shared nav — lives in
  `src/routes/_shell.tsx`, a component wrapping `props.children`. This is the
  one composition point; there are no nested layouts in M1.
- A link is a plain `<a href>`. **Navigation is a full page load in M1**: there
  is no client router, no route prefetching, and no client-side state across
  navigation. Route-level code splitting follows trivially, because each page
  loads exactly one script or none.
- `props.params` values are strings. Typed params are v1.1.

### 7.8 Styling

- CSS lives in `.css` files imported from a page or from a component
  (`import "./button.css"`). Rolldown bundles them; the emitted `<link>` is per
  page.
- `class` takes a string. `style` takes a string. No style objects, no
  automatic `px` suffixing, no CSS-in-JS, no scoping transform in M1 — the
  ambiguity those introduce is exactly what this framework exists to remove.
- A component may read `props.class` and must merge it explicitly rather than
  having the framework merge it invisibly (`RA013_CLASS_CONFLICT` if a
  component declares `class` in props but does not pass it to a root element).

### 7.9 Agent tooling

| Command | Purpose |
|---|---|
| `runai check` | All diagnostics, coded, `--json`, non-zero exit on error |
| `runai build` | Production build; static routes emit HTML with no JS |
| `runai dev` | Dev server; `--background` with a lockfile, `--json` logs, `/_runai/status` |
| `runai inspect <route>` | Boot the route in happy-dom, dump the element tree as JSON |
| `runai fmt` | Canonical formatter — one way to write each construct |
| `runai skills` | Write `skills/` into `node_modules/runai` so every diagnostic links locally |
| `runai bench` | Shell out to `bench/run.ts` (see §7.10) |

Every command is idempotent and safe to call repeatedly: starting a running dev
server returns the existing instance; stopping a stopped one succeeds silently.

**`runai inspect` boots the island**, not just the prerendered HTML. It loads
the route in happy-dom, runs the island mount, and prints the resulting element
tree. Flags: `--json`, `--click <selector>`, `--type <selector> <text>`,
`--wait <ms>`, `--snapshot` for a second dump after interaction. This is what
makes the inner loop usable by an agent with no browser: it can click a button
and read the changed tree. Playwright remains the authority in the eval harness;
`inspect` is the fast local approximation and is explicitly documented as such.

### 7.10 Eval harness — the part that makes the thesis falsifiable

`bench/` is a TypeScript program, not compiled into the Rust binary.
`runai bench` shells out to it.

20 tasks of rising difficulty (counter, todo, filtered list, form validation,
async fetch with error path, tabs, modal, optimistic update, multi-route app).
Each task has a prompt, Playwright acceptance tests, and a reference solution.

The runner builds each task in the target framework with a fixed model and
fixed budget, records the transcript, and reports:

- **first-attempt pass rate** — tests green with no human or reviewer input
- **repair rounds** — build/test cycles until green
- **time-to-green** — wall clock
- **tokens** — input plus output
- **runtime perf** of the finished app, measured separately

Control arm: the same tasks in React, same model, same harness. Control results
are cached and checked in, keyed by model and prompt hash, so re-runs are cheap
and comparisons are stable. Model credentials come from the environment
(`ANTHROPIC_API_KEY` or the configured OpenCode provider); the runner never
prompts interactively. The project succeeds only if the numbers move in the
right direction. If they do not, the strictness is theatre and we say so.

---

## 8. Milestone 1 scope

Ships:

- compiler with rules `RA001`–`RA013`, client and prerender codegen
- runtime within budget
- file-based router with full-page navigation, shell, 404, dynamic segments
- CSS pipeline
- `check`, `build`, `dev`, `inspect`, `fmt`, `skills`, `bench` commands
- `skills/` documentation for every diagnostic and for the authoring model
- examples: counter (island), todo (island), async list with error and empty
  states (island), 5-route app mixing static and island routes
- eval harness with 20 tasks and a checked-in React control

Does not ship: SSR, streaming, resumability, server functions, cache semantics,
WASM, custom language, component library, client router, cross-island state,
component-level islands, `store`/`batch`/`untrack`, typed route params.

## 9. Success criteria

1. The 5-route example app builds and runs; every `"use static"` route's HTML
   references **0 bytes** of JavaScript; the largest island payload is
   **< 10 KB gzipped** including runtime.
2. **Benchmark subset**, on the official js-framework-benchmark vanilla entry at
   a pinned commit, Chrome 157 headless, Linux x64, same machine:
   create 1,000 rows, create 10,000 rows, replace all, update every 10th, select,
   swap two, remove one, clear — **≤ 1.25× vanilla on every operation**.
   The exact command is committed at `bench/perf/README.md`.
3. `runai check` on the examples: no false positives, and every intentionally
   broken fixture produces the expected code.
4. Eval harness: first-attempt pass rate **strictly higher** than React, repair
   rounds and tokens **strictly lower**, on the same model. Reported as a table
   with the exact commands to reproduce.
5. Every diagnostic resolves to an existing local `SKILL.md`.
6. Runtime build measured in CI: **≤ 6 KB gzipped** by the §5 definition.

Anything not measured does not count as achieved.

## 10. Risks

| Risk | Mitigation |
|---|---|
| Oxc API churn breaks the compiler | Pin the Oxc version; isolate behind a thin internal AST boundary |
| Rolldown's programmatic API is unstable | Single fallback to Vite's build API; no second bundler |
| `tsc` integration friction, two error streams | Normalize on ingest; one output schema; one exit code |
| `inspect` diverges from real browser behavior | Documented as an approximation; Playwright is the authority; shared fixture set asserts both agree |
| Eval harness burns tokens and time | Small suite, cached controls, single model, per-task budget caps |
| Scope creep back into SSR | Milestone 1 is frozen by §8; SSR work goes to a v2 doc |
| Strictness produces false positives that block real apps | Every rule needs a documented escape hatch and a fixture proving it |
| Runtime exceeds 6 KB | Budget tracked in CI on every commit; a failing budget fails the build |
| `RA010`'s file-level granularity blocks real pages | Accepted for M1; component-level islands are the first v1.1 item |
| The custom-language temptation returns | Gated behind eval numbers from the same harness, not vibes |

## 11. Left to the builder

- Exact template/binding IR shape and codegen strategy per JSX form.
- Signal-graph internals (versioning, batching, scheduling).
- Dev-server implementation and the `inspect` element-tree schema.
- Formatter rules and the canonical form per construct.
- Eval task prompts, acceptance tests, and budget caps.
- Repository layout and package naming.
- Whether `<When>` generalizes into a single control-flow family in v1.1.
- Whether `RA013` survives contact with real components.

## 12. Calls made while resolving review questions

These were open in the first draft. They are my decisions, listed so they can be
challenged rather than discovered later:

1. **Route-file mode directive** (`"use static"` / `"use island"`) instead of an
   `<Island>` wrapper or inference. §7.3.
2. **Component-level islands are out of M1**; one island per route file.
3. **No client router in M1**; navigation is a full page load. §7.7.
4. **No cross-island state sharing**; one graph per island. §7.6.
5. **`runai inspect` boots islands in happy-dom** rather than only dumping
   prerendered HTML. §7.9.
6. **`store`, `batch`, `untrack` dropped from M1**, and the diagnostics that
   pointed at them rewritten. §7.4.
7. **`resource` returns an accessor to a discriminated union**, so "everything
   reactive is a call" holds without exception. §7.1.
8. **`signal()` returns one callable/assignable**, not a tuple. §7.1.
9. **Components are compile-time inlined**; dynamic component selection is an
   error. §7.2.
10. **`fix` is prose plus a snippet, not an applied patch.** §7.5.
11. **`runai bench` shells out to a TypeScript harness**, not embedded in Rust.
12. **Plain imported CSS files; string `class` and `style`.** §7.8.
13. **Rolldown**, with Vite's build API as the only fallback. §5.
