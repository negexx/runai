// packages/runai/test/smoke-island.mjs — usage: bun test/smoke-island.mjs <dist dir>
//
// Loads a **real build** of `examples/counter` and clicks it. This is the only
// check in the repository that starts from the published artifact rather than
// from an emitted string: it reads `dist/index.html`, mounts the same module the
// browser would fetch, clicks the button, and asserts on the rendered text.
//
// The module imports its runtime from `/assets/runtime.js` — a browser-absolute
// path that means nothing to a filesystem. A Bun runtime plugin maps that prefix
// onto the built `dist` directory, so the artifact is imported **unmodified**: the
// specifier the browser resolves is the specifier this run resolves. Rewriting the
// module to a `file://` URL would ship a test of a file the browser never sees.
import { plugin } from "bun"
import { Window } from "happy-dom"
import { readFileSync } from "node:fs"
import { join, resolve } from "node:path"

const dist = resolve(process.argv[2] ?? "")
if (!process.argv[2]) {
  console.error("usage: bun test/smoke-island.mjs <dist dir>")
  process.exit(2)
}

plugin({
  name: "dist-assets",
  setup(build) {
    // `/assets/index.js` and `/assets/runtime.js` both live under `<dist>/assets/`.
    build.onResolve({ filter: /^\/assets\// }, (args) => ({ path: join(dist, args.path) }))
  },
})

// The globals the emitted module and the runtime touch. The list matches
// `crates/runai_compiler/tests/island_runs.rs`, which runs emitted islands the
// same way, so a global missing here would fail both checks rather than one.
const window = new Window({ url: "http://localhost/" })
for (const name of [
  "document",
  "Node",
  "Element",
  "HTMLElement",
  "HTMLTemplateElement",
  "DocumentFragment",
  "Text",
  "Event",
]) {
  globalThis[name] = window[name]
}
globalThis.window = window
globalThis.document = window.document

// The markup, exactly as published, minus the `<script>` tag: the module is
// imported below, which is what the tag would have done. Everything between the
// tags — including the empty `#app` container the island mounts into — is the
// build's own HTML.
const html = readFileSync(join(dist, "index.html"), "utf8")
const body = html.slice(html.indexOf("<body>") + "<body>".length, html.indexOf("</body>"))
document.body.innerHTML = body.replace(/<script[^>]*><\/script>/g, "")

await import("/assets/index.js")

const button = document.querySelector("#app button")
if (!button) {
  console.error(`smoke: the island did not mount a button into #app: ${document.body.innerHTML}`)
  process.exit(1)
}

// The label is two text bindings and a static separator, so a pass means both
// bindings addressed their own node: "0 — even" is `count()`, " — ", `parity()`.
const expect = (want) => {
  if (button.textContent !== want) {
    console.error(`smoke: expected "${want}", got "${button.textContent}"`)
    process.exit(1)
  }
}

expect("0 — even")
button.click()
expect("1 — odd")
button.click()
expect("2 — even")
console.log("smoke ok")
