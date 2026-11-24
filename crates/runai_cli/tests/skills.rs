//! `runai skills` end to end: the repair command that writes one Markdown file
//! per diagnostic code.
//!
//! The primary copy is committed at `packages/runai/skills/` and shipped by the
//! package's `"files"`; this command exists so a project whose install lost them
//! can put them back. So the tests here care about the two things that make it a
//! repair rather than a new source of truth: every code gets a file, and running
//! it twice produces the same bytes.

use std::process::{Command, Output};

use runai_compiler::diagnostic::Code;

fn run(args: &[&str], dir: &std::path::Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_runai"))
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap()
}

#[test]
fn writes_one_file_per_code_with_frontmatter() {
    let tmp = tempfile::tempdir().unwrap();
    let out = run(&["skills", "--out", "skills"], tmp.path());
    assert!(out.status.success(), "{out:?}");
    for code in [
        "RA001_UNTRACKED_READ",
        "RA002_EFFECT_WRITE",
        "RA010_STATIC_VIOLATION",
        "RA012_MISSING_MODE",
        "RA014_NOT_SUPPORTED_YET",
    ] {
        let body = std::fs::read_to_string(tmp.path().join(format!("skills/{code}.md"))).unwrap();
        assert!(body.starts_with("---\n"), "{code}: {body}");
        assert!(body.contains(&format!("name: {code}")), "{code}: {body}");
        assert!(body.contains("description:"), "{code}: {body}");
        assert!(body.contains("## Fix"), "{code}: {body}");
    }
}

/// Every diagnostic carries a `doc` path. A code with no file is a 404 in an
/// agent's file read, so the count has to match [`Code::all`] exactly — no
/// missing code and no stale extra `*.md` left behind by an older run.
#[test]
fn writes_exactly_one_file_per_code() {
    let tmp = tempfile::tempdir().unwrap();
    let out = run(&["skills", "--out", "skills"], tmp.path());
    assert!(out.status.success(), "{out:?}");

    let mut written: Vec<String> = std::fs::read_dir(tmp.path().join("skills"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    written.sort();
    let mut expected: Vec<String> = Code::all().iter().map(Code::skill_file).collect();
    expected.sort();
    assert_eq!(written, expected);
}

/// The default is the path every diagnostic's `doc` names, so a project can run
/// `runai skills` with no arguments and fix the links it is already printing.
#[test]
fn default_out_is_the_doc_path_directory() {
    let tmp = tempfile::tempdir().unwrap();
    let out = run(&["skills"], tmp.path());
    assert!(out.status.success(), "{out:?}");
    let body = std::fs::read_to_string(
        tmp.path()
            .join("node_modules/runai/skills/RA001_UNTRACKED_READ.md"),
    )
    .unwrap();
    assert!(body.starts_with("---\n"), "{body}");
}

/// Spec §7.9: every command is idempotent and safe to call repeatedly. A repair
/// command that changed the bytes on a second run would make two checkouts
/// differ for no reason.
#[test]
fn running_twice_writes_the_same_bytes() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(
        run(&["skills", "--out", "skills"], tmp.path())
            .status
            .success()
    );
    let first =
        std::fs::read_to_string(tmp.path().join("skills/RA014_NOT_SUPPORTED_YET.md")).unwrap();
    assert!(
        run(&["skills", "--out", "skills"], tmp.path())
            .status
            .success()
    );
    let second =
        std::fs::read_to_string(tmp.path().join("skills/RA014_NOT_SUPPORTED_YET.md")).unwrap();
    assert_eq!(first, second);
}

#[test]
fn unknown_flag_is_a_usage_error() {
    let tmp = tempfile::tempdir().unwrap();
    let out = run(&["skills", "--json"], tmp.path());
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty(), "exit 2 writes nothing to stdout");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("unknown flag"), "{stderr}");
    assert!(stderr.contains("usage: runai"), "{stderr}");
}

#[test]
fn missing_out_value_is_a_usage_error() {
    let tmp = tempfile::tempdir().unwrap();
    let out = run(&["skills", "--out"], tmp.path());
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("needs a directory"), "{stderr}");
    assert!(stderr.contains("usage: runai"), "{stderr}");
}
