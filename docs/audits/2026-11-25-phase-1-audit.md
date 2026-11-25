# runai phase 1 — audit findings

Three parallel audits (architecture, code quality, testing) plus independent
adjudication of every load-bearing claim. A fourth (performance) was still
running when this was written; its results belong in a separate section.

**Baseline, verified in this session:**

| Check | Result |
|---|---|
| `cargo test --workspace` | **589 passed, 0 failed** |
| `cargo clippy --workspace --all-targets` | **0 warnings** |
| `cargo check --workspace` | clean |
| `scripts/size-budget.sh` | **2194 / 6144 bytes gzipped**, `size ok` |
| Runtime tests (`bun test`) | **78 passed** |
| `unsafe` blocks | **0** |
| TODO / FIXME markers | **1** |

Every finding below marked CONFIRMED was reproduced by running the built
binary. Findings marked READ-ONLY come from code reading and were not
executed — that distinction is stated because three of the four auditors
independently claimed facts they had not run.

---

## Critical

### C1. `runai check` crashes the process on deeply nested JSX — CONFIRMED

`runai check` aborts with a stack overflow rather than emitting a diagnostic:

```
thread 'main' (112097) has overflowed its stack
fatal runtime error: stack overflow, aborting
exit 134 (SIGABRT)
```

Threshold measured by bisection on `<div>` nesting depth. **The threshold
depends on build profile, and both fail:**

| Depth | debug build | release build |
|---|---|---|
| 2 000 | exit 0 | exit 0 |
| 3 000 | exit 0 | — |
| 4 000 | exit 0 | — |
| **4 500** | **overflow, exit 134** | — |
| 10 000 | — | exit 0 |
| **20 000** | — | **overflow, exit 134** |
| 40 000 | — | overflow, exit 134 |

Exit 134 is `SIGABRT`, not a catchable panic — the process dies and no
diagnostic reaches the agent. This violates the project's own rule that the
compiler must never abort on malformed input. A 220 KB generated file (4 500
nested divs) is well within what an agent will produce, and the release
threshold of 20 000 is reachable by generated markup.

Note the stack frames are much larger in debug, so the debug threshold is not
the shipping threshold — an agent running a debug binary during development
hits this far sooner than a release user would.

Recursion sites, all unbounded on user-controlled depth: `jsx.rs:1273`
(`walk_jsx_element`), `ir.rs:719` (`Lowerer::element`),
`prerender.rs:246` (`write_node`), `jsx.rs:2175/2196`
(`element_has_binding`/`child_has_binding`).

**Remedy:** a depth counter threaded through the JSX walk that emits a
diagnostic past a fixed limit (Oxc's own parser has one; the crash occurs in
this project's recursive passes). Pin it with a test at the limit plus one
just inside it, and run that test under both profiles.

### C2. `check` passes two trees that `build` refuses — CONFIRMED

This is the documented F2 "check/build asymmetry", fixed for `[slug]` but not
for its two siblings. Both reproduced:

```
src/routes/.hidden/index.page.tsx
  runai check .   → exit 0, no output
  runai build     → error: has the dot-prefixed directory `.hidden` …

src/routes/RUNTIME.page.tsx      ("use island")
  runai check .   → exit 0, no output
  runai build     → error: asset path is `assets/RUNTIME.js` — reserved for the runtime copy
```

A CI that runs only `runai check` goes green on a tree that cannot build — the
precise false-green the F2 comment says must not recur. `build` guards both
(`build.rs:1106`, `build.rs:1411`) and both are tested, but only in
`tests/build.rs`. `tests/check.rs` has no test for either.

**Remedy:** extend `tests/check.rs` with both trees, asserting exit 1 and the
same refusal message, mirroring `check_refuses_a_dynamic_route_segment_like_build`.

---

## Important

### I1. The framework's own whitespace fix costs runtime bytes the static path does not pay — CONFIRMED

`RA007`'s advice is "write `{" "}` where you want the space". That spelling
compiles very differently per mode. Same source, `"use static"`:

```html
<p>a b</p>
```

Same source, `"use island"`:

```js
const _c = (root) => { const w = document.createTreeWalker(root, 128), a = []; … }
const _t0 = template(`<p>a<!>b</p>`)
  const _a = _c(_el)
  _setText(_a[0], () => " ")
```

So a literal space becomes a comment marker in the template, a full
TreeWalker pass over the fragment, and an effect that writes a text node at
mount. The diagnostic teaches a spelling that costs runtime bytes, in a
project whose hard constraint is a 6 KB budget and whose second axis is
near-hand-written DOM performance.

Root cause: `Lowerer::children` (`ir.rs:863-873`) turns every
`JSXExpressionContainer` into `IrNode::Expr` opaque source, and
`collect_sites` (`client.rs:269-305`) turns every `IrNode::Expr` into a text
site. Nothing asks whether the expression is a literal.

**Remedy:** constant-fold at the IR boundary. In `Lowerer::children`, when the
expression is a string/numeric literal or a substitution-free template, append
`IrNode::Text` instead of `IrNode::Expr`. Both emitters already treat
`IrNode::Text` as template text with no marker, so no emitter change is
needed. Pin with a `client.rs` test asserting `!js.contains("_setText")` for
`{" "}` — the mirror of `static_text_is_in_the_template_not_the_runtime`,
which today covers only bare JSX text.

### I2. `analyze` imports `codegen`, against the plan's stated rule — CONFIRMED

`analyze/emitted_names.rs:715` calls
`crate::codegen::island_reserved_names(source, program)` in production code.
The plan states: *"`analyze/` produces diagnostics only and never imports
`codegen/`."*

Consequence is not stylistic: `runai check` now lowers the page, and the
dependency direction is maintained by a convention about not calling the gate
rather than by the module graph — `codegen/mod.rs:198-202` concedes the
recursion hazard in a comment.

**Remedy:** move `used_helpers` into `ir.rs` (it is a pure `IrNode` walk, the
same shape as `emitted_module_parts`), then `island_reserved_names` becomes
private to `analyze::emitted_names` and `codegen` keeps only the emitter.

### I3. A route is lowered up to three times per emit — CONFIRMED

`lower_route` is called at `codegen/mod.rs:99`, `:156`, `:219` and `:291`.
`emit_client_checked` lowers at `:291`, then `client_imports` lowers again at
`:156`, and `island_reserved_names` again at `:219`. `analyze` additionally
runs semantic analysis three times (`entry.rs`: `find_signal_bindings`,
`check_reachable_expressions`, `check_emitted_names`).

The comment at `analyze/entry.rs:150-155` says *"This is the one second
semantic build in the pipeline, and it is deliberate."* That is now false by
two. Per the project's own rule — a message may state only a mechanism the
code has — the comment is itself a defect.

**Remedy:** compute `lower_route` once, thread `&RouteIr` through
`check_reachable_expressions`, `check_emitted_names`, and `client_imports`.
Signature change in `analyze/entry.rs::check` only.

### I4. The only real-ESM artifact test asserts nothing when `node` is absent — CONFIRMED

`tests/build.rs:1110-1113`:

```rust
Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
    eprintln!("node is not installed: the ESM check was skipped");
}
```

The test passes. It is the only build-level test that validates the island
artifact with a real parser, and it exists to catch the duplicate-export
defect — which is therefore unpinned whenever `node` is missing. It ran in my
session only because I installed Node.

Contrast `analyze/emitted_names.rs:1800` (`assert_renders`), which degrades a
missing `node` to an **in-process Oxc parse**. That is the correct shape.

**Remedy:** use the same fallback. Re-parse the artifact with `oxc_parser`
(`SourceType::mjs`) instead of `continue`/`eprintln`.

### I5. RA007's span is never asserted beyond "non-zero" — CONFIRMED

Every JSX-syntax-error assertion is `assert!(errs[0].span.0 > 0)`
(`source.rs:285`, `ir.rs:1474`). A zero-width span at EOF passes. The only
exact-span test (`source.rs:370`) uses a **TypeScript** error, not JSX.

Probed against the binary, the common `<div> }` case reports line 3 col 16 and
the span slices nothing meaningful. A regression to a zero-width span would
not fail any test.

**Remedy:** one fixture per JSX-error shape (`<div> }`, `class=>`, `{a b}`)
asserting `&src[span.0..span.1]` equals the offending construct — mirroring
`a_span_slices_the_text_the_agent_wrote`, which already does this for
whitespace.

### I6. RA011's fix is never applied in a test, so a broken fix passes — READ-ONLY

`component.rs:1580` (`following_the_missing_export_fix_is_clean`) analyses an
**already valid** source and asserts `codes.is_empty()`. It never constructs
the violation, never reads the fix, never re-checks. Deleting RA011's fix text
fails no assertion — while RA006 (`props.rs:495`) and RA014
(`instruction.rs:584`) both apply their repairs verbatim.

Same gap for RA010: its fix ("change to island") is never applied to a
violating file in a test, though I verified by hand that it works.

**Remedy:** mirror `following_the_propless_fix_is_clean` for RA011 and RA010.

### I7. `every_repair_applied_by_hand_is_accepted` only rejects two codes — READ-ONLY

`instruction.rs:657` filters `matches!(d.code, Code::RA014 | Code::RA011)`. A
"repair" that traded one refusal for RA001/RA002/RA006/RA009/RA010 would pass,
which is the repair-loop class the invariant exists to forbid.

**Remedy:** filter on any error diagnostic and assert the repaired source
analyses clean.

### I8. `client_imports` is public and analyses under a fabricated path — READ-ONLY

`codegen/mod.rs:322-327` passes the literal `"route.page.tsx"` so page-gated
rules run, then `emit_client_checked` re-analyses with the real path. A caller
wanting only the import list gets one computed under route classification. The
only legitimate consumer already has the real path in hand.

**Remedy:** make `client_imports` take `path`, delete `analyze_source`, and
drop the redundant second `analyze`.

### I9. A function-valued attribute diverges between modes, and the static side ships the function source as the class name — CONFIRMED

`attr_value` (`client.rs:348-368`) decides "wrap in `() => …`" by testing
whether the expression text contains `(`. Runtime `_setAttr`
(`runtime.js:673-679`) does `typeof value === "function" ? value() : value`.

Same source, `"use island"`, for `const cls = () => "hello"`:

```js
_setAttr(_a[0].parentNode, "class", cls)    // unwrapped
```

Run against the real runtime, `_setAttr` calls it:
`setAttribute(class, "hello")` — behaviour the author never wrote, executed
inside an effect.

Same source, `"use static"`:

```html
<div class="() =&gt; &quot;hello&quot;">
```

The served HTML contains the **source text of the function** as the class
name. That is a silent wrong output on a legal input — the class of defect the
project treats as worse than a coded refusal.

Both modes were reproduced by building the fixture and reading the artifact.

**Remedy:** the static emitter must refuse a non-literal attribute expression
rather than stringifying a function into the class name — a coded refusal, not
a silent wrong output. For the island emitter, do **not** blanket-wrap the
expression in `() => …`: I verified against the real runtime that a blanket
wrap breaks the signal-accessor case.

| Emission | Runtime result |
|---|---|
| `_setAttr(el, "class", cls)` (current) | `class = "hello"` |
| `_setAttr(el, "class", () => cls)` | `class = "() => \"hello\""` — the same bug, moved |
| `_setAttr(el, "class", () => cls())` | `class = "hello"` |
| `_setAttr(el, "data-n", count)` — accessor | `data-n = "0"` — must keep working |

So the island form is correct only because the runtime's `typeof` guard calls
it; wrapping unconditionally destroys the accessor case. The heuristic in
`attr_value` is wrong for a *function value*, but the fix is not to remove the
runtime guard — it is to stop guessing from the expression's spelling.

---

## Minor

- **M1.** `analyze/entry.rs:230-255` vs `:258-277` — `ambient_statement_span`
  and `ambient_declaration_span` are the same 25-line match, five arms
  identical.
- **M2.** `build.rs:248-275` vs `skills.rs:39-66` — the `--out` argument loop
  is duplicated almost verbatim.
- **M3.** `dev.rs:393-401`, `:439-447`, `:462-470` — the 405 response is
  written three times.
- **M4.** Three hand-rolled JSON routines with divergent behaviour:
  `build.rs:1605` (surrogate-aware), `dev.rs:1491` (encoder), `dev.rs:1523`
  (rejects `\uD83D\uDE00` pairs). `serde_json` is already a compiler
  dependency.
- **M5.** `build.rs:755-757` — the doc comment "Whether `dir` has no entries."
  is written twice consecutively.
- **M6.** `instruction.rs` is `pub mod` in `lib.rs:6` but contains only
  `#[cfg(test)]` code. Should be `#[cfg(test)] mod instruction;`.
- **M7.** `analyze/reachable.rs:299-320` — `judge`'s
  `inside_emitted_function` parameter is documented as not load-bearing, is
  computed at every call site, and never changes an outcome.
- **M8.** Span representation is mixed: `(u32, u32)` in `Diagnostic` and
  `ComponentInfo`, `oxc_span::Span` in `ir.rs`/`jsx.rs`, converted at every
  boundary with no type distinguishing them. Easy to transpose.
- **M9.** `analyze/rules.rs:488` (`is_uppercase_handler_name`) and
  `ir.rs:993` (`event_name`) are two definitions of "is this an `on…`
  listener", kept in agreement by a comment.
- **M10.** `diagnostic.rs:445-448`/`:481` — `Severity::Warning` is never
  constructed, but `check.rs:119` and `build.rs:898` gate exit codes on
  `== Severity::Error`, so a future warning would be silently discarded.
- **M11.** `source.rs:162` links `crate::ir::lower_text`, which is private — an
  unresolvable intra-doc link.
- **M12.** `every_code_explains_itself_and_names_a_fix`
  (`diagnostic.rs:790`) asserts `fix_template().len() > 20` — a length, not a
  quality.
- **M13.** `status_answers_while_a_rebuild_is_in_flight` (`dev.rs:403`)
  asserts `elapsed < 900ms` while a 3 s rebuild runs. Tight on a contended CI.
- **M14.** `build.rs:1679` — the build lock keys on `DefaultHasher`, whose
  algorithm std does not guarantee across releases. During a `cargo install`
  upgrade, old and new binaries could take different locks on the same
  output directory.

---

## What is genuinely strong — preserve it

Stated because a later contributor should not refactor these away.

1. **Panic hygiene in production paths.** The 706 `.unwrap()`/`panic!` hits
   are dominated by `#[cfg(test)]`. Outside tests there are exactly two
   `.expect()` calls, both infallible (`diagnostic.rs:509`, `:514`, serializing
   a hand-written `Serialize` over primitives), plus three
   `debug_assert!(false)` guards. No `.unwrap()` on user input survives into a
   production path.
2. **Zero clippy warnings across all targets** with this much diagnostic-string
   formatting and iterator-heavy AST walking. That is not luck.
3. **Zero `unsafe`.**
4. **`*_checked` as the only public artifact path, enforced by `pub(crate)`.**
   A public emitter that did not end in `_checked` would be a path from source
   to artifact with no rule pass between. Compile-time unreachability beats a
   runtime guard.
5. **Analysis on the original source, lowering on the stripped source.** The
   only arrangement where a span points at the agent's file *and* the emitted
   module is plain JavaScript. Do not "unify the two parses".
6. **The fix-text harness is the best-tested area of the project.**
   `instruction.rs` applies a "typeable instruction" predicate across every
   `fix_template`, every skill `## Fix`, every per-site message via the real
   pipeline, a **derived** tag domain (so a new tag is covered the day it is
   added), and ten applied-by-hand repairs. This closes the class of defect the
   project calls its worst.
7. **`write_node::<S>` with `TemplateSites`.** Void elements, entity policy,
   and template-literal escaping — the things that must not drift — are one
   function.
8. **Marker-ordinal addressing instead of index paths**, documented with the
   six measured defects of `el.firstChild.firstChild`, and proven against a
   real DOM.
9. **`Diagnostic::new` as the only constructor**, filling `explain`/`fix`/`doc`
   from `Code`, so a rule cannot invent a fix that disagrees with its skill.

---

## Review Focus, adjudicated

The plan names six input classes the tests must pin.

| # | Class | Status |
|---|---|---|
| 1 | Signal read/write in an event handler not flagged | **PINNED (strong)** — three tests plus a runtime test that constructs a live effect to prove the read was not tracked |
| 2 | Signal read in a deferred callback not flagged, documented | **PINNED (strong)** — including a skill-text assertion |
| 3 | JSX entities and meaningful whitespace not mangled | **PINNED at unit level, not end-to-end** |
| 4 | JSX syntax error → RA007, correct span, no panic | **FAILS on both halves** — span unpinned (I5), and deep nesting aborts (C1) |
| 5 | Props-less component still requires explicit type | **PINNED (strongest)** — the fix is applied and re-checked |
| 6 | Typed TSX reaches the browser as valid JS | **PINNED for island, PARTIAL for static** — no prerender fixture uses a type annotation, though I confirmed it works |

---

## Diagnostic code coverage

| Code | Unit | CLI e2e | Exact span | Fix applied in a test |
|---|---|---|---|---|
| RA001 | yes | yes | yes | n/a |
| RA002 | yes | **no** | no | no |
| RA006 | yes | yes | via message | **yes** |
| RA007 | yes | **no** | **no** | no |
| RA009 | yes | **no** | no | no |
| RA010 | yes | yes | no | **no** (I6) |
| RA011 | yes | **no** | no | **no** (I6) |
| RA012 | yes | **no** | line/col | no |
| RA014 | yes | yes | yes | **yes**, ten cases |

Four codes have no CLI-level test: RA002, RA007, RA009, RA011. RA007 is the
sharpest gap, because Review Focus #4 is an end-to-end claim and the only CLI
assertion about RA007 is that a non-`.tsx` argument must not produce one.

---

## Performance — measured

All figures are **release** builds (`cargo build --release`), median of 3 runs,
on a 6C/12T machine under WSL2. Not debug numbers.

### C3. The size gate certifies 2 194 bytes, but users download 9 616 — CONFIRMED

This is the most consequential finding in the audit. The gate measures the
**minified** runtime; `build` copies the runtime **verbatim**.

```sh
$ runai build --out ./dist          # one island route
$ wc -c dist/assets/runtime.js
28518
$ cmp packages/runai/runtime.js dist/assets/runtime.js
# identical
$ gzip -9 -n -c dist/assets/runtime.js | wc -c
9616
```

| Form | Gzipped | vs 6 144 budget |
|---|---|---|
| minified (what the gate measures) | **2 194** | 35.7% used, passes |
| shipped to the browser | **9 616** | **57% OVER, by 3 472 bytes** |

`build.rs:1478-1486` is `fs::read(&runtime)` then `fs::write(&path, bytes)`.
There is no minification anywhere in the CLI. The spec defines the budget as
"minified + gzipped", so the gate measures the spec correctly — the gap is that
**no build step produces that form**. Phase 1 has no bundler by design, so this
is an expected hole that is currently invisible: the build output reads as
"budget met" when the deliverable is over it.

**Remedy:** have `build.rs` run the same minify step the gate runs before
copying — it already shells out to `bun` for prerender, so the dependency
exists. Do not raise 6 144.

### C4. `build` spawns one `bun` process per static route — CONFIRMED

`build.rs:1427-1449` loops over rendered modules and calls
`runtime.render(&modules, &filename)` → `Command::new("bun").output()`, serially.

Measured on identical 4-line static pages:

| Routes | Build wall (release) | ms/route |
|---|---|---|
| 1 | 0.03 s | — |
| 100 | 1.61 s | 16.1 |
| 500 | 8.15 s | 16.3 |

Per-route cost is flat at ~16 ms, so this is purely `fork`+`exec`+`bun` init,
not anything that grows with project size.

Isolated, to prove the mechanism:

```sh
$ time (for i in $(seq 100); do bun -e '1'; done)    # 0.635 s
$ runai check .                                       # < 0.01 s  (entire compiler)
$ runai build --out ./dist                            # 1.61 s
```

So ~99% of a static build is `fork`+`exec`+`bun` init, and the compiler is a
rounding error. The auditor measured 500 spawns at 8 122 ms versus **82 ms** for
one process importing all 500 modules — a 99× difference for an identical
result.

**Remedy:** write all prerender modules to the scratch dir, then one child per
build that imports them all and emits a JSON array; `assemble_into` splits by
index. ~40 lines in `Runner::render`. Takes a 500-route build from 8–12 s to
well under 1 s with no architectural change.

### C5. An island route is parsed 20 times — CONFIRMED by instrumentation

The auditor instrumented a copy of `runai_compiler` with counters on `parse`,
`strip_types`, `analyze`, and `SemanticBuilder::new()`:

```
STATIC route   parses=4   semantic=7   analyze=2  strip=1
ISLAND route   parses=20  semantic=20  analyze=4  strip=8
```

`analyze` → `check_emitted_names` → (island only)
`codegen::island_reserved_names` → `lower_route`, purely to discover which
helpers the emitted artifact calls. That is a full lowering inside the analysis
pass, paid on every `check` and every `build`.

Measured: `analyze()` on the same source is 5.30 µs static vs 20.87 µs island —
3.9× for identical input.

This is the same defect as I2/I3 (the inverted dependency and the triple
lowering), measured rather than inferred.

**Remedy:** one `Outline { program, bindings, route: OnceCell<RouteIr> }`
threaded through `analyze`, `island_reserved_names`, `check_reachable_expressions`,
and both emitters.

### I11. `build::emit` discovers the route mode by trial — CONFIRMED

`build.rs:1238-1246` tries `emit_prerender_checked` first and treats its
refusal as "it must be an island". For an island route that means a full
analysis plus two strips plus a lowering, all discarded on
`route.mode != Mode::Static`, before `emit_client_checked` starts from zero.

Measured on 400 identical island routes:

```
check                                          26.7 ms
build (analyze + emit_pre + emit_client)      100.5 ms   → 3.8× check
```

No `bun` is spawned for islands, so the whole 3.8× is duplicated compiler work.

**Remedy:** call `detect_mode` once in `prepare` and dispatch on it, exactly as
`check` already does.

### I12. One file edit rebuilds every route in `dev` — CONFIRMED

`dev.rs:566` → `build::run` → `build::prepare` walks and recompiles all routes.
On a 400-route tree, editing one file:

```
warm request, nothing changed:   1.34 ms
one-file edit, then request:   115.29 ms
```

**What is already correct and should not be changed:** change detection is
poll-on-request, not an inotify watcher, so there is no event queue to
accumulate — 30 rapid edits with no intervening request cost 0 builds. RSS
across 60 rebuilds of a 100-route tree is flat (6 144 → 6 272 kB), so there is
no leak. Staleness semantics are correct, including edits made mid-build.

**Remedy:** batch the spawns (C4) first — that alone takes a 400-route rebuild
from 115 ms to near the ~27 ms compile floor. Then key `Prepared` reuse by
mtime+size so only the edited route is re-analysed.

### Minor performance

- **O(n²) diagnostic dedup** (`entry.rs:71-79`, `Vec::contains` inside
  `retain`). Measured: 0.34 ms at 1 600 diagnostics, 1.8 ms end-to-end on a
  file engineered to emit 799. Invisible at realistic scale; `HashSet` is a
  two-line fix.
- **Per-route filesystem work**: `verify_under` (`build.rs:1520-1547`) does two
  `canonicalize` plus two `create_dir_all` **per output file** — 37.9 µs/file,
  19 ms at 500 routes. `check.rs:243` canonicalizes the cwd once per file.
- **`IrNode` owns `String`** where the lowering already holds `source: &'s str`
  (`ir.rs:90-117`, `:663`). Measured 85 234 allocations / 24.2 MiB to build one
  5 001-line page. Structural, not local — `clone()` appears only 16 times in
  1.25 MB of source.
- **`prepare` sorts with an allocating closure** (`build.rs:862`):
  `to_path_string().replace('\\', "/")` allocates twice per comparison.

### What is already right (measured, not assumed)

- **Runtime copied once per build, not per island** (`build.rs:1470-1487`).
- **No config re-read per route**; `runai.json` is read once.
- **No regex compiled in a loop** — the compiler has no regex dependency at all.
- **Scaling is linear over routes**, confirmed at 1/100/500; memory flat at
  ~32 MB from 100 to 500 routes.
- **Prerender output is correct at scale**: 5 000 `<li>` in, 5 000 in the
  317 KB output, no truncation.
- **`build.rs`'s 76 `format!` calls** are overwhelmingly error-message
  construction on paths that abort the build — the right place to spend an
  allocation.

---

## Recommended order

1. **C1** (stack overflow) — a crash on input, and the only Critical that kills
   the process.
2. **C3** (shipped runtime 9 616 B vs 6 144 budget) — the hard shipping
   constraint is currently missed by 57%, and the gate's green output hides it.
3. **C4** (one `bun` per static route) — 99% of build time is process spawn;
   ~40 lines takes a 500-route build from ~12 s to under 1 s.
4. **I9** (function attribute) — the only finding where the compiler silently
   ships a wrong page: the static side emits the function's source text as a
   class name. Read the corrected remedy above before touching the emitters.
5. **C5 + I2 + I3** together — one shared `Outline`/`RouteIr` fixes the
   inverted dependency, the triple lowering, and the 20-parse island path.
6. **C2** (check/build asymmetry) — a live CI false-green, cheapest to pin.
7. **I11** (mode by trial) — `detect_mode` once; island build drops from 3.8×
   `check` to ~1.3×.
8. **I4** (`node`-absent skip) — one fallback restores a whole test's value.
9. **I1** (literal folding) — the framework currently teaches a spelling that
   costs its own budget.
10. **I5, I6, I7** — three missing assertions, cheap, covering the project's
    stated worst-defect class.
11. **The size-gate assertion** — one line; `[ -s "$min" ]` before gzipping.
12. **I12** (dev rebuilds all routes) — matters once trees grow past ~200.

Three of these share one cause: the compiler recomputes artifacts it already
had. C5, I2, I3, and I11 are all the same missing seam.

Three of these share one cause: the compiler recomputes artifacts it already
had. C5, I2, I3, and I11 are all the same missing seam.

## Verification commands

```sh
cd ~/runai
cargo test --workspace --offline                       # 589 passed
cargo clippy --workspace --all-targets --offline       # 0 warnings
bash scripts/size-budget.sh                            # 2194 / 6144, size ok
(cd packages/runai && bun test)                        # 78 passed
```

Reproducing the Critical findings:

```sh
# C1: stack overflow. Debug overflows at ~4500, release at ~20000.
python3 -c "n=4500; print('\"use static\"'); print('export default function P(props: Record<never, never>) {'); print('  return ' + '<div>'*n + 'x' + '</div>'*n); print('}')" > /tmp/c1.page.tsx
mkdir -p /tmp/c1dir/src/routes && cp /tmp/c1.page.tsx /tmp/c1dir/src/routes/deep.page.tsx
cd /tmp/c1dir && runai check .     # debug binary: "has overflowed its stack", exit 134

# C2: check passes, build refuses
mkdir -p /tmp/f2/src/routes/.hidden
printf '"use island"\nexport default function P(props: Record<never, never>) { return <div>x</div> }\n' \
  > /tmp/f2/src/routes/.hidden/index.page.tsx
cd /tmp/f2 && runai check .               # exit 0  <-- the false green
cd /tmp/f2 && runai build --out ./dist    # refuses the dot-prefixed directory

# C3: the shipped runtime is not minified
cd /tmp/ship && runai build --out ./dist
cmp packages/runai/runtime.js dist/assets/runtime.js     # identical
gzip -9 -n -c dist/assets/runtime.js | wc -c             # 9616, budget 6144
```
