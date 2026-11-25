# `AGENTS.md` — the generated GitNexus block and the `.claude/` paths

What in the `<!-- gitnexus:start -->` … `<!-- gitnexus:end -->` block
(`AGENTS.md:77-126`) depends on Claude-related artifacts, what a regeneration would
break, and the two options for dealing with it.

**Not edited here.** The block is machine-generated and rewritten on every
`gitnexus analyze` that does not pass `--skip-agents-md`. This document reports;
the fixes are the options below.

## The block has already been hand-edited, and that is the state to reason about

The generator (GitNexus **1.6.12**, `~/.npm/_npx/e46929201c1128dd/node_modules/gitnexus`,
`dist/cli/ai-context.js:82-197`) emits a `## CLI` table of **file paths**:

```
| Understand architecture / "How does X work?" | `.claude/skills/gitnexus-exploring/SKILL.md` |
```

The table in `AGENTS.md:117-124` does **not** say that. It names skill IDs:

```
| Understand architecture / "How does X work?" | `gitnexus-exploring` |
```

`ai-context.js:101` builds each row as
`` `| ${skill.agentTableTask} | \`.claude/skills/${skill.name}/SKILL.md\` |` `` — the
`.claude/skills/<name>/SKILL.md` cell is hard-coded in the generator, not a repo
setting. So `AGENTS.md:113-124` is a **hand edit inside the generated markers**. The
same is true of two more passages:

| Line | What the generator emits | What the file says | Verdict |
|---|---|---|---|
| `AGENTS.md:82` | `… run.cjs analyze --index-only …` (`ai-context.js:116`) | `… analyze --index-only --skip-agents-md --skip-skills …` | hand edit |
| `AGENTS.md:83-84` | nothing — the generator emits no second paragraph | the "flags are required in this repo" note | hand edit |
| `AGENTS.md:113-115` | nothing — the generator goes straight to the table | "GitNexus skills are registered globally … `.claude/skills/` path, which no longer exists" | hand edit |
| `AGENTS.md:117-124` | six `.claude/skills/<name>/SKILL.md` paths | six skill IDs | hand edit |

Verified by running the generator directly with this repo's name and stats:
`generateGitNexusContent("runai", {nodes:2116,edges:5618,processes:187}, {})`
reproduces the block **except** for those four passages.

## So what is actually stale

Nothing in the block is stale *today*. The `.claude/skills/` paths were removed by
hand, and they were wrong when they were there: `.claude/` does not exist

```sh
$ cd ~/runai && ls .claude/
ls: cannot access '.claude/': No such file or directory
```

and `.gitignore:12-13` ignores `.claude/` and `CLAUDE.md`, so they could not have
been committed. The real skills live at `~/.config/opencode/skills/` in a
**different layout** — `gitnexus-exploring.md` and `gitnexus-plan/`, not
`gitnexus-exploring/SKILL.md` — so the generated paths would have been wrong on two
counts: wrong directory *and* wrong filename shape.

The stale thing is the **risk**, not the text: every hand edit above is inside the
markers, and `upsertGitNexusSection` (`ai-context.js:492-497`) replaces everything
between them rather than merging. One `gitnexus analyze` without `--skip-agents-md`
puts the six `.claude/skills/` rows back and deletes the three explanatory
paragraphs.

## What the regeneration would still get right

Re-derived from `.gitnexus/gitnexus.json` and the source, so the report is not
all-bad-news:

- The header counts (`AGENTS.md:80`) — `2116 symbols, 5618 relationships, 187
  execution flows`. `.gitnexus/gitnexus.json` `stats` reads
  `{'files': 62, 'nodes': 2159, 'edges': 5677, 'communities': 109, 'processes': 191}`.
  The small differences (`2116`/`2159`, `187`/`191`) are ordinary drift between the
  run that wrote the block and the index on disk; the next analyze corrects it. Not
  a `.claude/` problem.
- Every `node .gitnexus/run.cjs …` command — the runner exists and resolves its own
  runner (`run.cjs:357-380`).
- `## Always Do`, `## Never Do`, `## Resources` — no Claude-specific text, and the
  generator would emit them identically.
- The `base_ref: "master"` in the `detect_changes` bullet (`AGENTS.md:89`) comes
  from the configured default branch; it is generated, not hand-written.

## Option A — pass `--skip-agents-md` (and `--skip-skills`) on every analyze

```sh
node .gitnexus/run.cjs analyze --index-only --skip-agents-md --skip-skills
```

Trade-offs:

- **Wins:** the block is never regenerated, so the six `.claude/skills/` rows never
  return and the three hand-written paragraphs survive. `CLAUDE.md` and
  `.claude/skills/` are not written at all (`ai-context.js:492-515`: `--skip-agents-md`
  skips both `AGENTS.md` and `CLAUDE.md`, `--skip-skills` skips the install).
- **Costs:** the block stops being maintained by the tool. The header counts and
  `base_ref` freeze. `refreshBaseRefLine` — the in-place `base_ref` update
  (`ai-context.js:530-545`) — returns early under `--skip-agents-md`.
- **Depends on remembering the flag.** `.gitnexusrc` can carry it:
  `analyze-config.js:62-65` maps `skipAgentsMd` and `skipSkills`, with
  `skipContextFiles`/`skipAiContext` as aliases, and the nested shape
  `{"analyze":{"skipAgentsMd":true,"skipSkills":true}}` is documented at
  `analyze-config.js:21-28`. **No `.gitnexusrc` exists on this tree**
  (`ls .gitnexusrc` → not found), so today the flag must be typed by hand and
  `AGENTS.md:84` is prose standing in for a config file.

## Option B — keep hand-editing the generated block and accept it returns

This is the **current state**. Keep it, and re-apply the four edits after any full
analyze.

Trade-offs:

- **Wins:** the file is correct right now, with no workflow change, and today the
  paths are already gone.
- **Costs:** a full `gitnexus analyze` overwrites all four passages, and nothing
  fails when it does — the regeneration is silent, so the regression is only
  noticed by reading the block. There is no test or hook that catches it.
- Note that Option A and Option B together are self-consistent: the hand edit is what
  makes the block correct, and `--skip-agents-md` is what stops the tool undoing it.
  Either alone is worse than both.

## Recommendation, stated as a trade-off not a decision

The block is correct now **because** it was hand-edited (Option B, already done). To
keep it correct, add a committed `.gitnexusrc` with
`{"analyze":{"skipAgentsMd":true,"skipSkills":true}}` so Option A's protection is
the default rather than a flag someone has to remember, and treat the four
hand-written passages as owned text that a regeneration would delete.

If the `.gitnexusrc` is not wanted, the choice is between A and B; do not take B
alone and assume it holds. It does not: the next unflagged analyze restores six
`.claude/skills/` paths that do not exist.

## What could not be verified

- **Whether `.gitnexusrc` is honoured end to end.** `analyze-config.js` documents it
  as repo-local config read by `gitnexus analyze`; `run.cjs` passes arguments through
  (`run.cjs:372-380`) and does not itself read `.gitnexusrc`. Proving it needs an
  `analyze` run, which reindexes and rewrites `AGENTS.md` — the exact side effect
  this task must not cause. So the `.gitnexusrc` recommendation is reasoned from the
  config parser's own docs, not from a successful run.
- **Whether the `.agents/skills/` mirror matters.** `installSkills` mirrors to
  `.agents/skills/` only when a `.agents/` directory exists
  (`ai-context.js:413-416`). This repo has none (`ls -d .agents` fails), so the
  mirror does not apply and the block does not reference it.
