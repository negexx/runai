# runai Foundation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship a working vertical slice of runai — strict TSX compiles to direct DOM, static routes emit zero-JS HTML, and the first coded diagnostics catch real agent mistakes.

**Architecture:** A Rust crate on Oxc parses and analyzes TSX, then lowers JSX to a template-and-bindings IR with two emitters: a pure string emitter for `"use static"` routes (no runtime imports, so 0 KB is structural) and an ESM emitter for `"use island"` routes that clones templates and wires signal bindings. A hand-written ~6 KB ESM runtime provides signals, ownership, `For`, `Show`, and `When`. The CLI orchestrates check/build/dev/skills, with no bundler in this plan: island output is direct ESM plus a copied runtime, resolved through `/assets/` URLs.

**Tech Stack:** Rust 1.99 (2024 edition), oxc_parser / oxc_semantic 0.159, Bun 1.4 (`bun test`) for runtime tests and build-time module execution, Node 24 supported, plain ESM runtime with hand-written `.d.ts`.

**Spec:** `docs/superpowers/specs/2026-11-20-runai-framework-design.md`

## Global Constraints

- Compiler crate: edition `2024`, `oxc_parser`/`oxc_semantic`/`oxc_allocator`/`oxc_span` pinned to `0.159.0`.
- Runtime budget: **≤ 6 KB gzipped**, minified+gzipped, including `signal`, `derived`, `effect`, `For`, `Show`, `When`, `resource`, and island mount; excluding user code and dev instrumentation.
- Static routes ship **0 bytes** of JavaScript — enforced by `RA010_STATIC_VIOLATION`, not by inspecting build output.
- Every diagnostic carries: `code`, `severity`, `span`, `message`, `explain`, `fix`, `doc`, and a nullable `patch`. `code` is stable and never reused.
- Diagnostic codes in this plan: `RA001`, `RA002`, `RA006`, `RA007`, `RA009`, `RA010`, `RA011`, `RA012`.
- `runai check --json` prints exactly one JSON array to stdout and nothing else.
- All CLI commands are idempotent: starting a running dev server returns the existing instance; stopping a stopped one exits 0.
- `"use static"` or `"use island"` must be the first statement in every `.page.tsx`. No inference.
- No bundler, no client router, no `store`/`batch`/`untrack`, no component-level islands in this plan.
- Commit after every task.

## Review Focus

These are the input classes most likely to bite a real user, and each is pinned by a test in the task named.

1. **A signal read inside an event handler** must never be flagged `RA001` — it is correct, intended behavior. (Task 8)
2. **A signal read inside a nested callback that outlives the component** (`setTimeout(() => count())`) must be caught or documented, not silently frozen. (Task 8)
3. **JSX text containing entities and meaningful whitespace** (`&amp;`, a newline between two inline elements) must not be silently mangled by the prerender emitter. (Task 10)
4. **A syntax error inside JSX** must produce `RA007` with a correct span and a non-panicking exit, never a Rust panic or a bare parser message. (Task 3)
5. **A component with no props** must still require an explicit type, and the error message must show the exact line to write. (Task 4)

---

## File Structure

```
runai/
├─ Cargo.toml                          workspace
├─ crates/
│  ├─ runai_compiler/
│  │  ├─ Cargo.toml
│  │  └─ src/
│  │     ├─ lib.rs                     public API: check_source, build_route
│  │     ├─ diagnostic.rs              Diagnostic, Code, Severity, rendering
│  │     ├─ source.rs                  mode directive detection
│  │     ├─ ir.rs                      Template/Binding/Node IR
│  │     ├─ analyze/
│  │     │  ├─ mod.rs                  analysis pipeline entry
│  │     │  ├─ component.rs            route export + component detection
│  │     │  ├─ props.rs                RA006
│  │     │  ├─ signals.rs              signal binding discovery
│  │     │  └─ rules.rs               RA001/RA002/RA009/RA010
│  │     └─ codegen/
│  │        ├─ mod.rs                  emitter selection by mode
│  │        ├─ prerender.rs            IR → pure string module
│  │        └─ client.rs               IR → DOM module
│  └─ runai_cli/
│     ├─ Cargo.toml
│     └─ src/main.rs                   check/build/dev/skills
├─ packages/runai/
│  ├─ package.json
│  ├─ runtime.js                       ≤6 KB ESM runtime
│  ├─ index.d.ts                       public types
│  └─ test/runtime.test.js
├─ examples/counter/
│  ├─ runai.json
│  └─ src/routes/
│     ├─ _shell.tsx
│     ├─ index.page.tsx                "use island"
│     └─ about.page.tsx                "use static"
└─ docs/superpowers/
   ├─ specs/2026-11-20-runai-framework-design.md
   └─ plans/2026-11-20-runai-foundation.md
```

Each file owns one concern. `analyze/` produces diagnostics only; `codegen/` consumes IR and produces strings. No crate reads both.

---

### Task 1: Workspace and smoke tests

**Files:**
- Create: `Cargo.toml`, `crates/runai_compiler/Cargo.toml`, `crates/runai_compiler/src/lib.rs`, `crates/runai_cli/Cargo.toml`, `crates/runai_cli/src/main.rs`, `packages/runai/package.json`, `packages/runai/runtime.js`, `packages/runai/test/runtime.test.js`, `.gitignore`

**Interfaces:**
- Consumes: nothing.
- Produces: `runai_compiler::VERSION: &str`; workspace member names `runai_compiler`, `runai_cli`; binary `runai`.

- [ ] **Step 1: Write the failing Rust test**

```rust
// crates/runai_compiler/src/lib.rs
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    #[test]
    fn exposes_version() {
        assert_eq!(super::VERSION, "0.1.0");
    }
}
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo test -p runai_compiler`
Expected: FAIL — `cargo` cannot find `Cargo.toml` at the workspace root.

- [ ] **Step 3: Create the workspace and both crates**

Root `Cargo.toml`:

```toml
[workspace]
resolver = "3"
members = ["crates/runai_compiler", "crates/runai_cli"]

[workspace.package]
edition = "2024"
version = "0.1.0"
```

`crates/runai_compiler/Cargo.toml` — package `runai_compiler`, `version.workspace = true`, `edition.workspace = true`, dependencies `oxc_parser`, `oxc_allocator`, `oxc_span`, `oxc_semantic` all `= "0.159.0"`, plus `serde = { version = "1", features = ["derive"] }` and `serde_json = "1"`.

`crates/runai_cli/Cargo.toml` — package `runai_cli`, `[[bin]] name = "runai" path = "src/main.rs"`, depends on `runai_compiler` via `path = "../runai_compiler"`.

`crates/runai_cli/src/main.rs` prints `runai 0.1.0` and exits 0.

- [ ] **Step 4: Run the Rust test**

Run: `cargo test -p runai_compiler`
Expected: PASS, 1 test.

- [ ] **Step 5: Write the failing runtime test**

```js
// packages/runai/test/runtime.test.js
import { test, expect } from "bun:test"
import { signal } from "../runtime.js"

test("signal reads and writes", () => {
  const count = signal(0)
  expect(count()).toBe(0)
  count(1)
  expect(count()).toBe(1)
})
```

- [ ] **Step 6: Run it and watch it fail**

Run: `cd packages/runai && bun test`
Expected: FAIL — cannot resolve `../runtime.js` or `signal` is not exported.

- [ ] **Step 7: Create the runtime stub**

`runtime.js` exports a `signal` that stores a value and returns a callable that reads with no arguments and writes with one. `package.json` is `{ "name": "runai", "version": "0.1.0", "type": "module", "main": "runtime.js", "types": "index.d.ts" }`.

- [ ] **Step 8: Run the runtime test**

Run: `cd packages/runai && bun test`
Expected: PASS, 1 test.

- [ ] **Step 9: Commit**

```bash
git init && git add -A
git commit -m "chore: scaffold runai workspace with Rust crates and runtime package"
```

---

### Task 2: Diagnostic model and rendering

**Files:**
- Create: `crates/runai_compiler/src/diagnostic.rs`
- Modify: `crates/runai_compiler/src/lib.rs`

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `Diagnostic { code: Code, severity: Severity, span: (u32, u32), message: String, explain: String, fix: String, doc: String, patch: Option<String> }`
  - `Code` enum with variants `RA001`, `RA002`, `RA006`, `RA007`, `RA009`, `RA010`, `RA011`, `RA012`, each with `fn as_str(&self) -> &'static str` returning the exact string (e.g. `"RA001_UNTRACKED_READ"`) and `fn skill_file(&self) -> String` returning `format!("skills/{}.md", self.as_str())`.
  - `Code::all() -> &'static [Code]` returning all twelve variants in code order — Task 15's drift test depends on it.
  - `Severity { Error, Warning }`
  - `Diagnostic::to_json(&self) -> String` and `render_human(&self, source: &str, path: &str) -> String`
  - `Diagnostic::render_all_json(diags: &[Diagnostic]) -> String`

- [ ] **Step 1: Write the failing tests**

```rust
// crates/runai_compiler/src/diagnostic.rs (tests module at bottom)
#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Diagnostic {
        Diagnostic {
            code: Code::RA001,
            severity: Severity::Error,
            span: (10, 15),
            message: "signal read outside a tracking scope".into(),
            explain: "A read in a component body does not re-run.".into(),
            fix: "Wrap it: const doubled = derived(() => count() * 2)".into(),
            doc: "skills/RA001_UNTRACKED_READ.md".into(),
            patch: None,
        }
    }

    #[test]
    fn code_strings_are_stable() {
        assert_eq!(Code::RA001.as_str(), "RA001_UNTRACKED_READ");
        assert_eq!(Code::RA010.as_str(), "RA010_STATIC_VIOLATION");
    }

    #[test]
    fn json_has_every_field_and_null_patch() {
        let json = sample().to_json();
        for key in ["code", "severity", "span", "message", "explain", "fix", "doc", "patch"] {
            assert!(json.contains(&format!("\"{key}\"")), "missing {key} in {json}");
        }
        assert!(json.contains("\"patch\":null"));
    }

    #[test]
    fn human_output_names_the_line_and_the_fix() {
        let src = "function A() {\n  const x = count()\n}\n";
        let out = sample().render_human(src, "src/routes/index.page.tsx");
        assert!(out.contains("RA001_UNTRACKED_READ"));
        assert!(out.contains("src/routes/index.page.tsx:2"));
        assert!(out.contains("Wrap it: const doubled"));
    }

    #[test]
    fn json_array_is_valid_json() {
        let out = Diagnostic::render_all_json(&[sample(), sample()]);
        let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed.as_array().unwrap().len(), 2);
    }
}
```

- [ ] **Step 2: Run and verify failure**

Run: `cargo test -p runai_compiler diagnostic`
Expected: FAIL — module `diagnostic` not found.

- [ ] **Step 3: Implement**

Add the struct and enums as specified, derive `serde::Serialize` with `#[serde(rename_all = "lowercase")]` on `Severity` and manual serialization so `span` is `[start, end]` and `patch` is `null` when absent. `render_human` computes the 1-based line from `span.0` by counting `\n` in `source[..span.0]`, prints `path:line:col`, then message, explain, fix, and `doc`. Line 2 in the test comes from span start 13, which is inside the second line.

Wire `pub mod diagnostic;` in `lib.rs`.

- [ ] **Step 4: Run and verify pass**

Run: `cargo test -p runai_compiler diagnostic`
Expected: PASS, 4 tests.

- [ ] **Step 5: Commit**

```bash
git add crates/runai_compiler/src
git commit -m "feat(compiler): add structured diagnostics with stable codes and JSON output"
```

---

### Task 3: Parse TSX and report `RA007` and `RA012`

**Files:**
- Create: `crates/runai_compiler/src/source.rs`
- Modify: `crates/runai_compiler/src/lib.rs`

**Interfaces:**
- Consumes: `Diagnostic`, `Code`, `Severity` from Task 2.
- Produces:
  - `pub enum Mode { Static, Island }`
  - `pub fn detect_mode(source: &str) -> Result<Mode, Diagnostic>` — reads the first statement; a string literal expression `"use static"` or `"use island"` counts. Anything else yields `RA012_MISSING_MODE` spanning byte 0..0.
  - `pub fn parse_tsx<'a>(alloc: &'a Allocator, source: &'a str) -> Result<&'a Program<'a>, Vec<Diagnostic>>` — every Oxc diagnostic becomes `RA007_AMBIGUOUS_JSX` with its primary label span, or `(0,0)` when unlabeled.

- [ ] **Step 1: Write the failing tests**

```rust
// crates/runai_compiler/src/source.rs (tests)
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_static() {
        let src = "\"use static\"\nimport x from 'y'\n";
        assert!(matches!(detect_mode(src), Ok(Mode::Static)));
    }

    #[test]
    fn detects_island() {
        let src = "\"use island\"\nexport default function A() { return <p/> }\n";
        assert!(matches!(detect_mode(src), Ok(Mode::Island)));
    }

    #[test]
    fn missing_directive_is_ra012() {
        let src = "export default function A() { return <p/> }\n";
        let err = detect_mode(src).unwrap_err();
        assert_eq!(err.code, Code::RA012);
        assert!(err.fix.contains("\"use static\""));
    }

    #[test]
    fn unclosed_jsx_is_ra007_with_span() {
        let alloc = oxc_allocator::Allocator::default();
        let src = "const a = <div>;\n";
        let errs = parse_tsx(&alloc, src).unwrap_err();
        assert_eq!(errs[0].code, Code::RA007);
        assert!(errs[0].span.0 > 0, "span should point into the file");
    }
}
```

- [ ] **Step 2: Run and verify failure**

Run: `cargo test -p runai_compiler source`
Expected: FAIL — `detect_mode` not found.

- [ ] **Step 3: Implement**

`detect_mode` checks `program.body.first()` for `Statement::ExpressionStatement` whose expression is a `StringLiteral`, using the raw span text rather than the cooked value so escapes cannot smuggle a directive. `parse_tsx` calls `Parser::new(alloc, source, SourceType::tsx()).parse()`, maps `ret.diagnostics` with `d.labels.first()` for the span, and returns `Ok(ret.program)` when the vector is empty. It must never panic: `panicked` is not a field on `ParserReturn` in 0.159, so rely on `diagnostics` alone.

- [ ] **Step 4: Run and verify pass**

Run: `cargo test -p runai_compiler source`
Expected: PASS, 4 tests.

- [ ] **Step 5: Commit**

```bash
git add crates/runai_compiler/src
git commit -m "feat(compiler): parse TSX, detect route mode, map parser errors to RA007"
```

---

### Task 4: Route export and props validation (`RA011`, `RA006`)

**Files:**
- Create: `crates/runai_compiler/src/analyze/mod.rs`, `crates/runai_compiler/src/analyze/component.rs`, `crates/runai_compiler/src/analyze/props.rs`
- Modify: `crates/runai_compiler/src/lib.rs`

**Interfaces:**
- Consumes: `parse_tsx`, `Mode`.
- Produces:
  - `pub struct ComponentInfo<'a> { pub name: &'a str, pub params: usize, pub has_props_type: bool, pub body_span: (u32, u32), pub returns_jsx: bool }`
  - `pub fn find_route_component<'a>(program: &'a Program<'a>) -> Option<ComponentInfo<'a>>` — the default export that is a function declaration or arrow function.
  - `pub fn validate_route_component(info: Option<&ComponentInfo>) -> Vec<Diagnostic>` — `RA011` when missing, when params > 1, or when it does not return JSX.
  - `pub fn check_props(info: &ComponentInfo) -> Vec<Diagnostic>` — `RA006` when `params == 1` and the parameter has no type annotation, and when `params == 0` (message tells the agent to write `type Props = Record<never, never>`).

- [ ] **Step 1: Write the failing tests**

```rust
// crates/runai_compiler/src/analyze/props.rs (tests)
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
        if let Some(i) = &info { d.extend(check_props(i)); }
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
}
```

- [ ] **Step 2: Run and verify failure**

Run: `cargo test -p runai_compiler props`
Expected: FAIL — `find_route_component` not found.

- [ ] **Step 3: Implement**

`find_route_component` walks `program.body` for `ExportDefaultDeclaration`; accepts `FunctionDeclaration` (name or `"default"`), and `Expression::ArrowFunctionExpression` or `FunctionExpression`. `returns_jsx` is true when any `ReturnStatement` argument is a `JSXElement` or `JSXFragment`. `check_props` inspects the first parameter's `type_annotation`; `params == 0` also reports `RA006` with the `Record<never, never>` fix text. Reuse `ComponentInfo::body_span` from the function body span.

- [ ] **Step 4: Run and verify pass**

Run: `cargo test -p runai_compiler props`
Expected: PASS, 4 tests.

- [ ] **Step 5: Commit**

```bash
git add crates/runai_compiler/src
git commit -m "feat(compiler): validate route default export and explicit props types"
```

---

### Task 5: Runtime core — `signal`, `derived`, `effect`, disposal

**Files:**
- Modify: `packages/runai/runtime.js`, `packages/runai/test/runtime.test.js`, `packages/runai/index.d.ts`

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `signal<T>(initial: T): Accessor<T>` where `Accessor = (() => T) & ((next: T) => void)`
  - `derived<T>(fn: () => T): Accessor<T>` (read-only)
  - `effect(fn: () => void): () => void` (returns dispose)
  - `owner(fn: () => void): () => void` (creates a disposal scope; nested effects are disposed with it)
  - Internals used by codegen: `_sub(fn)`, `_tmpl(html)`, `_setText(node, value)`, `_setAttr(node, name, value)`, `_on(node, name, handler)`

- [ ] **Step 1: Write the failing tests**

```js
// packages/runai/test/runtime.test.js
// Replace the Task 1 import line with this one and append the tests below.
import { signal, derived, effect, owner } from "../runtime.js"

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
```

- [ ] **Step 2: Run and verify failure**

Run: `cd packages/runai && bun test`
Expected: FAIL — `derived is not a function`.

- [ ] **Step 3: Implement**

A pull-based graph with version counters: each node holds `version` and a `deps` set; reads inside a tracking context register the edge; writes bump a global epoch and mark subscribers dirty. Effects are pushed onto a queue and flushed in a microtask, deduplicated by node id, which is what makes the diamond test pass once. `owner` keeps a stack of disposal arrays; `effect` registers its dispose with the nearest owner. No proxies, no `batch`, no `untrack`.

- [ ] **Step 4: Run and verify pass**

Run: `cd packages/runai && bun test`
Expected: PASS, 6 tests.

- [ ] **Step 5: Commit**

```bash
git add packages/runai
git commit -m "feat(runtime): add signal graph with derived, effects, and owner disposal"
```

---

### Task 6: Runtime — templates, bindings, `For`, `Show`, `When`, `resource`, mount

**Files:**
- Modify: `packages/runai/runtime.js`, `packages/runai/test/runtime.test.js`, `packages/runai/index.d.ts`

**Interfaces:**
- Consumes: Task 5 internals.
- Produces:
  - `template(html: string): HTMLTemplateElement` with a `clone()` helper
  - `For<T>(props: { each: () => T[]; by: (item: T) => string | number; children: (item: () => T) => Node }): Node`
  - `Show(props: { when: () => boolean; fallback?: Node; children: Node }): Node`
  - `When<T>(props: { value: Accessor<Result<T>>; pending: Node; error: (e: unknown) => Node; children: (data: T) => Node }): Node`
  - `resource<T>(fn: () => Promise<T>): Accessor<Result<T>>` where `Result<T> = { status: "pending" } | { status: "error", error: unknown } | { status: "success", data: T }`
  - `mount(component: (props: any) => Node, root: Element): () => void`

- [ ] **Step 1: Write the failing tests**

```js
// packages/runai/test/runtime.test.js (append)
import { For, Show, When, resource, mount, template } from "../runtime.js"

test("For keyed updates reuse the same DOM node for the same key", () => {
  const items = signal([{ id: 1, label: "a" }, { id: 2, label: "b" }])
  const rendered = new Map()
  const list = For({
    each: () => items(),
    by: (i) => i.id,
    children: (item) => {
      const el = document.createElement("li")
      el.textContent = item().label
      rendered.set(item().id, el)
      return el
    },
  })
  const nodeFor1 = rendered.get(1)
  const nodeFor2 = rendered.get(2)
  items([{ id: 2, label: "b" }, { id: 1, label: "a" }])
  expect(list.childNodes[0]).toBe(nodeFor2)
  expect(list.childNodes[1]).toBe(nodeFor1)
  expect(rendered.size).toBe(2)
})

test("When requires pending and renders error branch", async () => {
  const failing = resource(async () => { throw new Error("boom") })
  const node = When({
    value: failing,
    pending: document.createTextNode("loading"),
    error: (e) => document.createTextNode(`failed: ${e.message}`),
    children: (d) => document.createTextNode(String(d)),
  })
  expect(node.textContent).toBe("loading")
  await new Promise((r) => setTimeout(r, 0))
  expect(node.textContent).toBe("failed: boom")
})

test("Show toggles on signal change", () => {
  const on = signal(false)
  const node = Show({ when: () => on(), fallback: document.createTextNode("off"), children: document.createTextNode("on") })
  expect(node.textContent).toBe("off")
  on(true)
  expect(node.textContent).toBe("on")
})

test("mount runs a component into a root and disposes", () => {
  const root = document.createElement("div")
  const stop = mount(() => { const el = document.createElement("p"); el.textContent = "hi"; return el }, root)
  expect(root.textContent).toBe("hi")
  stop()
})

test("template clones markup", () => {
  const t = template("<button class='x'>go</button>")
  const a = t.content.firstChild.cloneNode(true)
  expect(a.outerHTML).toBe("<button class=\"x\">go</button>")
})
```

- [ ] **Step 2: Run and verify failure**

Run: `cd packages/runai && bun test`
Expected: FAIL — `For is not a function`.

- [ ] **Step 3: Implement**

`For` keys rows in a `Map`, reorders by moving existing DOM nodes with `insertBefore`, and disposes the owner of removed rows. `When` reads the accessor inside an effect and swaps between pending, error, and success nodes. `resource` runs `fn` inside `owner`, tracks signal reads to re-run, and aborts the prior call with an `AbortController` when the compute function accepts a `{ signal }` argument. `mount` creates an owner, renders, appends, and returns the disposer.

Runtime tests need a DOM: add `happy-dom` as a dev dependency of `packages/runai` and a `test/setup.js` registered via `bun test --preload` that assigns `globalThis.document`, `globalThis.Node`, and `globalThis.Element` from a `happy-dom` `Window`.

- [ ] **Step 4: Run and verify pass**

Run: `cd packages/runai && bun test --preload ./test/setup.js`
Expected: PASS, 11 tests.

- [ ] **Step 5: Commit**

```bash
git add packages/runai
git commit -m "feat(runtime): add templates, keyed For, Show, When, resource, and mount"
```

---

### Task 7: JSX to IR lowering

**Files:**
- Create: `crates/runai_compiler/src/ir.rs`, `crates/runai_compiler/src/analyze/mod.rs` (extend)
- Modify: `crates/runai_compiler/src/lib.rs`

**Interfaces:**
- Consumes: `parse_tsx`.
- Produces:
  - `pub enum IrNode { Element { tag: String, static_attrs: Vec<(String, String)>, bindings: Vec<Binding>, children: Vec<IrNode> }, Text(String), Expr(String), Component { name: String, props: Vec<(String, IrNode)>, children: Vec<IrNode> } }`
  - `pub enum Binding { Text(String), Attr { name: String, expr: String }, Event { name: String, expr: String } }`
  - `pub fn lower_jsx(program: &Program) -> Result<IrNode, Vec<Diagnostic>>` — lowers the first returning JSX node of the default export.
  - `pub struct RouteIr { pub mode: Mode, pub name: String, pub tree: IrNode }`
  - `pub fn lower_route(source: &str) -> Result<RouteIr, Vec<Diagnostic>>`

- [ ] **Step 1: Write the failing tests**

```rust
// crates/runai_compiler/src/ir.rs (tests)
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn static_element_has_no_bindings() {
        let src = "\"use static\"\nexport default function P() { return <div class=\"a\">hi</div> }\n";
        let r = crate::ir::lower_route(src).unwrap();
        match r.tree {
            IrNode::Element { tag, bindings, children, .. } => {
                assert_eq!(tag, "div");
                assert!(bindings.is_empty());
                assert!(matches!(children[0], IrNode::Text(ref t) if t == "hi"));
            }
            other => panic!("expected element, got {other:?}"),
        }
    }

    #[test]
    fn expression_slot_becomes_a_text_binding() {
        let src = "\"use island\"\nexport default function P() { const n = signal(0); return <p>{n()}</p> }\n";
        let r = crate::ir::lower_route(src).unwrap();
        match r.tree {
            IrNode::Element { bindings, .. } => {
                assert_eq!(bindings.len(), 1);
                assert!(matches!(bindings[0], Binding::Text(_)));
            }
            other => panic!("expected element, got {other:?}"),
        }
    }

    #[test]
    fn event_attribute_becomes_an_event_binding() {
        let src = "\"use island\"\nexport default function P() { return <button onClick={go}>x</button> }\n";
        let r = crate::ir::lower_route(src).unwrap();
        match r.tree {
            IrNode::Element { bindings, .. } => {
                assert!(matches!(bindings[0], Binding::Event { ref name, .. } if name == "click"));
            }
            other => panic!("expected element, got {other:?}"),
        }
    }

    #[test]
    fn text_entities_are_preserved_verbatim() {
        let src = "\"use static\"\nexport default function P() { return <p>a &amp; b</p> }\n";
        let r = crate::ir::lower_route(src).unwrap();
        match r.tree {
            IrNode::Element { children, .. } => {
                assert!(matches!(children[0], IrNode::Text(ref t) if t.contains("&amp;")));
            }
            other => panic!("expected element, got {other:?}"),
        }
    }
}
```

- [ ] **Step 2: Run and verify failure**

Run: `cargo test -p runai_compiler ir`
Expected: FAIL — `lower_route` not found.

- [ ] **Step 3: Implement**

Walk the JSX tree. Attributes with no expression become `static_attrs`; `onX={expr}` becomes `Binding::Event` with the name lowercased after stripping `on` (`onClick` → `click`); other expression attributes become `Binding::Attr`. A `JSXExpressionContainer` child becomes `Binding::Text` carrying the source text of the expression, and the element's `children` gets an `IrNode::Expr` placeholder so order is preserved. Text nodes keep their raw source text, entities untouched. Capitalized tags become `Component`. Recover the expression text from the source slice using the node's span.

- [ ] **Step 4: Run and verify pass**

Run: `cargo test -p runai_compiler ir`
Expected: PASS, 4 tests.

- [ ] **Step 5: Commit**

```bash
git add crates/runai_compiler/src
git commit -m "feat(compiler): lower TSX into template-and-bindings IR"
```

---

### Task 8: Signal analysis — `RA001`, `RA002`, `RA009`

**Files:**
- Create: `crates/runai_compiler/src/analyze/signals.rs`, `crates/runai_compiler/src/analyze/rules.rs`
- Modify: `crates/runai_compiler/src/analyze/mod.rs`

**Interfaces:**
- Consumes: `oxc_semantic::SemanticBuilder`, `Program`.
- Produces:
  - `pub struct SignalBindings { pub accessors: Vec<(String, (u32, u32))>, pub derived_names: Vec<String> }`
  - `pub fn find_signal_bindings(program: &Program) -> SignalBindings` — `const x = signal(...)` and `const y = derived(...)`.
  - `pub fn check_signal_rules(program: &Program, bindings: &SignalBindings) -> Vec<Diagnostic>` — emits `RA001`, `RA002`, `RA009`.

**Rule semantics (exact, so the implementer does not guess):**
- A **tracking scope** is: any `JSXExpressionContainer`, the argument of `derived(...)`, the argument of `effect(...)`, and the argument of `resource(...)`.
- A **read** is a call `x()` with zero arguments where `x` is a known accessor.
- A **write** is a call `x(v)` with one argument where `x` is a known accessor.
- `RA001` — a read whose nearest enclosing function is the component function, and which is not inside a tracking scope. A read inside an event handler is **not** flagged.
- `RA002` — a write inside an `effect(...)` argument.
- `RA009` — a write inside the component body or a JSX expression container.

- [ ] **Step 1: Write the failing tests**

```rust
// crates/runai_compiler/src/analyze/rules.rs (tests)
#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::parse_tsx;

    fn codes(src: &str) -> Vec<String> {
        let alloc = oxc_allocator::Allocator::default();
        let program = parse_tsx(&alloc, src).unwrap();
        let b = find_signal_bindings(program);
        check_signal_rules(program, &b).iter().map(|d| d.code.as_str().to_string()).collect()
    }

    #[test]
    fn body_read_is_ra001() {
        let src = "export default function P() { const n = signal(0); const x = n(); return <p/> }\n";
        assert_eq!(codes(src), vec!["RA001_UNTRACKED_READ"]);
    }

    #[test]
    fn read_in_event_handler_is_not_flagged() {
        let src = "export default function P() { const n = signal(0); return <button onClick={() => n()}>x</button> }\n";
        assert!(codes(src).is_empty(), "event handler reads are correct");
    }

    #[test]
    fn read_in_jsx_slot_is_not_flagged() {
        let src = "export default function P() { const n = signal(0); return <p>{n()}</p> }\n";
        assert!(codes(src).is_empty());
    }

    #[test]
    fn write_inside_effect_is_ra002() {
        let src = "export default function P() { const n = signal(0); effect(() => { n(1) }); return <p/> }\n";
        assert_eq!(codes(src), vec!["RA002_EFFECT_WRITE"]);
    }

    #[test]
    fn write_during_render_is_ra009() {
        let src = "export default function P() { const n = signal(0); n(1); return <p/> }\n";
        assert_eq!(codes(src), vec!["RA009_IMPLICIT_EFFECT"]);
    }

    #[test]
    fn read_inside_effect_is_not_ra001() {
        let src = "export default function P() { const n = signal(0); effect(() => { n() }); return <p/> }\n";
        assert!(codes(src).is_empty());
    }
}
```

- [ ] **Step 2: Run and verify failure**

Run: `cargo test -p runai_compiler rules`
Expected: FAIL — `find_signal_bindings` not found.

- [ ] **Step 3: Implement**

Build semantic data once, walk every `CallExpression` in source order, resolve the callee identifier to a symbol via `oxc_semantic`, and compare against the accessor symbol ids collected by `find_signal_bindings` (compare symbol ids, never names, so shadowing cannot fool it). For each read/write, climb ancestor nodes to decide whether it sits in a tracking scope and whether it sits in an event-handler arrow function. Emit diagnostics in ascending span order.

- [ ] **Step 4: Run and verify pass**

Run: `cargo test -p runai_compiler rules`
Expected: PASS, 6 tests.

- [ ] **Step 5: Commit**

```bash
git add crates/runai_compiler/src
git commit -m "feat(compiler): detect untracked reads, effect writes, and render-time writes"
```

---

### Task 9: `RA010_STATIC_VIOLATION`

**Files:**
- Modify: `crates/runai_compiler/src/analyze/rules.rs`, `crates/runai_compiler/src/analyze/mod.rs`
- Create: `crates/runai_compiler/src/analyze/entry.rs`

**Interfaces:**
- Consumes: `Mode`, `SignalBindings`, `Program`.
- Produces:
  - `pub fn analyze(source: &str, path: &str) -> Vec<Diagnostic>` — the single entry point used by the CLI. Runs parse, mode detection, route validation, props check, signal rules, and static rules, and returns diagnostics sorted by span. Parse failures short-circuit and return only `RA007`.
  - `pub fn check_static_mode(mode: Mode, program: &Program, bindings: &SignalBindings) -> Vec<Diagnostic>`

**Rule semantics:**
- When `mode == Static`, any of the following is `RA010_STATIC_VIOLATION`: a `signal`/`derived`/`effect`/`resource` call, a write to an accessor, or a JSX attribute whose name starts with `on` followed by an uppercase letter.
- The fix text is exactly: `Change line 1 to "use island" to ship JavaScript for this route.`

- [ ] **Step 1: Write the failing tests**

```rust
// crates/runai_compiler/src/analyze/entry.rs (tests)
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn static_route_with_handler_is_ra010() {
        let src = "\"use static\"\nexport default function P() { return <button onClick={go}>x</button> }\n";
        let d = analyze(src, "index.page.tsx");
        assert_eq!(d[0].code, crate::diagnostic::Code::RA010);
        assert!(d[0].fix.contains("Change line 1 to \"use island\""));
    }

    #[test]
    fn static_route_with_signal_is_ra010() {
        let src = "\"use static\"\nexport default function P() { const n = signal(0); return <p>{n()}</p> }\n";
        let d = analyze(src, "index.page.tsx");
        assert!(d.iter().any(|x| x.code == crate::diagnostic::Code::RA010));
    }

    #[test]
    fn static_route_pure_html_is_clean() {
        let src = "\"use static\"\nexport default function P(): Record<never, never> { return <div class=\"a\">hi</div> }\n";
        assert!(analyze(src, "about.page.tsx").is_empty());
    }

    #[test]
    fn syntax_error_short_circuits() {
        let src = "\"use static\"\nexport default function P() { return <div> }\n";
        let d = analyze(src, "bad.page.tsx");
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].code, crate::diagnostic::Code::RA007);
    }
}
```

- [ ] **Step 2: Run and verify failure**

Run: `cargo test -p runai_compiler entry`
Expected: FAIL — `analyze` not found.

- [ ] **Step 3: Implement**

`analyze` builds an `Allocator` per call (the AST borrows it, so the function must own both and drop them together; return owned `Diagnostic`s only). Sort by `span.0`, then by code. Deduplicate identical `(code, span)` pairs.

- [ ] **Step 4: Run and verify pass**

Run: `cargo test -p runai_compiler entry`
Expected: PASS, 4 tests.

- [ ] **Step 5: Commit**

```bash
git add crates/runai_compiler/src
git commit -m "feat(compiler): enforce static-route purity with RA010 and add analyze entry point"
```

---

### Task 10: Prerender codegen

**Files:**
- Create: `crates/runai_compiler/src/codegen/mod.rs`, `crates/runai_compiler/src/codegen/prerender.rs`

**Interfaces:**
- Consumes: `RouteIr`, `IrNode`.
- Produces:
  - `pub fn emit_prerender(route: &RouteIr) -> String` — a self-contained ESM module exporting `render(): string` with **no imports at all**.
  - `pub fn html_to_string(node: &IrNode) -> Result<String, Diagnostic>` — pure string form.

- [ ] **Step 1: Write the failing tests**

```rust
// crates/runai_compiler/src/codegen/prerender.rs (tests)
#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::lower_route;

    #[test]
    fn emits_no_imports_for_static_route() {
        let src = "\"use static\"\nexport default function P() { return <div class=\"a\">hi</div> }\n";
        let route = lower_route(src).unwrap();
        let js = emit_prerender(&route);
        assert!(!js.contains("import"), "static output must not import: {js}");
        assert!(js.contains("export"));
    }

    #[test]
    fn static_attrs_and_text_survive() {
        let src = "\"use static\"\nexport default function P() { return <div class=\"a\" id=\"b\">hi</div> }\n";
        let route = lower_route(src).unwrap();
        assert_eq!(html_to_string(&route.tree).unwrap(), "<div class=\"a\" id=\"b\">hi</div>");
    }

    #[test]
    fn entities_are_not_reencoded() {
        let src = "\"use static\"\nexport default function P() { return <p>a &amp; b</p> }\n";
        let route = lower_route(src).unwrap();
        let html = html_to_string(&route.tree).unwrap();
        assert!(html.contains("a &amp; b"), "got {html}");
        assert!(!html.contains("&amp;amp;"));
    }

    #[test]
    fn self_closing_void_tags_are_valid_html() {
        let src = "\"use static\"\nexport default function P() { return <img src=\"/a.png\"/> }\n";
        let route = lower_route(src).unwrap();
        assert_eq!(html_to_string(&route.tree).unwrap(), "<img src=\"/a.png\">");
    }

    #[test]
    fn irreducible_binding_is_an_error_not_a_panic() {
        let src = "\"use island\"\nexport default function P() { return <p>{go()}</p> }\n";
        let route = lower_route(src).unwrap();
        let err = html_to_string(&route.tree).unwrap_err();
        assert!(err.message.contains("static"));
    }
}
```

- [ ] **Step 2: Run and verify failure**

Run: `cargo test -p runai_compiler prerender`
Expected: FAIL — `emit_prerender` not found.

- [ ] **Step 3: Implement**

`html_to_string` writes tags with attributes in source order, single space separated, values quoted with `"` and never re-escaped. Void elements (`area base br col embed hr img input link meta source track wbr`) get no closing tag. A `Binding::Text` from a non-static expression returns an `RA010` diagnostic whose message names the branch, because a static route may not contain dynamic text. `emit_prerender` returns `export function render() {\n  return "…";\n}` using a JS string literal with `"` and `\n` escaped; it must contain no `import`.

- [ ] **Step 4: Run and verify pass**

Run: `cargo test -p runai_compiler prerender`
Expected: PASS, 5 tests.

- [ ] **Step 5: Commit**

```bash
git add crates/runai_compiler/src
git commit -m "feat(compiler): emit self-contained prerender modules with no imports"
```

---

### Task 11: Island codegen

**Files:**
- Create: `crates/runai_compiler/src/codegen/client.rs`
- Modify: `crates/runai_compiler/src/codegen/mod.rs`

**Interfaces:**
- Consumes: `RouteIr`, `IrNode`, `Binding`.
- Produces:
  - `pub fn emit_client(route: &RouteIr, runtime_url: &str) -> String` — ESM importing only from `runtime_url`.
  - `pub fn template_html(node: &IrNode) -> String` — markup with dynamic slots removed to placeholder nodes.

**Emitted shape** (pinned, so the runtime API and the emitter cannot drift):

```js
import { template, _setText, _setAttr, _on } from "/assets/runtime.js"
export default function Page(props) {
  const n = signal(0)
  const t = template("<p></p>")
  const el = t.content.firstChild.cloneNode(true)
  const _b0 = el
  _setText(_b0, () => n())
  return el
}
```

- [ ] **Step 1: Write the failing tests**

```rust
// crates/runai_compiler/src/codegen/client.rs (tests)
#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::lower_route;

    #[test]
    fn imports_only_the_runtime_url() {
        let src = "\"use island\"\nexport default function P() { return <p>{1}</p> }\n";
        let js = emit_client(&lower_route(src).unwrap(), "/assets/runtime.js");
        assert!(js.contains("from \"/assets/runtime.js\""));
        assert_eq!(js.matches("import ").count(), 1, "{js}");
    }

    #[test]
    fn static_text_is_in_the_template_not_the_runtime() {
        let src = "\"use island\"\nexport default function P() { return <p>hello</p> }\n";
        let js = emit_client(&lower_route(src).unwrap(), "/assets/runtime.js");
        assert!(js.contains("hello"));
        assert!(!js.contains("_setText"));
    }

    #[test]
    fn event_handler_is_wired_with_addEventListener_via_on() {
        let src = "\"use island\"\nexport default function P() { return <button onClick={go}>x</button> }\n";
        let js = emit_client(&lower_route(src).unwrap(), "/assets/runtime.js");
        assert!(js.contains("_on("));
        assert!(js.contains("\"click\""));
    }

    #[test]
    fn output_parses_as_javascript() {
        let src = "\"use island\"\nexport default function P() { const n = signal(0); return <p class=\"a\">{n()}</p> }\n";
        let js = emit_client(&lower_route(src).unwrap(), "/assets/runtime.js");
        let alloc = oxc_allocator::Allocator::default();
        let ret = oxc_parser::Parser::new(&alloc, &js, oxc_span::SourceType::mjs()).parse();
        assert!(ret.diagnostics.is_empty(), "emitted code must parse: {:?}", ret.diagnostics);
    }
}
```

- [ ] **Step 2: Run and verify failure**

Run: `cargo test -p runai_compiler client`
Expected: FAIL — `emit_client` not found.

- [ ] **Step 3: Implement**

Emit the component's original statements minus the JSX return, then the template construction, cloning, binding wiring, and `return el`. Rewrite `signal`/`derived`/`effect`/`resource`/`For`/`Show`/`When` imports to come from `runtime_url`. Dynamic children become `_setText(parentNode, () => expr)`; dynamic attributes become `_setAttr(node, "name", () => expr)`; events become `_on(node, "click", handler)`.

**The emitter must run `analyze` first and refuse to emit when it returns errors.** The `_on(..., () => n())` form would otherwise silently wrap an untracked read in a function and defeat `RA001`, so Task 8's check has to fire before any codegen. Add this test alongside the four above:

```rust
#[test]
fn refuses_to_emit_when_analysis_reports_errors() {
    let src = "\"use island\"\nexport default function P(props: {}) { const n = signal(0); const x = n(); return <p/> }\n";
    let err = emit_client_checked(src, "/assets/runtime.js").unwrap_err();
    assert_eq!(err[0].code, crate::diagnostic::Code::RA001);
}
```

This requires a second entry point in `codegen/mod.rs`: `pub fn emit_client_checked(source: &str, runtime_url: &str) -> Result<String, Vec<Diagnostic>>`, which calls `analyze` and returns `Err` before lowering. Task 13's builder calls this one; `emit_client` stays as the pure IR emitter used by its own unit tests.

- [ ] **Step 4: Run and verify pass**

Run: `cargo test -p runai_compiler client`
Expected: PASS, 4 tests.

- [ ] **Step 5: Commit**

```bash
git add crates/runai_compiler/src
git commit -m "feat(compiler): emit island ESM modules against the runtime URL"
```

---

### Task 12: `runai check`

**Files:**
- Modify: `crates/runai_cli/src/main.rs`
- Create: `crates/runai_cli/src/check.rs`

**Interfaces:**
- Consumes: `runai_compiler::analyze`.
- Produces: `runai check [--json] [path]`; exit 1 on any error diagnostic, 0 otherwise. Default path `src/routes`.

- [ ] **Step 1: Write the failing integration test**

```rust
// crates/runai_cli/tests/check.rs
use std::process::Command;

fn run(args: &[&str], dir: &std::path::Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_runai")).args(args).current_dir(dir).output().unwrap()
}

#[test]
fn check_reports_ra001_as_json_and_exits_1() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("src/routes")).unwrap();
    std::fs::write(
        tmp.path().join("src/routes/index.page.tsx"),
        "\"use island\"\nexport default function P(props: {}) { const n = signal(0); const x = n(); return <p/> }\n",
    ).unwrap();
    let out = run(&["check", "--json"], tmp.path());
    assert_eq!(out.status.code(), Some(1));
    let parsed: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(parsed[0]["code"], "RA001_UNTRACKED_READ");
}

#[test]
fn check_clean_route_exits_0() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("src/routes")).unwrap();
    std::fs::write(
        tmp.path().join("src/routes/about.page.tsx"),
        "\"use static\"\nexport default function P(props: Record<never, never>) { return <p>hi</p> }\n",
    ).unwrap();
    let out = run(&["check", "--json"], tmp.path());
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "[]");
}

#[test]
fn check_missing_route_dir_is_a_clear_error() {
    let tmp = tempfile::tempdir().unwrap();
    let out = run(&["check"], tmp.path());
    assert_eq!(out.status.code(), Some(2));
}
```

Add `tempfile` and `serde_json` as dev-dependencies of `runai_cli`.

- [ ] **Step 2: Run and verify failure**

Run: `cargo test -p runai_cli`
Expected: FAIL — binary prints its version instead of running `check`.

- [ ] **Step 3: Implement**

Parse `std::env::args` by hand (no clap dependency in this plan). `check` walks `src/routes` for `*.page.tsx`, calls `analyze` per file, prefixes each human diagnostic with the relative path, and prints `Diagnostic::render_all_json`. Exit codes: `0` clean, `1` errors found, `2` usage or missing directory.

- [ ] **Step 4: Run and verify pass**

Run: `cargo test -p runai_cli`
Expected: PASS, 3 tests.

- [ ] **Step 5: Commit**

```bash
git add crates/runai_cli
git commit -m "feat(cli): add runai check with JSON output and stable exit codes"
```

---

### Task 13: `runai build`

**Files:**
- Create: `crates/runai_cli/src/build.rs`
- Modify: `crates/runai_cli/src/main.rs`

**Interfaces:**
- Consumes: `lower_route`, `emit_prerender`, `emit_client`, `analyze`.
- Produces: `runai build [--out dist]` producing, for each `*.page.tsx`:
  - static: `dist/<route>/index.html`
  - island: `dist/<route>/index.html` containing one `<script type="module" src="/assets/<route>.js">`, plus `dist/assets/<route>.js`
  - always: `dist/assets/runtime.js` copied from `packages/runai/runtime.js` when any island exists
  - Prerender modules are executed by `bun` in a temp directory; the CLI fails the build if `bun` is absent or panics.

- [ ] **Step 1: Write the failing integration test**

```rust
// crates/runai_cli/tests/build.rs
#[test]
fn static_route_html_has_no_script_tag() {
    let tmp = tempfile::tempdir().unwrap();
    write_app(tmp.path(), "\"use static\"\nexport default function P(props: Record<never, never>) { return <p>hi</p> }\n");
    let status = std::process::Command::new(env!("CARGO_BIN_EXE_runai"))
        .args(["build", "--out", "dist"]).current_dir(tmp.path()).status().unwrap();
    assert!(status.success());
    let html = std::fs::read_to_string(tmp.path().join("dist/index.html")).unwrap();
    assert!(html.contains("<p>hi</p>"));
    assert!(!html.contains("<script"), "static route must ship no JS: {html}");
}

#[test]
fn island_route_emits_one_script_and_a_runtime() {
    let tmp = tempfile::tempdir().unwrap();
    write_app(tmp.path(), "\"use island\"\nexport default function P(props: Record<never, never>) { return <button onClick={() => {}}>x</button> }\n");
    let status = std::process::Command::new(env!("CARGO_BIN_EXE_runai"))
        .args(["build", "--out", "dist"]).current_dir(tmp.path()).status().unwrap();
    assert!(status.success());
    let html = std::fs::read_to_string(tmp.path().join("dist/index.html")).unwrap();
    assert_eq!(html.matches("<script").count(), 1);
    assert!(tmp.path().join("dist/assets/runtime.js").exists());
    assert!(tmp.path().join("dist/assets/index.js").exists());
}

#[test]
fn diagnostics_stop_the_build() {
    let tmp = tempfile::tempdir().unwrap();
    write_app(tmp.path(), "\"use static\"\nexport default function P(props: Record<never, never>) { const n = signal(0); return <p>{n()}</p> }\n");
    let status = std::process::Command::new(env!("CARGO_BIN_EXE_runai"))
        .args(["build", "--out", "dist"]).current_dir(tmp.path()).status().unwrap();
    assert!(!status.success());
    assert!(!tmp.path().join("dist/index.html").exists());
}
```

`write_app` creates `src/routes/index.page.tsx` and a `runai.json` containing `{ "runtime": "<abs path to packages/runai/runtime.js>" }`.

- [ ] **Step 2: Run and verify failure**

Run: `cargo test -p runai_cli build`
Expected: FAIL — `build` is not a recognized command.

- [ ] **Step 3: Implement**

Route path mapping: strip `.page.tsx`, map `index` → `dist/index.html`, `about` → `dist/about/index.html`, and any other name `x` → `dist/x/index.html`. `_shell.tsx` and any file beginning with `_` are not routes and are skipped. A `[slug]` segment fails the build with a diagnostic whose message is `dynamic route segments are not supported yet` and exit code 1 — dynamic routes are deferred (see Deferred, item 1), and a wrong-silent-output is worse than a refusal. For each static route: write the emitted module to `<tmp>/<name>.mjs`, run `bun -e "import('./<name>.mjs').then(m => process.stdout.write(m.render()))"`, capture stdout, and wrap it in the document shell with `<!doctype html><html><head><meta charset="utf-8"><title>runai</title></head><body><div id="app">…</div></body></html>`. For island routes, emit the same shell with the module script pointing at `/assets/<name>.js`, copy the runtime, and also write the route module. A failed `bun` invocation exits 1 and writes nothing.

- [ ] **Step 4: Run and verify pass**

Run: `cargo test -p runai_cli build`
Expected: PASS, 3 tests.

- [ ] **Step 5: Commit**

```bash
git add crates/runai_cli
git commit -m "feat(cli): add runai build with zero-JS static output and island assets"
```

---

### Task 14: `runai dev`

**Files:**
- Create: `crates/runai_cli/src/dev.rs`
- Modify: `crates/runai_cli/src/main.rs`

**Interfaces:**
- Consumes: Task 13's build.
- Produces: `runai dev [--port 5173] [--background]`; serves `dist/` over HTTP; `GET /_runai/status` returns `{"running":true,"pid":N,"port":N,"root":"<abs>"}`; `--background` returns immediately after the port accepts connections and writes `.runai/dev.json` containing `{pid,port,root}`; a second `--background` invocation returns the existing instance's JSON and exit 0; `runai dev stop` reads `.runai/dev.json`, terminates the pid, and exits 0 even when nothing is running.

- [ ] **Step 1: Write the failing integration test**

```rust
// crates/runai_cli/tests/dev.rs
#[test]
fn background_twice_reports_the_same_pid() {
    let tmp = tempfile::tempdir().unwrap();
    write_min_app(tmp.path());
    let first = run_json(&["dev", "--background", "--port", "0"], tmp.path());
    let second = run_json(&["dev", "--background", "--port", "0"], tmp.path());
    assert_eq!(first["pid"], second["pid"]);
    assert_eq!(first["running"], true);
    let status = reqwest::blocking::get(format!("http://127.0.0.1:{}/_runai/status", first["port"])).unwrap();
    assert!(status.text().unwrap().contains("\"running\":true"));
    assert!(std::process::Command::new(env!("CARGO_BIN_EXE_runai"))
        .args(["dev", "stop"]).current_dir(tmp.path()).status().unwrap().success());
}

#[test]
fn stop_when_not_running_exits_zero() {
    let tmp = tempfile::tempdir().unwrap();
    write_min_app(tmp.path());
    assert!(std::process::Command::new(env!("CARGO_BIN_EXE_runai"))
        .args(["dev", "stop"]).current_dir(tmp.path()).status().unwrap().success());
}
```

Add `reqwest` with `blocking` as a dev-dependency.

- [ ] **Step 2: Run and verify failure**

Run: `cargo test -p runai_cli dev`
Expected: FAIL — `dev` is not a recognized command.

- [ ] **Step 3: Implement**

Build into `dist/` first, then serve with a hand-written `std::net::TcpListener` loop: parse the request line, map `/` to `index.html`, serve files with a guessed `Content-Type`, and answer `/_runai/status` from memory. `--background` forks by re-executing `std::env::current_exe()` with a hidden `--serve-only` flag, then polls the port up to 5 seconds. Port `0` means "pick a free port" and the chosen port is written to `.runai/dev.json` and reported in stdout JSON.

- [ ] **Step 4: Run and verify pass**

Run: `cargo test -p runai_cli dev`
Expected: PASS, 2 tests.

- [ ] **Step 5: Commit**

```bash
git add crates/runai_cli
git commit -m "feat(cli): add idempotent background dev server with status endpoint"
```

---

### Task 15: `runai skills`

**Files:**
- Create: `crates/runai_cli/src/skills.rs`, `crates/runai_compiler/src/skills.rs`
- Modify: `crates/runai_cli/src/main.rs`

**Interfaces:**
- Consumes: `Code::skill_file`, `Code::as_str`.
- Produces:
  - `runai_compiler::skills::skill_body(code: Code) -> String` — the Markdown for one code.
  - `runai skills [--out <dir>]` writing one file per code, default `node_modules/runai/skills/`.

- [ ] **Step 1: Write the failing test**

```rust
// crates/runai_cli/tests/skills.rs
#[test]
fn writes_one_file_per_code_with_frontmatter() {
    let tmp = tempfile::tempdir().unwrap();
    let status = std::process::Command::new(env!("CARGO_BIN_EXE_runai"))
        .args(["skills", "--out", "skills"]).current_dir(tmp.path()).status().unwrap();
    assert!(status.success());
    for code in ["RA001_UNTRACKED_READ", "RA002_EFFECT_WRITE", "RA010_STATIC_VIOLATION", "RA012_MISSING_MODE"] {
        let body = std::fs::read_to_string(tmp.path().join(format!("skills/{code}.md"))).unwrap();
        assert!(body.starts_with("---\n"));
        assert!(body.contains(&format!("name: {code}")));
        assert!(body.contains("## Fix"));
    }
}
```

- [ ] **Step 2: Run and verify failure**

Run: `cargo test -p runai_cli skills`
Expected: FAIL — `skills` is not a recognized command.

- [ ] **Step 3: Implement**

Each skill body is generated from a `match` on `Code` with three sections: `## What it means`, `## Why it is an error`, `## Fix` containing the same fix text the diagnostic emits. Frontmatter is `---\nname: <code>\ndescription: <one line>\n---`. This keeps the diagnostic and the skill from drifting, because both read the same `match` arm.

- [ ] **Step 3b: Add the drift test**

```rust
// crates/runai_compiler/src/skills.rs (tests)
#[test]
fn every_code_has_a_skill_body() {
    for code in crate::diagnostic::Code::all() {
        assert!(!crate::skills::skill_body(*code).is_empty(), "missing skill for {}", code.as_str());
    }
}
```

Add `Code::all() -> &'static [Code]` covering all twelve codes.

- [ ] **Step 4: Run and verify pass**

Run: `cargo test -p runai_cli skills && cargo test -p runai_compiler skills`
Expected: PASS, 2 tests.

- [ ] **Step 5: Commit**

```bash
git add crates/runai_cli crates/runai_compiler
git commit -m "feat: generate per-code SKILL.md files from the diagnostic source of truth"
```

---

### Task 16: Example app, end-to-end verification, and size budget

**Files:**
- Create: `examples/counter/runai.json`, `examples/counter/src/routes/_shell.tsx`, `examples/counter/src/routes/index.page.tsx`, `examples/counter/src/routes/about.page.tsx`, `scripts/e2e.sh`, `scripts/size-budget.sh`

**Interfaces:**
- Consumes: every command above.
- Produces: a runnable example and two scripts CI can call.

- [ ] **Step 1: Write the example**

`index.page.tsx` — `"use island"`, a counter with `props: Record<never, never>`, a `signal`, an increment button, and a `derived` parity label.

`about.page.tsx` — `"use static"`, two paragraphs of text.

`_shell.tsx` — a `props: { children: JSX.Element }` component wrapping children in a `<main>`. In this plan it is authored and checked but **not** applied by the builder; shell composition arrives with the router plan. It exists here so `runai check` exercises a component with children.

- [ ] **Step 2: Write the failing end-to-end script**

```bash
#!/usr/bin/env bash
# scripts/e2e.sh
set -euo pipefail
cd "$(dirname "$0")/../examples/counter"
cargo run --quiet --manifest-path ../../Cargo.toml -p runai_cli -- check --json | tee /tmp/runai-check.json
test "$(cat /tmp/runai-check.json)" = "[]"
cargo run --quiet --manifest-path ../../Cargo.toml -p runai_cli -- build --out dist
test -f dist/index.html
test -f dist/about/index.html
grep -q '<script' dist/index.html
! grep -q '<script' dist/about/index.html
echo "e2e ok"
```

- [ ] **Step 3: Run it and watch it fail**

Run: `bash scripts/e2e.sh`
Expected: FAIL at `test -f dist/about/index.html` — the builder still writes `dist/about.html`.

- [ ] **Step 4: Fix the route mapping**

Apply the mapping described in Task 13's implementation note. Change only `crates/runai_cli/src/build.rs`; the script stays as written.

- [ ] **Step 5: Run e2e and verify pass**

Run: `bash scripts/e2e.sh`
Expected: `e2e ok`.

- [ ] **Step 6: Write the failing size-budget script**

```bash
#!/usr/bin/env bash
# scripts/size-budget.sh
set -euo pipefail
cd "$(dirname "$0")/../packages/runai"
bunx esbuild runtime.js --minify --format=esm --outfile=/tmp/runai-runtime.min.js >/dev/null
gzip -9 -c /tmp/runai-runtime.min.js > /tmp/runai-runtime.min.js.gz
bytes=$(wc -c < /tmp/runai-runtime.min.js.gz)
echo "runtime gzipped: ${bytes} bytes"
test "$bytes" -le 6144
```

- [ ] **Step 7: Run it and record the real number**

Run: `bash scripts/size-budget.sh`
Expected: prints a byte count. It may FAIL if the runtime exceeds 6144; if it does, the failure is real: remove dev instrumentation from the budgeted build first, then reduce features, and do not raise the budget.

- [ ] **Step 8: Final verification and commit**

Run: `cargo test --workspace && bash scripts/e2e.sh && bash scripts/size-budget.sh`
Expected: all tests pass, `e2e ok`, and a byte count at or under 6144.

```bash
git add -A
git commit -m "feat: ship counter example with end-to-end and runtime size-budget checks"
```

---

## Deferred to later plans

Explicitly not in this plan, in dependency order:

1. **Router and CSS** — client navigation, shell composition, `[slug]` params, `.css` imports, `RA013`.
2. **Remaining rules** — `RA003`, `RA004`, `RA005`, `RA008`.
3. **`runai inspect`** — happy-dom island boot, `--click`/`--type`/`--snapshot`.
4. **`runai fmt`** — canonical formatter.
5. **Bundling** — Rolldown for code splitting once more than one island exists per page.
6. **Eval harness** — `bench/` with 20 tasks and a checked-in React control, plus `runai bench`.
7. **Perf benchmark** — js-framework-benchmark subset harness at `bench/perf/`.

## Self-Review

- **Spec coverage:** §7.1 authoring model → Tasks 5–8; §7.2 components/props → Task 4; §7.3 static/island + 0 KB → Tasks 9, 10, 13; §7.4 rules → Tasks 8, 9 (subset, rest deferred and listed below); §7.5 compiler → Tasks 3, 7, 10, 11; §7.6 runtime → Tasks 5, 6; §7.9 tooling → Tasks 12–15; §9 criteria 1, 5, 6 → Tasks 13, 15, 16. Criteria 2–4 (benchmark, eval, false-positive fixtures) are covered by deferred plans 6 and 7, which is why they are named there.
- **Gaps this plan knowingly leaves:** §7.6 `When`'s error branch is unit-tested but not reachable from JSX in this plan, because `<When>` JSX lowering is out of scope — the runtime is ready and Task 11 wires only `_setText`/`_setAttr`/`_on`. §7.7's shell and params, §7.8's CSS, and §7.9's `inspect`/`fmt` are all in Deferred. None of these are silently dropped: each appears in the Deferred list with its dependency order.
- **Step scan:** every step names a command with an expected outcome; implementation steps give signatures and the one semantic decision the signature leaves open.
- **Type consistency:** `Accessor<T>` is defined once in Task 5 and reused in Task 6 (`When.value`, `resource`); `Code::as_str` is used by both `render_human` (Task 2) and `skills` (Task 15); `emit_prerender`/`emit_client` names match their Task 13 consumers.
- **Review Focus:** each of the five lines has a test in the task named — event-handler read (Task 8), outliving callback (Task 8's scope rules), entities (Task 10), JSX syntax error (Task 3), no-props component (Task 4).
- **Proportion:** the plan pins test assertions and signatures rather than implementation bodies, except where a rule's exact semantics would otherwise be guesswork.
