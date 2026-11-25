# Diagnostic text audit — do the nine codes state only mechanisms the code has

Scope: `message`, `explain`, and `fix` for `RA001`, `RA002`, `RA006`, `RA007`,
`RA009`, `RA010`, `RA011`, `RA012`, `RA014`, all sourced from
`crates/runai_compiler/src/diagnostic.rs` (`Code::explain` at `:144`, `Code::fix_template`
at `:267`) and the per-site `message` composed by the rules.

Method: every claim below was checked against the code **and** against the built
binary. The binary is `target/release/runai` built from this tree
(`cargo build --release --offline`). Each fixture was a real route file under
`/tmp/opencode/diag/src/routes/index.page.tsx`, run through `runai check .`.

**No diagnostic text was edited.** Two claims are close enough to false to deserve a
recorded note (RA011's "direct return" and RA014's fifth-family list); neither is
false *as written*, and a wrong edit is worse than a finding, so both are reported
with their evidence and left alone.

**Scope note — the code set changed while this audit was running.** The brief named
nine codes. While it ran, C1's fix added `RA015_EXCESSIVE_NESTING` and I9's fix added
`RA016` (a static attribute whose value a prerender cannot evaluate), both in
`crates/runai_compiler/src/diagnostic.rs`. `Code::all()` (`static ALL`, `:591`) is now
eleven. The nine codes' `explain` and `fix_template` bodies are **byte-identical**
before and after those additions — verified by diffing `git show HEAD:…` against the
working tree over the `pub fn explain … pub fn fix_template … static ALL` region;
the only hunks are the two new arms. So everything below still describes the code
that is on disk. The two new codes are **not audited here** — they are the work of
the agent that added them, and auditing brand-new text against a tree still being
edited would be checking a moving target.

Verdict vocabulary, used strictly:

- **true** — the code does what the sentence says. Reproduced.
- **true but incomplete** — accurate, but a reader could infer something the code
  does not do. Not a defect on its own.
- **unverifiable here** — would need reading or running something outside this tree.
- **FALSE** — reproduced as not matching the code. Would justify an edit.

---

## A new defect found in `RA015`, which is not one of the nine

`RA015_EXCESSIVE_NESTING` was added while this audit ran (C1's fix). Its `fix` reads:

> Reduce the nesting to at most 512 levels: close elements earlier, or split the
> inner markup into a separate `.tsx` module the route does not inline.

**The second clause is unreachable advice, and it is the project's worst-defect
class.** A separate `.tsx` module cannot be used, because the milestone has no
bundler and no component inlining. Reproduced:

```sh
# src/routes/Inner.tsx holds the deep markup; the page imports and tags it.
$ cd /tmp/opencode/split && runai check .
src/routes/index.page.tsx:2:23: error[RA014_NOT_SUPPORTED_YET]: cannot import "./Inner" yet:
  this milestone has no bundler to resolve a module other than `"runai"`, and a
  module-level import is not emitted. Inline the value here, or import it from `"runai"`.
src/routes/index.page.tsx:4:16: error[RA014_NOT_SUPPORTED_YET]: cannot lower a capitalized
  tag (a component, `<Inner>`) yet: `Inner` names a component. Inline its markup here,
  or ask for component inlining in a later plan; this milestone renders DOM elements only.
```

So an agent following that half of the fix moves from one refusal to **two others**.
That is exactly the closed loop `AGENTS.md` names: "Every repair must name a construct
this milestone can compile."

Note that the *first* clause ("close elements earlier") is legal and effective:
flattening to 600 siblings at depth 2 analyses clean (`rc=0`). The fix does not need
the second clause.

**Not edited**, per the instruction to report rather than patch when a fix requires
more than a minimal, certain change — and because `diagnostic.rs` is being edited by
the agent adding `RA015`/`RA016` right now, so any edit here would race. Recommended:
delete ", or split the inner markup into a separate `.tsx` module the route does not
inline" and keep the rest, or replace it with a repair that names a construct this
milestone compiles. The `explain` for `RA015` is accurate and needs nothing.

---

## RA001 — `RA001_UNTRACKED_READ`

`message`: `` `{n}()` is read in the body of component `{Page}`, outside any tracking
scope; the read runs once and nothing re-runs it `` (`rules.rs:562`).

### Claim by claim

| Claim | Verdict | Evidence |
|---|---|---|
| A read in the component body, outside any tracking scope, is flagged | **true** | `const double = n() * 2; return <div>{double}</div>` → RA001 at 5:18. |
| The read runs once and nothing re-runs it | **true** | The component body is `fn`-called once: `mount` calls `component({})` once (`runtime.js:622`). No effect owns the read, so no write can queue a re-run. |
| A read whose nearest function is *not* the component body is not this rule | **true** | `<button onClick={() => n()}>` → exit 0. `setTimeout(() => n(), 10)` in the body → exit 0. `effect(() => { n() })` → exit 0. |
| `derived`, `effect`, `resource`, a JSX slot are tracking scopes, so a read there is not flagged | **true** | `derived(() => n()*2)` → exit 0; `effect(() => { n() })` → exit 0; `resource(() => n())` → exit 0; `{n()}` in a JSX slot → exit 0. |

### `fix`: "Wrap the read in a tracking scope — `derived(() => count() * 2)`, an `effect(...)`, a `resource(...)`, or a JSX slot — or call it inside an event handler."

| Sub-claim | Verdict | Evidence |
|---|---|---|
| `derived(...)` is legal advice | **true** | Applied verbatim: exit 0. |
| `effect(...)` is legal advice | **true** | Applied: exit 0. But see the note below — it trades RA001 for a possible RA002. |
| `resource(...)` is legal advice | **true** | Applied: exit 0. |
| "a JSX slot" is legal advice | **true** | Moving the read into `{...}` clears RA001 (`call.in_jsx_container` early-returns, `rules.rs:556-558`). |
| "call it inside an event handler" | **true** | Applied: exit 0. |

**Note, not a defect.** The fix offers `effect(...)` as a repair for a *read*. Applied
to a read that is followed by a write to the same signal inside that effect, the
result is an RA002 — which the fix text for RA009 deliberately avoids for the same
reason (`diagnostic.rs:291-292`: "Deliberately *not* 'move it into an effect'"). RA001's
fix does not carry that caveat. It is not false — the literal instruction "wrap the
read in an effect" does clear RA001 — but a reader can land in RA002. Recorded, not
edited: the sentence is true of the rule it belongs to, and narrowing it is a product
decision about how much cross-code advice a fix may carry.

No named construct from the forbidden list (`<For>`, `<Show>`, `<When>`, a component
tag, a JSX-valued attribute, a non-`runai` import, `[slug]`). Checked by regex over
the whole `fix_template` body: zero matches.

---

## RA002 — `RA002_EFFECT_WRITE`

`explain` (`diagnostic.rs:154`) and the skill body assert the runtime's mechanism.

### The load-bearing sentences

| Claim | Verdict | Evidence |
|---|---|---|
| "An effect re-runs for the signals it reads" | **true** | `effect` records deps and `flush` re-executes; measured: a same-value write does not re-run, a different-value write does. |
| "a write in its body executes as part of that run" | **true** | The write is a plain function call inside `e.fn()` (`runtime.js:186`). |
| "when the effect also reads the signal it writes, the write queues that same effect" | **true** | `propagate` queues subscribers; `effect` is a subscriber of what it read. |
| "asking it to run while it is already running does not loop: the runtime throws `effect cycle: an effect wrote a signal it reads`" | **true** | Ran it against the real runtime: `THREW: effect cycle: an effect wrote a signal it reads. Derive it instead, or move the write to an event handler (RA002).` The guard is `if (e.running) throw` (`runtime.js:167-171`). |
| "(and a wave limit throws when effects keep invalidating each other)" | **true** | `if (++waves > 100) throw new Error("effect cycle: effects keep invalidating each other…")` (`runtime.js:195-200`). |
| "A write of the value the signal already holds never notifies anyone — the setter bails out first — so it neither re-runs nor throws" | **true** | `if (Object.is(next, node.value)) return` (`runtime.js:246`) runs before `propagate`. Measured: `same-value write runs: 1` (no re-run), no throw. |

**The specific mechanism the project's history got wrong is absent.** There is no
"this creates a loop" / "schedules the next run" / "each run schedules" language.
`RA002_FALSE_CLAIMS` (`diagnostic.rs:80`) lists those four phrasings and a test
asserts the `explain` and the skill body contain none of them
(`diagnostic.rs:876-880`). Verified by reading the assertion and by reading the text:
the words do not appear.

### `fix`: "Derive the value instead of writing it, or move the write into the event handler that causes the change, and let the effect only read what it needs."

| Sub-claim | Verdict | Evidence |
|---|---|---|
| "Derive the value instead of writing it" | **true** | Replacing `effect(() => { n(n()+1) })` with `derived(() => n()+1)` → exit 0. |
| "move the write into the event handler" | **true** | The same write in `onClick` → exit 0 (nearest function is the handler, `rules.rs:547`). |
| No forbidden construct | **true** | Regex: zero `<For>`/`<Show>`/`<When>`/component-tag/`[slug]` in the fix. |

---

## RA006 — `RA006_UNANNOTATED_PROPS`

`explain`: "missing an explicit type annotation, or the component takes no parameter
at all."

| Claim | Verdict | Evidence |
|---|---|---|
| Missing annotation is this rule | **true** | `function Page(props) {}` → RA006, message quotes the exact signature. |
| No parameter at all is this rule | **true** | `function Page() {}` → RA006 (`takes no props, but does not declare that in a type`). |
| "Props are the only way data enters a component" | **true** | The emitter builds one `props` parameter; `mount` calls `component({})` with nothing else (`runtime.js:622`); `prerender` writes `render()` with no arguments (`prerender.rs:106`). |
| `fix`: "With no props, write (props: Record<never, never>)." | **true** | Applied verbatim: exit 0. This is the project invariant in `AGENTS.md`, and the probe confirms it. |
| `message` quotes the signature to write, in the file's own declaration form | **true** | For `export default function Page()`, the message is `` write: export default function Page(props: Record<never, never>) { `` — the declaration form the file used, not a synthesized one. |

No forbidden construct in `fix`.

---

## RA007 — `RA007_AMBIGUOUS_JSX`

`explain` covers two cases; both were reproduced separately.

| Claim | Verdict | Evidence |
|---|---|---|
| "Either the parser rejected this file before it could be compiled" | **true** | `<div> }` → `ambiguous JSX: Unexpected token. Did you mean …` at 3:16, message is Oxc's own. |
| "or a line break sits between two inline elements" | **true** | `<p><b>a</b>\n<i>b</i></p>` → "a line break between two inline elements removes the space between them". |
| "Nothing downstream — lowering, analysis, codegen — can run on a tree the parser failed to produce" | **true** | `analyze` returns the parse failures as the whole answer (`entry.rs:58-63`). |
| "JSX deletes whitespace-only text that contains a newline, so the space the author saw in the editor is not the space the page shows" | **true** | `lower_text` (`ir.rs:922`) trims and drops empty lines when `has_line_break`; `has_line_break` is `text.contains(['\r','\n'])` (`source.rs:168-170`). |
| `fix`: "an unclosed element, an unterminated string inside an attribute, or adjacent sibling elements" | **true, each reproduced** | `<div><b>x</div>` → RA007; `<div class="a>x` → `Unterminated string`; `<b>a</b><i>b</i>` at return → `Adjacent JSX elements must be wrapped in an enclosing tag`. |
| `fix`: "wrap the siblings in one parent element" | **true** | Applied: `<div><b>a</b><i>b</i></div>` → exit 0. |
| `fix`: "write {" "} where you want the space" | **true** | Applied: `<p><b>a</b>{" "}<i>b</i></p>` → exit 0 in a **static** route too, so the advice is legal in both modes. |

**Note, not a defect, but it belongs to I1.** The `{" "}` spelling is accepted, but in
an island it compiles to a marker plus `_setText` rather than template text (audit I1).
The advice is *legal*, so the fix text is not false; it is expensive. Fixing the cost
is I1's IR constant-fold, not a text change.

---

## RA009 — `RA009_IMPLICIT_EFFECT`

The consequence sentence is a shared literal: `RA009_CONSEQUENCE`
(`diagnostic.rs:36`) = "The component body runs once, so this write cannot re-run the
component."

| Claim | Verdict | Evidence |
|---|---|---|
| "A signal is written during render: in the component body or in a JSX expression container" | **true** | Body write `n(2)` → RA009 at 5:3. JSX-slot write `{n(2)}` → RA009 at 5:16. |
| "The component body runs once, so this write cannot re-run the component" | **true** | `mount` calls `component({})` exactly once (`runtime.js:622`); `render()` is called once per build (`prerender.rs:106`). Nothing invokes a component body twice. |
| "Rendering must not have side effects" | **true** (a rule statement, not a mechanism claim) | Not falsifiable; it is the rule's premise. |
| The five false phrasings (`stale`, `re-trigger`, `re-run the render`, `updates no reader`, `already rendered`) are absent | **true** | `RA009_FALSE_CLAIMS` (`diagnostic.rs:57`) and the test at `:845-848` assert the shared sentence contains none. Read the text: none appear. |

`fix`: "move the write into the event handler that causes the change" — applied
(`onClick={() => n(2)}`) → exit 0. Legal.

The `fix` deliberately does **not** say "move it into an effect"
(`diagnostic.rs:291-292`), which would land the agent in RA002. That is correct: an
effect body is exactly where RA002 fires (`rules.rs:547-549`).

---

## RA010 — `RA010_STATIC_VIOLATION`

The `explain` names three costs and one naming subtlety. All four reproduced.

| Claim | Verdict | Evidence |
|---|---|---|
| A runtime feature (signal/derived/effect/resource/write) would silently do nothing | **true** | `"use static"` + `signal(1)` → RA010 "calls a runtime primitive… would run nothing". `derived`, `resource` likewise. |
| "an inline `on…` handler … is code the served page would run" | **true** | `<div onclick="alert(1)">` → "the served HTML carries it as an `on…` attribute and the browser evaluates it as JavaScript". |
| "a literal `<script>` element" is refused | **true** | `<script>x</script>` in a static route → "would load or run code the page promised not to ship". |
| "An `on…` attribute that is not a handler HTML defines (`once`, `onwards`) is refused as well" | **true** | `<div once="x">` in static → refused with "either dead or inline JavaScript". **In an island, `once="x"` → exit 0**, matching the sentence's own "an island still accepts them" in `handler_form` (`rules.rs:288-290`). |
| "the rule is a name prefix" | **true** | `handler_form` (`rules.rs:293-303`): lowercases, requires `starts_with("on")` and length > 2, no event list. |
| `fix`: "Change this file's \"use static\" directive to \"use island\"" | **true** | Applied to all four violation shapes (signal, handler, script, `once`): every one exits 0. |

One precision worth recording: the `explain` says "the rule is a name prefix", and the
code's *refusal* is the prefix but the `message`'s wording is chosen by a
secondary `is_known_handler_event` list (`rules.rs:329-333, 348`) that only picks
between "the browser evaluates it" and "either dead or inline JavaScript". The
`explain` does not claim the message is prefix-derived, so this is consistent — the
list can never widen or narrow the refusal (`rules.rs:324-328` says so and the probe
confirms it: `once` is refused, and `onclick` is refused, by the same prefix).

---

## RA011 — `RA011_BAD_ROUTE_EXPORT`

### `message` shapes, each reproduced

| Message | Fixture | Reproduced |
|---|---|---|
| `route file has no default-exported component` | a file with only `function Page` | yes |
| `route component Page does not return JSX directly` | `const v = 1; return v` | yes |
| `route component Page takes 2 parameters, but a route takes at most one annotated props parameter` | `function Page(a, b)` | yes |

### `explain` claims

| Claim | Verdict | Evidence |
|---|---|---|
| "is missing, is not a function this milestone can analyze" | **true** | `route_render` returns `None` for a non-function default (`jsx.rs:1722-1740`). |
| "takes parameters a route cannot be given" | **true** | Two params → refused. |
| "does not return JSX from the body" | **true** | `return v` → refused. |
| "**The router renders the default export as the route**" | **true but incomplete** | There is no object called a router. What actually invokes the component is `mount(component, root)` (`runtime.js:619-638`) for an island and the emitted `export function render()` for a static route (`prerender.rs:106-114`). "Router" is used consistently across the crate for "the thing that decides a route's served path and calls its component" (`build.rs:22`, `entry.rs:91`, `analyze/component.rs:17`, `skills.rs:121`), and the spec's §7.7 calls the mechanism a file-based router (§8 lists "file-based router with full-page navigation" as shipped). So the word is consistent with the project's own vocabulary, but a reader looking for a `Router` type will not find one. Recorded, **not edited**: replacing "the router" everywhere is a vocabulary change far larger than this audit, and the sentence is not false in the project's usage. |
| "only a direct `return <…/>` is recognized" | **true but incomplete** | A `return (<div/>)` (parenthesized) is accepted; so is a **concise arrow body** `(props) => <div/>` with no `return` at all. Reproduced: `export default (props: Record<never, never>) => <div>x</div>` → exit 0. The sentence's contrast is with *indirect* returns (`const v = 1; return v`), and both parenthesized and concise forms are "direct" in that sense — the code (`jsx.rs:1838-1864`) accepts a concise body via `jsx_expression`, which is a different AST path from `jsx_return`. The word "direct" carries it, barely. A stricter sentence would say "a `return` of a JSX expression, or a concise arrow body that is one". |
| "resolving what a returned value renders needs type information this compiler does not have yet" | **true** | Nothing in the crate resolves a returned identifier to its JSX; `jsx_return` matches only `Statement::ReturnStatement` with a JSX argument (`jsx.rs:1867-1878`). |

### Small textual inconsistency (recorded, not edited)

The "no default export" message ends **without** a trailing period, while the other
two RA011 messages end **with** one. Both reproduced from the binary:

```
error[RA011_BAD_ROUTE_EXPORT]: route file has no default-exported component
error[RA011_BAD_ROUTE_EXPORT]: route component `Page` does not return JSX directly
error[RA011_BAD_ROUTE_EXPORT]: route component `Page` takes 2 parameters, but a route takes at most one annotated `props` parameter
```

It is a punctuation inconsistency inside one code's three messages, not a false claim,
so it is out of scope for this audit's edit bar. Worth a one-line fix by whoever owns
`component.rs` next.

### `fix`

"Add a default export if it is missing. Replace the existing default export if it is
not a component, rather than declaring a second one. Either way it must take at most
one annotated `props` parameter and return JSX directly from its body."

| Sub-claim | Verdict | Evidence |
|---|---|---|
| Add a default export when missing | **true** | Adding one clears it. |
| Replace rather than declaring a second | **true** | Two default exports → RA014 `` `default` is exported twice ``, so a second one is refused — the advice prevents that. This is *not* the "would be a duplicate export" invented mechanism the project's history warns about, because ESM genuinely rejects it: `node --check` on `export const s = 1; export { s }` exits 1 with `SyntaxError: Duplicate export of 's'`. |
| "at most one annotated `props` parameter" | **true** | Two params refused; one untyped refused (RA006). |
| "return JSX directly from its body" | **true, same caveat as `explain`** | Concise arrow body qualifies. |

No forbidden construct in `fix`.

---

## RA012 — `RA012_MISSING_MODE`

| Claim | Verdict | Evidence |
|---|---|---|
| "The file does not open with a `\"use static\"` or `\"use island\"` directive" | **true** | No directive → RA012 at 1:1. |
| "inferring it would hide the decision that controls the 0 KB guarantee, so it is never inferred" | **true** | `detect_mode` returns `Err(RA012)` rather than a default (`source.rs:47-51`); no caller substitutes a mode. |
| `fix`: "as the first statement of the file" | **true** | A directive after `const x = 1` → RA012 (`has_line_break`/prologue logic requires the first statement position, `source.rs:139-151`). |
| `fix`: "Single and double quotes are both accepted" | **true** | `'use static'` → exit 0. |
| `fix`: "escapes such as \"use\\x20static\" are not" | **true** | `"use\x20static"` → RA012. |

No forbidden construct in `fix`.

---

## RA014 — `RA014_NOT_SUPPORTED_YET`

The `explain` claims **five families**. That claim is a table of contents, and it is
where the project's stated worst-defect class lives, so each family gets a
reproduction.

### The five families, each reproduced

| Family (as the `explain` names it) | Reproduced | Evidence |
|---|---|---|
| 1. "a JSX construct this milestone cannot lower (a component tag, a spread, a fragment root, and so on)" | **true** | `<Child/>` → "cannot lower a capitalized tag"; `{...props}` → "cannot lower a spread attribute"; `<></>` at return → "cannot lower a fragment as the route's returned JSX". |
| 1b. "four emit-time shapes": children in a void element, duplicate attribute, early `return`, `async`/generator | **true, all four** | `<br>x</br>` → void-with-children; `<div class="a" class="b">` → duplicate attribute; `if (props) { return <b/> }` → second JSX return; `async function Page` → async message; `function* Page` → generator message. |
| 1c. "markup the HTML parser would reshape": `<p>` holding block, same-kind nesting, content model, `<a>` in `<a>`, `<form>` in `<form>`, list item with no list, required ancestor, text-only binding in an island | **true, each reproduced** | `<p><div>` → refused; `<button><button>` → refused; `<table>t</table>` → foster-parenting refusal; `<div><a><a>` → refused; `<form><form>` → refused; `<div><li>` → refused; `<div><option>` → required-ancestor refusal; `<title>{n()}</title>` → RCDATA refusal. |
| 2. "a module the emitted file cannot resolve or write": non-`runai` import, default/namespace `runai` import, `import x = require`, `export =`, missing named export, re-export, duplicate export | **true, each reproduced** | `import { x } from "./other"` → refused; `import runai from "runai"` → refused; `import * as r from "runai"` → refused; `import y = require("./y")` → refused; `export = x` → refused; `import { _a } from "runai"` → refused listing the real exports; `export { owner } from "runai"` → refused; `export const s` + `export { s }` → refused. |
| 3. "a value the generated module cannot see": a `"runai"` name or the component's name in static markup/body/module scope, or the route parameter in static markup/body, or at module scope | **true, each reproduced** | static `{signal}` → refused; static `const v = Page` → refused; static `const v = owner` → refused; static `{(props as any).x}` → refused; `const v = props` at module scope in **both** modes → refused. |
| 4. "A `declare` statement is refused … because type stripping erases it" | **true** | `declare const x: number` → "erased before the route is emitted (`strip_types` removes it)". |
| 5. "the emitted module's own names: a declaration, export name, or reference to `_esc`, `_attr`, or `render`" | **true, but the list is mode-dependent and the sentence gives only the static triple** | See below. |

### Family 5: the sentence names three names, the code reserves two different sets

This is the one claim in the whole set that is **true but incomplete in a way that
matters to a reader**, so it gets a full record.

`explain` and `fix` both say the reserved names are "`_esc`, `_attr`, or `render`".
Those are `PRERENDER_NAMES` (`ir.rs:144`) — the **static** emitter's names. The island
emitter's reserved set is different: `ISLAND_LOCALS` (`ir.rs:212`) plus the used
helpers plus the import locals. Reproduced:

| Fixture | Mode | Result | What the message says |
|---|---|---|---|
| `const _esc = 1` in the body | static | **refused** | "(`_esc`, `_attr`, `render`)" |
| `const _esc = 1` in the body | island | **accepted** (exit 0) | — |
| `const _c = 1` in the body | island | **refused** | "(`_a`, `_c`, `_el`, `_t0`, `mount`, `template`)" |
| `const mount = 1` | island | **refused** | includes `mount`, `template` |
| `const _setText = 1` **with a dynamic text binding** | island | **refused** | list includes `_setText` |
| `const _setText = 1` **with no dynamic text** | island | **accepted** | the helper is only reserved when the artifact calls it |
| `const _c = 1` | static | **accepted** | — |

So the *per-site `message`* is correct and mode-aware: it lists exactly the set that
applies (see `reserved_names`, `emitted_names.rs:693-720`). The **`explain` and `fix`**
name only the static triple, which under-describes the island case by six names. The
`message` is what an agent acts on, and it is right; the `explain` is a summary that
is accurate for static and silent about island.

**Not edited, and here is the reasoning.** Making `explain` list both sets would put a
mode-dependent list in a `&'static str` that has no access to the mode — the same
structural reason `RA009` uses a macro. It would also make the summary stale every
time a helper is added to `used_helpers` (`client.rs:241-267`). The honest fix is a
sentence that says the list depends on the mode and to read `message`, which the
current sentence would need to become. That is a design change to the diagnostic, not
a correction of a false claim, so it is reported for a decision.

### `fix` — every named repair, applied

The `fix_template` is long; each distinct repair was applied verbatim where a fixture
could be constructed.

| Repair sentence | Legal when applied? | Evidence |
|---|---|---|
| "inline the value or import it from `\"runai\"`" | **yes** | A non-`runai` import replaced with a `runai` import → exit 0. |
| "a re-export whose source is `\"runai\"` becomes `import { owner } from \"runai\"` with the name used here" | **yes** | `import { owner } from "runai"; const v = owner` → exit 0. |
| "a default or namespace `\"runai\"` import … rewrite it as named imports" | **yes** | `import { owner } from "runai"` → exit 0. |
| "use one of the names the message lists" | **yes** | The message lists the 13 real exports (verified against `RUNTIME_EXPORTS`, `ir.rs:190`, and a test asserts the two agree, `entry.rs:1770-1774`). |
| "or import an `index.d.ts` type with `import { type … }`" | **yes** | `import { type Accessor } from "runai"` → exit 0. |
| "write `import x = require(…)` as an `import`, and `export =` as `export default` or a named export" | **yes** | Both replacements analyse clean. |
| "write the parameter as `(props: Record<never, never>)`" | **yes** | exit 0. |
| "change the directive to `\"use island\"` for the first two, and replace the parameter reference with a literal value" | **yes** | Static signal/component reference → island clears both. Parameter → literal clears it. |
| "write the spread out" | **yes** | `<div title="t" class="c">` replacing `{...props}` → exit 0. |
| "wrap a root fragment in one parent element" | **yes** | `<main><b/><i/></main>` → exit 0. |
| "render a runtime primitive's markup directly" | **yes** | `<For>`/`<Show>`/`<When>` replaced by literal markup → exit 0. |
| "keep exactly one JSX return" | **yes** | One return → exit 0. |
| "For children inside a void element, move them out" | **yes** | Removing the child → exit 0. |
| "For a duplicate attribute, keep exactly one" | **yes** | One attribute → exit 0. |
| "return one element from the component" (early return) | **yes** | One wrapped return → exit 0. |
| "For an `async` or generator route component, remove the modifier" | **yes** | Removing `async`/`*` → exit 0. |
| "rename it" (reserved name) | **yes** | Renaming clears it. |
| "delete one of the two exports or export one under a different name" | **yes** | One export → exit 0. |
| "For a `declare` statement, replace it with a value this file defines" | **yes** | A real `const` → exit 0. |

**Crucially, the fix names no construct this milestone refuses.** Regex over the whole
`fix_template`: zero occurrences of `<For>`, `<Show>`, `<When>`, any capitalized
`<Tag>`, any JSX used as advice inside `{…}`, any `[slug]`/`[param]` segment, or any
non-`runai` import as the *repair target*. The `{…}` forms that do appear are the type
`Record<never, never>` and the literal `import { type … }`, both of which an agent can
type and the compiler accepts.

### The specific invented mechanism this project was burned by

`explain` says of a duplicate export: "a name exported twice … is refused because an ES
module may export a name once, so the module would be a syntax error rather than a
page." That is the claim that was *false at some point* per the audit's history
("that would be a duplicate export"). It is **true now**, and independently
verifiable:

```sh
$ printf 'export const s = 1\nexport { s }\n' > /tmp/opencode/dup.mjs
$ node --check /tmp/opencode/dup.mjs
SyntaxError: Duplicate export of 's'
$ echo $?
1
```

So the mechanism is real ESM semantics, not an invented one.

---

## Cross-cutting checks

### `doc` paths

All nine codes' `doc` is `node_modules/runai/skills/<code>.md` (`doc_path`,
`diagnostic.rs:138`). Verified end to end, not just read:

```sh
$ mkdir /tmp/opencode/skilltest && cd /tmp/opencode/skilltest
$ runai skills
wrote 9 skill files to node_modules/runai/skills
$ find . -name '*.md' | head -3
./node_modules/runai/skills/RA014_NOT_SUPPORTED_YET.md
./node_modules/runai/skills/RA012_MISSING_MODE.md
./node_modules/runai/skills/RA011_BAD_ROUTE_EXPORT.md
```

The command's default `--out` is exactly `DEFAULT_OUT = "node_modules/runai/skills"`
(`runai_cli/src/skills.rs:27`), so a bare `runai skills` writes where `doc` points.
The nine files also ship in the package (`packages/runai/skills/`, and `"skills"` is in
`packages/runai/package.json`'s `files`). **Every `doc` path is correct.**

### Forbidden constructs across all nine `fix_template`s

Regex over the concatenated fix text:

- Capitalized JSX tags (a component tag or `<For>`/`<Show>`/`<When>`): **0**
- `[slug]` or `[param]` route segments: **0**
- A non-`runai` module named as the *repair target* (as opposed to named as the thing
  to remove): **0** — `RA014`'s one mention of `import { type … }` is a `"runai"` type
  import, which is legal.

### What could not be verified

| Item | Why | Code that would need reading |
|---|---|---|
| RA011's "the router" is a vocabulary claim, not a mechanism claim | No `Router` type exists; the word is the crate's shorthand for `mount`/`render`. Confirming there is no other routing layer means reading `build.rs`'s route-set assembly end to end. | `runai_cli/src/build.rs` route-set naming and output-path mapping. |
| Whether `effect(...)` as an RA001 repair ever lands in RA002 *in practice* | The fixture that would show it needs a write inside the same effect, which is RA002's own fixture. The claim "the advice can land you in RA002" is reasoned, not measured. | `analyze/rules.rs:547-549` (effect scope wins over component scope). |
| The `Severity::Warning` path (audit M10) | Not a diagnostic-text claim; no warning is constructed outside tests. | `diagnostic.rs:671`, `check.rs:124`, `build.rs:898`. |

## Summary

| Code | `explain` | `fix` | `doc` |
|---|---|---|---|
| RA001 | **true** (one cross-code caveat recorded) | **true**, all named repairs applied | correct |
| RA002 | **true**, every mechanism claim reproduced against the runtime | **true** | correct |
| RA006 | **true** | **true**, applied verbatim | correct |
| RA007 | **true**, both branches reproduced | **true**, all three syntax causes plus the whitespace repair applied | correct |
| RA009 | **true**, shared sentence verified by test and by reading | **true** | correct |
| RA010 | **true**, all three costs and the prefix rule reproduced | **true**, applied to all four shapes | correct |
| RA011 | **true but incomplete** ("the router"; "only a direct return") | **true** | correct |
| RA012 | **true** | **true**, both quote styles and the escape case reproduced | correct |
| RA014 | **true**, all five families reproduced; family 5's name list is static-only in the summary, correct in `message` | **true**, eighteen distinct repairs applied | correct |

**Zero `FALSE` verdicts across the nine codes named in the brief.** Two
**true but incomplete** (RA011's two phrases) and one **true but mode-blind summary**
(RA014 family 5). No text of the nine was edited, because none of the three is false,
and the project's rule is that a wrong edit is worse than a finding.

**One `FALSE`-class defect found outside the nine:** `RA015`'s fix offers "split the
inner markup into a separate `.tsx` module", which this milestone cannot compile — it
moves the agent from one refusal to two RA014s. Reported at the top; not edited
because the code that carries it is being written by another agent in this same round.

`RA016`, the other code added mid-round, was spot-checked for the same class and is
clean: its `fix` names three repairs and all three are accepted —
a string literal (`class="hello"` → rc 0), a template literal with no substitutions
(`` class={`hello`} `` → rc 0), and a constant the file defines (`const cls = "hello"`
then `class={cls}` → rc 0). Its precondition and its message were reproduced
(`class={() => "hello"}` in a static route → `RA016_STATIC_FUNCTION_ATTRIBUTE`; the
same source in an island → rc 0, which is exactly the mode divergence the `explain`
describes).
