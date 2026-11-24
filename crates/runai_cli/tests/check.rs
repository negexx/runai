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

// F3: one unreadable file used to abort the whole run, hiding every other error.
// It is reported on stderr and the walk continues; exit is 1 because diagnostics
// were produced. Stdout stays pure JSON.
#[test]
fn check_unreadable_file_is_reported_and_the_walk_continues() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("src/routes")).unwrap();
    std::fs::write(
        tmp.path().join("src/routes/a.page.tsx"),
        "\"use static\"\nexport default function A(props) { return <p/> }\n",
    )
    .unwrap();
    // Sorts between the two readable files, so a continued walk covers both sides.
    std::fs::write(
        tmp.path().join("src/routes/b.page.tsx"),
        b"export default function B(props) { return <p>caf\xe9</p> }\n".as_slice(),
    )
    .unwrap();
    std::fs::write(
        tmp.path().join("src/routes/c.page.tsx"),
        "\"use static\"\nexport default function C(props) { return <c/> }\n",
    )
    .unwrap();

    let out = run(&["check", "--json"], tmp.path());
    assert_eq!(out.status.code(), Some(1), "diagnostics were produced");

    let parsed: serde_json::Value = serde_json::from_slice(&out.stdout)
        .expect("stdout is pure JSON even with an unreadable file");
    let files: Vec<&str> = parsed
        .as_array()
        .unwrap()
        .iter()
        .map(|diagnostic| diagnostic["file"].as_str().unwrap())
        .collect();
    assert_eq!(
        files,
        vec!["src/routes/a.page.tsx", "src/routes/c.page.tsx"],
        "both readable files are still analyzed; the unreadable one is not a diagnostic"
    );

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("src/routes/b.page.tsx"), "stderr: {stderr}");
    assert!(stderr.contains("UTF-8"), "stderr names the OS error");
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
