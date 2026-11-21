// DOM globals for the runtime tests. Registered via `bunfig.toml` so the
// existing command (`bun test`, no flags) keeps working, and the same file
// works under `bun test --preload ./test/setup.js`.
//
// Only the globals the runtime touches are installed: it calls
// `document.createElement`, `appendChild`/`insertBefore`/`removeChild`, and
// reads `parentNode`. AbortController/AbortSignal are Bun's natives (they are
// spec-complete) rather than happy-dom's copies, which would not be the same
// class Bun's timers/HTTP layers expect.
import { Window } from "happy-dom"

const window = new Window({ url: "http://localhost/" })

const globals = {
  window,
  document: window.document,
  Node: window.Node,
  Element: window.Element,
  HTMLElement: window.HTMLElement,
  HTMLTemplateElement: window.HTMLTemplateElement,
  DocumentFragment: window.DocumentFragment,
  Text: window.Text,
  Event: window.Event,
}

for (const [name, value] of Object.entries(globals)) {
  if (value) globalThis[name] = value
}
