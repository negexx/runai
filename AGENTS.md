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
| Wondering whether a skill already exists for something | `find-skills` |
| Non-trivial feature work, planning, TDD, debugging a failure | superpowers skills (already installed as a plugin) |

Loading a skill and then not applying it is worse than skipping it: it costs context and
still produces the wrong shape.

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
- **Runtime budget: 6144 bytes gzipped.** `scripts/size-budget.sh` is the gate.

## Toolchain facts, verified — do not re-derive

- `oxc_*` crates are pinned exactly `=0.159.0`. `ParserReturn` has `diagnostics`, not
  `errors`/`panicked`. `LabeledSpan.span` is private — use `.span()`.
- Oxc hoists a leading string literal into `program.directives` and removes it from
  `program.body`.
- Runtime exports are exactly `signal`, `derived`, `effect`, `owner`, `_setText`,
  `_setAttr`, `_on` (plus `template`/`For`/`Show`/`When`/`resource`/`mount` from the
  runtime plan's later task).
- Bun 1.4 (`bun test`) runs the runtime tests; `bun` also executes prerender modules at
  build time.
