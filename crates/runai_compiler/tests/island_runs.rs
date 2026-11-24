//! The island module actually **runs** under a real DOM.
//!
//! Every other island test asserts the *emitted text*. This one proves the
//! artifact: it emits a real route, writes the module to a temp directory, runs it
//! under `bun` with happy-dom (the same engines the runtime's own tests use, via
//! `packages/runai/test/setup.js`), clicks the button, and asserts on the
//! **rendered DOM** after each click.
//!
//! That difference is the whole point. A test that greps the emitted text cannot
//! tell a module that wires `_a[2]` to the button from one that wires it to the
//! wrong node: both parse, both contain `_on(_a[2]`. Only executing the module and
//! reading `innerHTML` proves the right element received the text, the attribute,
//! and the listener. This is the test that would have caught the index-path defect
//! the marker scheme replaced — the fatal case where a div's text landed in a
//! table because the browser re-parented it.
//!
//! The fixture deliberately includes the shapes the parser relocates when the
//! author writes them wrong, plus a module-level const, a signal, a dynamic
//! attribute, a text binding, an event handler, and a reparse-safe nested
//! structure — so one run exercises every binding kind and the ordering contract
//! between `template_html` and the emitter's binding list.

use std::path::PathBuf;
use std::process::Command;

use runai_compiler::codegen::emit_client_checked;

/// The route the test emits. It carries every binding kind in one tree:
///
/// - a module-level `const start = signal(...)` (module_statements);
/// - a body `const n = signal(0)` (body statements);
/// - `class={...}` (a dynamic attribute → `_setAttr`);
/// - `{start() + n()}` and `{n() * 2}` (two text bindings → `_setText`);
/// - `onClick={...}` (an event → `_on`);
/// - a table with a tbody (a reparse-safe structure the parser keeps), and a
///   `<p>` whose children are all inline, so the page is legal *and* non-trivial.
///
/// The dynamic attribute is on the root `<main>` and the two text bindings sit
/// inside a `<p>` and a `<b>` so their marker ordinals differ from the attribute's
/// — the ordering contract is exercised, not just its degenerate one-marker case.
const ROUTE: &str = r#""use island"
import { signal } from "runai"
const start = signal(5)
export default function Page(props: Record<never, never>) {
  const n = signal(0)
  return <main class={n() > 0 ? "on" : "off"}>
    <p>{"start: "}{start() + n()}</p>
    <button onClick={() => n(n() + 1)}>+</button>
    <b>{n() * 2}</b>
    <table><tbody><tr><td>{"cells: "}{n()}</td></tr></tbody></table>
  </main>
}
"#;

/// The path the module is emitted for. A page path, so the checked wrapper's page
/// gate passes; the temp-dir name is irrelevant to the artifact.
const PAGE: &str = "src/routes/index.page.tsx";

/// Where the runtime and happy-dom live, relative to the compiler crate.
fn packages_dir() -> PathBuf {
    // `CARGO_MANIFEST_DIR` is `crates/runai_compiler`; the packages live two up.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("packages")
        .join("runai")
}

/// Whether `bun` is installed. The test skips (with a loud eprintln) when it is
/// not, because this crate's unit tests do not depend on a JS runtime — but the
/// skip is visible, never silent.
fn bun_available() -> bool {
    Command::new("bun")
        .arg("--version")
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

/// The whole test: emit, run under bun, click twice, assert the rendered DOM.
#[test]
fn the_emitted_island_renders_and_reacts_under_bun() {
    if !bun_available() {
        eprintln!("bun is not installed: skipping the island integration test");
        return;
    }

    let packages = packages_dir();
    let happy_dom = packages.join("node_modules/happy-dom/lib/index.js");
    let runtime = packages.join("runtime.js");
    assert!(
        happy_dom.is_file(),
        "happy-dom is not installed at {}: run `bun install` in packages/runai",
        happy_dom.display()
    );
    assert!(
        runtime.is_file(),
        "the shipped runtime is missing at {}",
        runtime.display()
    );

    // The runtime URL the module imports. A `file:` URL resolves under bun without
    // a bundler, and it is the *only* import the module has — so a stray `"runai"`
    // import would fail to resolve here and fail the test, exactly as it would in a
    // browser with no bare-specifier map.
    let runtime_url = format!("file://{}", runtime.display());

    let module = emit_client_checked(ROUTE, PAGE, &runtime_url)
        .expect("a clean island route must emit a module");

    // The module must not smuggle a `runai` import the URL cannot resolve; the
    // checked emitter already refuses that, but asserting it here means a regression
    // fails at the artifact, not two crates away.
    assert!(
        !module.contains("\"runai\""),
        "the emitted module must not name the bare `runai` specifier: {module}"
    );

    let dir = std::env::temp_dir().join(format!(
        "runai-island-runs-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let module_path = dir.join("route.mjs");
    std::fs::write(&module_path, &module).expect("write the emitted module");

    // The driver: install a global DOM, import the emitted module (which mounts
    // itself into `#app`), then click the button twice, printing the rendered
    // `innerHTML` before and after each click. `happy-dom` is imported by absolute
    // path, so the driver needs no package resolution of its own.
    let driver = format!(
        r#"import {{ Window }} from {happy_dom:?}
const window = new Window({{ url: "http://localhost/" }})
const globals = ["document", "Node", "Element", "HTMLElement", "HTMLTemplateElement", "DocumentFragment", "Text", "Event"]
for (const name of globals) globalThis[name] = window[name]
globalThis.document = window.document
globalThis.Event = window.Event
// The mount point the emitted module looks up by id.
const app = window.document.createElement("div")
app.id = "app"
window.document.body.appendChild(app)
await import({module_path:?})
const dump = () => window.document.getElementById("app").innerHTML
const click = () => window.document.querySelector("button").dispatchEvent(new window.Event("click"))
const out = {{ before: dump() }}
click()
out.after1 = dump()
click()
out.after2 = dump()
process.stdout.write(JSON.stringify(out))
"#,
        happy_dom = happy_dom.display().to_string(),
        module_path = module_path.display().to_string(),
    );
    let driver_path = dir.join("driver.mjs");
    std::fs::write(&driver_path, driver).expect("write the driver");

    let output = Command::new("bun")
        .arg(driver_path.as_os_str())
        .output()
        .expect("run bun");

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let _ = std::fs::remove_dir_all(&dir);

    assert!(
        output.status.success(),
        "the emitted island must run under bun.\nmodule:\n{module}\nstderr:\n{stderr}"
    );

    // Parse the JSON the driver printed. It is a flat object of four strings.
    let rendered: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|error| panic!("the driver must print JSON, got {stdout:?}: {error}"));
    let before = rendered["before"].as_str().expect("before");
    let after1 = rendered["after1"].as_str().expect("after1");
    let after2 = rendered["after2"].as_str().expect("after2");

    // Before any click: the module-level signal is 5, the body signal is 0, so the
    // class is "off", the paragraph reads "start: 5", the bold number is "0", the
    // table cell reads "cells: 0". The markers are gone — `_setText` replaced each
    // comment with a text node — so no `<!>` survives in the rendered DOM.
    assert!(
        before.contains("class=\"off\""),
        "the dynamic attribute lands on <main>: {before}"
    );
    assert!(before.contains("start: 5"), "the first text slot: {before}");
    assert!(before.contains("cells: 0"), "the table cell slot: {before}");
    assert!(
        !before.contains("<!>"),
        "every marker is consumed by _setText: {before}"
    );

    // After one click: n == 1, so the class flips to "on", the paragraph becomes
    // "start: 6", the bold number "2", the cell "cells: 1". Every binding re-ran —
    // this is the proof the markers addressed the right nodes.
    assert!(
        after1.contains("class=\"on\""),
        "the attribute tracked the click: {after1}"
    );
    assert!(after1.contains("start: 6"), "the text tracked: {after1}");
    assert!(after1.contains("cells: 1"), "the cell tracked: {after1}");
    assert!(
        after1.contains("<b>2</b>"),
        "the second text slot tracked: {after1}"
    );

    // After the second click: n == 2.
    assert!(
        after2.contains("start: 7"),
        "the second click re-ran every effect: {after2}"
    );
    assert!(after2.contains("cells: 2"), "the cell after two: {after2}");
    assert!(
        after2.contains("<b>4</b>"),
        "the second slot after two: {after2}"
    );

    // The table kept its structure: the marker inside the `<td>` did not escape it,
    // and the text is inside the cell, not fostered out of the table. That is the
    // exact defect index paths produced.
    assert!(
        after2.contains("<table><tbody><tr><td>cells: 2</td></tr></tbody></table>"),
        "the table cell holds the text, not fostered out: {after2}"
    );
}

/// A **void element** with a dynamic attribute (`<img src={u()}/>`): the marker
/// scheme's hardest case, and one the brief's "marker as first child" cannot
/// express — a void element has no children, so the emitter writes the marker as
/// its **following sibling** and resolves it through `previousElementSibling`.
///
/// This runs the module to prove the attribute lands on the `<img>` and a text
/// binding beside it addresses its own node — not the image, not the fragment.
#[test]
fn a_void_element_dynamic_attribute_lands_on_the_element() {
    if !bun_available() {
        eprintln!("bun is not installed: skipping the void-element integration test");
        return;
    }

    let packages = packages_dir();
    let happy_dom = packages.join("node_modules/happy-dom/lib/index.js");
    let runtime = packages.join("runtime.js");
    let runtime_url = format!("file://{}", runtime.display());
    let route = r#""use island"
import { signal } from "runai"
export default function Page(props: Record<never, never>) {
  const u = signal("/a.png")
  return <main><img src={u()} alt="pic"/><span>{u()}</span></main>
}
"#;
    let module = emit_client_checked(route, PAGE, &runtime_url)
        .expect("a void element with a dynamic attribute must emit");
    assert!(
        module.contains("previousElementSibling"),
        "a void element's markers are its following siblings: {module}"
    );

    let dir = std::env::temp_dir().join(format!(
        "runai-island-void-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let module_path = dir.join("route.mjs");
    std::fs::write(&module_path, &module).expect("write the emitted module");

    let driver = format!(
        r#"import {{ Window }} from {happy_dom:?}
const window = new Window({{ url: "http://localhost/" }})
const globals = ["document", "Node", "Element", "HTMLElement", "HTMLTemplateElement", "DocumentFragment", "Text", "Event"]
for (const name of globals) globalThis[name] = window[name]
globalThis.document = window.document
globalThis.Event = window.Event
const app = window.document.createElement("div")
app.id = "app"
window.document.body.appendChild(app)
await import({module_path:?})
const img = window.document.querySelector("img")
process.stdout.write(JSON.stringify({{ html: window.document.getElementById("app").innerHTML, src: img.getAttribute("src") }}))
"#,
        happy_dom = happy_dom.display().to_string(),
        module_path = module_path.display().to_string(),
    );
    let driver_path = dir.join("driver.mjs");
    std::fs::write(&driver_path, driver).expect("write the driver");

    let output = Command::new("bun")
        .arg(driver_path.as_os_str())
        .output()
        .expect("run bun");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let _ = std::fs::remove_dir_all(&dir);

    assert!(
        output.status.success(),
        "the void-element island must run.\nmodule:\n{module}\nstderr:\n{stderr}"
    );
    let rendered: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|error| panic!("the driver must print JSON, got {stdout:?}: {error}"));
    assert_eq!(
        rendered["src"].as_str(),
        Some("/a.png"),
        "the dynamic attribute landed on the <img>, not its sibling"
    );
    let html = rendered["html"].as_str().expect("html");
    assert!(
        html.contains("<img alt=\"pic\" src=\"/a.png\">"),
        "the image carries the attribute: {html}"
    );
    assert!(
        html.contains("<span>/a.png</span>"),
        "the text binding beside the image addressed the span: {html}"
    );
}
