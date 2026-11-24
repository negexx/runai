# runai — project rules

An agent-first web framework. Read `docs/superpowers/specs/2026-11-20-runai-framework-design.md`
before proposing anything structural.

## Installed skills and when to load them

These live globally in `~/.agents/skills/` and are already registered. Load one with
the `skill` tool before starting the work it covers.

| Task at hand | Load |
|---|---|
| Writing or fixing any Rust test; adding coverage; compiler crate tests | `rust-testing` |
| Writing, debugging, or de-flaking Playwright / E2E / acceptance tests | `playwright-best-practices` |
| Designing an agent eval, an eval dataset, or comparing eval runs | `google-agents-cli-eval` |
| Diagnosing a failure whose cause is not yet known — BEFORE the second attempt | `systematic-debugging` |
| Non-trivial feature work, planning, TDD, debugging a failure | superpowers skills (already installed as a plugin) |

Do not install further skills without explicit approval for each one. A skill is an
instruction set an agent executes with full tool access, so an unreviewed install is
arbitrary code execution by proxy. The `find-skills` workflow is deliberately not wired
to this project.

Loading a skill and then not applying it is worse than skipping it: it costs context and
still produces the wrong shape.

## Debugging: one probe, then the `debugger` agent

`/verify-claim <claim>` and `/debug <evidence>` are the two commands. Use them in that
order and do not substitute controller debugging for either.

- **Reproducing a claim to adjudicate it** — inline, one probe, then a ruling
  (CONFIRMED / REFUTED / PARTIAL). This is controller work and it is cheap.
- **A defect whose cause is unknown, or that survived ONE fix attempt** — `/debug`.
  One failed fix means the cause was never identified; a second patch from the
  controller is the failure this rule exists to prevent.
- **A second probe for the same defect is the signal.** At that point you are
  debugging, not adjudicating. Dispatch, and hand over the reproduction you already
  have so the debugger starts at localize.

## Project invariants — cheaper to honour than to rediscover

- **Static routes ship 0 bytes of JavaScript.** A `"use static"` route touching a signal,
  handler, `effect`, or `resource` is `RA010`. Never work around this; change the directive.
- **Every route file opens with `"use static"` or `"use island"`.** No inference, no default.
  Single and double quotes are both accepted; escapes such as `"use\x20static"` are not.
- **A props-less component still needs an explicit type**: `props: Record<never, never>`.
  A return-type annotation does not satisfy this.
- **`<When>` requires all three branches** — `pending`, `error`, and children.
- **Effects flush synchronously.** A write is fully applied when the setter returns.
- **Diagnostics are the product.** Every code carries `code`, `span`, `message`, `explain`,
  `fix`, `doc`. When you touch one, the fix text must be *legal* when applied verbatim and
  must name something that actually exists in the file. A diagnostic that sends an agent in
  a loop is the worst possible defect in this repository.
- **Every repair must name a construct this milestone can compile.** Six closed repair
  loops were found in one pass of this project's own messages: advice suggesting `<For>`,
  `<Show>`, `<When>`, a component tag, or a JSX-valued attribute, each of which is itself
  refused. If you write or change a repair, apply it verbatim and confirm it is accepted.
- **A message may state only a mechanism the code has.** Claims like "the parser fosters
  it", "this makes a loop", or "that would be a duplicate export" were each false at some
  point. Run the case before writing the sentence; the four worst defects in this project
  were invented mechanisms, not wrong rules.
- **Do not claim coverage a test cannot provide.** A predicate that scans only
  `fix_template` cannot catch a defect in a per-site message. Assert the positive fact,
  not only the absence of one phrasing.
- **Runtime budget: 6144 bytes gzipped.** `scripts/size-budget.sh` is the gate, and it
  also asserts `RUNAI_DEV` is absent from the production build.

## Toolchain facts, verified — do not re-derive

- `oxc_*` crates are pinned exactly `=0.159.0`. `ParserReturn` has `diagnostics`, not
  `errors`/`panicked`. `LabeledSpan.span` is private — use `.span()`.
- Oxc hoists a leading string literal into `program.directives` and removes it from
  `program.body`.
- Runtime exports are exactly `signal`, `derived`, `effect`, `owner`, `template`,
  `For`, `Show`, `When`, `resource`, `mount`, `_setText`, `_setAttr`, `_on`. The analyzer
  refuses an import of any other name from `"runai"`.
- Bun 1.4 (`bun test`) runs the runtime tests; `bun` also executes prerender modules at
  build time. Bun's `onResolve` plugins DO resolve bare `/assets/` specifiers, so the
  e2e harness never needs to rewrite an artifact.
- `bun -e` and `node -e` exit **0** even when the imported module throws or fails to
  parse. A build must check exit status AND empty stderr AND non-empty stdout.
- happy-dom is **more lenient than the HTML spec**: it keeps orphan `<td>` and nested
  `<search>` where parse5 and a browser do not. Never use it to decide whether markup
  is shaped correctly; use `parse5` for that, and use happy-dom only for DOM behaviour.

