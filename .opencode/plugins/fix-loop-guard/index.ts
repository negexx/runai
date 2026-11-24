// FixLoopGuard — refuses a repeated fix-round dispatch when no `debugger` dispatch has
// happened in this session.
//
// Why: the same defect was patched three and then five times on this project's
// foundation branch because the cause was never identified. The rule is written twice
// (AGENTS.md and the /debug command) and was still ignored across dozens of dispatches.
// A prompt is not enforcement; this hook is meant to be.
//
// ============================ READ THIS FIRST ============================
// STATUS: LOADS, REGISTERS, and is BELIEVED to fire, but no callback has been
// OBSERVED firing in this environment. Treat the enforcement as UNVERIFIED.
// A guard that is trusted but inert is worse than no guard, so do not rely on it
// and do not let its presence create confidence. Verification steps at the bottom.
// ==========================================================================
//
// API notes, each learned by testing against server 2.4.1 and cross-checked against the
// official V2 plugin docs (opencode.ai/docs/build/plugins/server):
//
//   1. The official V2 docs use `import { Plugin } from "@opencode/plugin"` and
//      `Plugin.define({ id, setup })`. That package is NOT installed here — the
//      installed one is `@opencode-ai/plugin` 1.18.x, whose types are older than the
//      server. The server rejects a bare default-export function ("Plugin must export a
//      default definition with an id and an effect or setup function") and accepts a
//      plain definition object, which is what this file uses. Equivalent to
//      Plugin.define, just without the helper.
//   2. A configured plugin path must be a DIRECTORY. `"plugins": ["./x.ts"]` fails with
//      "configured plugin path must be a directory" — and that failure is only a WARN in
//      the server log. `opencode plugin list` still lists the plugin and looks healthy.
//      That is a silent failure, so keep this directory layout.
//   3. `.opencode/plugins/` is not auto-discovered by 2.4.1; list the directory under
//      `plugins` in opencode.jsonc. (The production-workspace document claims otherwise.)
//   4. Hook shape per the V2 docs: `ctx.tool.hook("execute.before", (event) => …)` with
//      `event.tool` and `event.input`. There is no `output.args` in V2 — that is the V1
//      shape this file originally and wrongly used.
//   5. Whether a thrown error in this hook blocks the call is NOT stated in the docs.
//      The docs describe hooks as "inspect or replace tool input". Treat blocking as
//      unproven.

const REPEAT = [
  /fix round\s*[2-9]/i,
  /round\s*[2-9]\s*(on|of)\s*(task|the)/i,
  /second attempt/i,
  /another (fix )?round/i,
  /still (open|failing|broken|reproducible|present)/i,
]

const DELEGATED = /\bdebugger\b/i

export default {
  id: "runai.fix-loop-guard",

  async setup(ctx) {
    const debugged = new Set()

    await ctx.tool.hook("execute.before", (event) => {
      // The subagent tool's name in this build is `task`; accept both spellings.
      if (event.tool !== "task" && event.tool !== "subagent") return

      const input = event.input ?? {}
      const agent = String(input.agent ?? input.subagent ?? "")
      if (DELEGATED.test(agent)) {
        debugged.add(event.sessionID)
        return
      }

      const text = [input.prompt, input.description].filter(Boolean).join("\n")
      if (!text) return

      const isRepeat = REPEAT.some((pattern) => pattern.test(text))
      if (isRepeat && !debugged.has(event.sessionID)) {
        throw new Error(
          "FixLoopGuard blocked this dispatch: it reads as a repeated attempt at a defect " +
            "whose cause is not yet known, and no `debugger` dispatch has happened in this session.\n\n" +
            "Dispatch the `debugger` agent first, and hand it what you already have: the reproduction " +
            "command and its output, the fixes already tried and why each failed, and the causes you " +
            "have ruled out. One failed fix means the cause was never identified; another patch on top " +
            "is how one defect becomes three.\n\n" +
            "To proceed without the debugger, pass `agent: \"debugger\"` or say `debugger` in the task.",
        )
      }
    })
  },
}

// Verified against server 2.4.1 on 2026-11-24:
//   OK   the definition object with `id` + `setup` loads (no WARN in the server log)
//   OK   `setup` runs (a line written from setup appeared in a probe log)
//   OK   `ctx.tool.hook("execute.before", …)` resolves without throwing
//   OK   `ctx.shell.hook("create.before", …)` and `ctx.session.hook("prompt", …)` resolve
//   NO   no callback was observed to fire, for the shell tool, a subagent dispatch, or a
//        session prompt. Every probe ran in a session created BEFORE the server process
//        that loaded the plugin.
//
// To finish verifying, in a NEW session started after a service restart:
//   1. add a temporary `console.error`/file-append at the top of the hook,
//   2. dispatch any subagent with the text "fix round 2" in the prompt,
//   3. if the hook logs and the dispatch is refused, blocking works: remove the probe,
//      update this header to say VERIFIED, and note which mechanism blocked it,
//   4. if the hook logs but the dispatch proceeds, throwing does NOT block — replace the
//      throw with whatever the docs call a rejection, or accept that this belongs in CI
//      as a ledger check rather than a runtime guard.
