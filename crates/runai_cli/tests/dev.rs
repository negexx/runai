//! `runai dev` end to end: the real binary, a real temp app, a real HTTP socket.
//!
//! The test that matters most is `edits_are_rebuilt_on_the_next_page_request`: it is
//! the only assertion that the rebuild loop actually runs a build. A server that
//! merely served the `dist/` the first build produced would pass every other test here
//! and be useless in the inner loop it exists for.

use std::io::{Read, Write};
use std::path::Path;
use std::process::Command;

/// Stops the background server even when an assertion fails, so tests never leak processes.
struct StopOnDrop(std::path::PathBuf);
impl Drop for StopOnDrop {
    fn drop(&mut self) {
        let _ = std::process::Command::new(env!("CARGO_BIN_EXE_runai"))
            .args(["dev", "stop"])
            .current_dir(&self.0)
            .status();
    }
}

/// Minimal std-only HTTP client: no reqwest, no async runtime.
fn http(port: u64, method: &str, path: &str) -> (u16, String) {
    let mut s = std::net::TcpStream::connect(("127.0.0.1", port as u16)).unwrap();
    write!(
        s,
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\nContent-Length: 0\r\n\r\n"
    )
    .unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).unwrap();
    let code = out[9..12].parse().unwrap();
    let body = out.split("\r\n\r\n").nth(1).unwrap_or("").to_string();
    (code, body)
}

#[test]
fn background_twice_reports_the_same_pid() {
    let tmp = tempfile::tempdir().unwrap();
    write_min_app(tmp.path());
    let _guard = StopOnDrop(tmp.path().to_path_buf());
    let first = run_json(&["dev", "--background", "--port", "0"], tmp.path());
    let second = run_json(&["dev", "--background", "--port", "0"], tmp.path());
    assert_eq!(first["pid"], second["pid"]);
    assert_eq!(first["running"], true);
    let (code, body) = http(first["port"].as_u64().unwrap(), "GET", "/_runai/status");
    assert_eq!(code, 200);
    assert!(body.contains("\"running\":true"));
}

#[test]
fn stop_shuts_the_server_down() {
    let tmp = tempfile::tempdir().unwrap();
    write_min_app(tmp.path());
    let _guard = StopOnDrop(tmp.path().to_path_buf());
    let first = run_json(&["dev", "--background", "--port", "0"], tmp.path());
    let port = first["port"].as_u64().unwrap() as u16;
    assert!(
        std::process::Command::new(env!("CARGO_BIN_EXE_runai"))
            .args(["dev", "stop"])
            .current_dir(tmp.path())
            .status()
            .unwrap()
            .success()
    );
    assert!(std::net::TcpStream::connect(("127.0.0.1", port)).is_err());
    assert!(!tmp.path().join(".runai/dev.json").exists());
}

#[test]
fn stop_when_not_running_exits_zero() {
    let tmp = tempfile::tempdir().unwrap();
    write_min_app(tmp.path());
    assert!(
        std::process::Command::new(env!("CARGO_BIN_EXE_runai"))
            .args(["dev", "stop"])
            .current_dir(tmp.path())
            .status()
            .unwrap()
            .success()
    );
}

#[test]
fn path_traversal_is_404() {
    let tmp = tempfile::tempdir().unwrap();
    write_min_app(tmp.path());
    std::fs::write(tmp.path().join("secret.txt"), "nope").unwrap();
    let _guard = StopOnDrop(tmp.path().to_path_buf());
    let port = run_json(&["dev", "--background", "--port", "0"], tmp.path())["port"]
        .as_u64()
        .unwrap();
    assert_eq!(http(port, "GET", "/../secret.txt").0, 404);
    assert_eq!(http(port, "GET", "/%2e%2e/secret.txt").0, 404);
}

/// A lockfile naming a server that belongs to a different checkout must not be reused
/// or stopped through: `dev stop` there would end another project's server.
#[test]
fn a_lockfile_from_another_checkout_is_not_trusted() {
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    write_min_app(first.path());
    write_min_app(second.path());
    let _guard = StopOnDrop(first.path().to_path_buf());
    let _second_guard = StopOnDrop(second.path().to_path_buf());

    let running = run_json(&["dev", "--background", "--port", "0"], first.path());
    let port = running["port"].as_u64().unwrap() as u16;
    let pid = running["pid"].as_u64().unwrap();

    // Copy the lockfile into the other checkout, as a `cp -r` of a project would.
    std::fs::create_dir_all(second.path().join(".runai")).unwrap();
    std::fs::copy(
        first.path().join(".runai/dev.json"),
        second.path().join(".runai/dev.json"),
    )
    .unwrap();

    assert!(
        std::process::Command::new(env!("CARGO_BIN_EXE_runai"))
            .args(["dev", "stop"])
            .current_dir(second.path())
            .status()
            .unwrap()
            .success()
    );

    // The first checkout's server must still be serving, and its lockfile untouched.
    assert_eq!(http(u64::from(port), "GET", "/_runai/status").0, 200);
    let status = run_json(&["dev", "--background", "--port", "0"], first.path());
    assert_eq!(status["pid"].as_u64().unwrap(), pid);
    assert!(!second.path().join(".runai/dev.json").exists());
}

#[test]
fn edits_are_rebuilt_on_the_next_page_request() {
    let tmp = tempfile::tempdir().unwrap();
    write_min_app(tmp.path());
    let _guard = StopOnDrop(tmp.path().to_path_buf());
    let port = run_json(&["dev", "--background", "--port", "0"], tmp.path())["port"]
        .as_u64()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(1100)); // coarse mtime resolution on some filesystems
    std::fs::write(
        tmp.path().join("src/routes/index.page.tsx"),
        "\"use static\"\nexport default function P(props: Record<never, never>) { return <p>changed</p> }\n",
    )
    .unwrap();
    assert!(http(port, "GET", "/").1.contains("<p>changed</p>"));
}

/// A minimal app: one static root route rendering `<p>hi</p>`, plus a `runai.json`
/// naming the shipped runtime by absolute path (the shape `runai build` documents).
fn write_min_app(root: &Path) {
    std::fs::create_dir_all(root.join("src/routes")).unwrap();
    std::fs::write(
        root.join("src/routes/index.page.tsx"),
        "\"use static\"\nexport default function P(props: Record<never, never>) { return <p>hi</p> }\n",
    )
    .unwrap();
    let runtime = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("packages")
        .join("runai")
        .join("runtime.js");
    std::fs::write(
        root.join("runai.json"),
        format!("{{\"runtime\":{:?}}}\n", runtime.display().to_string()),
    )
    .unwrap();
}

/// Run the binary and parse its stdout as JSON, so a failure shows what it printed.
fn run_json(args: &[&str], dir: &Path) -> serde_json::Value {
    let out = Command::new(env!("CARGO_BIN_EXE_runai"))
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "`runai {}` failed ({}):\nstdout: {stdout}\nstderr: {stderr}",
        args.join(" "),
        out.status
    );
    serde_json::from_str(&stdout).unwrap_or_else(|error| {
        panic!("stdout is not JSON ({error}):\nstdout: {stdout}\nstderr: {stderr}")
    })
}
