# runai phase 1 — remediation plan

Status of every finding in `docs/audits/2026-11-25-phase-1-audit.md`, one row each.

**Updated after the remediation round landed.** The statuses below were verified
by running each finding's reproduction against the rebuilt binary, not taken
from an agent's report. Where a claim did not reproduce, the row says so.

Baseline after remediation, all measured in this session:

| Check | Before | After |
|---|---|---|
| `cargo test --workspace` | 589 pass | **623 pass, 0 fail** |
| `cargo clippy --all-targets` | 0 warnings | **0 warnings** |
| shipped runtime, gzipped | 9 616 B | **2 131 B** |
| 100 static routes | 1.61 s | **0.13 s** |
| 500 static routes | 8.15 s | **0.53 s** |
| 20 000-deep JSX | SIGABRT, exit 134 | **RA015, exit 1** |

Three statuses are used:

- **fixed (verified)** — reproduced as broken before, and as fixed after.
- **partly fixed (what remains)** — the dominant cost is gone; the residual is named.
- **deferred (reason)** — not this round, with a concrete reason. "Too large" alone
  is not a reason; each names what the work actually waits on.
- **accepted risk (reason)** — will not be fixed, and the reason says what is being
  accepted and what bounds the exposure.

**No finding in this document is assigned `accepted risk`.** The brief's instruction is
that everything outside the fixed set is deferred, so that is what every unfixed row
says. Three rows (M8, M10, M14) are marked **candidate for `accepted risk`** in their
reason column: they are the ones where "we will not fix this" is a better answer than
"we have not fixed this yet", and the choice between the two statuses is the
controller's, not this document's.

**"fixed (verified)" means this document verified it** — by running the finding's
reproduction against a binary built from the working tree, not by reading an agent's
report. Two rows below (M5, M11) were handed to another agent after this document was
first drafted and landed there; their rows say so and carry the check to run.

## Critical

| ID | Finding | Status | Evidence |
|---|---|---|---|
| C1 | Deep JSX nesting overflows the stack, exit 134 | **fixed (verified)** | `RA015_EXCESSIVE_NESTING` at depth 513; 512 clean; 20 000 exits 1 with a diagnostic instead of SIGABRT. Limit 512 is half the measured abort threshold. |
| C2 | `check` passes two trees `build` refuses (dot-directory, `RUNTIME`) | **fixed (verified)** | Both now exit 1 from `check` with `build`'s exact wording; `check --json` still one valid array. 14/14 in `--test check`. |
| C3 | Shipped runtime is 9 616 B gzipped against a 6 144 B budget | **fixed (verified)** | Shipped artifact now minified: 5 244 raw / **2 131 gzipped**, no `RUNAI_DEV`. Build asserts the result shrank and is dev-free, so a failed minify cannot ship silently. |
| C4 | One `bun` process per static route (~16 ms each) | **fixed (verified)** | One child per build, length-prefixed record protocol. 100 routes 1.61 s → **0.13 s**; 500 routes 8.15 s → **0.53 s**. |
| C5 | An island route is parsed 20 times per emit | **partly fixed** | I1's folding and the `detect_mode` dispatch removed work, but the `Outline`/`RouteIr` threading that eliminates the re-lowering is **deferred (blocked on I3/I8)**: one signature change across three call sites, deliberately not attempted alongside four other in-flight edits. |

## Important

| ID | Finding | Status | Evidence / reason |
|---|---|---|---|
| I1 | `RA007`'s `{" "}` advice costs runtime bytes the static path does not pay | **fixed (verified)** | `<p>a{" "}b</p>` in an island now emits `template(\`<p>a b</p>\`)` — no marker, no `_setText`, no TreeWalker. Constant-folded at the IR boundary. |
| I2 | `analyze` imports `codegen` (`island_reserved_names`) | **deferred (blocked on C5)** | Moving `used_helpers` into `ir` is the seam C5 needs. Doing it twice guarantees a conflict. |
| I3 | A route is lowered up to three times per emit | **partly fixed** | The mode-by-trial lowering is gone (I11). The remaining duplicate lowering is C5's, deferred with it. |
| I4 | The real-ESM artifact test asserts nothing when `node` is absent | **fixed (verified)** | Falls back to an in-process Oxc parse; a new test pins that the fallback rejects all four duplicate-export shapes by name, so it cannot go vacuous. |
| I5 | `RA007`'s span is never asserted beyond "non-zero" | **deferred (test-only)** | Deferred so it did not collide with C1's rewrite of the same JSX walk. Lands cleanly now that C1 is in. |
| I6 | `RA011`'s fix is never applied in a test | **deferred (test-only, needs a design decision)** | Needs the violating fixture plus a way to extract the per-site repair from `message`, which RA011 now composes in three shapes. A harness change, not a one-line assertion. |
| I7 | `every_repair_applied_by_hand_is_accepted` only rejects two codes | **deferred (widens a passing gate)** | Changing the filter to all error diagnostics turns the harness loose on nine codes at once; a failure would not be attributable to this change. Needs its own round. |
| I8 | `client_imports` is public and analyses under a fabricated path | **deferred (blocked on C5/I3)** | Same signature change as C5; one edit after the `Outline`/`RouteIr` threading lands. |
| I9 | A function-valued attribute diverges between modes | **fixed (verified)** | `RA016_STATIC_FUNCTION_ATTRIBUTE` now fires from both `check` and `build` on a static route, instead of emitting `class="() =&gt; &quot;hello&quot;"`. The island path keeps the runtime `typeof` guard — verified necessary, because a blanket `() => cls` wrap turns the signal-accessor case into `"() => n"`. |
| I11 | `build::emit` discovers the route mode by trial | **fixed (verified)** | `Prepared` carries `mode`; dispatch is explicit. Honest note: the predicted 3.8× did **not** reproduce as wall-clock (58 ms vs 14 ms at 400 island routes, before and after) — the removed work is real, but per-route I/O dominates. The code comment says so rather than repeating the claim. |
| I12 | One file edit rebuilds every route in `dev` | **partly fixed** | C4's batching removed the dominant cost. `Prepared` reuse keyed by mtime+size is **deferred**: incremental reuse is correctness-sensitive and deserves its own round with its own tests. |

## Minor

| ID | Finding | Status | Reason |
|---|---|---|---|
| M1 | `ambient_statement_span` and `ambient_declaration_span` are the same 25-line match, five arms identical | **fixed (verified)** | The duplication is gone, and the two names now do different jobs: `ambient_statement_span` (`entry.rs:250`) handles the two node kinds that are ambient by kind (`declare module`, `declare global`) and delegates every other form through `statement.as_declaration()?` to `ambient_declaration_span` (`:274`), which holds the five `… if ….declare` arms once. Read the two side by side: no arm is written twice. The span-asserting tests still pass. |
| M2 | The `--out` argument loop is duplicated in `build.rs` and `skills.rs` | **fixed (verified)** | `skills::parse_out_flag` is now `pub(crate)` and `build.rs` calls it; its own copy is deleted. |
| M3 | The 405 response is written three times in `dev.rs` | **fixed (verified)** | One `method_not_allowed` helper, three call sites. |
| M4 | Three hand-rolled JSON routines with divergent behaviour | **deferred (too large for this round)** | Replacing them means `runai_cli` takes `serde_json` (it deliberately has no JSON dependency — see `build.rs:1582-1586`, `runtime_value`). That is a dependency decision for the CLI crate, not a refactor, and it changes `Cargo.toml` for both the encoder and the surrogate-aware reader. |
| M5 | `build.rs:755-757` writes "Whether `dir` has no entries." twice | **fixed (verified)** | Landed by another agent after this document was drafted. Check it yourself: `grep -c "Whether \`dir\` has no entries" crates/runai_cli/src/build.rs` → `1`. The comment is now one line, and the block below it still carries the full explanation. This row existed because the documentation executor could not write `.rs`; the agent that owns `build.rs` applied it. |
| M6 | `instruction.rs` is `pub mod` but contains only `#[cfg(test)]` code | **deferred (not in this round's assignment list)** | Not among the findings this round was assigned, so per the plan of record it is deferred. **Observation, not a status:** the working tree already contains the change — `git diff crates/runai_compiler/src/lib.rs` replaces `pub mod instruction;` with `#[cfg(test)] mod instruction;`, and the diff carries a rationale comment. The audit's M6 text is therefore stale rather than the finding unfixed. Measurable effect: `cargo doc --no-deps --offline` drops from 47 warnings at `HEAD` to 40 here, because a `#[cfg(test)]` module is no longer documented. |
| M7 | `judge`'s `inside_emitted_function` never changes an outcome (`reachable.rs:316-322`) | **deferred (structural, needs the I3 seam)** | Removing the parameter touches every `judge` call site, which I3 is already rewriting to thread `RouteIr`. Deferred to avoid two agents editing the same signatures. |
| M8 | Span representation is mixed (`(u32, u32)` vs `oxc_span::Span`) | **deferred (migration larger than its defect)** | A newtype over the pair would ripple through `Diagnostic`, `ComponentInfo`, every rule, and the JSON encoder, for a class of bug (transposition) that no reproduced defect in this audit belongs to. The conversion points are few and each is a `(span.start, span.end)` on one line. **Candidate for `accepted risk`** if the controller prefers to close it permanently; recorded as deferred because this round's instruction is that findings outside the fixed set are deferred. |
| M9 | Two definitions of "is this an `on…` listener" (`rules.rs:488`, `ir.rs:993`) | **deferred (they are not the same predicate)** | `event_name` (`ir.rs:993`) requires `on` + a capital and returns a lowercased event; `is_uppercase_handler_name` (`rules.rs:488`) asks only about capitalisation, because the static rule is a prefix over all spellings. Merging them would either widen the static refusal or narrow the emitter's. The comment keeping them in agreement is the correct instrument until the two questions are shown to be one. |
| M10 | `Severity::Warning` is never constructed outside a test | **deferred (needs a product decision)** | `check.rs:124` and `build.rs:898` gate on `== Severity::Error`, so a future warning would be dropped silently. Nothing constructs a warning today (`diagnostic.rs:671` is the only site, inside `#[cfg(test)]`), so there is no current behaviour to lose. Fixing it properly means deciding what a warning's exit code is — a product decision, not a bug fix. **Candidate for `accepted risk`.** |
| M11 | `source.rs:162` links private `crate::ir::lower_text` | **fixed (verified)** | Landed by another agent after this document was drafted; see the doc-link section below for how it was found and why the brief's own `cargo doc` command never reported it. The link is now plain backticks, and the line has moved to `source.rs:179` because C1 added a doc section to `parse_program` above it. Check: `grep -n "lower_text" crates/runai_compiler/src/source.rs` shows no `[...]` around it. |
| M12 | `every_code_explains_itself_and_names_a_fix` asserts a length (`diagnostic.rs:790-795`) | **deferred (test-only, the real coverage is elsewhere)** | The assertion is weak, but `instruction.rs`'s corpus harness is the real coverage: it applies a typeable-instruction predicate across every `fix_template`, every skill `## Fix`, every per-site message through the real pipeline, and ten applied repairs. Strengthening this one length check adds no coverage the corpus harness lacks. |
| M13 | `status_answers_while_a_rebuild_is_in_flight` asserts `elapsed < 900ms` while a 3 s rebuild runs | **deferred (timing test, needs a scheduling fix not a number)** | Raising the bound makes the test pass without testing anything; lowering it makes it flaky. The real fix is to assert on the *response*, not the clock — that is a rewrite of the test's premise, and it belongs with I12's rebuild-batching work, which changes what "in flight" means. |
| M14 | The build lock keys on `DefaultHasher` (`build.rs:1679`) | **deferred (bounded to one release pair)** | The documented cost is exactly what the finding says: two *different releases* running concurrently on one `<out>` could take different locks (`build.rs:1676-1678`). The exposure needs a `cargo install` upgrade to land concurrently with a running build over the same output directory. Not fixed because any stable replacement (e.g. hashing the path bytes into a fixed-width digest) is a new dependency or a hand-rolled hash, and neither is worth it for a window that closes on the next release. **Candidate for `accepted risk`.** |

## Performance (the two minor items named in the audit)

| Item | Where | Status | Reason |
|---|---|---|---|
| O(n²) diagnostic dedup | `analyze/entry.rs:71-79` (`Vec::contains` inside `retain`) | **deferred (not landed — still `Vec`)** | The brief listed it among this round's fixes, but the code on disk still uses `let mut seen: Vec<(Code, (u32, u32))>` with `seen.contains(&key)` inside `retain` (`entry.rs:72-79`). Replacing it with a `HashSet` is two lines and the audit measured the cost as invisible at realistic scale (0.34 ms at 1 600 diagnostics), so nothing is blocked; it simply was not done. Whoever picks it up: `(Code, (u32, u32))` is `Hash`, so `HashSet` is a drop-in. |
| Per-route filesystem work | `build.rs:1962-1984` (`verify_under`: two `canonicalize` + two `create_dir_all` per output file) | **deferred (not landed — still per-file)** | Same: the brief listed it, the code still canonicalises `root` and `parent` inside `verify_under`, which runs once per output file. The audit measured 37.9 µs/file, 19 ms at 500 routes; C4 cut the dominant cost (per-route `bun`) so this is now a smaller share of a much smaller total, which is a reason to deprioritise it, not a reason it is safe to leave forever. |

## The remaining performance findings

Not named in the round's brief and not in the table above, so that every ID from
the audit appears exactly once:

| Finding | Status | Reason |
|---|---|---|
| `IrNode` owns `String` where the lowering holds `source: &'s str` | **deferred (structural)** | The audit calls it "structural, not local": it changes `IrNode`'s lifetime parameters across both emitters. A 24 MiB allocation peak for one page is real but does not break a budget, and it is the wrong shape of change to land beside a correctness round. |
| `prepare` sorts with an allocating closure (`build.rs:862`) | **deferred (marginal)** | Two allocations per comparison on the route sort. Invisible next to the per-route `bun` spawn C4 removes, so measuring it after C4 is the only way to know whether it still matters. |

## Not a finding, but read this before closing the round

The audit's stated doc baseline — "0 doc warnings currently reported" — cannot be
reproduced on this tree. The count depends on which items rustdoc documents:

```sh
$ cd ~/runai
$ cargo doc --no-deps --offline 2>&1 | grep generated
warning: `runai_compiler` (lib doc) generated 40 warnings

$ cargo doc --no-deps --offline --document-private-items 2>&1 | grep generated
warning: `runai_compiler` (lib doc) generated 50 warnings
```

The 40 default-mode warnings are three kinds, and only one kind is a broken link:

| Kind | Count (default mode) | Is it a defect? |
|---|---|---|
| `public documentation for X links to private item Y` | 34 | No. The link resolves under `--document-private-items`; rustdoc complains only because the *doc* is public and the *item* is crate-private. |
| `unresolved link to ...` | 5 | **Yes** — these resolve to nothing under any flag. |
| `invalid self-closing HTML tag` (`<h1/>` in a doc comment, `analyze/jsx.rs:55`) | 1 | Cosmetic; the comment is writing JSX, not HTML. |

### The broken links, by mode

`cargo doc --no-deps` (the brief's own verification command) reported **five**
unresolved links before M11 was fixed. `--document-private-items` reports more. A link
can only be checked in the mode that documents the item carrying it, and that is why
M11's link never appeared in the default run: the doc comment sat on
`pub(crate) fn has_line_break`, which default `cargo doc` does not document. It was
only visible under `--document-private-items`.

**Default `cargo doc --no-deps` — 5 unresolved links:**

| File:line | Broken link | Why it does not resolve | Fix |
|---|---|---|---|
| `analyze/props.rs:10` | `[find_components]` | `props.rs:21` imports `ComponentInfo, ComponentKind`, not `find_components` | Add it to the import, or fully qualify `crate::analyze::component::find_components` (it is `pub`, `component.rs:276`). |
| `analyze/reachable.rs:11` | `[RouteIr::statements]` | `RouteIr` is not imported into `reachable.rs` | Fully qualify `crate::ir::RouteIr::statements` (`pub statements`, `ir.rs:232`). |
| `analyze/reachable.rs:81` | `[RouteIr::statements]` | Same | Same. |
| `codegen/mod.rs:133` | `[runai_imports]` | It lives in `crate::analyze::signals`, and `codegen/mod.rs` does not import it | Fully qualify `crate::analyze::signals::runai_imports` (it is `pub(crate)`, `signals.rs:447`). |
| `strip.rs:37` | `[SourceType::tsx]` | `SourceType` is not imported into `strip.rs` | Demote to backticks, or fully qualify `oxc_span::SourceType`. |

**Additional unresolved links only under `--document-private-items`:**

| File:line | Broken link |
|---|---|
| `analyze/component.rs:846` | `[Span]` |
| `analyze/entry.rs:380` | `[crate::ir::module_statements]` |
| `analyze/reachable.rs:527` | `[BindingPattern::get_binding_identifiers]` |
| `analyze/reachable.rs:548` | `[RouteIr::name]` and `[RouteIr::statements]` (two on one line) |
| `analyze/rules.rs:481` | `[crate::ir::event_name]` |
| `codegen/prerender.rs:30` | `[PRERENDER_NAMES]` |
| `codegen/prerender.rs:39` | `[_attr]` |
| `source.rs:162` (was; now `:179`) | `[crate::ir::lower_text]` — this is M11, **since fixed** |

Seven sites, eight links, on top of the five above. M11's is now closed; the other
seven sites are still open.

M11 says `source.rs:162` links private `crate::ir::lower_text`, which was correct:
`lower_text` is a bare `fn` (`ir.rs:922`), so `crate::ir::lower_text` named nothing
rustdoc would accept.

**Another agent fixed this mid-round.** The link is now plain backticks, at what is
now `source.rs:179` (the line moved because C1's depth-ceiling work added a doc
section to `parse_program` above it):

```
$ git diff crates/runai_compiler/src/source.rs | grep lower_text
 (the line appears only as context, on the unchanged side)
$ grep -n "lower_text" crates/runai_compiler/src/source.rs
179:/// to report the one ambiguous case, `crate::ir::lower_text` to collapse the
```

Two facts remained worth recording even though the fix landed:

1. **The default `cargo doc` run never reported it**, because `has_line_break` is
   `pub(crate)` and therefore undocumented in that mode. A fix verified with the
   brief's command alone would appear to "fix" a warning that was never printed —
   and could equally have left it in place and appeared clean. The check that
   catches it is `--document-private-items`.
2. The link was not load-bearing: the sentence's point is that both consumers
   (`check_jsx`, `lower_text`) ask the same question, and `has_line_break` already
   names `[check_jsx]` by its fully-qualified path. Demoting it lost a link to a
   private function and nothing else.

The five default-mode links are the ones a reader of the rendered docs can reach
and click, so they are the higher priority; M11's is invisible in that mode.

**One of these has been closed in this round:** M11's, by another agent. The five
default-mode links are untouched. They are listed here so the next round has the
file:line list rather than re-deriving it.

### Snapshot, and two regressions introduced while this document was written

Every count above is a snapshot taken at `2026-11-25T09:42Z`, working tree at
`HEAD adca791` plus the in-flight remediation edits. The tree changed under this
audit — C1's depth-ceiling work and M11's fix both landed on top of it — so the
counts moved:

| Run | Warnings |
|---|---|
| `HEAD adca791`, before any remediation edit | 47 |
| this tree, after C1/M11 landed | **41** compiler + **2** CLI |

The drop from 47 to 41 is M6 (`instruction.rs` no longer documented) and M11 (link
demoted). The **+2 CLI warnings are new**, and they were introduced by the C1 work,
not present at `HEAD`:

| File:line | Broken link | Why |
|---|---|---|
| `runai_cli/src/build.rs:1122` | `[detect_mode]` | `build.rs` does not import `runai_compiler::source::detect_mode`; the sentence says so itself ("`runai_cli` does not depend on `oxc_allocator`"). |
| `runai_cli/src/build.rs:1128` | `[detect_mode]` | Same, in the following paragraph. |
| `analyze/jsx.rs:1181` (private-items only) | `[MAX_JSX_DEPTH]` | `jsx.rs` does not import the constant, which lives in `crate::diagnostic`. |
| `analyze/jsx.rs:2204` (private-items only) | `[MAX_JSX_DEPTH]` | Same. |

The two `build.rs` ones are reportable by the brief's own `cargo doc --no-deps`
command, so they fail the "no warning appears" bar that task 2 is judged by. The two
`jsx.rs` ones need `--document-private-items`.

All four are the same defect shape as the five above: a partially-qualified intra-doc
link to an item the module does not import. The fix is one qualifier each
(`crate::diagnostic::MAX_JSX_DEPTH`, `runai_compiler::source::detect_mode`) or plain
backticks. **Not fixed here** — they are inside `.rs` files this executor cannot
write, and they belong to the agent that added them.

### Counts, restated for the current tree

To avoid the document contradicting itself, the authoritative figure is whichever
`cargo doc` prints, not a number copied into this file. At the snapshot:

- `cargo doc --no-deps --offline` → **5** unresolved links in `runai_compiler`
  (the table above, unchanged) + **2** in `runai_cli` (new, from C1).
- `cargo doc --no-deps --offline --document-private-items` → **17** unresolved
  links across the workspace: **15** in `runai_compiler` and **2** in `runai_cli`.
  Of the 15, the 12 in the second table plus the 2 new `MAX_JSX_DEPTH` references
  make 14, plus `analyze/reachable.rs:548`'s two links on one line, which the
  per-site table lists once. The count is `grep -c '\-\->'` over the warning
  output; the tables above are the file:line walk.

---

## Two defects fixed after this plan was written

Both were found by verification work at the end of the round, not by the
original audit, and both were in the product's diagnostic text — the surface
this project treats as the product itself.

### RA015's fix named a construct the milestone refuses

The fix read: *"…or split the inner markup into a separate `.tsx` module the
route does not inline."* Following it verbatim produces **two refusals**, not a
clean file:

```
src/routes/index.page.tsx  →  RA014_NOT_SUPPORTED_YET  (cannot import "./Inner")
src/routes/index.page.tsx  →  RA014_NOT_SUPPORTED_YET  (cannot lower a capitalized tag)
```

This milestone has no bundler and no component inlining, so a sibling `.tsx`
module cannot be imported. The advice was unreachable — the exact class the
project calls its worst defect, and the same class recorded six times before.

**Fixed.** The fix now names only legal moves and says explicitly why the
module split is wrong:

> Reduce the nesting to at most 512 levels by closing elements earlier: move a
> deeply nested subtree into a sibling element, or flatten repeated wrappers
> that carry no attribute. Do not split the markup into another `.tsx` module —
> this milestone has no bundler and no component inlining, so importing one is
> refused with `RA014`.

Verified: the flattened form is accepted (exit 0), and the skill file was
regenerated so `committed_skills_match_the_generator` passes.

### RA014's fix omitted the route-path family

`RA014`'s `fix` enumerated six families — imports, `declare`, route signature,
JSX constructs, duplicate attribute, reserved names — and **none covered a
route-path refusal**. So `[slug]`, dot-prefixed directories, and reserved
`RUNTIME` all received advice about imports and JSX. This was pre-existing: the
`[slug]` refusal had shipped with the same unusable text.

Each refusal's own `message` did name the right repair ("Rename the
directory"), so it was not a total loop, but the `fix` did not.

**Fixed.** A seventh family was added, naming the three path shapes and why each
is refused.

### Pre-existing gaps recorded, not fixed

- **RA011's `explain` is true but incomplete in two places.** It says "the
  router" although no router type exists in this milestone, and "only a direct
  `return`" although a concise arrow body is accepted. Both were verified true
  of the code as written; the wording overstates. Needs a `diagnostic.rs` edit
  with its own review, because it is the message half of a refusal that
  currently works.
- **`RA014` family 5 under-lists the island reserved set.** The summary names
  `_esc`/`_attr`/`render`; the island reserved set is also
  `_a`/`_c`/`_el`/`_t0`/`mount`/`template`. The per-site `message` is correct
  and mode-aware, so the summary is incomplete rather than false.
- **`export declare module` / `export declare global` are accepted while the
  unexported spelling is refused.** Found while de-duplicating the ambient-span
  walks. Closing it adds a diagnostic, so it was left alone and pinned by a test
  that asserts today's behaviour, so it cannot drift silently.
- **A dot-prefixed file stem (`.modules.page.tsx`) is refused by `check` but
  silently skipped by `build`**, whose walk filters dot-files before the rule
  sees them. The code documents this as the safe direction: `check` is never
  less strict than `build`. Exact parity would be a one-line change, but it
  would make `check` green-light a route that never ships.

### Verification at close

| Gate | Result |
|---|---|
| `cargo test --workspace` | **623 passed, 0 failed** |
| `cargo clippy --workspace --all-targets` | **0 warnings** |
| `scripts/size-budget.sh` | **2194 B / 6144, `size ok`** |
| `runai check` on 20 000-deep JSX | `RA015`, exit 1 (was SIGABRT 134) |
| shipped `assets/runtime.js` | 2 131 B gzipped (was 9 616) |
| 100 / 500 static routes | 0.12 s / 0.53 s (were 1.61 s / 8.15 s) |
