# runai — project rules

An agent-first web framework. Read `docs/superpowers/specs/2026-11-20-runai-framework-design.md`
before proposing anything structural.

## Installed skills and when to load them

Which skill to load for which task is defined once, globally, in
`~/.config/opencode/rules/50-skills.md`. Do not duplicate that table here.

Project-specific additions: `rust-testing` applies to the compiler crate, and
`playwright-best-practices` to the acceptance/E2E harness.

The rule about not installing unreviewed skills is also global — see
`~/.config/opencode/rules/50-skills.md`.

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

<!-- gitnexus:start -->
# GitNexus — Code Intelligence

This project is indexed by GitNexus as **runai** (2116 symbols, 5618 relationships, 187 execution flows).

> Index stale? Run `node .gitnexus/run.cjs analyze --index-only --skip-agents-md --skip-skills` from the project root — it auto-selects an available runner. No `.gitnexus/run.cjs` yet? Bootstrap with `npx`, `bunx`, or `pnpm dlx` — e.g. `bunx gitnexus@latest analyze` (npm 11 npx crash; #1939).
>
> **The `--skip-agents-md --skip-skills` flags are required in this repo.** Without them `gitnexus analyze` regenerates `CLAUDE.md` and `.claude/skills/`, which this project does not use — Claude Code is not a supported harness here, and the skills already live in `~/.config/opencode/skills/`. Both paths are gitignored as a second line of defence.

## Always Do

- **MUST run impact before editing.** Use `impact({target: "symbolName", direction: "upstream"})` or `node .gitnexus/run.cjs impact "symbolName" --direction upstream --repo .`; report callers, processes, and risk. Never substitute grep for graph analysis.
- **MUST analyze graph changes before committing.** Use `detect_changes({scope: "all"})` (MCP) or `node .gitnexus/run.cjs detect-changes --scope all --repo .` (CLI fallback). `partial: true` or `truncated: true` is not a clean check — a zero means unseen, not unaffected; re-run it. For regression review: `detect_changes({scope: "compare", base_ref: "master"})` or `node .gitnexus/run.cjs detect-changes --scope compare --base-ref "master" --repo .`.
- MUST warn on HIGH/CRITICAL `risk` pre-edit; never use `riskSharedAxes` to waive a HIGH/CRITICAL `risk` warning. Compare File/symbol: MCP File omits axes; Graph-RAG expands File.
- **MUST treat `risk: UNKNOWN` as unresolved, not as low.** An empty caller set is not evidence the symbol is unused — it can also mean the callers are not resolvable by the index (plain-object property access, dynamic dispatch, cross-language calls). `impact` pairs `UNKNOWN` with a `riskNote` saying so. Confirm with a text search before treating the symbol as safe to change or delete; do not proceed on the strength of a zero.
- **MUST use `query({search_query: "concept"})` for concepts/flows, `context({name: "symbolName"})` for a named symbol, or `impact` for blast radius, on read-only callers, dependencies, imports, or execution flow.** Graph first; text search only for empty/`UNKNOWN`/literals.
- For security review, `explain({target: "fileOrSymbol"})` lists taint findings (source→sink flows; needs `analyze --pdg`).

## Never Do

- NEVER edit a function, class, or method before MCP/CLI impact analysis.
- NEVER ignore HIGH or CRITICAL risk warnings from impact analysis, and never read `UNKNOWN` as an all-clear — it means the walk could not answer, which is the one verdict that requires confirming by other means.
- NEVER rename symbols with find-and-replace — use `rename` which understands the call graph.
- NEVER commit before MCP/CLI graph change analysis.

## Resources

| Resource | Use for |
| --- | --- |
| `gitnexus://repo/runai/context` | Codebase overview, check index freshness |
| `gitnexus://repo/runai/clusters` | All functional areas |
| `gitnexus://repo/runai/processes` | All execution flows |
| `gitnexus://repo/runai/process/{name}` | Step-by-step execution trace |

## CLI

GitNexus skills are registered globally by OpenCode, under
`~/.config/opencode/skills/`. Load one with the `skill` tool — do not read a
repo-local `.claude/skills/` path, which no longer exists.

| Task | Skill ID |
| --- | --- |
| Understand architecture / "How does X work?" | `gitnexus-exploring` |
| Blast radius / "What breaks if I change X?" | `gitnexus-impact-analysis` |
| Trace bugs / "Why is X failing?" | `gitnexus-debugging` |
| Rename / extract / split / refactor | `gitnexus-refactoring` |
| Tools, resources, schema reference | `gitnexus-guide` |
| Index, status, clean, wiki CLI commands | `gitnexus-cli` |

<!-- gitnexus:end -->
