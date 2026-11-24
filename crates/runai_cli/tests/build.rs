//! `runai build` end to end: a real temp app, the real binary, a real `dist/`.
//!
//! The one test that matters most is `static_expressions_are_evaluated_and_escaped`:
//! it is the only assertion that a prerender module was actually **executed**. A
//! build that emitted the module but never ran it would still pass every
//! containment check here and ship an empty page, so that test is not optional.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

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

fn run(args: &[&str], dir: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_runai"))
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap()
}

/// A static route body that renders `text`.
fn static_route(text: &str) -> String {
    format!(
        "\"use static\"\nexport default function P(props: Record<never, never>) {{ return <p>{text}</p> }}\n"
    )
}

/// Every file under `root`, relative, sorted — the tree a rebuild must not change.
fn tree(root: &Path) -> Vec<String> {
    let mut found = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else {
                found.push(
                    path.strip_prefix(root)
                        .unwrap()
                        .to_string_lossy()
                        .replace('\\', "/"),
                );
            }
        }
    }
    found.sort();
    found
}

// C1, the most important test in the task: `build --out src` must never publish
// over the source tree. Before the stamp existed, `publish` renamed `src` aside and
// deleted it, taking `src/routes/index.page.tsx` with it and exiting 0.
#[test]
fn build_into_the_source_tree_is_refused_and_leaves_it_intact() {
    let tmp = tempfile::tempdir().unwrap();
    write_app(tmp.path(), &static_route("hi"));
    let before = std::fs::read(tmp.path().join("src/routes/index.page.tsx")).unwrap();

    let out = run(&["build", "--out", "src"], tmp.path());

    assert_eq!(
        out.status.code(),
        Some(2),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stderr).contains("usage: runai"));
    let route = tmp.path().join("src/routes/index.page.tsx");
    assert!(
        route.exists(),
        "the source route must survive the refused build"
    );
    assert_eq!(std::fs::read(&route).unwrap(), before);
    assert!(!tmp.path().join("src/.runai-build").exists());
}

// The stamp is what makes a second build legal: a directory this tool created is
// recognisably a build, and a fresh path works the first time.
#[test]
fn a_first_build_writes_the_stamp_and_a_second_build_succeeds() {
    let tmp = tempfile::tempdir().unwrap();
    write_app(tmp.path(), &static_route("v1"));

    let first = run(&["build", "--out", "dist"], tmp.path());
    assert_eq!(
        first.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    assert!(tmp.path().join("dist/.runai-build").is_file());
    assert!(tmp.path().join("dist/index.html").is_file());

    let second = run(&["build", "--out", "dist"], tmp.path());
    assert_eq!(
        second.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );
    assert!(tmp.path().join("dist/.runai-build").is_file());
    assert!(tmp.path().join("dist/index.html").is_file());
}

// `--out` values that name the working tree, the parent, or nothing are usage
// errors, not builds that fail halfway.
#[test]
fn degenerate_out_values_are_usage_errors() {
    let tmp = tempfile::tempdir().unwrap();
    write_app(tmp.path(), &static_route("hi"));
    for args in [
        vec!["build", "--out", "."],
        vec!["build", "--out", ".."],
        vec!["build", "--out="],
        vec!["build", "--out", ""],
    ] {
        let out = run(&args, tmp.path());
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        assert!(String::from_utf8_lossy(&out.stderr).contains("usage: runai"));
        assert!(!tmp.path().join("index.html").exists());
    }
}

// C2: `about.page.tsx` and `about/index.page.tsx` both map to `about`. Silently
// keeping the last one shipped a page that was never written.
#[test]
fn colliding_route_names_are_refused_naming_both_files() {
    let tmp = tempfile::tempdir().unwrap();
    write_app(tmp.path(), &static_route("root"));
    std::fs::create_dir_all(tmp.path().join("src/routes/about")).unwrap();
    std::fs::write(
        tmp.path().join("src/routes/about.page.tsx"),
        static_route("STATIC"),
    )
    .unwrap();
    std::fs::write(
        tmp.path().join("src/routes/about/index.page.tsx"),
        "\"use island\"\nexport default function P(props: Record<never, never>) { return <p>ISLAND</p> }\n",
    )
    .unwrap();

    let out = run(&["build", "--out", "dist"], tmp.path());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("about.page.tsx"), "{stderr}");
    assert!(stderr.contains("about/index.page.tsx"), "{stderr}");
    assert!(stderr.contains("about"), "{stderr}");
    assert!(!tmp.path().join("dist").exists());
}

// C3: the island asset `assets/runtime.js` is also where the runtime is copied, so
// a route named `runtime` shipped the runtime as its own module and mounted nothing.
#[test]
fn an_island_route_named_runtime_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    write_app(
        tmp.path(),
        "\"use island\"\nexport default function P(props: Record<never, never>) { return <button onClick={() => {}}>x</button> }\n",
    );
    std::fs::write(
        tmp.path().join("src/routes/runtime.page.tsx"),
        "\"use island\"\nexport default function P(props: Record<never, never>) { return <button onClick={() => {}}>x</button> }\n",
    )
    .unwrap();

    let out = run(&["build", "--out", "dist"], tmp.path());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("runtime.page.tsx"), "{stderr}");
    assert!(!tmp.path().join("dist").exists());
}

// A failed rebuild must leave the previous `dist/` byte-identical, not merely
// present. The island-then-no-config path fails after every route was written, in
// the middle of publishing, which is the window the reviewer cared about.
#[test]
fn a_failed_rebuild_leaves_the_previous_dist_byte_identical() {
    let tmp = tempfile::tempdir().unwrap();
    write_app(tmp.path(), &static_route("v1"));
    assert_eq!(
        run(&["build", "--out", "dist"], tmp.path()).status.code(),
        Some(0)
    );

    let page = tmp.path().join("dist/index.html");
    let before_page = std::fs::read(&page).unwrap();
    let before_tree = tree(&tmp.path().join("dist"));

    std::fs::write(
        tmp.path().join("src/routes/index.page.tsx"),
        "\"use island\"\nexport default function P(props: Record<never, never>) { return <button onClick={() => {}}>x</button> }\n",
    )
    .unwrap();
    std::fs::remove_file(tmp.path().join("runai.json")).unwrap();

    let out = run(&["build", "--out", "dist"], tmp.path());
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(std::fs::read(&page).unwrap(), before_page);
    assert_eq!(tree(&tmp.path().join("dist")), before_tree);
    assert!(!tmp.path().join("dist.tmp").exists());
    assert!(!tmp.path().join("dist.old").exists());
}

// The runtime path is resolved against `runai.json`, and the message names that
// resolved path so the repair is obvious. A copy of a missing file ships an island
// whose script 404s and looks fine.
#[test]
fn a_missing_runtime_is_named_and_writes_no_dist() {
    let tmp = tempfile::tempdir().unwrap();
    write_app(
        tmp.path(),
        "\"use island\"\nexport default function P(props: Record<never, never>) { return <button onClick={() => {}}>x</button> }\n",
    );
    std::fs::write(
        tmp.path().join("runai.json"),
        "{\"runtime\":\"missing/runtime.js\"}\n",
    )
    .unwrap();

    let out = run(&["build", "--out", "dist"], tmp.path());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("missing/runtime.js"), "{stderr}");
    assert!(!tmp.path().join("dist").exists());
}

// The scratch directory is `<out>.tmp/.modules`, so a route under `.modules` was
// written and then deleted with the scratch tree: a 404 that built cleanly.
#[test]
fn a_dot_modules_route_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    write_app(tmp.path(), &static_route("root"));
    std::fs::create_dir_all(tmp.path().join("src/routes/.modules")).unwrap();
    std::fs::write(
        tmp.path().join("src/routes/.modules/hidden.page.tsx"),
        static_route("hidden"),
    )
    .unwrap();

    let out = run(&["build", "--out", "dist"], tmp.path());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains(".modules"), "{stderr}");
    assert!(!tmp.path().join("dist").exists());
}

// A hidden *file* is skipped by `walk` before `route_name` sees it, so it cannot be
// a route; a hidden *directory* is reached, and `route_name` refuses it. Pinning only
// the directory case is deliberate: a test on `.page.tsx` would assert a filter that
// already ran, not a rule the build enforces.
#[test]
fn a_hidden_route_directory_is_refused_not_silently_skipped() {
    let tmp = tempfile::tempdir().unwrap();
    write_app(tmp.path(), &static_route("root"));
    std::fs::create_dir_all(tmp.path().join("src/routes/.cache")).unwrap();
    std::fs::write(
        tmp.path().join("src/routes/.cache/x.page.tsx"),
        static_route("hidden"),
    )
    .unwrap();

    let out = run(&["build", "--out", "dist"], tmp.path());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains(".cache"), "{stderr}");
    assert!(stderr.contains("hidden directory"), "{stderr}");
    assert!(!tmp.path().join("dist").exists());
}

// The stamp gate itself, not just the `.tsx` scan: a non-empty directory with no
// stamp and no source file is still not ours, so it is refused with exit 2, naming
// it, and nothing is deleted.
#[test]
fn a_non_build_out_directory_is_refused_and_left_alone() {
    let tmp = tempfile::tempdir().unwrap();
    write_app(tmp.path(), &static_route("hi"));
    std::fs::create_dir_all(tmp.path().join("dist")).unwrap();
    std::fs::write(tmp.path().join("dist/notes.txt"), "keep me\n").unwrap();

    let out = run(&["build", "--out", "dist"], tmp.path());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(2), "{stderr}");
    assert!(stderr.contains("dist"), "{stderr}");
    assert!(stderr.contains("previous build"), "{stderr}");
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("dist/notes.txt")).unwrap(),
        "keep me\n"
    );
    assert!(!tmp.path().join("dist/index.html").exists());
}

// I4: a crash between publish's two renames strands the good tree in `<out>.old`.
// The next build — here one that fails — must put it back first.
#[test]
fn a_stranded_previous_build_is_restored_before_the_next_build() {
    let tmp = tempfile::tempdir().unwrap();
    write_app(tmp.path(), &static_route("v1"));
    assert_eq!(
        run(&["build", "--out", "dist"], tmp.path()).status.code(),
        Some(0)
    );
    let before = std::fs::read(tmp.path().join("dist/index.html")).unwrap();

    // Simulate the crash: the live tree is gone, the good one sits in `.old`.
    std::fs::rename(tmp.path().join("dist"), tmp.path().join("dist.old")).unwrap();
    assert!(!tmp.path().join("dist").exists());

    // A failing build, so nothing republishes: only the recovery is under test.
    std::fs::write(
        tmp.path().join("runai.json"),
        "{\"runtime\":\"missing.js\"}\n",
    )
    .unwrap();
    std::fs::write(
        tmp.path().join("src/routes/index.page.tsx"),
        "\"use island\"\nexport default function P(props: Record<never, never>) { return <button onClick={() => {}}>x</button> }\n",
    )
    .unwrap();

    let out = run(&["build", "--out", "dist"], tmp.path());
    assert_eq!(out.status.code(), Some(1));
    assert!(
        tmp.path().join("dist").exists(),
        "the stranded build must be restored: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        std::fs::read(tmp.path().join("dist/index.html")).unwrap(),
        before
    );
    assert!(!tmp.path().join("dist.old").exists());
}

// A route named `assets` would write `assets/index.html` next to `assets/*.js`.
// That is not a collision today, but the runtime copy lives in `assets/runtime.js`
// and a first segment of `assets` is the one shape a future asset name could hit;
// pin the current behaviour: it builds, and the JS still lands where the HTML says.
#[test]
fn a_route_named_assets_still_places_its_script_where_the_html_points() {
    let tmp = tempfile::tempdir().unwrap();
    write_app(tmp.path(), &static_route("root"));
    std::fs::create_dir_all(tmp.path().join("src/routes/assets")).unwrap();
    std::fs::write(
        tmp.path().join("src/routes/assets/index.page.tsx"),
        "\"use island\"\nexport default function P(props: Record<never, never>) { return <button onClick={() => {}}>x</button> }\n",
    )
    .unwrap();

    let out = run(&["build", "--out", "dist"], tmp.path());
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let html = std::fs::read_to_string(tmp.path().join("dist/assets/index.html")).unwrap();
    assert!(html.contains("/assets/assets.js"), "{html}");
    assert!(tmp.path().join("dist/assets/assets.js").is_file());
}

// `mkdir dist && runai build` is a normal CI shape. An empty directory holds nothing
// to destroy, so it is replaced and stamped; the gate is about content, not names.
#[test]
fn an_empty_existing_out_directory_is_built_over() {
    let tmp = tempfile::tempdir().unwrap();
    write_app(tmp.path(), &static_route("hi"));
    std::fs::create_dir(tmp.path().join("dist")).unwrap();

    let out = run(&["build", "--out", "dist"], tmp.path());
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(tmp.path().join("dist/.runai-build").is_file());

    // And the second build over its own stamped output still works.
    let again = run(&["build", "--out", "dist"], tmp.path());
    assert_eq!(
        again.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&again.stderr)
    );
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

// N3: `<out>.tmp` is scratch for THIS build, but its name is derived from `--out`.
// A pre-existing `dist.tmp` that is not a stamped leftover is somebody's directory,
// and the old build deleted it with everything in it (exit 0).
#[test]
fn a_preexisting_unstamped_scratch_directory_is_refused_and_left_intact() {
    let tmp = tempfile::tempdir().unwrap();
    write_app(tmp.path(), &static_route("hi"));
    std::fs::create_dir_all(tmp.path().join("dist.tmp/src")).unwrap();
    std::fs::write(tmp.path().join("dist.tmp/src/main.rs"), "fn main() {}\n").unwrap();
    std::fs::write(tmp.path().join("dist.tmp/IMPORTANT.txt"), "keep me\n").unwrap();

    let out = run(&["build", "--out", "dist"], tmp.path());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(2), "{stderr}");
    assert!(stderr.contains("dist.tmp"), "{stderr}");
    assert!(stderr.contains("scratch space"), "{stderr}");
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("dist.tmp/IMPORTANT.txt")).unwrap(),
        "keep me\n"
    );
    assert!(tmp.path().join("dist.tmp/src/main.rs").exists());
    assert!(!tmp.path().join("dist").exists());
}

// N2: a static route ships no asset, so a static route named `runtime` collides with
// nothing and must build. The old guard ran by name, before the mode was known, and
// refused it with a message that was false for it.
#[test]
fn a_static_route_named_runtime_builds_normally() {
    let tmp = tempfile::tempdir().unwrap();
    write_app(tmp.path(), &static_route("root"));
    std::fs::write(
        tmp.path().join("src/routes/runtime.page.tsx"),
        static_route("runtime page"),
    )
    .unwrap();

    let out = run(&["build", "--out", "dist"], tmp.path());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{stderr}");
    let html = std::fs::read_to_string(tmp.path().join("dist/runtime/index.html")).unwrap();
    assert!(html.contains("<p>runtime page</p>"), "{html}");
    assert!(!html.contains("<script"), "{html}");
    assert!(!tmp.path().join("dist/assets/runtime.js").exists());
}

// N1: on a case-insensitive filesystem `Runtime.page.tsx` → `assets/Runtime.js` IS
// `assets/runtime.js`, so the runtime copy silently overwrote the page module. The
// rule compares case-insensitively everywhere, so it fails closed on Linux too.
#[test]
fn an_island_route_named_uppercase_runtime_is_refused_case_insensitively() {
    let tmp = tempfile::tempdir().unwrap();
    write_app(tmp.path(), &static_route("root"));
    std::fs::write(
        tmp.path().join("src/routes/Runtime.page.tsx"),
        "\"use island\"\nexport default function P(props: Record<never, never>) { return <button onClick={() => {}}>x</button> }\n",
    )
    .unwrap();

    let out = run(&["build", "--out", "dist"], tmp.path());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("Runtime.page.tsx"), "{stderr}");
    assert!(stderr.contains("overwrite this route's module"), "{stderr}");
    assert!(!tmp.path().join("dist").exists());
}

// N1: `About.page.tsx` and `about/index.page.tsx` map to one output on a
// case-insensitive filesystem (`About/index.html` == `about/index.html`), so the
// duplicate-name guard compares case-insensitively as well.
#[test]
fn case_colliding_route_names_are_refused() {
    let tmp = tempfile::tempdir().unwrap();
    write_app(tmp.path(), &static_route("root"));
    std::fs::write(
        tmp.path().join("src/routes/About.page.tsx"),
        static_route("upper"),
    )
    .unwrap();
    std::fs::create_dir_all(tmp.path().join("src/routes/about")).unwrap();
    std::fs::write(
        tmp.path().join("src/routes/about/index.page.tsx"),
        static_route("lower"),
    )
    .unwrap();

    let out = run(&["build", "--out", "dist"], tmp.path());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("About.page.tsx"), "{stderr}");
    assert!(stderr.contains("about/index.page.tsx"), "{stderr}");
    assert!(!tmp.path().join("dist").exists());
}

// N4: `--out src/dist` does not exist yet, so the old `*.tsx` scan never ran and the
// build wrote into the source tree. The nearest existing ancestor is inside `src`.
#[test]
fn out_inside_the_source_tree_is_refused_even_when_it_does_not_exist() {
    let tmp = tempfile::tempdir().unwrap();
    write_app(tmp.path(), &static_route("hi"));

    let out = run(&["build", "--out", "src/dist"], tmp.path());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(2), "{stderr}");
    assert!(stderr.contains("usage: runai"), "{stderr}");
    assert!(!tmp.path().join("src/dist").exists());
    assert!(tmp.path().join("src/routes/index.page.tsx").exists());
}

// N5: `route_name(..)?` returned early and dropped the diagnostics already collected.
// One run must report both the reserved name and the other route's problem.
#[test]
fn a_reserved_name_and_a_route_diagnostic_are_both_reported() {
    let tmp = tempfile::tempdir().unwrap();
    write_app(tmp.path(), &static_route("root"));
    std::fs::write(
        tmp.path().join("src/routes/bad.page.tsx"),
        "\"use static\"\nimport { signal } from \"runai\"\nexport default function P(props: Record<never, never>) { const n = signal(0); return <p>{n()}</p> }\n",
    )
    .unwrap();
    std::fs::create_dir_all(tmp.path().join("src/routes/.modules")).unwrap();
    std::fs::write(
        tmp.path().join("src/routes/.modules/x.page.tsx"),
        static_route("reserved"),
    )
    .unwrap();

    let out = run(&["build", "--out", "dist"], tmp.path());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains("RA010"),
        "the diagnostic must survive: {stderr}"
    );
    assert!(
        stderr.contains(".modules"),
        "the reserved name must be reported: {stderr}"
    );
    assert!(!tmp.path().join("dist").exists());
}
