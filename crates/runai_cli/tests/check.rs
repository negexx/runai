use std::process::Command;

fn run(args: &[&str], dir: &std::path::Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_runai"))
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap()
}

#[test]
fn check_reports_ra001_as_json_and_exits_1() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("src/routes")).unwrap();
    std::fs::write(
        tmp.path().join("src/routes/index.page.tsx"),
        "\"use island\"\nimport { signal } from \"runai\"\nexport default function P(props: Record<never, never>) { const n = signal(0); const x = n(); return <p/> }\n",
    ).unwrap();
    let out = run(&["check", "--json"], tmp.path());
    assert_eq!(out.status.code(), Some(1));
    let parsed: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(parsed[0]["code"], "RA001_UNTRACKED_READ");
    assert_eq!(parsed[0]["file"], "src/routes/index.page.tsx");
    assert_eq!(parsed[0]["line"], 3);
}

#[test]
fn check_also_covers_non_route_components() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("src/routes")).unwrap();
    std::fs::write(
        tmp.path().join("src/routes/_shell.tsx"),
        "export default function Shell(props) { return <main>{props.children}</main> }\n",
    )
    .unwrap();
    let out = run(&["check", "--json"], tmp.path());
    assert_eq!(out.status.code(), Some(1));
    let parsed: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(parsed[0]["code"], "RA006_UNANNOTATED_PROPS");
}

#[test]
fn check_clean_route_exits_0() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("src/routes")).unwrap();
    std::fs::write(
        tmp.path().join("src/routes/about.page.tsx"),
        "\"use static\"\nexport default function P(props: Record<never, never>) { return <p>hi</p> }\n",
    ).unwrap();
    let out = run(&["check", "--json"], tmp.path());
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "[]");
}

// F2: `check` used to pass a tree `build` refuses. A `[slug]` page is refused by
// both now, with the same message, so a CI running only `check` cannot green-light
// a page the builder will not produce.
#[test]
fn check_refuses_a_dynamic_route_segment_like_build() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("src/routes/blog")).unwrap();
    std::fs::write(
        tmp.path().join("src/routes/blog/[slug].page.tsx"),
        "\"use static\"\nexport default function P(props: Record<never, never>) { return <p>post</p> }\n",
    )
    .unwrap();
    let out = run(&["check", "--json"], tmp.path());
    assert_eq!(out.status.code(), Some(1));
    let parsed: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(parsed[0]["code"], "RA014_NOT_SUPPORTED_YET");
    assert_eq!(
        parsed[0]["message"],
        "dynamic route segments are not supported yet"
    );
}

// F2: `build` refuses a route under a dot-prefixed directory at `build.rs`'s
// `route_name` (the output path would be `dist/.hidden/index.html`, which many
// static servers 404), but `check` passes the same tree with `[]` and exit 0. A CI
// that runs only `runai check` therefore goes green on a tree the builder will not
// produce. This is the documented check/build asymmetry; the assertion is the
// builder's own refusal, so `check` cannot pass a tree `build` rejects.
//
// Both channels are accepted, because `build` reports this on stderr while a
// diagnostic-shaped fix would report it on stdout: the pinned facts are the exit
// code and the reason, not which stream carries it.
#[test]
fn check_refuses_a_dot_prefixed_route_directory_like_build() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("src/routes/.hidden")).unwrap();
    std::fs::write(
        tmp.path().join("src/routes/.hidden/index.page.tsx"),
        "\"use static\"\nexport default function P(props: Record<never, never>) { return <p>hidden</p> }\n",
    )
    .unwrap();
    let out = run(&["check", "--json"], tmp.path());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let said = format!("{stdout}{stderr}");
    assert_eq!(
        out.status.code(),
        Some(1),
        "check must refuse what build refuses, got stdout {stdout:?} stderr {stderr:?}"
    );
    assert!(
        said.contains("dot-prefixed") && said.contains(".hidden"),
        "the refusal names the dot-prefixed directory, like build does: {said}"
    );
}

// F2, second shape: `build` refuses an island route whose asset path is
// `assets/RUNTIME.js` because the runtime copy overwrites the module and the page
// mounts nothing (`build.rs`, case-insensitive). `check` passed it with exit 0, so
// only a build caught it. Pin the same refusal in `check`.
#[test]
fn check_refuses_a_reserved_runtime_route_like_build() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("src/routes")).unwrap();
    std::fs::write(
        tmp.path().join("src/routes/index.page.tsx"),
        "\"use static\"\nexport default function P(props: Record<never, never>) { return <p>root</p> }\n",
    )
    .unwrap();
    std::fs::write(
        tmp.path().join("src/routes/RUNTIME.page.tsx"),
        "\"use island\"\nexport default function P(props: Record<never, never>) { return <button onClick={() => {}}>x</button> }\n",
    )
    .unwrap();
    let out = run(&["check", "--json"], tmp.path());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let said = format!("{stdout}{stderr}");
    assert_eq!(
        out.status.code(),
        Some(1),
        "check must refuse what build refuses, got stdout {stdout:?} stderr {stderr:?}"
    );
    assert!(
        said.contains("reserved for the runtime copy") && said.contains("RUNTIME"),
        "the refusal names the reserved runtime asset, like build does: {said}"
    );
}

// The dynamic-segment rule is page-only: a component module under a bracketed
// directory is not a route and must stay clean.
#[test]
fn check_passes_a_component_module_under_a_bracketed_directory() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("src/routes/[slug]")).unwrap();
    std::fs::write(
        tmp.path().join("src/routes/[slug]/_shell.tsx"),
        "export default function Shell(props: Record<never, never>) { return <main>{props.children}</main> }\n",
    )
    .unwrap();
    let out = run(&["check", "--json"], tmp.path());
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "[]");
}

#[test]
fn check_missing_route_dir_is_a_clear_error() {
    let tmp = tempfile::tempdir().unwrap();
    let out = run(&["check", "--json"], tmp.path());
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty(), "exit 2 writes nothing to stdout");
    assert!(String::from_utf8_lossy(&out.stderr).contains("src/routes"));
}

// F1: a file argument that is not `*.tsx` is not a route, so analyzing it as TSX
// would manufacture a diagnostic (`package.json` → RA007). It is a usage error.
#[test]
fn check_non_tsx_file_argument_is_a_usage_error() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("src/routes")).unwrap();
    std::fs::write(
        tmp.path().join("src/routes/about.page.tsx"),
        "\"use static\"\nexport default function P(props: Record<never, never>) { return <p>hi</p> }\n",
    )
    .unwrap();
    std::fs::write(
        tmp.path().join("package.json"),
        "{\"name\":\"x\",\"version\":\"1.0.0\"}\n",
    )
    .unwrap();

    let out = run(&["check", "--json", "package.json"], tmp.path());
    assert_eq!(out.status.code(), Some(2), "not a route, so a usage error");
    assert!(out.stdout.is_empty(), "exit 2 writes nothing to stdout");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("package.json"), "stderr names the argument");
    assert!(
        !stderr.contains("RA007"),
        "a JSON file must not become a diagnostic"
    );
}

// F2: the last path used to win and the first was silently dropped, which turned
// two real errors into `[]` and exit 0 — a false green in CI.
#[test]
fn check_second_path_argument_is_a_usage_error() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("src/a")).unwrap();
    std::fs::create_dir_all(tmp.path().join("src/b")).unwrap();
    std::fs::write(
        tmp.path().join("src/a/a.tsx"),
        "export default function A(props) { return <p/> }\n",
    )
    .unwrap();
    std::fs::write(
        tmp.path().join("src/b/b.tsx"),
        "\"use static\"\nexport default function B(props: Record<never, never>) { return <p/> }\n",
    )
    .unwrap();

    // The first path alone reports one error.
    let single = run(&["check", "--json", "src/a"], tmp.path());
    assert_eq!(single.status.code(), Some(1));

    let out = run(&["check", "--json", "src/a", "src/b"], tmp.path());
    assert_eq!(out.status.code(), Some(2), "two paths is a usage error");
    assert!(
        out.stdout.is_empty(),
        "exit 2 writes nothing to stdout, so no `[]` false green"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("unexpected argument"), "stderr: {stderr}");
    assert!(stderr.contains("usage:"), "stderr carries the usage line");
}

// N1: a route that could not be read means the run did not complete. Reporting
// `[]` on stdout and a warning on stderr contradicts itself, and a CI reading
// either channel stays green while one route was never analyzed. Exit 2, and
// stdout carries nothing at all.
#[test]
fn check_unreadable_route_is_not_a_clean_verdict() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("src/routes")).unwrap();
    std::fs::write(
        tmp.path().join("src/routes/a.page.tsx"),
        "\"use static\"\nexport default function A(props: Record<never, never>) { return <p/> }\n",
    )
    .unwrap();
    // The clean file sorts first: `[]` here was the false green, not a missing array.
    std::fs::write(
        tmp.path().join("src/routes/b.page.tsx"),
        b"export default function B(props) { return <p>caf\xe9</p> }\n".as_slice(),
    )
    .unwrap();

    let out = run(&["check", "--json"], tmp.path());

    assert_eq!(
        out.status.code(),
        Some(2),
        "an unanalyzed route means the run could not complete"
    );
    assert!(
        out.stdout.is_empty(),
        "no partial array and no `[]`: stdout is silent when the run did not complete, got {:?}",
        String::from_utf8_lossy(&out.stdout)
    );

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("src/routes/b.page.tsx"), "stderr: {stderr}");
    assert!(stderr.contains("UTF-8"), "stderr names the OS error");
}

// N2: an unreadable subdirectory used to propagate out of `walk`, so the whole
// run aborted with exit 2 and every diagnostic the readable files produced was
// thrown away. The walk must keep going and the run still ends at exit 2 (N1),
// with the reasons — including the diagnostics it cannot print — on stderr.
#[test]
fn check_unreadable_subdirectory_keeps_its_reason_and_fails_the_run() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = tempfile::tempdir().unwrap();
    let sub = tmp.path().join("src/routes/sub");
    std::fs::create_dir_all(&sub).unwrap();
    // A real error, in a file the walk can reach.
    std::fs::write(
        tmp.path().join("src/routes/a.page.tsx"),
        "export default function A(props) { return <p/> }\n",
    )
    .unwrap();
    std::fs::write(
        tmp.path().join("src/routes/sub/ok.page.tsx"),
        "// unreachable\n",
    )
    .unwrap();

    std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o000)).unwrap();
    // Under a root CI the mode is not enforced and the test would assert nothing.
    if std::fs::read_dir(&sub).is_ok() {
        std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o755)).unwrap();
        eprintln!("skipping: file permissions are not enforced for this user");
        return;
    }

    let out = run(&["check", "--json"], tmp.path());
    // Restore before the tempdir is dropped so cleanup can remove the tree.
    std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o755)).unwrap();

    assert_eq!(out.status.code(), Some(2), "a directory was never read");
    assert!(
        out.stdout.is_empty(),
        "stdout stays silent, got {:?}",
        String::from_utf8_lossy(&out.stdout)
    );

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("src/routes/sub"),
        "stderr names the unreadable directory: {stderr}"
    );
    assert!(
        stderr.contains("RA006_UNANNOTATED_PROPS"),
        "the RA006 from the readable file is not discarded silently: {stderr}"
    );
    assert!(
        stderr.contains("did not complete"),
        "stderr states the run failed rather than reporting a verdict: {stderr}"
    );
}

// F3: when nothing at all could be read there is no analysis to trust, so the
// exit code is 2 rather than a clean 0 or a false 1.
#[test]
fn check_only_unreadable_files_exits_2() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("src/routes")).unwrap();
    std::fs::write(
        tmp.path().join("src/routes/bad.page.tsx"),
        b"export default function B(props) { return <p>caf\xe9</p> }\n".as_slice(),
    )
    .unwrap();

    let out = run(&["check", "--json"], tmp.path());
    assert_eq!(out.status.code(), Some(2), "nothing could be read");
    assert!(out.stdout.is_empty(), "exit 2 writes nothing to stdout");
    assert!(String::from_utf8_lossy(&out.stderr).contains("bad.page.tsx"));
}

// F5: pins the determinism and ordering the module doc claims.
#[test]
fn check_output_is_deterministic_and_ordered_by_file() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("src/routes/nested")).unwrap();
    std::fs::write(
        tmp.path().join("src/routes/zzz.page.tsx"),
        "export default function Z(props) { return <p/> }\n",
    )
    .unwrap();
    std::fs::write(
        tmp.path().join("src/routes/nested/aaa.page.tsx"),
        "export default function A(props) { return <a/> }\n",
    )
    .unwrap();
    std::fs::write(
        tmp.path().join("src/routes/mmm.page.tsx"),
        "export default function M(props) { return <m/> }\n",
    )
    .unwrap();

    let first = run(&["check", "--json"], tmp.path());
    let second = run(&["check", "--json"], tmp.path());

    assert_eq!(first.status.code(), Some(1));
    assert_eq!(
        first.stdout, second.stdout,
        "two runs on the same tree are byte-identical"
    );

    let parsed: serde_json::Value = serde_json::from_slice(&first.stdout).unwrap();
    let files: Vec<&str> = parsed
        .as_array()
        .unwrap()
        .iter()
        .map(|diagnostic| diagnostic["file"].as_str().unwrap())
        .collect();

    // Each route carries two diagnostics, so the files repeat; the claim is that
    // they never go backwards, which groups each file's diagnostics together in
    // `/`-separated path order.
    assert!(
        files.windows(2).all(|pair| pair[0] <= pair[1]),
        "diagnostics rise by file path, got {files:?}"
    );
    let mut distinct = files.clone();
    distinct.dedup();
    assert_eq!(
        distinct,
        vec![
            "src/routes/mmm.page.tsx",
            "src/routes/nested/aaa.page.tsx",
            "src/routes/zzz.page.tsx",
        ],
        "every route is covered, in sorted `/`-separated path order"
    );
}
