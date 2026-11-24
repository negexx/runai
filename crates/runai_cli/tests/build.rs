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

/// A static route that busy-waits for `millis` inside `render`, so a build can be
/// observed while it holds the output lock. The loop is in the route body, so it runs
/// inside the prerender child — i.e. **after** the lock is taken and the scratch
/// directory exists.
fn slow_route(millis: u64) -> String {
    format!(
        "\"use static\"\nexport default function P(props: Record<never, never>) {{\n\
         const until = Date.now() + {millis};\n\
         while (Date.now() < until) {{}}\n\
         return <p>slow</p>\n}}\n"
    )
}

/// Whether `root` holds a scratch directory of this build's naming scheme.
fn has_scratch(root: &Path) -> bool {
    std::fs::read_dir(root)
        .map(|entries| {
            entries.filter_map(Result::ok).any(|entry| {
                let name = entry.file_name().to_string_lossy().into_owned();
                name.starts_with("dist.tmp.")
            })
        })
        .unwrap_or(false)
}

/// Kills and reaps the child on drop, including when the test panics, so a slow build
/// under test cannot outlive its assertions.
struct Kill(std::process::Child);

impl Drop for Kill {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
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
    assert!(stderr.contains("dot-prefixed directory"), "{stderr}");
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

// D: the dot-*file* skip in `walk` is what stops `.page.tsx` becoming the root route
// and shipping its contents (that was a real defect). It is a different rule from the
// hidden-directory refusal above, so it needs its own end-to-end pin: with both files
// present, the REAL `index.page.tsx` must build and no route may come from the dot file.
#[test]
fn a_dot_page_file_beside_the_real_index_ships_only_the_real_index() {
    let tmp = tempfile::tempdir().unwrap();
    write_app(tmp.path(), &static_route("REAL INDEX"));
    std::fs::write(
        tmp.path().join("src/routes/.page.tsx"),
        static_route("DOT FILE"),
    )
    .unwrap();

    let out = run(&["build", "--out", "dist"], tmp.path());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{stderr}");
    let html = std::fs::read_to_string(tmp.path().join("dist/index.html")).unwrap();
    assert!(html.contains("<p>REAL INDEX</p>"), "{html}");
    assert!(
        !html.contains("DOT FILE"),
        "the dot file must not ship: {html}"
    );
    assert_eq!(
        tree(&tmp.path().join("dist")),
        vec![".runai-build", "index.html"]
    );
}

// B: the dot rule covers every segment of the relative route path. `a/.b/c.page.tsx`
// shipped `dist/a/.b/c/index.html` while only the first segment was checked, which is
// the same "static servers 404 a dot-prefixed path" defect one level deeper.
#[test]
fn a_dot_segment_below_the_first_is_refused_and_a_normal_route_still_builds() {
    let tmp = tempfile::tempdir().unwrap();
    write_app(tmp.path(), &static_route("root"));
    std::fs::create_dir_all(tmp.path().join("src/routes/a/.b")).unwrap();
    std::fs::write(
        tmp.path().join("src/routes/a/.b/c.page.tsx"),
        static_route("hidden"),
    )
    .unwrap();

    let out = run(&["build", "--out", "dist"], tmp.path());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains(".b"), "{stderr}");
    assert!(stderr.contains("dot-prefixed directory"), "{stderr}");
    assert!(!tmp.path().join("dist").exists());

    // The same tree without the hidden segment builds: the rule is about the segment,
    // not about nesting.
    std::fs::remove_dir_all(tmp.path().join("src/routes/a/.b")).unwrap();
    std::fs::create_dir_all(tmp.path().join("src/routes/blog")).unwrap();
    std::fs::write(
        tmp.path().join("src/routes/blog/x.page.tsx"),
        static_route("post"),
    )
    .unwrap();
    let ok = run(&["build", "--out", "dist"], tmp.path());
    assert_eq!(
        ok.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&ok.stderr)
    );
    assert!(tmp.path().join("dist/blog/x/index.html").is_file());
}

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

// A leftover scratch directory with content and NO marker is not ours, so it is never
// deleted — but it is also not a reason to refuse the build: scratch names are unique
// per build now, so an unrecognised one sits beside the build instead of blocking it.
// Round 1 deleted such a directory (exit 0, user data gone); rounds 2 and 3 refused it
// (exit 2 forever). Both were wrong: the build simply does not need that name.
//
// Both naming schemes are covered because both can be left on disk: `dist.tmp` by the
// previous release, `dist.tmp.<pid>.<n>` by this one when a run dies before the marker.
#[test]
fn an_unmarked_scratch_directory_is_left_alone_and_the_build_succeeds() {
    let tmp = tempfile::tempdir().unwrap();
    write_app(tmp.path(), &static_route("hi"));
    for name in ["dist.tmp", "dist.tmp.4294967294.0"] {
        let leftover = tmp.path().join(name);
        std::fs::create_dir_all(leftover.join("keep")).unwrap();
        std::fs::write(leftover.join("keep/notes.md"), "mine\n").unwrap();

        let out = run(&["build", "--out", "dist"], tmp.path());
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(0), "{name}: {stderr}");
        assert!(tmp.path().join("dist/index.html").is_file(), "{name}");
        assert_eq!(
            std::fs::read_to_string(leftover.join("keep/notes.md")).unwrap(),
            "mine\n",
            "{name} must survive"
        );
        std::fs::remove_dir_all(&leftover).unwrap();
    }
}

// A build killed mid-flight (Ctrl-C, OOM, power loss) leaves its scratch directory with
// the marker and no completeness stamp, because the stamp is written last. The marker
// licenses deleting it: refusing would wedge every later build with advice about a
// directory this tool created and abandoned.
#[test]
fn a_crashed_builds_scratch_is_swept_and_rebuilt() {
    let tmp = tempfile::tempdir().unwrap();
    write_app(tmp.path(), &static_route("rebuilt"));

    // The window a real crash leaves: marker present, route-ish content present,
    // `.modules/` scratch present, completeness stamp absent.
    let leftover = tmp.path().join("dist.tmp.4294967294.0");
    std::fs::create_dir_all(leftover.join(".modules")).unwrap();
    std::fs::create_dir_all(leftover.join("assets")).unwrap();
    std::fs::write(leftover.join(".runai-scratch"), "runai 0.1.0 build\n").unwrap();
    std::fs::write(leftover.join("index.html"), "abandoned\n").unwrap();
    std::fs::write(leftover.join(".modules/route0.mjs"), "partial\n").unwrap();
    std::fs::write(leftover.join("assets/index.js"), "partial\n").unwrap();
    assert!(!leftover.join(".runai-build").exists());

    let out = run(&["build", "--out", "dist"], tmp.path());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{stderr}");
    let html = std::fs::read_to_string(tmp.path().join("dist/index.html")).unwrap();
    assert!(html.contains("<p>rebuilt</p>"), "{html}");
    assert!(!leftover.exists(), "the dead build's scratch is swept");
    assert!(tmp.path().join("dist/.runai-build").is_file());
    // The marker is scratch bookkeeping, not site content.
    assert!(!tmp.path().join("dist/.runai-scratch").exists());
}

// The previous release used the fixed name `<out>.tmp`. A build killed after creating it
// but before writing the marker leaves exactly an EMPTY directory with that name; under
// the content-sniffing scheme that wedged every later build (exit 2 forever). An empty
// directory holds nothing to lose, so it is removed and the build runs.
#[test]
fn an_empty_legacy_scratch_directory_does_not_wedge_the_build() {
    let tmp = tempfile::tempdir().unwrap();
    write_app(tmp.path(), &static_route("built"));
    std::fs::create_dir(tmp.path().join("dist.tmp")).unwrap();

    let out = run(&["build", "--out", "dist"], tmp.path());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{stderr}");
    assert!(tmp.path().join("dist/index.html").is_file());
    assert!(
        !tmp.path().join("dist.tmp").exists(),
        "the empty leftover is swept"
    );
}

// A live build's scratch carries the same marker, so a marker alone cannot authorize
// deletion: a concurrent build's sweep would delete the peer's tree mid-run, which is
// the round-3 defect moved one level up. The marker is a lock, so a sweep must leave a
// locked tree alone.
#[test]
fn a_live_builds_scratch_is_not_swept() {
    let tmp = tempfile::tempdir().unwrap();
    write_app(tmp.path(), &static_route("hi"));

    // Stand in for a concurrent build: a marked, locked scratch directory.
    let live = tmp.path().join("dist.tmp.4294967294.0");
    std::fs::create_dir_all(&live).unwrap();
    std::fs::write(live.join(".runai-scratch"), "runai 0.1.0 build\n").unwrap();
    let held = std::fs::OpenOptions::new()
        .write(true)
        .open(live.join(".runai-scratch"))
        .unwrap();
    held.lock().unwrap();

    let out = run(&["build", "--out", "dist"], tmp.path());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{stderr}");
    assert!(live.is_dir(), "a locked scratch tree must not be swept");

    // Once the owner lets go, the tree is abandoned and the next build sweeps it.
    drop(held);
    let again = run(&["build", "--out", "dist"], tmp.path());
    assert_eq!(
        again.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&again.stderr)
    );
    assert!(!live.exists(), "an abandoned scratch tree is swept");
}

// The test that motivates the whole redesign: `build & build` used to end with one
// success and one `cannot resolve dist.tmp: No such file or directory`, because both
// shared one scratch directory. With a scratch name per build they cannot touch each
// other, and every build publishes a complete tree.
//
// A barrier makes the builds collide on purpose: with plain startup jitter they
// stagger, and the old defect does not reproduce reliably.
#[test]
fn concurrent_builds_all_succeed_and_leave_one_complete_dist() {
    let tmp = tempfile::tempdir().unwrap();
    write_app(tmp.path(), &static_route("home"));
    let root = tmp.path().to_path_buf();
    std::fs::create_dir_all(root.join("src/routes/blog")).unwrap();
    std::fs::write(
        root.join("src/routes/about.page.tsx"),
        static_route("about"),
    )
    .unwrap();
    std::fs::write(
        root.join("src/routes/blog/index.page.tsx"),
        static_route("blog"),
    )
    .unwrap();
    std::fs::write(
        root.join("src/routes/blog/post.page.tsx"),
        static_route("post"),
    )
    .unwrap();

    const BUILDS: usize = 4;
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(BUILDS));
    let handles: Vec<_> = (0..BUILDS)
        .map(|_| {
            let dir = root.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                run(&["build", "--out", "dist"], &dir)
            })
        })
        .collect();
    let results: Vec<Output> = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect();

    for (index, result) in results.iter().enumerate() {
        assert_eq!(
            result.status.code(),
            Some(0),
            "build {index}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
    }

    // Whichever published last, the tree is one complete build.
    assert_eq!(
        tree(&root.join("dist")),
        vec![
            ".runai-build",
            "about/index.html",
            "blog/index.html",
            "blog/post/index.html",
            "index.html",
        ]
    );
    for (page, text) in [
        ("about/index.html", "about"),
        ("blog/index.html", "blog"),
        ("blog/post/index.html", "post"),
        ("index.html", "home"),
    ] {
        let html = std::fs::read_to_string(root.join("dist").join(page)).unwrap();
        assert!(html.contains(&format!("<p>{text}</p>")), "{page}: {html}");
    }

    // No build left scratch or a backup behind.
    let left: Vec<String> = std::fs::read_dir(&root)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name != "dist" && name.starts_with("dist"))
        .collect();
    assert!(left.is_empty(), "leftovers: {left:?}");
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

// M1, the missing test: a build killed between `assemble_into` and `publish` leaves
// its scratch directory STAMPED but (on the pre-fix code) UNMARKED, because the
// ownership marker was unlinked before publishing. `sweep_scratch`'s unique-name arm
// required the marker, so that tree was invisible to every later build and stayed on
// disk forever — the reviewer reproduced `dist.tmp.999.0` surviving every build.
//
// The marker must cover the whole scratch lifetime: written before any content and
// unlinked only from the PUBLISHED tree, after the rename. A stamped-only leftover is
// therefore ours and is swept.
#[test]
fn a_stamped_but_unmarked_scratch_left_by_a_kill_before_publish_is_swept() {
    let tmp = tempfile::tempdir().unwrap();
    write_app(tmp.path(), &static_route("rebuilt"));

    // Exactly the window M1 names: a complete tree — stamp AND content — with no marker.
    let leftover = tmp.path().join("dist.tmp.999.0");
    std::fs::create_dir_all(&leftover).unwrap();
    std::fs::write(leftover.join(".runai-build"), "runai 0.1.0 build\n").unwrap();
    std::fs::write(leftover.join("index.html"), "abandoned complete tree\n").unwrap();

    let out = run(&["build", "--out", "dist"], tmp.path());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{stderr}");
    assert!(
        !leftover.exists(),
        "a stamped scratch tree with no marker is ours and must be swept, not orphaned"
    );
    assert!(tmp.path().join("dist/index.html").is_file());
    assert!(tmp.path().join("dist/.runai-build").is_file());
}

// The other side of the same predicate: neither marker nor stamp says nothing about
// who owns the directory, so it is neither deleted nor a reason to fail. Scratch names
// are unique per build, so an unrecognised directory cannot be in this build's way.
#[test]
fn a_scratch_named_directory_with_neither_marker_nor_stamp_survives_a_build() {
    let tmp = tempfile::tempdir().unwrap();
    write_app(tmp.path(), &static_route("hi"));

    let foreign = tmp.path().join("dist.tmp.999.0");
    std::fs::create_dir_all(foreign.join("keep")).unwrap();
    std::fs::write(foreign.join("keep/notes.md"), "mine\n").unwrap();

    let out = run(&["build", "--out", "dist"], tmp.path());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{stderr}");
    assert_eq!(
        std::fs::read_to_string(foreign.join("keep/notes.md")).unwrap(),
        "mine\n",
        "a directory that is not ours must survive the build"
    );
    assert!(tmp.path().join("dist/index.html").is_file());
}

// M2: the lock is keyed by the RESOLVED output path, so equivalent spellings of one
// directory must agree. `dist` and `./dist` already did; `nodir/../dist` did not,
// because the lock fell back to the LITERAL parent when no ancestor existed. Two
// different locks let the publish sequences interleave, which surfaced as
// `dist.old exists but is not a previous build`.
//
// All three spellings are one path, so all three builds must succeed and leave one
// complete tree.
#[test]
fn equivalent_out_spellings_share_one_lock_and_all_publish() {
    let tmp = tempfile::tempdir().unwrap();
    write_app(tmp.path(), &static_route("home"));
    let root = tmp.path().to_path_buf();

    // A barrier so the three builds collide deliberately rather than by luck.
    const BUILDS: usize = 3;
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(BUILDS));
    let handles: Vec<_> = ["dist", "./dist", "nodir/../dist"]
        .into_iter()
        .map(|spelling| {
            let dir = root.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                (spelling, run(&["build", "--out", spelling], &dir))
            })
        })
        .collect();
    let results: Vec<(&str, Output)> = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect();

    for (spelling, result) in &results {
        assert_eq!(
            result.status.code(),
            Some(0),
            "`--out {spelling}`: {}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
    assert_eq!(
        tree(&root.join("dist")),
        vec![".runai-build", "index.html"],
        "one complete tree, whichever build published last"
    );
    assert!(!root.join("nodir").exists(), "`nodir/../dist` is `dist`");
}

// M3: the lock is bounded. A blocking `lock()` with no timeout means a build stopped
// with SIGSTOP, or one looping forever in a user's static module, hangs every later
// build silently and forever. The second build must fail at about five seconds with a
// message naming the output and the lock path.
#[test]
fn a_held_lock_fails_the_next_build_quickly_and_names_the_lock() {
    let tmp = tempfile::tempdir().unwrap();
    write_app(tmp.path(), &slow_route(30_000));

    let mut first = Kill(
        Command::new(env!("CARGO_BIN_EXE_runai"))
            .args(["build", "--out", "dist"])
            .current_dir(tmp.path())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap(),
    );

    // Wait until the build is demonstrably holding the lock: it takes the lock before
    // creating scratch, so a scratch directory means the lock is ours.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while !has_scratch(tmp.path()) {
        assert!(
            std::time::Instant::now() < deadline,
            "the first build never got as far as taking the lock"
        );
        assert!(
            first.0.try_wait().unwrap().is_none(),
            "the first build exited instead of holding the lock"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }

    let started = std::time::Instant::now();
    let second = run(&["build", "--out", "dist"], tmp.path());
    let elapsed = started.elapsed();
    let stderr = String::from_utf8_lossy(&second.stderr);

    assert_eq!(second.status.code(), Some(1), "{stderr}");
    assert!(
        elapsed < std::time::Duration::from_secs(12),
        "the second build must fail fast, not wait for the first: {elapsed:?}"
    );
    assert!(
        stderr.contains("dist") && stderr.contains("runai-build-"),
        "the message must name the output and the lock path: {stderr}"
    );
    assert!(
        stderr.contains("another `runai build`"),
        "the message must say another build holds it: {stderr}"
    );
    let _ = first.0.kill();
}
