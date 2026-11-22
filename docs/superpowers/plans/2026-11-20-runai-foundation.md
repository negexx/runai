# runai Foundation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship a working vertical slice of runai. Strict TSX compiles to direct DOM, static routes emit zero-JS HTML, the counter island actually runs (proven by clicking it in happy-dom), and the first coded diagnostics catch real agent mistakes.

**Architecture:** A Rust crate on Oxc analyzes TSX on the **original source** (so every span points at what the agent wrote). After analysis passes, it strips types and lowers the JSX to a template-and-bindings IR with two emitters:

- a self-contained **prerender** emitter for `"use static"` routes. Its module has no imports and runs once at build time, so the served HTML references 0 KB of JavaScript.
- an **ESM** emitter for `"use island"` routes. It clones module-level templates, wires signal bindings, and mounts the page into `#app`.

A hand-written ESM runtime of 6 KB or less provides signals, ownership, `For`, `Show`, and `When`. The CLI orchestrates check/build/dev/skills. There is no bundler in this plan: island output is direct ESM plus a copied runtime, resolved through `/assets/` URLs.

**Tech Stack:** Rust 1.99 (2024 edition); oxc_parser, oxc_semantic, oxc_transformer, oxc_codegen 0.159; Bun 1.4 (`bun test`) for runtime tests; Bun or Node 24 for build-time module execution (the spec §5 detection rule); plain ESM runtime with hand-written `.d.ts`.

**Spec:** `docs/superpowers/specs/2026-11-20-runai-framework-design.md`

## Global Constraints

- Compiler crate: edition `2024`, `oxc_parser`/`oxc_semantic`/`oxc_allocator`/`oxc_span`/`oxc_transformer`/`oxc_codegen` pinned to `0.159.0`.
- **Before Task 3, confirm the Oxc 0.159 API names used below with ctx7**: `Program::directives`, the `OxcDiagnostic` label shape, the `ParserReturn` fields, and the transformer's TypeScript-strip and JSX-preserve options. Where the docs differ from this plan, follow the docs and say so in the commit message.
- Runtime budget: **≤ 6 KB gzipped**, minified+gzipped, including `signal`, `derived`, `effect`, `For`, `Show`, `When`, `resource`, and island mount; excluding user code and dev instrumentation.
- Static routes ship **0 bytes** of JavaScript. This is enforced by `RA010_STATIC_VIOLATION`, not by inspecting build output.
- Every diagnostic carries: `code`, `severity`, `file`, `line`, `col`, `span`, `message`, `explain`, `fix`, `doc`, and a nullable `patch`. `code` is stable and never reused. `doc` is `node_modules/runai/skills/<code>.md` (spec §7.4). `file`/`line`/`col` extend the spec §7.5 field list so that `check --json` over many files says where each error is. `line` and `col` are 1-based.
- Diagnostic codes in this plan (nine): `RA001`, `RA002`, `RA006`, `RA007`, `RA009`, `RA010`, `RA011`, `RA012`, `RA014`. **`RA014_NOT_SUPPORTED_YET` is new and not in the spec.** It is the coded refusal for constructs a later plan adds: component tags, JSX inside expressions, imports other than `"runai"`, and `[slug]` segments. A wrong-silent-output is worse than a refusal, and the spec's anti-goal forbids an uncoded one.
- Reactive primitives come from `import { … } from "runai"`. The analyzer identifies `signal`/`derived`/`effect`/`resource` by resolving the callee to that import binding, never by name.
- Every component (any function returning JSX) takes **exactly one** parameter with a type annotation. A component with no props writes `props: Record<never, never>`. This reconciles spec §7.2's RA006 (a props type is always written) with RA011 (a route accepts no props).
- `runai check --json` prints exactly one JSON array to stdout and nothing else, on exit codes 0 and 1. On exit 2 (usage error or missing directory), stdout is empty and the message goes to stderr.
- All CLI commands are idempotent: starting a running dev server returns the existing instance; stopping a stopped one exits 0.
- `"use static"` or `"use island"` must be the first directive in every `.page.tsx`, above imports. No inference. Files starting with `_` are not routes and take no directive.
- No bundler, no client router, no `store`/`batch`/`untrack`, no component-level islands, no island prerender in this plan.
- Commit after every task.

## Review Focus

These are the input classes most likely to bite a real user, and each is pinned by a test in the task named.

1. **A signal read or write inside an event handler** must never be flagged `RA001` or `RA009`. It is correct, intended behavior, and the counter example depends on it. (Task 8)
2. **A signal read inside a nested callback that outlives the component** (`setTimeout(() => count())`) is not flagged, and that is documented in the RA001 skill. The read happens when the callback runs, so it sees the current value; nothing is frozen. (Task 8, pinned by a test)
3. **JSX text containing entities and meaningful whitespace** (`&amp;`, a newline between two inline elements) must not be silently mangled. Entities pass through verbatim (Task 10). A line break between two inline siblings is `RA007`, because JSX drops that space and the agent probably meant to keep it (Task 7).
4. **A syntax error inside JSX** must produce `RA007` with a correct span and a non-panicking exit, never a Rust panic or a bare parser message. (Task 3)
5. **A component with no props** must still require an explicit type, and the fix must show the exact line to write. (Task 4)
6. **Typed TSX must reach the browser as valid JavaScript.** Every route has typed props, so an emitter that forgets to strip types breaks every page. (Tasks 7 and 11)

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
│  │     ├─ source.rs                  parse + mode directive detection
│  │     ├─ strip.rs                   TS → JS (JSX preserved) via oxc_transformer + oxc_codegen
│  │     ├─ ir.rs                      Template/Binding/Node IR + lowering
│  │     ├─ skills.rs                  SKILL.md bodies per code
│  │     ├─ analyze/
│  │     │  ├─ mod.rs                  module wiring
│  │     │  ├─ entry.rs                analyze(): the single entry point
│  │     │  ├─ component.rs            component discovery + route export (RA011)
│  │     │  ├─ props.rs                RA006
│  │     │  ├─ jsx.rs                  RA007 whitespace, RA014 unsupported constructs
│  │     │  ├─ signals.rs              signal binding discovery
│  │     │  └─ rules.rs                RA001/RA002/RA009/RA010
│  │     └─ codegen/
│  │        ├─ mod.rs                  checked entry points (analyze first, then emit)
│  │        ├─ prerender.rs            IR → self-contained render() module
│  │        └─ client.rs               IR → DOM module + mount
│  └─ runai_cli/
│     ├─ Cargo.toml
│     ├─ src/{main,check,build,dev,skills}.rs
│     └─ tests/{check,build,dev,skills}.rs
├─ packages/runai/
│  ├─ package.json                     "files" includes skills/
│  ├─ runtime.js                       ≤6 KB ESM runtime
│  ├─ index.d.ts                       public types
│  ├─ skills/                          generated SKILL.md files, committed
│  └─ test/{setup.js,runtime.test.js,smoke-island.mjs}
├─ examples/counter/
│  ├─ runai.json
│  └─ src/routes/
│     ├─ _shell.tsx
│     ├─ index.page.tsx                "use island"
│     └─ about.page.tsx                "use static"
├─ scripts/{e2e.sh,size-budget.sh}
└─ docs/superpowers/
   ├─ specs/2026-11-20-runai-framework-design.md
   └─ plans/2026-11-20-runai-foundation.md
```

Each file owns one concern. `analyze/` produces diagnostics only and never imports `codegen/`. `codegen/` consumes IR built from type-stripped source, and reaches analysis only through `analyze()` in its checked entry points, which refuse to emit when analysis reports errors. Lowering may therefore assume its input already passed every rule.

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

`crates/runai_compiler/Cargo.toml` — package `runai_compiler`, `version.workspace = true`, `edition.workspace = true`, dependencies `oxc_parser`, `oxc_allocator`, `oxc_span`, `oxc_semantic`, `oxc_transformer`, `oxc_codegen` all `= "0.159.0"`, plus `serde = { version = "1", features = ["derive"] }` and `serde_json = "1"`.

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

`.gitignore` lists `target/`, `node_modules/`, `dist/`, `.runai/`.

- [ ] **Step 9: Commit**

This runs inside the existing `runai-foundation` worktree, so there is no `git init`.

```bash
git add -A
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
  - `Diagnostic { code: Code, severity: Severity, file: String, line: u32, col: u32, span: (u32, u32), message: String, explain: String, fix: String, doc: String, patch: Option<String> }`
  - `Code` enum with variants `RA001`, `RA002`, `RA006`, `RA007`, `RA009`, `RA010`, `RA011`, `RA012`, `RA014`. Each has:
    - `fn as_str(&self) -> &'static str`, returning the exact string (e.g. `"RA001_UNTRACKED_READ"`, `"RA014_NOT_SUPPORTED_YET"`)
    - `fn skill_file(&self) -> String`, returning `format!("{}.md", self.as_str())`
    - `fn doc_path(&self) -> String`, returning `format!("node_modules/runai/skills/{}", self.skill_file())`
    - `fn fix_template(&self) -> &'static str`, the canonical fix text. It is the single source for both the diagnostic's `fix` and the skill's `## Fix` section.
  - `Code::all() -> &'static [Code]` returning all nine variants in code order. Task 15's drift test depends on it.
  - `Severity { Error, Warning }`
  - `Diagnostic::new(code: Code, span: (u32, u32), message: impl Into<String>) -> Diagnostic`. It fills `severity: Error`, `explain` and `fix` from the code, `doc: code.doc_path()`, `file: ""`, `line: 0`, `col: 0`, and `patch: None`. Rules build diagnostics only through this constructor, so `fix` can never drift from the skill.
  - `Diagnostic::locate(&mut self, source: &str, path: &str)`. It sets `file` and derives the 1-based `line` and `col` (counted in chars) from `span.0`. `analyze` calls it on every diagnostic before returning.
  - `Diagnostic::to_json(&self) -> String` and `render_human(&self, source: &str) -> String`
  - `Diagnostic::render_all_json(diags: &[Diagnostic]) -> String`

- [ ] **Step 1: Write the failing tests**

```rust
// crates/runai_compiler/src/diagnostic.rs (tests module at bottom)
#[cfg(test)]
mod tests {
    use super::*;

    // Byte 27 is the `c` of `count()` on line 2: line 1 is 15 bytes including
    // its newline, and line 2 starts with `  const x = ` (12 bytes).
    const SRC: &str = "function A() {\n  const x = count()\n}\n";

    fn sample() -> Diagnostic {
        let mut d = Diagnostic::new(Code::RA001, (27, 34), "signal read outside a tracking scope");
        d.locate(SRC, "src/routes/index.page.tsx");
        d
    }

    #[test]
    fn code_strings_are_stable() {
        assert_eq!(Code::RA001.as_str(), "RA001_UNTRACKED_READ");
        assert_eq!(Code::RA010.as_str(), "RA010_STATIC_VIOLATION");
        assert_eq!(Code::RA014.as_str(), "RA014_NOT_SUPPORTED_YET");
        assert_eq!(Code::all().len(), 9);
    }

    #[test]
    fn constructor_fills_fix_and_doc_from_the_code() {
        let d = sample();
        assert_eq!(d.fix, Code::RA001.fix_template());
        assert_eq!(d.doc, "node_modules/runai/skills/RA001_UNTRACKED_READ.md");
    }

    #[test]
    fn locate_computes_one_based_line_and_col() {
        let d = sample();
        assert_eq!((d.line, d.col), (2, 13));
        assert_eq!(d.file, "src/routes/index.page.tsx");
    }

    #[test]
    fn json_has_every_field_and_null_patch() {
        let json = sample().to_json();
        for key in ["code", "severity", "file", "line", "col", "span", "message", "explain", "fix", "doc", "patch"] {
            assert!(json.contains(&format!("\"{key}\"")), "missing {key} in {json}");
        }
        assert!(json.contains("\"patch\":null"));
    }

    #[test]
    fn human_output_names_the_line_and_the_fix() {
        let out = sample().render_human(SRC);
        assert!(out.contains("RA001_UNTRACKED_READ"));
        assert!(out.contains("src/routes/index.page.tsx:2:13"));
        assert!(out.contains(Code::RA001.fix_template()));
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

Add the struct and enums as specified. Derive `serde::Serialize` with `#[serde(rename_all = "lowercase")]` on `Severity`, and serialize the rest by hand so that `span` is `[start, end]` and `patch` is `null` when absent.

`locate` computes the line by counting `\n` in `source[..span.0]`, and the column as the char count since the last `\n`, plus one. Clamp `span.0` to `source.len()` and to a char boundary so a bad span can never panic. `render_human` prints `file:line:col`, then the code, message, explain, fix, and `doc`.

The `as_str` strings are exactly the names in spec §7.4's table (`RA006_UNANNOTATED_PROPS`, `RA011_BAD_ROUTE_EXPORT`, and so on), plus `RA014_NOT_SUPPORTED_YET`. Write the `explain` and `fix_template` text for all nine codes now; rules in later tasks only choose the code and the message. RA006's fix is exactly `Give the component exactly one typed parameter. With no props, write (props: Record<never, never>).`. The RA006 message carries the exact rewritten signature line for that component (Task 4). RA010's fix is exactly `Change line 1 to "use island" to ship JavaScript for this route.`

Wire `pub mod diagnostic;` in `lib.rs`.

- [ ] **Step 4: Run and verify pass**

Run: `cargo test -p runai_compiler diagnostic`
Expected: PASS, 6 tests.

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
  - `pub fn detect_mode(program: &Program, source: &str) -> Result<Mode, Diagnostic>`. It reads `program.directives.first()`, because Oxc parses a leading string-literal statement as a **directive**, not as an `ExpressionStatement` in `program.body`. A first directive whose raw source text is `"use static"` or `"use island"` (either quote style) counts. Anything else yields `RA012_MISSING_MODE` spanning byte 0..0. That includes no directives, a different first directive, or the string placed after an import (it is then an ordinary expression statement).
  - `pub fn parse_tsx<'a>(alloc: &'a Allocator, source: &'a str) -> Result<&'a Program<'a>, Vec<Diagnostic>>`. Every Oxc diagnostic becomes `RA007_AMBIGUOUS_JSX` with its primary label span, or `(0,0)` when unlabeled.

- [ ] **Step 0: Confirm the Oxc API**

Run `npx ctx7@latest library oxc "parser directives and diagnostics"`, then `npx ctx7@latest docs <id> "Program directives ParserReturn OxcDiagnostic labels"`. Check that `Program::directives`, `ParserReturn { program, diagnostics, .. }`, and the label accessor on `OxcDiagnostic` match this task in 0.159. Fix the names below before writing code.

- [ ] **Step 1: Write the failing tests**

```rust
// crates/runai_compiler/src/source.rs (tests)
#[cfg(test)]
mod tests {
    use super::*;

    fn mode(src: &str) -> Result<Mode, Diagnostic> {
        let alloc = oxc_allocator::Allocator::default();
        let program = parse_tsx(&alloc, src).unwrap();
        detect_mode(program, src)
    }

    #[test]
    fn detects_static() {
        assert!(matches!(mode("\"use static\"\nimport x from 'y'\n"), Ok(Mode::Static)));
    }

    #[test]
    fn detects_island() {
        let src = "\"use island\"\nexport default function A() { return <p/> }\n";
        assert!(matches!(mode(src), Ok(Mode::Island)));
    }

    #[test]
    fn missing_directive_is_ra012() {
        let err = mode("export default function A() { return <p/> }\n").unwrap_err();
        assert_eq!(err.code, Code::RA012);
        assert!(err.fix.contains("\"use static\""));
    }

    #[test]
    fn directive_after_an_import_is_ra012() {
        let err = mode("import x from 'y'\n\"use static\"\n").unwrap_err();
        assert_eq!(err.code, Code::RA012);
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

`detect_mode` takes `program.directives.first()` and compares `&source[span]` of its string literal, raw and including quotes, against the two accepted forms. It uses the raw text rather than the cooked value so escapes cannot smuggle a directive in. `parse_tsx` calls `Parser::new(alloc, source, SourceType::tsx()).parse()`, maps each diagnostic's first label to the span, and returns `Ok(ret.program)` when the vector is empty. It must never panic. If Step 0 shows a `panicked` flag on `ParserReturn`, treat `panicked == true` as an RA007 even when `diagnostics` is empty.

- [ ] **Step 4: Run and verify pass**

Run: `cargo test -p runai_compiler source`
Expected: PASS, 5 tests.

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
  - `pub struct ComponentInfo<'a> { pub name: &'a str, pub params: usize, pub has_props_type: bool, pub signature_span: (u32, u32), pub body_span: (u32, u32), pub returns_jsx: bool, pub is_default_export: bool }`
  - `pub fn find_components<'a>(program: &'a Program<'a>) -> Vec<ComponentInfo<'a>>`. Every function in the file that returns JSX: function declarations, and arrow or function expressions bound to a `const`, exported or not.
  - `pub fn find_route_component<'a>(program: &'a Program<'a>) -> Option<ComponentInfo<'a>>`. The default export, when it is a function declaration or an arrow/function expression.
  - `pub fn validate_route_component(info: Option<&ComponentInfo>) -> Vec<Diagnostic>`. Emits `RA011` when the default export is missing, when params > 1, or when it does not return JSX. Only `.page.tsx` files run it (Task 9).
  - `pub fn check_props(info: &ComponentInfo, source: &str) -> Vec<Diagnostic>`. Emits `RA006` when `params == 0`, or when `params == 1` and that parameter has no type annotation. The message carries the exact signature line to write, built from the real component name. For example: ``write: export default function Page(props: Record<never, never>) {``. It runs on every component in every `.tsx` file, so `_shell.tsx` is checked too.

- [ ] **Step 1: Write the failing tests**

```rust
// crates/runai_compiler/src/analyze/props.rs (tests)
#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyze::component::find_route_component;
    use crate::source::parse_tsx;

    use crate::analyze::component::{find_components, validate_route_component};
    use crate::diagnostic::Code;

    fn check(src: &str) -> Vec<crate::diagnostic::Diagnostic> {
        let alloc = oxc_allocator::Allocator::default();
        let program = parse_tsx(&alloc, src).unwrap();
        let mut d = validate_route_component(find_route_component(program).as_ref());
        for c in find_components(program) { d.extend(check_props(&c, src)); }
        d
    }

    #[test]
    fn untyped_props_is_ra006() {
        let d = check("export default function Page(props) { return <p/> }\n");
        assert_eq!(d[0].code, Code::RA006);
    }

    #[test]
    fn typed_props_is_clean() {
        let src = "type Props = { id: string }\nexport default function Page(props: Props) { return <p/> }\n";
        assert!(check(src).is_empty());
    }

    #[test]
    fn no_props_still_requires_a_type_and_shows_the_line() {
        let d = check("export default function Page() { return <p/> }\n");
        assert_eq!(d[0].code, Code::RA006);
        assert!(d[0].fix.contains("props: Record<never, never>"));
        assert!(d[0].message.contains("export default function Page(props: Record<never, never>) {"));
    }

    #[test]
    fn non_route_component_needs_a_props_type_too() {
        let src = "export default function Page(props: Record<never, never>) { return <p/> }\nfunction Badge() { return <b/> }\n";
        let d = check(src);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].code, Code::RA006);
        assert!(d[0].message.contains("function Badge(props: Record<never, never>) {"));
    }

    #[test]
    fn missing_default_export_is_ra011() {
        let d = check("export function Page(props: Record<never, never>) { return <p/> }\n");
        assert_eq!(d[0].code, Code::RA011);
    }
}
```

- [ ] **Step 2: Run and verify failure**

Run: `cargo test -p runai_compiler props`
Expected: FAIL — `find_route_component` not found.

- [ ] **Step 3: Implement**

`find_route_component` walks `program.body` for `ExportDefaultDeclaration`. It accepts `FunctionDeclaration` (named, or `"default"`), `Expression::ArrowFunctionExpression`, and `FunctionExpression`. `find_components` walks top-level function declarations and `const` declarators whose initializer is an arrow or function expression, exported or not. `returns_jsx` is true when any `ReturnStatement` argument, or an arrow's expression body, is a `JSXElement` or `JSXFragment`.

`check_props` inspects the first parameter's `type_annotation`. For the message, it rebuilds the signature line from the source slice of `signature_span` (from the start of the declaration through the `{`), with the parameter list replaced by `(props: Record<never, never>)`. Point the RA006 span at the parameter list, or at the component name when there are no parameters.

- [ ] **Step 4: Run and verify pass**

Run: `cargo test -p runai_compiler props`
Expected: PASS, 5 tests.

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
  - Internals used by codegen, added in Task 6: `_setText(anchor, fn)`, `_setAttr(node, name, fn)`, `_on(node, name, handler)`
  - **Updates are synchronous.** When `count(1)` returns, every affected `derived` reads its new value and every affected effect has already re-run. Tests and `Show`/`For`/`When` all depend on this.

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

Use a push-pull graph with version counters. Each node holds a `version` and a `deps` set, and a read inside a tracking context registers the edge. A write does three things: it bumps a global epoch, marks downstream `derived` nodes stale (they recompute lazily on their next read, checking dependency versions), and adds downstream effects to a pending set. Effects are deduplicated by node id, so the diamond test runs once. The pending set flushes **synchronously** before the outermost write returns, with no microtask. If an effect writes while a flush is running, its targets join the same flush instead of starting a nested one; RA002 rejects that pattern at compile time anyway. `owner` keeps a stack of disposal arrays, and `effect` registers its dispose with the nearest owner. No proxies, no `batch`, no `untrack`.

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
  - `template(html: string): HTMLTemplateElement`
  - `For<T>(props: { each: () => T[]; by: (item: T) => string | number; children: (item: () => T) => Node }): Node`
  - `Show(props: { when: () => boolean; fallback?: Node; children: Node }): Node`
  - `When<T>(props: { value: Accessor<Result<T>>; pending: Node; error: (e: unknown) => Node; children: (data: T) => Node }): Node`
  - `resource<T>(fn: (ctx: { signal: AbortSignal }) => Promise<T>): Accessor<Result<T>>` where `Result<T> = { status: "pending" } | { status: "error", error: unknown } | { status: "success", data: T }`
  - `mount(component: (props: Record<never, never>) => Node, root: Element): () => void`
  - `_setText(anchor: Comment, fn: () => unknown): void`. It replaces the placeholder comment with a `Text` node and keeps its `data` current inside an effect.
  - `_setAttr(node: Element, name: string, fn: () => unknown): void`, which tracks the value inside an effect, and `_on(node: Element, name: string, handler: EventListener): void`
  - **No wrapper elements.** `For`, `Show`, and `When` return a `DocumentFragment` that holds their current nodes plus an end-marker comment. Later updates insert and remove relative to that marker's `parentNode`. The DOM the agent wrote is the DOM that ships, with no extra `<div>`/`<span>`.

- [ ] **Step 1: Write the failing tests**

```js
// packages/runai/test/runtime.test.js (append)
import { For, Show, When, resource, mount, template, _setText } from "../runtime.js"

const host = (node) => { const div = document.createElement("div"); div.append(node); return div }

test("For keyed updates reuse the same DOM node for the same key", () => {
  const items = signal([{ id: 1, label: "a" }, { id: 2, label: "b" }])
  const rendered = new Map()
  const ul = document.createElement("ul")
  ul.append(For({
    each: () => items(),
    by: (i) => i.id,
    children: (item) => {
      const el = document.createElement("li")
      el.textContent = item().label
      rendered.set(item().id, el)
      return el
    },
  }))
  const nodeFor1 = rendered.get(1)
  const nodeFor2 = rendered.get(2)
  items([{ id: 2, label: "b" }, { id: 1, label: "a" }])
  expect(ul.children[0]).toBe(nodeFor2)
  expect(ul.children[1]).toBe(nodeFor1)
  expect(ul.children.length).toBe(2)
  expect(rendered.size).toBe(2)
})

test("When requires pending and renders error branch", async () => {
  const failing = resource(async () => { throw new Error("boom") })
  const div = host(When({
    value: failing,
    pending: document.createTextNode("loading"),
    error: (e) => document.createTextNode(`failed: ${e.message}`),
    children: (d) => document.createTextNode(String(d)),
  }))
  expect(div.textContent).toBe("loading")
  await new Promise((r) => setTimeout(r, 0))
  expect(div.textContent).toBe("failed: boom")
})

test("Show toggles on signal change without a wrapper element", () => {
  const on = signal(false)
  const div = host(Show({ when: () => on(), fallback: document.createTextNode("off"), children: document.createTextNode("on") }))
  expect(div.textContent).toBe("off")
  on(true)
  expect(div.textContent).toBe("on")
  expect(div.children.length).toBe(0)
})

test("_setText replaces its anchor and tracks the value", () => {
  const n = signal(1)
  const p = document.createElement("p")
  p.append("a ", document.createComment(""), " b")
  _setText(p.childNodes[1], () => n())
  expect(p.textContent).toBe("a 1 b")
  n(2)
  expect(p.textContent).toBe("a 2 b")
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

`For` keys rows in a `Map`, reorders by moving existing DOM nodes with `insertBefore` relative to its end marker, and disposes the owner of removed rows. `When` reads the accessor inside an effect and swaps between pending, error, and success nodes before its marker. `resource` runs `fn` inside `owner` and tracks signal reads so it re-runs. Each run gets a fresh `AbortController`: it always passes `{ signal }`, aborts the previous run, and drops a settled result from a superseded run so a stale response can never overwrite a newer one. `mount` creates an owner, calls the component with `{}`, appends the result, and returns the disposer.

Runtime tests need a DOM: add `happy-dom` as a dev dependency of `packages/runai` and a `test/setup.js` registered via `bun test --preload` that assigns `globalThis.document`, `globalThis.Node`, and `globalThis.Element` from a `happy-dom` `Window`.

- [ ] **Step 4: Run and verify pass**

Run: `cd packages/runai && bun test --preload ./test/setup.js`
Expected: PASS, 12 tests.

- [ ] **Step 5: Commit**

```bash
git add packages/runai
git commit -m "feat(runtime): add templates, keyed For, Show, When, resource, and mount"
```

---

### Task 7: JSX checks, type stripping, and IR lowering

**Files:**
- Create: `crates/runai_compiler/src/ir.rs`, `crates/runai_compiler/src/strip.rs`, `crates/runai_compiler/src/analyze/jsx.rs`
- Modify: `crates/runai_compiler/src/lib.rs`, `crates/runai_compiler/src/analyze/mod.rs`

**Interfaces:**
- Consumes: `parse_tsx`, `detect_mode`, `find_route_component`.
- Produces:
  - `pub fn check_jsx(program: &Program, source: &str) -> Vec<Diagnostic>`. It runs on the **original** source, so its spans are the agent's own. It emits:
    - `RA007` for a whitespace-only JSX text containing a line break between two **inline** siblings. Inline means an element whose tag is one of `a abbr b code em i kbd label mark s small span strong sub sup time u`, a non-whitespace text, or an expression container. JSX drops that space, so the agent must say which it meant. The fix is `Put both on one line, or write {" "} where you want the space.`
    - `RA014` for a capitalized or member-expression tag (component inlining and `<For>`/`<Show>`/`<When>` JSX come with a later plan), JSX nested inside an expression container, or a spread attribute or child.
  - `pub fn strip_types(source: &str) -> Result<String, Vec<Diagnostic>>`. Runs `oxc_transformer` with TypeScript stripping on and the JSX transform **off**, then `oxc_codegen`. The output is plain JS that still contains JSX.
  - `pub enum IrNode { Element { tag: String, static_attrs: Vec<(String, String)>, bindings: Vec<Binding>, children: Vec<IrNode> }, Text(String), Expr(String) }`. `Expr` is a dynamic child in its source position. Its string is JS source with types already stripped.
  - `pub enum Binding { Attr { name: String, expr: String }, Event { name: String, expr: String } }`. Dynamic text is not a binding; it is an `Expr` child, so its position is never lost.
  - `pub struct RouteIr { pub mode: Mode, pub name: String, pub statements: String, pub tree: IrNode }`. `statements` holds the route component's body statements except the JSX `return`, as stripped JS.
  - `pub fn lower_route(source: &str) -> Result<RouteIr, Vec<Diagnostic>>`. Calls `strip_types`, re-parses the result as JSX, then lowers the default export's returned JSX. It assumes `check_jsx` passed. Errors are internal invariants only, never the agent's first signal.

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
    fn expression_slot_becomes_an_expr_child_in_position() {
        let src = "\"use island\"\nexport default function P() { const n = signal(0); return <p>a {n()} b</p> }\n";
        let r = crate::ir::lower_route(src).unwrap();
        match r.tree {
            IrNode::Element { bindings, children, .. } => {
                assert!(bindings.is_empty());
                assert_eq!(children.len(), 3);
                assert!(matches!(children[1], IrNode::Expr(ref e) if e.trim() == "n()"));
            }
            other => panic!("expected element, got {other:?}"),
        }
        assert!(r.statements.contains("signal(0)"));
    }

    #[test]
    fn types_are_stripped_before_lowering() {
        let src = "\"use island\"\nexport default function P(props: Record<never, never>) { const n: number = 1; return <p>{n as number}</p> }\n";
        let r = crate::ir::lower_route(src).unwrap();
        assert!(!r.statements.contains(": number"), "{}", r.statements);
        match r.tree {
            IrNode::Element { children, .. } => {
                assert!(matches!(children[0], IrNode::Expr(ref e) if e.trim() == "n"));
            }
            other => panic!("expected element, got {other:?}"),
        }
    }

    #[test]
    fn block_level_line_breaks_are_dropped_like_jsx() {
        let src = "\"use static\"\nexport default function P() { return <div>\n  <p>a</p>\n  <p>b</p>\n</div> }\n";
        let r = crate::ir::lower_route(src).unwrap();
        match r.tree {
            IrNode::Element { children, .. } => assert_eq!(children.len(), 2),
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

```rust
// crates/runai_compiler/src/analyze/jsx.rs (tests)
#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostic::Code;
    use crate::source::parse_tsx;

    fn codes(src: &str) -> Vec<Code> {
        let alloc = oxc_allocator::Allocator::default();
        let program = parse_tsx(&alloc, src).unwrap();
        check_jsx(program, src).iter().map(|d| d.code).collect()
    }

    #[test]
    fn line_break_between_inline_siblings_is_ra007() {
        assert_eq!(codes("const a = <p><b>a</b>\n<i>b</i></p>\n"), vec![Code::RA007]);
    }

    #[test]
    fn explicit_space_is_clean() {
        assert!(codes("const a = <p><b>a</b>{\" \"}<i>b</i></p>\n").is_empty());
    }

    #[test]
    fn line_break_between_block_siblings_is_clean() {
        assert!(codes("const a = <div>\n  <p>a</p>\n  <p>b</p>\n</div>\n").is_empty());
    }

    #[test]
    fn component_tag_is_ra014() {
        assert_eq!(codes("const a = <div><Greeting name=\"Ada\" /></div>\n"), vec![Code::RA014]);
    }

    #[test]
    fn jsx_inside_an_expression_is_ra014() {
        assert_eq!(codes("const a = <div>{ok ? <b>y</b> : null}</div>\n"), vec![Code::RA014]);
    }
}
```

- [ ] **Step 2: Run and verify failure**

Run: `cargo test -p runai_compiler ir && cargo test -p runai_compiler jsx`
Expected: FAIL — `lower_route` and `check_jsx` not found.

- [ ] **Step 3: Implement**

Confirm the transformer options with ctx7 first (Global Constraints). `strip_types` must keep JSX untouched; if 0.159 has no JSX-off switch, use its "preserve" JSX runtime.

`lower_route` walks the JSX tree of the stripped source:
- Attributes with a string value become `static_attrs`, with the raw value kept.
- `onX={expr}` becomes `Binding::Event`, its name lowercased after stripping `on` (`onClick` → `click`). Other expression attributes become `Binding::Attr`.
- A `JSXExpressionContainer` child becomes `IrNode::Expr` holding the expression's source text, in place.
- Text follows the standard JSX whitespace rule (the one TypeScript and React apply):
  - text with no line break is kept verbatim;
  - text with a line break is split into lines and each line is trimmed;
  - empty lines are dropped, and what remains is joined with single spaces;
  - a whitespace-only text containing a line break disappears.
- Entities are never decoded or re-encoded.
- `statements` is the stripped source slice of every body statement before the final JSX `return`.

`check_jsx` applies the same whitespace rule to the original AST. It reports exactly the cases where the rule deletes a space between two inline siblings.

- [ ] **Step 4: Run and verify pass**

Run: `cargo test -p runai_compiler ir && cargo test -p runai_compiler jsx`
Expected: PASS, 6 + 5 tests.

- [ ] **Step 5: Commit**

```bash
git add crates/runai_compiler/src
git commit -m "feat(compiler): check JSX, strip types, and lower TSX into template-and-bindings IR"
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
  - `pub fn find_signal_bindings(program: &Program) -> SignalBindings`. Finds `const x = signal(...)` and `const y = derived(...)`, where the callee resolves to a named import from `"runai"`.
  - `pub fn check_signal_rules(program: &Program, bindings: &SignalBindings) -> Vec<Diagnostic>`. Emits `RA001`, `RA002`, and `RA009`.

**Rule semantics (exact, so the implementer does not guess):**
- A **primitive call** is a call whose callee identifier resolves, through `oxc_semantic`, to a named import binding of `signal`, `derived`, `effect`, or `resource` from `"runai"`, aliases included. A local function named `signal` is not one. An unimported `signal(...)` is not one either; `tsc` reports it once the type-check plan lands.
- A **tracking scope** is any `JSXExpressionContainer`, or the function passed to a `derived`, `effect`, or `resource` primitive call.
- A **read** is a call `x()` with zero arguments, where `x` resolves to a known accessor symbol.
- A **write** is a call `x(v)` with one argument, where `x` resolves to a known accessor symbol.
- The **nearest function** of a call is the innermost enclosing function or arrow.
- `RA001`: a read whose nearest function is the component function and which is not inside a tracking scope. A read inside an event handler, or any other nested callback such as `setTimeout(() => n())`, is **not** flagged. It reads when the callback runs, so it sees the current value. The RA001 skill documents this.
- `RA002`: a write whose nearest function is the function passed to an `effect` primitive.
- `RA009`: a write whose nearest function is the component function. That covers the body and the JSX slots, which run during render. A write inside an event handler (`onClick={() => n(n() + 1)}`) is **not** flagged, because its nearest function is the handler.

- [ ] **Step 1: Write the failing tests**

```rust
// crates/runai_compiler/src/analyze/rules.rs (tests)
#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::parse_tsx;

    // Every fixture gets the runai import, because primitives are resolved by import binding.
    fn codes(body: &str) -> Vec<String> {
        let src = format!("import {{ signal, derived, effect }} from \"runai\"\n{body}");
        let alloc = oxc_allocator::Allocator::default();
        let program = parse_tsx(&alloc, &src).unwrap();
        let b = find_signal_bindings(program);
        check_signal_rules(program, &b).iter().map(|d| d.code.as_str().to_string()).collect()
    }

    #[test]
    fn write_in_event_handler_is_not_flagged() {
        let src = "export default function P() { const n = signal(0); return <button onClick={() => n(n() + 1)}>x</button> }\n";
        assert!(codes(src).is_empty(), "the counter's increment is correct");
    }

    #[test]
    fn read_in_deferred_callback_is_not_flagged() {
        let src = "export default function P() { const n = signal(0); setTimeout(() => console.log(n()), 10); return <p/> }\n";
        assert!(codes(src).is_empty(), "reads at call time, nothing is frozen");
    }

    #[test]
    fn a_local_function_named_signal_is_not_a_primitive() {
        let src = "function signal2(v) { return () => v }\nexport default function P() { const signal = signal2; const n = signal(0); const x = n(); return <p/> }\n";
        assert!(codes(src).is_empty(), "shadowed name must not match the import");
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

Build the semantic data once. First collect the symbol ids of the `"runai"` import specifiers, then the accessor symbol ids, from `const` declarators initialized by a `signal`/`derived` primitive call. Walk every `CallExpression` in source order and resolve its callee identifier to a symbol. Compare symbol ids, never names, so shadowing cannot fool the check. For each read or write, climb the ancestor nodes once to find the nearest function and whether a tracking scope lies between the call and that function. Build diagnostics with `Diagnostic::new` and emit them in ascending span order.

- [ ] **Step 4: Run and verify pass**

Run: `cargo test -p runai_compiler rules`
Expected: PASS, 9 tests.

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
  - `pub fn analyze(source: &str, path: &str) -> Vec<Diagnostic>`. This is the single entry point used by the CLI and by the checked emitters. It runs, in order:
    1. parse;
    2. for `.page.tsx` files only: mode detection and route validation;
    3. for every `.tsx` file: props on every component, `check_jsx`, the import rule, and signal rules;
    4. for static pages: static rules.

    It calls `locate(source, path)` on every diagnostic and returns them sorted by span. Parse failures short-circuit and return only `RA007`. A file whose name starts with `_` is never treated as a page.
  - `pub fn check_static_mode(mode: Mode, program: &Program, bindings: &SignalBindings) -> Vec<Diagnostic>`

**Rule semantics:**
- When `mode == Static`, any of the following is `RA010_STATIC_VIOLATION`: a primitive call (Task 8's definition), a write to an accessor, or a JSX attribute whose name is `on` followed by an uppercase letter. Plain expressions are allowed: `{title}` where `title` is a local constant is evaluated once at build time (Task 10).
- **Import rule:** an `import` whose source is not `"runai"` is `RA014` in this plan, because there is no bundler yet to resolve it. `import type` is exempt, since types are stripped.
- The fix text is RA010's `fix_template` from Task 2.

- [ ] **Step 1: Write the failing tests**

```rust
// crates/runai_compiler/src/analyze/entry.rs (tests)
#[cfg(test)]
mod tests {
    use super::*;

    use crate::diagnostic::Code;

    const P: &str = "export default function P(props: Record<never, never>)";

    #[test]
    fn static_route_with_handler_is_ra010() {
        let src = format!("\"use static\"\n{P} {{ return <button onClick={{() => {{}}}}>x</button> }}\n");
        let d = analyze(&src, "src/routes/index.page.tsx");
        assert_eq!(d.len(), 1, "{d:?}");
        assert_eq!(d[0].code, Code::RA010);
        assert!(d[0].fix.contains("Change line 1 to \"use island\""));
        assert_eq!(d[0].line, 2);
    }

    #[test]
    fn static_route_with_signal_is_ra010() {
        let src = format!("\"use static\"\nimport {{ signal }} from \"runai\"\n{P} {{ const n = signal(0); return <p>{{n()}}</p> }}\n");
        let d = analyze(&src, "src/routes/index.page.tsx");
        assert!(d.iter().any(|x| x.code == Code::RA010));
    }

    #[test]
    fn static_route_pure_html_is_clean() {
        let src = format!("\"use static\"\n{P} {{ return <div class=\"a\">hi</div> }}\n");
        assert!(analyze(&src, "src/routes/about.page.tsx").is_empty());
    }

    #[test]
    fn static_route_with_a_plain_expression_is_clean() {
        let src = format!("\"use static\"\n{P} {{ const title = \"Hi\"; return <h1>{{title}}</h1> }}\n");
        assert!(analyze(&src, "src/routes/about.page.tsx").is_empty());
    }

    #[test]
    fn shell_file_needs_no_directive() {
        let src = "export default function Shell(props: { children: JSX.Element }) { return <main>{props.children}</main> }\n";
        assert!(analyze(src, "src/routes/_shell.tsx").is_empty());
    }

    #[test]
    fn foreign_import_is_ra014() {
        let src = format!("\"use static\"\nimport x from \"./data\"\n{P} {{ return <p>hi</p> }}\n");
        let d = analyze(&src, "src/routes/about.page.tsx");
        assert_eq!(d[0].code, Code::RA014);
    }

    #[test]
    fn syntax_error_short_circuits() {
        let src = format!("\"use static\"\n{P} {{ return <div> }}\n");
        let d = analyze(&src, "src/routes/bad.page.tsx");
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].code, Code::RA007);
    }
}
```

- [ ] **Step 2: Run and verify failure**

Run: `cargo test -p runai_compiler entry`
Expected: FAIL — `analyze` not found.

- [ ] **Step 3: Implement**

`analyze` builds an `Allocator` per call. The AST borrows it, so the function must own both and drop them together, and it returns only owned `Diagnostic`s. Sort by `span.0`, then by code. Deduplicate identical `(code, span)` pairs.

- [ ] **Step 4: Run and verify pass**

Run: `cargo test -p runai_compiler entry`
Expected: PASS, 7 tests.

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
  - `pub(crate) fn emit_prerender(route: &RouteIr) -> String`. A self-contained ESM module exporting `render(): string`, with **no imports at all**. It defines an inline `_esc` HTML-escaping helper, runs the route's `statements`, and returns a template literal. `pub(crate)` because only `emit_prerender_checked` is public: a caller cannot lower without gating on `analyze`.
  - `pub fn template_literal_body(node: &IrNode) -> String`. The HTML as the body of a JS template literal. Literal parts are escaped for template literals (`` ` ``, `\`, and `${`), and each `Expr` becomes `${_esc(<expr>)}`.
  - `pub fn emit_prerender_checked(source: &str, path: &str) -> Result<String, Vec<Diagnostic>>`, in `codegen/mod.rs`. It calls `analyze`, refuses on any error, then lowers and emits.

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

    fn body(src: &str) -> String {
        template_literal_body(&lower_route(src).unwrap().tree)
    }

    #[test]
    fn static_attrs_and_text_survive() {
        let src = "\"use static\"\nexport default function P() { return <div class=\"a\" id=\"b\">hi</div> }\n";
        assert_eq!(body(src), "<div class=\"a\" id=\"b\">hi</div>");
    }

    #[test]
    fn entities_are_not_reencoded() {
        let html = body("\"use static\"\nexport default function P() { return <p>a &amp; b</p> }\n");
        assert!(html.contains("a &amp; b"), "got {html}");
        assert!(!html.contains("&amp;amp;"));
    }

    #[test]
    fn self_closing_void_tags_are_valid_html() {
        let src = "\"use static\"\nexport default function P() { return <img src=\"/a.png\"/> }\n";
        assert_eq!(body(src), "<img src=\"/a.png\">");
    }

    #[test]
    fn a_double_quote_in_a_single_quoted_attribute_is_escaped() {
        let src = "\"use static\"\nexport default function P() { return <p title='say \"hi\"'>x</p> }\n";
        assert_eq!(body(src), "<p title=\"say &quot;hi&quot;\">x</p>");
    }

    #[test]
    fn expressions_are_escaped_at_render_time() {
        let src = "\"use static\"\nexport default function P() { const name = \"<Ada>\"; return <p>{name}</p> }\n";
        let js = emit_prerender(&lower_route(src).unwrap());
        assert!(js.contains("${_esc(name)}"), "{js}");
        assert!(js.contains("const name = \"<Ada>\""), "{js}");
    }

    #[test]
    fn backticks_in_text_cannot_break_the_template_literal() {
        let src = "\"use static\"\nexport default function P() { return <p>a ` b ${c}</p> }\n";
        let js = emit_prerender(&lower_route(src).unwrap());
        let alloc = oxc_allocator::Allocator::default();
        let ret = oxc_parser::Parser::new(&alloc, &js, oxc_span::SourceType::mjs()).parse();
        assert!(ret.diagnostics.is_empty(), "{js}");
    }
}
```

The real escaping of `<Ada>` to `&lt;Ada&gt;` is asserted end to end in Task 13, where the module actually runs.

- [ ] **Step 2: Run and verify failure**

Run: `cargo test -p runai_compiler prerender`
Expected: FAIL — `emit_prerender` not found.

- [ ] **Step 3: Implement**

`template_literal_body` writes tags with their attributes in source order, separated by single spaces, and quotes every value with `"`. Entities in values and text are never decoded or re-encoded. The one change: a literal `"` inside a value that was single-quoted in the source becomes `&quot;`, so the output stays valid HTML. Void elements (`area base br col embed hr img input link meta source track wbr`) get no closing tag.

`emit_prerender` returns:

```js
const _esc = (v) => String(v).replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c])
export function render() {
  <statements>
  return `<template_literal_body>`
}
```

It must contain no `import`. RA010 has already guaranteed that `statements` holds no primitives or handlers.

- [ ] **Step 4: Run and verify pass**

Run: `cargo test -p runai_compiler prerender`
Expected: PASS, 7 tests.

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
  - `pub(crate) fn emit_client(route: &RouteIr, runtime_url: &str) -> String`. An ESM module with exactly **one** import, from `runtime_url`. That import merges the route's own `"runai"` specifiers with the internal helpers it uses. The module ends by mounting the page into `#app`. `pub(crate)` for the same reason as `emit_prerender`: only the `*_checked` form is public.
  - `pub fn template_html(node: &IrNode) -> String`. The markup, with each `Expr` child replaced by a `<!>` placeholder comment.

**Emitted shape** (pinned, so the runtime API and the emitter cannot drift):

```js
import { signal, template, mount, _setText } from "/assets/runtime.js"
const _t0 = template("<p class=\"a\"><!></p>")
function Page(props) {
  const n = signal(0)
  const el = _t0.content.firstChild.cloneNode(true)
  _setText(el.firstChild, () => n())
  return el
}
mount(Page, document.getElementById("app"))
```

Templates sit at module scope, so each template is parsed once, not on every render. The node path to each binding is walked from `el` with `firstChild`/`nextSibling`, following the binding's position in the IR. The runtime only receives `_on` and `_setAttr` when the page uses them.

- [ ] **Step 1: Write the failing tests**

```rust
// crates/runai_compiler/src/codegen/client.rs (tests)
#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::lower_route;

    #[test]
    fn imports_only_the_runtime_url_in_one_statement() {
        let src = "\"use island\"\nimport { signal } from \"runai\"\nexport default function P() { const n = signal(0); return <p>{n()}</p> }\n";
        let js = emit_client(&lower_route(src).unwrap(), "/assets/runtime.js");
        assert!(js.contains("from \"/assets/runtime.js\""));
        assert!(!js.contains("\"runai\""), "{js}");
        assert_eq!(js.matches("import ").count(), 1, "{js}");
        assert!(js.contains("signal"), "{js}");
    }

    #[test]
    fn mounts_into_app() {
        let src = "\"use island\"\nexport default function P() { return <p>hi</p> }\n";
        let js = emit_client(&lower_route(src).unwrap(), "/assets/runtime.js");
        assert!(js.contains("mount(P, document.getElementById(\"app\"))"), "{js}");
    }

    #[test]
    fn template_is_hoisted_to_module_scope() {
        let src = "\"use island\"\nexport default function P() { return <p>hi</p> }\n";
        let js = emit_client(&lower_route(src).unwrap(), "/assets/runtime.js");
        let t = js.find("const _t0 = template(").expect("hoisted template");
        assert!(t < js.find("function P").unwrap(), "{js}");
    }

    #[test]
    fn nested_binding_walks_a_child_path() {
        let src = "\"use island\"\nimport { signal } from \"runai\"\nexport default function P() { const n = signal(0); return <div><span>{n()}</span></div> }\n";
        let js = emit_client(&lower_route(src).unwrap(), "/assets/runtime.js");
        assert!(js.contains("_setText(el.firstChild.firstChild, () => n())"), "{js}");
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
    fn typed_source_emits_parseable_javascript() {
        let src = "\"use island\"\nimport { signal } from \"runai\"\ntype Props = Record<never, never>\nexport default function P(props: Props) { const n = signal<number>(0); return <p class=\"a\">{n() as number}</p> }\n";
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

Emit in this order:
1. the merged import;
2. the hoisted `const _tN = template(...)` lines;
3. `function <Name>(props) {`, then the route's `statements` (already type-stripped), the clone, and the binding wiring, then `return el` and `}`;
4. the `mount` call.

The default export becomes a plain named function, so the module's only effect is mounting. Use `Page` when the default export is anonymous.

Bindings are wired like this:
- an `Expr` child becomes `_setText(<path to its placeholder>, () => expr)`;
- a dynamic attribute becomes `_setAttr(<path>, "name", () => expr)`;
- an event becomes `_on(<path>, "click", handler)`.

**The emitter must run `analyze` first and refuse to emit when it returns errors.** The `_setText(..., () => n())` form would otherwise silently wrap an untracked read in a function and defeat `RA001`, so Task 8's check has to fire before any codegen. Add this test alongside the others:

```rust
#[test]
fn refuses_to_emit_when_analysis_reports_errors() {
    let src = "\"use island\"\nimport { signal } from \"runai\"\nexport default function P(props: Record<never, never>) { const n = signal(0); const x = n(); return <p/> }\n";
    let err = emit_client_checked(src, "src/routes/index.page.tsx", "/assets/runtime.js").unwrap_err();
    assert_eq!(err[0].code, crate::diagnostic::Code::RA001);
}
```

This requires a second entry point in `codegen/mod.rs`: `pub fn emit_client_checked(source: &str, path: &str, runtime_url: &str) -> Result<String, Vec<Diagnostic>>`. It calls `analyze` and returns `Err` before lowering. Task 13's builder calls this one and `emit_prerender_checked`.

Only the `*_checked` forms are public. `emit_client` and `emit_prerender` are `pub(crate)`, existing only for their own unit tests, so a caller inside this crate and outside the codegen module cannot lower without gating on `analyze`: the unchecked path is unreachable at compile time from outside, not merely discouraged by convention.

- [ ] **Step 4: Run and verify pass**

Run: `cargo test -p runai_compiler client`
Expected: PASS, 8 tests.

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
- Produces: `runai check [--json] [path]`. Exit 1 on any error diagnostic, 0 otherwise. The default path is `src/routes`. It checks every `*.tsx` file under that path, recursively; `analyze` decides by file name which page rules apply. Each diagnostic's `file` is relative to the working directory, using `/` separators.

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
        "\"use island\"\nimport { signal } from \"runai\"\nexport default function P(props: Record<never, never>) { const n = signal(0); const x = n(); return <p/> }\n",
    ).unwrap();
    let out = run(&["check", "--json"], tmp.path());
    assert_eq!(out.status.code(), Some(1));
    let parsed: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(parsed[0]["code"], "RA001_UNTRACKED_READ");
    assert_eq!(parsed[0]["file"], "src/routes/index.page.tsx");
    assert_eq!(parsed[0]["line"], 3);
}

#[test]
fn check_also_covers_non_route_components() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("src/routes")).unwrap();
    std::fs::write(
        tmp.path().join("src/routes/_shell.tsx"),
        "export default function Shell(props) { return <main>{props.children}</main> }\n",
    ).unwrap();
    let out = run(&["check", "--json"], tmp.path());
    assert_eq!(out.status.code(), Some(1));
    let parsed: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(parsed[0]["code"], "RA006_UNANNOTATED_PROPS");
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
    let out = run(&["check", "--json"], tmp.path());
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty(), "exit 2 writes nothing to stdout");
    assert!(String::from_utf8_lossy(&out.stderr).contains("src/routes"));
}
```

Add `tempfile` and `serde_json` as dev-dependencies of `runai_cli`.

- [ ] **Step 2: Run and verify failure**

Run: `cargo test -p runai_cli`
Expected: FAIL — binary prints its version instead of running `check`.

- [ ] **Step 3: Implement**

Parse `std::env::args` by hand (no clap dependency in this plan). `check` walks the path for `*.tsx` files in sorted order, so output is deterministic, and calls `analyze(source, relative_path)` on each. With `--json` it prints `Diagnostic::render_all_json` of everything; otherwise it prints `render_human` per diagnostic. Exit codes: `0` clean, `1` errors found, `2` usage error or missing directory. Exit 2 writes only to stderr.

- [ ] **Step 4: Run and verify pass**

Run: `cargo test -p runai_cli`
Expected: PASS, 4 tests.

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
- Consumes: `analyze`, `emit_prerender_checked`, `emit_client_checked`.
- Produces: `runai build [--out dist]` producing, for each `*.page.tsx`:
  - static: `dist/<route>/index.html`, with no `<script>`
  - island: `dist/<route>/index.html` containing an empty `<div id="app"></div>` and one `<script type="module" src="/assets/<route>.js">`, plus `dist/assets/<route>.js`. Island prerender, meaning a server-rendered first paint, is deferred, so the island's container starts empty.
  - when any island exists: `dist/assets/runtime.js`, copied from the `runtime` path in `runai.json`, resolved relative to `runai.json`
  - Prerender modules are run with `bun` when it is on `PATH`, otherwise with `node` (spec §5). The build fails with exit 1 when neither exists or the module throws.
  - The build is all-or-nothing. Everything is written to `<out>.tmp` and renamed over `<out>` only on success, so a failed build leaves no partial `dist/`.

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
    write_app(tmp.path(), "\"use static\"\nimport { signal } from \"runai\"\nexport default function P(props: Record<never, never>) { const n = signal(0); return <p>{n()}</p> }\n");
    let status = std::process::Command::new(env!("CARGO_BIN_EXE_runai"))
        .args(["build", "--out", "dist"]).current_dir(tmp.path()).status().unwrap();
    assert!(!status.success());
    assert!(!tmp.path().join("dist/index.html").exists());
}

#[test]
fn static_expressions_are_evaluated_and_escaped() {
    let tmp = tempfile::tempdir().unwrap();
    write_app(tmp.path(), "\"use static\"\nexport default function P(props: Record<never, never>) { const name = \"<Ada>\"; return <p>Hi {name}</p> }\n");
    let status = std::process::Command::new(env!("CARGO_BIN_EXE_runai"))
        .args(["build", "--out", "dist"]).current_dir(tmp.path()).status().unwrap();
    assert!(status.success());
    let html = std::fs::read_to_string(tmp.path().join("dist/index.html")).unwrap();
    assert!(html.contains("<p>Hi &lt;Ada&gt;</p>"), "{html}");
}

#[test]
fn dynamic_segment_is_ra014_not_silent_output() {
    let tmp = tempfile::tempdir().unwrap();
    write_app(tmp.path(), "\"use static\"\nexport default function P(props: Record<never, never>) { return <p>hi</p> }\n");
    std::fs::create_dir_all(tmp.path().join("src/routes/blog")).unwrap();
    std::fs::write(tmp.path().join("src/routes/blog/[slug].page.tsx"),
        "\"use static\"\nexport default function P(props: Record<never, never>) { return <p>post</p> }\n").unwrap();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_runai"))
        .args(["build", "--out", "dist"]).current_dir(tmp.path()).output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("RA014_NOT_SUPPORTED_YET"));
    assert!(!tmp.path().join("dist").exists());
}
```

`write_app` creates `src/routes/index.page.tsx` and a `runai.json` containing `{ "runtime": "<abs path to packages/runai/runtime.js>" }`. Find the runtime from `env!("CARGO_MANIFEST_DIR")`.

- [ ] **Step 2: Run and verify failure**

Run: `cargo test -p runai_cli build`
Expected: FAIL — `build` is not a recognized command.

- [ ] **Step 3: Implement**

Route path mapping: strip `.page.tsx`. `index` maps to `dist/index.html`, `about` to `dist/about/index.html`, any other name `x` to `dist/x/index.html`, and nested directories map the same way (`blog/post` → `dist/blog/post/index.html`). `_shell.tsx` and any file beginning with `_` are not routes and are skipped. Run `analyze` over **every** route first. If any route has errors, or any path segment is a `[param]`, print the diagnostics to stderr, exit 1, and write nothing. A `[param]` segment gets `Diagnostic::new(Code::RA014, (0, 0), "dynamic route segments are not supported yet")`, located at the file.

For each static route:
1. write the emitted module to `<tmp>/<name>.mjs`;
2. run `bun -e "import('./<name>.mjs').then(m => process.stdout.write(m.render()))"`, or the same with `node --input-type=module -e`;
3. capture stdout and wrap it in the document shell: `<!doctype html><html><head><meta charset="utf-8"><title>runai</title></head><body><div id="app">…</div></body></html>`.

For island routes, emit the same shell with an empty `#app` and a module script pointing at `/assets/<name>.js`, write the route module, and copy the runtime. A failed `bun`/`node` invocation prints its stderr, exits 1, and removes `<out>.tmp`.

- [ ] **Step 4: Run and verify pass**

Run: `cargo test -p runai_cli build`
Expected: PASS, 5 tests.

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
- Produces: `runai dev [--port 5173] [--background]` and `runai dev stop`:
  - The server binds **`127.0.0.1` only** and serves `dist/` over HTTP.
  - **Before serving any `.html` request**, it rebuilds if any file under `src/` or `runai.json` has a newer mtime than the last build. A failed rebuild keeps serving the last good `dist/` and records the diagnostics.
  - `GET /_runai/status` returns `{"running":true,"pid":N,"port":N,"root":"<abs>","diagnostics":[…]}`, where `diagnostics` comes from the last build.
  - `POST /_runai/shutdown` answers `{"stopping":true}` and exits the server process. This is how `stop` works, so the CLI never kills a pid.
  - `--background` returns once the port accepts connections, prints the status JSON to stdout, and writes `.runai/dev.json` containing `{pid,port,root}`.
  - A second `--background` reads `.runai/dev.json` and calls `/_runai/status` on that port. If the answer has the same `pid` and `root`, it prints that JSON and exits 0. Otherwise the lockfile is stale (a dead server, or a reused port or pid), so it starts a new server and overwrites the lockfile.
  - `runai dev stop` reads `.runai/dev.json`, verifies `pid` and `root` the same way, sends `POST /_runai/shutdown`, waits for the port to close, and deletes the lockfile. With no lockfile, a refused connection, or a mismatched answer, it deletes the lockfile if present and exits 0. It never signals a process it has not just verified.
  - A request path that is not a normalized relative path inside `dist/` (it contains `..`, a backslash, a drive prefix, or decodes to one of those) gets `404` and is never read from disk.

- [ ] **Step 1: Write the failing integration tests**

```rust
// crates/runai_cli/tests/dev.rs
use std::io::{Read, Write};

/// Stops the background server even when an assertion fails, so tests never leak processes.
struct StopOnDrop(std::path::PathBuf);
impl Drop for StopOnDrop {
    fn drop(&mut self) {
        let _ = std::process::Command::new(env!("CARGO_BIN_EXE_runai"))
            .args(["dev", "stop"]).current_dir(&self.0).status();
    }
}

/// Minimal std-only HTTP client: no reqwest, no async runtime.
fn http(port: u64, method: &str, path: &str) -> (u16, String) {
    let mut s = std::net::TcpStream::connect(("127.0.0.1", port as u16)).unwrap();
    write!(s, "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\nContent-Length: 0\r\n\r\n").unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).unwrap();
    let code = out[9..12].parse().unwrap();
    let body = out.split("\r\n\r\n").nth(1).unwrap_or("").to_string();
    (code, body)
}

#[test]
fn background_twice_reports_the_same_pid() {
    let tmp = tempfile::tempdir().unwrap();
    write_min_app(tmp.path());
    let _guard = StopOnDrop(tmp.path().to_path_buf());
    let first = run_json(&["dev", "--background", "--port", "0"], tmp.path());
    let second = run_json(&["dev", "--background", "--port", "0"], tmp.path());
    assert_eq!(first["pid"], second["pid"]);
    assert_eq!(first["running"], true);
    let (code, body) = http(first["port"].as_u64().unwrap(), "GET", "/_runai/status");
    assert_eq!(code, 200);
    assert!(body.contains("\"running\":true"));
}

#[test]
fn stop_shuts_the_server_down() {
    let tmp = tempfile::tempdir().unwrap();
    write_min_app(tmp.path());
    let _guard = StopOnDrop(tmp.path().to_path_buf());
    let first = run_json(&["dev", "--background", "--port", "0"], tmp.path());
    let port = first["port"].as_u64().unwrap() as u16;
    assert!(std::process::Command::new(env!("CARGO_BIN_EXE_runai"))
        .args(["dev", "stop"]).current_dir(tmp.path()).status().unwrap().success());
    assert!(std::net::TcpStream::connect(("127.0.0.1", port)).is_err());
    assert!(!tmp.path().join(".runai/dev.json").exists());
}

#[test]
fn stop_when_not_running_exits_zero() {
    let tmp = tempfile::tempdir().unwrap();
    write_min_app(tmp.path());
    assert!(std::process::Command::new(env!("CARGO_BIN_EXE_runai"))
        .args(["dev", "stop"]).current_dir(tmp.path()).status().unwrap().success());
}

#[test]
fn path_traversal_is_404() {
    let tmp = tempfile::tempdir().unwrap();
    write_min_app(tmp.path());
    std::fs::write(tmp.path().join("secret.txt"), "nope").unwrap();
    let _guard = StopOnDrop(tmp.path().to_path_buf());
    let port = run_json(&["dev", "--background", "--port", "0"], tmp.path())["port"].as_u64().unwrap();
    assert_eq!(http(port, "GET", "/../secret.txt").0, 404);
    assert_eq!(http(port, "GET", "/%2e%2e/secret.txt").0, 404);
}

#[test]
fn edits_are_rebuilt_on_the_next_page_request() {
    let tmp = tempfile::tempdir().unwrap();
    write_min_app(tmp.path());
    let _guard = StopOnDrop(tmp.path().to_path_buf());
    let port = run_json(&["dev", "--background", "--port", "0"], tmp.path())["port"].as_u64().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(1100)); // coarse mtime resolution on some filesystems
    std::fs::write(tmp.path().join("src/routes/index.page.tsx"),
        "\"use static\"\nexport default function P(props: Record<never, never>) { return <p>changed</p> }\n").unwrap();
    assert!(http(port, "GET", "/").1.contains("<p>changed</p>"));
}
```

`write_min_app` writes a static `index.page.tsx` rendering `<p>hi</p>` and a `runai.json`. `run_json` runs the binary and parses stdout as JSON. Only `serde_json` and `tempfile` are dev-dependencies.

- [ ] **Step 2: Run and verify failure**

Run: `cargo test -p runai_cli dev`
Expected: FAIL — `dev` is not a recognized command.

- [ ] **Step 3: Implement**

Build into `dist/` first, then serve with a hand-written `std::net::TcpListener` loop on `127.0.0.1`. Parse the request line and percent-decode the path. Reject anything Step 1's traversal rule names, map `/` and `/x/` to `index.html`, and serve files with a guessed `Content-Type`. Answer `/_runai/status` and `/_runai/shutdown` from memory.

`--background` re-executes `std::env::current_exe()` with a hidden `--serve-only` flag, detached with stdio set to null, then polls `.runai/dev.json` and the port for up to 5 seconds. Port `0` means "pick a free port": the child writes the port it bound to into `.runai/dev.json`, and the parent reports it in its stdout JSON.

- [ ] **Step 4: Run and verify pass**

Run: `cargo test -p runai_cli dev`
Expected: PASS, 5 tests.

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
- Consumes: `Code::skill_file`, `Code::as_str`, `Code::fix_template`.
- Produces:
  - `runai_compiler::skills::skill_body(code: Code) -> String`, the Markdown for one code.
  - `runai skills [--out <dir>]` writes one file per code; the default `--out` is `node_modules/runai/skills/` (spec §7.9). It is a **repair** command. The primary copy is `packages/runai/skills/`: generated, committed, and listed in the package's `"files"`. So every `npm install` already puts the skills at `node_modules/runai/skills/`, and a reinstall cannot wipe them.

- [ ] **Step 1: Write the failing test**

```rust
// crates/runai_cli/tests/skills.rs
#[test]
fn writes_one_file_per_code_with_frontmatter() {
    let tmp = tempfile::tempdir().unwrap();
    let status = std::process::Command::new(env!("CARGO_BIN_EXE_runai"))
        .args(["skills", "--out", "skills"]).current_dir(tmp.path()).status().unwrap();
    assert!(status.success());
    for code in ["RA001_UNTRACKED_READ", "RA002_EFFECT_WRITE", "RA010_STATIC_VIOLATION", "RA012_MISSING_MODE", "RA014_NOT_SUPPORTED_YET"] {
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

Each skill body has frontmatter `---\nname: <code>\ndescription: <one line>\n---` and three sections:
- `## What it means`, the code's `explain`;
- `## Why it is an error`, written per code;
- `## Fix`, which is `code.fix_template()` verbatim, the same function `Diagnostic::new` uses.

The diagnostic and the skill therefore cannot drift. RA001's skill also documents the deferred-callback case from Review Focus #2. RA014's skill lists each unsupported construct and the plan that adds it.

- [ ] **Step 3b: Add the drift tests**

```rust
// crates/runai_compiler/src/skills.rs (tests)
#[test]
fn every_code_has_a_skill_whose_fix_matches_the_diagnostic() {
    for code in crate::diagnostic::Code::all() {
        let body = crate::skills::skill_body(*code);
        assert!(body.contains(code.fix_template()), "fix drifted for {}", code.as_str());
    }
}

#[test]
fn committed_skills_match_the_generator() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packages/runai/skills");
    for code in crate::diagnostic::Code::all() {
        let committed = std::fs::read_to_string(dir.join(code.skill_file()))
            .unwrap_or_else(|_| panic!("missing packages/runai/skills/{}; run `runai skills --out packages/runai/skills`", code.skill_file()));
        assert_eq!(committed, crate::skills::skill_body(*code), "stale {}", code.skill_file());
    }
}
```

Then generate the committed copy with `cargo run -p runai_cli -- skills --out packages/runai/skills`, and add `"files": ["runtime.js", "index.d.ts", "skills"]` to `packages/runai/package.json`.

- [ ] **Step 4: Run and verify pass**

Run: `cargo test -p runai_cli skills && cargo test -p runai_compiler skills`
Expected: PASS, 3 tests.

- [ ] **Step 5: Commit**

```bash
git add crates/runai_cli crates/runai_compiler packages/runai
git commit -m "feat: generate per-code SKILL.md files from the diagnostic source of truth"
```

---

### Task 16: Example app, end-to-end verification, and size budget

**Files:**
- Create: `examples/counter/runai.json`, `examples/counter/src/routes/_shell.tsx`, `examples/counter/src/routes/index.page.tsx`, `examples/counter/src/routes/about.page.tsx`, `packages/runai/test/smoke-island.mjs`, `scripts/e2e.sh`, `scripts/size-budget.sh`

**Interfaces:**
- Consumes: every command above.
- Produces: a runnable example, an island smoke test that clicks the real build output, and two scripts CI can call.

- [ ] **Step 1: Write the failing end-to-end script**

```bash
#!/usr/bin/env bash
# scripts/e2e.sh
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root/examples/counter"
out="$(mktemp)"
cargo run --quiet --manifest-path "$root/Cargo.toml" -p runai_cli -- check --json > "$out"
test "$(cat "$out")" = "[]" || { cat "$out"; exit 1; }
cargo run --quiet --manifest-path "$root/Cargo.toml" -p runai_cli -- build --out dist
test -f dist/index.html
test -f dist/about/index.html
grep -q '<script type="module" src="/assets/index.js">' dist/index.html
grep -q '<div id="app"></div>' dist/index.html
# `! grep` is exempt from `set -e`, so it would never fail the script; test explicitly.
if grep -q '<script' dist/about/index.html; then echo "static route ships JS"; exit 1; fi
(cd "$root/packages/runai" && bun test/smoke-island.mjs "$root/examples/counter/dist")
echo "e2e ok"
```

- [ ] **Step 2: Run it and watch it fail**

Run: `bash scripts/e2e.sh`
Expected: FAIL at the first `cd`, because `examples/counter` does not exist yet.

- [ ] **Step 3: Write the example**

`runai.json` is `{ "runtime": "../../packages/runai/runtime.js" }`.

`index.page.tsx`:

```tsx
"use island"
import { signal, derived } from "runai"

export default function Counter(props: Record<never, never>) {
  const count = signal(0)
  const parity = derived(() => (count() % 2 === 0 ? "even" : "odd"))
  return <button onClick={() => count(count() + 1)}>{count()} — {parity()}</button>
}
```

`about.page.tsx`: `"use static"`, a `const title = "About"`, an `<h1>{title}</h1>`, and two paragraphs of text. This exercises a build-time expression in a static route.

`_shell.tsx`: a `props: { children: JSX.Element }` component wrapping children in a `<main>`. This plan authors and checks it, but the builder does **not** apply it; shell composition arrives with the router plan. It exists so that `runai check` exercises a non-route component with children.

- [ ] **Step 4: Write the island smoke test**

```js
// packages/runai/test/smoke-island.mjs — usage: bun test/smoke-island.mjs <dist dir>
import { plugin } from "bun"
import { Window } from "happy-dom"
import { readFileSync } from "node:fs"
import { join, resolve } from "node:path"

const dist = resolve(process.argv[2])
plugin({
  name: "dist-assets",
  setup(build) {
    build.onResolve({ filter: /^\/assets\// }, (args) => ({ path: join(dist, args.path) }))
  },
})

const window = new Window()
for (const k of ["window", "document", "Node", "Element", "Text", "Comment", "DocumentFragment", "HTMLTemplateElement"]) {
  globalThis[k] = k === "window" ? window : window[k] ?? window.document[k]
}
const html = readFileSync(join(dist, "index.html"), "utf8")
document.body.innerHTML = html.slice(html.indexOf("<body>") + 6, html.indexOf("</body>")).replace(/<script[^>]*><\/script>/g, "")
await import("/assets/index.js")

const button = document.querySelector("#app button")
const expect = (want) => {
  if (button?.textContent !== want) { console.error(`smoke: expected "${want}", got "${button?.textContent}"`); process.exit(1) }
}
expect("0 — even")
button.click()
expect("1 — odd")
console.log("smoke ok")
```

Check with ctx7 that Bun 1.4 runtime plugins support `onResolve` for bare absolute specifiers. If they do not, fall back to rewriting `/assets/` to a `file://` URL in a temporary copy of `index.js`, and say so in the commit.

- [ ] **Step 5: Run e2e and verify pass**

Run: `bash scripts/e2e.sh`
Expected: `smoke ok`, then `e2e ok`. This is the first point where the slice is proven to run: a real build, mounted, clicked, and updated.

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

Run: `cargo test --workspace && (cd packages/runai && bun test --preload ./test/setup.js) && bash scripts/e2e.sh && bash scripts/size-budget.sh`
Expected: all Rust and runtime tests pass, `smoke ok`, `e2e ok`, and a byte count at or under 6144.

```bash
git add -A
git commit -m "feat: ship counter example with end-to-end and runtime size-budget checks"
```

---

## Deferred to later plans

Explicitly not in this plan, in dependency order:

1. **Eval pilot.** 3–5 of the §7.10 tasks (counter, todo, async list with error path), runai against a React control, one model, with a checked-in result table. It comes first because it is the cheapest test of the thesis (§9.4). If the numbers do not move, the plans after it change.
2. **Type checking.** `tsc --noEmit` alongside `check`, normalized into the diagnostic schema with `fix: null` (§5, §7.5). It also supplies the `JSX` namespace and `IntrinsicElements` in `index.d.ts`. Until it lands, an unimported `signal(...)` is not reported (Task 8).
3. **Components and control flow in JSX.** Compile-time component inlining (§7.2), `<For>`/`<Show>`/`<When>` lowering, and JSX inside expressions. Each lifts a slice of `RA014`.
4. **Router and CSS.** Client navigation, shell composition, `[slug]` params, `.css` imports, `RA013`.
5. **Island prerender (§7.3).** The island's markup is rendered into its container at build time and replaced on mount. Until then, an island's container starts empty.
6. **Remaining rules.** `RA003`, `RA004`, `RA005` (with the dev-only readonly proxies of §7.4), `RA008`.
7. **`runai inspect`.** happy-dom island boot, `--click`/`--type`/`--snapshot`. Task 16's smoke script is its seed.
8. **`runai fmt`.** The canonical formatter.
9. **Dev server `--json` logs** (§7.9) and file-watch push instead of rebuild-on-request.
10. **Bundling.** Rolldown, once user imports or more than one island per page exist. That lifts the import slice of `RA014`.
11. **Eval harness.** The full `bench/`: 20 tasks, the checked-in React control, and `runai bench`.
12. **Perf benchmark.** The js-framework-benchmark subset harness at `bench/perf/`.

## Self-Review

- **Spec coverage:**
  - §7.1 authoring model → Tasks 5–8
  - §7.2 components/props → Task 4 (inlining deferred, item 3)
  - §7.3 static/island + 0 KB → Tasks 9, 10, 13 (island prerender deferred, item 5)
  - §7.4 rules → Tasks 7, 8, 9 (subset; the rest deferred, item 6)
  - §7.5 compiler → Tasks 3, 7, 10, 11
  - §7.6 runtime → Tasks 5, 6
  - §7.9 tooling → Tasks 12–15
  - §9 criteria 1, 5, 6 → Tasks 13, 15, 16. Criteria 2–4 are covered by deferred items 1, 11 and 12.
- **Divergences from the spec, chosen on purpose:**
  - `RA014_NOT_SUPPORTED_YET` is a new code, so that every refusal in this plan is coded.
  - Diagnostics gain `file`/`line`/`col`.
  - A propless component writes `props: Record<never, never>`. This reconciles §7.2's RA006 with RA011.
  - `dev stop` uses a shutdown endpoint instead of killing a pid.
  - Skills ship inside the package, and `runai skills` is a repair command.

  Update the spec alongside this plan if these hold.
- **Gaps this plan knowingly leaves:**
  - `When`'s error branch is unit-tested in the runtime, but it is not reachable from JSX until item 3.
  - Islands boot into an empty container until item 5.
  - §7.7's shell and params, §7.8's CSS, and §7.9's `inspect`/`fmt` are deferred.

  None is silently dropped: each is either coded as `RA014` at the point an agent would hit it, or listed above.
- **Step scan:** every step names a command with an expected outcome. Every "watch it fail" step fails for a reason that the next step fixes.
- **Type consistency:**
  - `Accessor<T>` is defined once in Task 5 and reused in Task 6 (`When.value`, `resource`).
  - `Code::fix_template` feeds both `Diagnostic::new` (Task 2) and the skill bodies (Task 15).
  - `emit_prerender_checked`/`emit_client_checked` match their Task 13 consumer.
  - `_setText(anchor, fn)` has one signature in Task 6, Task 11 and the pinned shape.
- **Review Focus:** each of the six items has a test in the task named:
  1. event-handler read and write → Task 8, two tests
  2. deferred callback → Task 8
  3. whitespace → Task 7; entities → Task 10
  4. JSX syntax error → Task 3
  5. no-props component → Task 4
  6. typed TSX to JS → Tasks 7 and 11
- **Proportion:** the plan pins test assertions and signatures rather than implementation bodies, except where a rule's exact semantics would otherwise be guesswork.
