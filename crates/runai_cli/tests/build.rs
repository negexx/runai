//! `runai build` end to end: a real temp app, the real binary, a real `dist/`.
//!
//! The one test that matters most is `static_expressions_are_evaluated_and_escaped`:
//! it is the only assertion that a prerender module was actually **executed**. A
//! build that emitted the module but never ran it would still pass every
//! containment check here and ship an empty page, so that test is not optional.

use std::path::{Path, PathBuf};

/// The shipped runtime, so `runai.json` can point at a real file.
fn runtime() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("packages")
        .join("runai")
        .join("runtime.js")
}

/// A minimal app: one `src/routes/index.page.tsx` plus a `runai.json` naming the
/// shipped runtime by absolute path.
fn write_app(root: &Path, route: &str) {
    std::fs::create_dir_all(root.join("src/routes")).unwrap();
    std::fs::write(root.join("src/routes/index.page.tsx"), route).unwrap();
    let runtime = runtime();
    std::fs::write(
        root.join("runai.json"),
        format!("{{\"runtime\":{:?}}}\n", runtime.display().to_string()),
    )
    .unwrap();
}

#[test]
fn static_route_html_has_no_script_tag() {
    let tmp = tempfile::tempdir().unwrap();
    write_app(
        tmp.path(),
        "\"use static\"\nexport default function P(props: Record<never, never>) { return <p>hi</p> }\n",
    );
    let status = std::process::Command::new(env!("CARGO_BIN_EXE_runai"))
        .args(["build", "--out", "dist"])
        .current_dir(tmp.path())
        .status()
        .unwrap();
    assert!(status.success());
    let html = std::fs::read_to_string(tmp.path().join("dist/index.html")).unwrap();
    assert!(html.contains("<p>hi</p>"));
    assert!(
        !html.contains("<script"),
        "static route must ship no JS: {html}"
    );
}

#[test]
fn island_route_emits_one_script_and_a_runtime() {
    let tmp = tempfile::tempdir().unwrap();
    write_app(
        tmp.path(),
        "\"use island\"\nexport default function P(props: Record<never, never>) { return <button onClick={() => {}}>x</button> }\n",
    );
    let status = std::process::Command::new(env!("CARGO_BIN_EXE_runai"))
        .args(["build", "--out", "dist"])
        .current_dir(tmp.path())
        .status()
        .unwrap();
    assert!(status.success());
    let html = std::fs::read_to_string(tmp.path().join("dist/index.html")).unwrap();
    assert_eq!(html.matches("<script").count(), 1);
    assert!(tmp.path().join("dist/assets/runtime.js").exists());
    assert!(tmp.path().join("dist/assets/index.js").exists());
}

#[test]
fn diagnostics_stop_the_build() {
    let tmp = tempfile::tempdir().unwrap();
    write_app(
        tmp.path(),
        "\"use static\"\nimport { signal } from \"runai\"\nexport default function P(props: Record<never, never>) { const n = signal(0); return <p>{n()}</p> }\n",
    );
    let status = std::process::Command::new(env!("CARGO_BIN_EXE_runai"))
        .args(["build", "--out", "dist"])
        .current_dir(tmp.path())
        .status()
        .unwrap();
    assert!(!status.success());
    assert!(!tmp.path().join("dist/index.html").exists());
}

#[test]
fn static_expressions_are_evaluated_and_escaped() {
    let tmp = tempfile::tempdir().unwrap();
    write_app(
        tmp.path(),
        "\"use static\"\nexport default function P(props: Record<never, never>) { const name = \"<Ada>\"; return <p>Hi {name}</p> }\n",
    );
    let status = std::process::Command::new(env!("CARGO_BIN_EXE_runai"))
        .args(["build", "--out", "dist"])
        .current_dir(tmp.path())
        .status()
        .unwrap();
    assert!(status.success());
    let html = std::fs::read_to_string(tmp.path().join("dist/index.html")).unwrap();
    assert!(html.contains("<p>Hi &lt;Ada&gt;</p>"), "{html}");
}

#[test]
fn dynamic_segment_is_ra014_not_silent_output() {
    let tmp = tempfile::tempdir().unwrap();
    write_app(
        tmp.path(),
        "\"use static\"\nexport default function P(props: Record<never, never>) { return <p>hi</p> }\n",
    );
    std::fs::create_dir_all(tmp.path().join("src/routes/blog")).unwrap();
    std::fs::write(
        tmp.path().join("src/routes/blog/[slug].page.tsx"),
        "\"use static\"\nexport default function P(props: Record<never, never>) { return <p>post</p> }\n",
    )
    .unwrap();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_runai"))
        .args(["build", "--out", "dist"])
        .current_dir(tmp.path())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("RA014_NOT_SUPPORTED_YET"));
    assert!(!tmp.path().join("dist").exists());
}
