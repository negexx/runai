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

/// Kills and reaps a child on drop, including when the test panics, so a process under
/// test cannot outlive its assertions.
struct KillOnDrop(std::process::Child);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
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

/// The raw response head and body, so headers can be asserted.
fn http_raw(port: u64, method: &str, path: &str) -> String {
    let mut s = std::net::TcpStream::connect(("127.0.0.1", port as u16)).unwrap();
    write!(
        s,
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\nContent-Length: 0\r\n\r\n"
    )
    .unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).unwrap();
    out
}

/// `http` for a caller that may legitimately lose its connection (the server may be
/// exiting under it), with a bounded read so a test can never hang.
fn http_lenient(port: u64, method: &str, path: &str) -> Option<(u16, String)> {
    let mut s = std::net::TcpStream::connect(("127.0.0.1", port as u16)).ok()?;
    s.set_read_timeout(Some(std::time::Duration::from_secs(5)))
        .ok()?;
    write!(
        s,
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\nContent-Length: 0\r\n\r\n"
    )
    .ok()?;
    let mut out = String::new();
    s.read_to_string(&mut out).ok()?;
    let code = out.get(9..12)?.parse().ok()?;
    let body = out.split("\r\n\r\n").nth(1).unwrap_or("").to_string();
    Some((code, body))
}

/// `GET /_runai/status`, parsed for its `built_at` epoch-millisecond stamp: the contract
/// the I3 test reads to prove no build happened between two requests.
fn built_at(port: u64) -> u64 {
    let (code, body) = http(port, "GET", "/_runai/status");
    assert_eq!(code, 200, "{body}");
    let value: serde_json::Value = serde_json::from_str(&body).unwrap();
    value["built_at"].as_u64().unwrap_or_else(|| {
        panic!("`/_runai/status` must report when the last build started: {body}")
    })
}

/// C2: a deleted route must stop being served. The walk compared file mtimes only, so
/// removing a file changed nothing it looked at (just the parent directory's mtime,
/// which it never read) and the deleted page kept answering 200.
#[test]
fn a_deleted_route_stops_being_served() {
    let tmp = tempfile::tempdir().unwrap();
    write_min_app(tmp.path());
    std::fs::write(
        tmp.path().join("src/routes/gone.page.tsx"),
        route_body("gone"),
    )
    .unwrap();
    let _guard = StopOnDrop(tmp.path().to_path_buf());
    let port = run_json(&["dev", "--background", "--port", "0"], tmp.path())["port"]
        .as_u64()
        .unwrap();
    assert_eq!(http(port, "GET", "/gone/").0, 200);

    settle();
    std::fs::remove_file(tmp.path().join("src/routes/gone.page.tsx")).unwrap();

    assert_eq!(http(port, "GET", "/").0, 200, "the rebuild must publish");
    assert_eq!(
        http(port, "GET", "/gone/").0,
        404,
        "a deleted route must stop being served"
    );
    assert!(
        http(port, "GET", "/").1.contains("<p>hi</p>"),
        "the surviving route must still work"
    );
}

/// The failed-rebuild property: a broken edit keeps the last good tree up and puts the
/// diagnostic on `/_runai/status`. Verified by hand in round 0; pinned here.
#[test]
fn a_failed_rebuild_keeps_serving_the_last_good_tree_and_reports_the_diagnostic() {
    let tmp = tempfile::tempdir().unwrap();
    write_min_app(tmp.path());
    let _guard = StopOnDrop(tmp.path().to_path_buf());
    let port = run_json(&["dev", "--background", "--port", "0"], tmp.path())["port"]
        .as_u64()
        .unwrap();
    assert!(http(port, "GET", "/").1.contains("<p>hi</p>"));

    settle();
    std::fs::write(tmp.path().join("src/routes/index.page.tsx"), broken_route()).unwrap();

    let (code, body) = http(port, "GET", "/");
    assert_eq!(code, 200, "a broken edit must not take the page down");
    assert!(
        body.contains("<p>hi</p>"),
        "the last good build must still be served: {body}"
    );

    let (code, status) = http(port, "GET", "/_runai/status");
    assert_eq!(code, 200);
    assert!(
        status.contains("RA010"),
        "the diagnostic must reach `/_runai/status`: {status}"
    );
}

/// C1(a) end to end: a lockfile whose port accepts the connection and then never answers
/// is a LIVE server that is busy. The round-0 defect deleted exactly this lockfile and
/// exited 0, orphaning a server no later `dev stop` could reach. The pinned shape here is
/// the bare socket, so the assertion is about the timeout, not about any rebuild.
#[test]
fn stop_leaves_the_lockfile_of_a_server_that_accepts_but_never_answers() {
    let tmp = tempfile::tempdir().unwrap();
    write_min_app(tmp.path());

    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    listener.set_nonblocking(true).unwrap();
    // Accept and hold: `TcpStream::connect` from `dev stop` succeeds, the read times out.
    // Non-blocking because the number of connections `dev stop` makes is its business,
    // not this test's — a blocking `accept` here would wait for one that never comes.
    let held = std::thread::spawn(move || {
        let mut accepted = Vec::new();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while std::time::Instant::now() < deadline {
            match listener.accept() {
                Ok((stream, _)) => accepted.push(stream),
                Err(_) => std::thread::sleep(std::time::Duration::from_millis(10)),
            }
        }
        drop(accepted);
    });

    std::fs::create_dir_all(tmp.path().join(".runai")).unwrap();
    std::fs::write(
        tmp.path().join(".runai/dev.json"),
        format!(
            "{{\"pid\":{},\"port\":{port},\"root\":{:?}}}\n",
            std::process::id(),
            tmp.path().canonicalize().unwrap().display().to_string()
        ),
    )
    .unwrap();
    let before = std::fs::read_to_string(tmp.path().join(".runai/dev.json")).unwrap();

    let stop = std::process::Command::new(env!("CARGO_BIN_EXE_runai"))
        .args(["dev", "stop"])
        .current_dir(tmp.path())
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&stop.stderr);
    assert!(
        !stop.status.success(),
        "a server that accepted the connection is not proven dead: {stderr}"
    );
    assert!(stderr.contains("busy"), "{stderr}");
    assert_eq!(
        std::fs::read_to_string(tmp.path().join(".runai/dev.json")).unwrap(),
        before,
        "the lockfile of a server this process could not disprove must survive"
    );

    held.join().unwrap();
    let _ = std::fs::remove_file(tmp.path().join(".runai/dev.json"));
}

/// C1(a): a lockfile whose server accepts the connection but does not answer in time is
/// BUSY, not stale. Deleting it strands a live server that `dev stop` can never reach
/// again. Reproduced in round 0 with a slow rebuild, which blocked the serial status
/// route for seconds.
#[test]
fn stop_during_a_slow_rebuild_keeps_the_lockfile_and_reports_busy() {
    let tmp = tempfile::tempdir().unwrap();
    write_min_app(tmp.path());
    let _guard = StopOnDrop(tmp.path().to_path_buf());
    let running = run_json(&["dev", "--background", "--port", "0"], tmp.path());
    let port = running["port"].as_u64().unwrap();
    let pid = running["pid"].as_u64().unwrap_or_default();

    settle();
    std::fs::write(
        tmp.path().join("src/routes/index.page.tsx"),
        slow_route(4000),
    )
    .unwrap();
    let rebuild = std::thread::spawn(move || {
        let _ = http_lenient(port, "GET", "/");
    });
    // Long enough for the page request to be inside the rebuild, short enough that it is
    // certainly still running when `stop` fires.
    std::thread::sleep(std::time::Duration::from_millis(700));

    let stop = std::process::Command::new(env!("CARGO_BIN_EXE_runai"))
        .args(["dev", "stop"])
        .current_dir(tmp.path())
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&stop.stderr);
    assert!(
        !stop.status.success(),
        "stopping a busy server must not report success: {stderr}"
    );
    assert!(
        stderr.contains("busy"),
        "the command must say the server is busy: {stderr}"
    );
    assert!(
        tmp.path().join(".runai/dev.json").exists(),
        "the lockfile of a LIVE server must survive a busy stop (pid {pid})"
    );

    rebuild.join().unwrap();

    // Once the build finishes the server must be stoppable: either it has already exited
    // after the shutdown it was holding, or `dev stop` now succeeds.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let closed = loop {
        if std::net::TcpStream::connect(("127.0.0.1", port as u16)).is_err() {
            break true;
        }
        let retry = std::process::Command::new(env!("CARGO_BIN_EXE_runai"))
            .args(["dev", "stop"])
            .current_dir(tmp.path())
            .output()
            .unwrap();
        if retry.status.success()
            && std::net::TcpStream::connect(("127.0.0.1", port as u16)).is_err()
        {
            break true;
        }
        if std::time::Instant::now() > deadline {
            break false;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    };
    assert!(closed, "the server must be stoppable once the rebuild ends");
}

/// C1(b): `/_runai/status` is answered from memory, so a rebuild cannot block it. This is
/// what keeps the busy case rare even when it cannot be made impossible.
#[test]
fn status_answers_while_a_rebuild_is_in_flight() {
    let tmp = tempfile::tempdir().unwrap();
    write_min_app(tmp.path());
    let _guard = StopOnDrop(tmp.path().to_path_buf());
    let port = run_json(&["dev", "--background", "--port", "0"], tmp.path())["port"]
        .as_u64()
        .unwrap();

    settle();
    std::fs::write(
        tmp.path().join("src/routes/index.page.tsx"),
        slow_route(3000),
    )
    .unwrap();
    let rebuild = std::thread::spawn(move || {
        let _ = http_lenient(port, "GET", "/");
    });
    std::thread::sleep(std::time::Duration::from_millis(500));

    let started = std::time::Instant::now();
    let (code, body) = http_lenient(port, "GET", "/_runai/status")
        .expect("`/_runai/status` must answer while a rebuild is in flight");
    let elapsed = started.elapsed();
    assert_eq!(code, 200, "{body}");
    assert!(body.contains("\"running\":true"), "{body}");
    assert!(
        elapsed < std::time::Duration::from_millis(900),
        "the control route queued behind the rebuild: {elapsed:?}"
    );

    rebuild.join().unwrap();
}

/// I3: an mtime ahead of `now` (a `touch -d tomorrow`, or clock skew on NFS) must not
/// rebuild on every request forever. The stamp is compared for equality across two
/// consecutive page requests.
#[test]
fn a_future_mtime_does_not_rebuild_on_every_request() {
    let tmp = tempfile::tempdir().unwrap();
    write_min_app(tmp.path());
    let _guard = StopOnDrop(tmp.path().to_path_buf());
    let port = run_json(&["dev", "--background", "--port", "0"], tmp.path())["port"]
        .as_u64()
        .unwrap();

    let route = tmp.path().join("src/routes/index.page.tsx");
    let future = std::time::SystemTime::now() + std::time::Duration::from_secs(3600);
    std::fs::File::options()
        .write(true)
        .open(&route)
        .unwrap()
        .set_modified(future)
        .unwrap();

    assert_eq!(http(port, "GET", "/").0, 200, "the first request rebuilds");
    let first = built_at(port);
    assert_eq!(http(port, "GET", "/").0, 200);
    let second = built_at(port);
    assert_eq!(
        first, second,
        "a future mtime rebuilt on every request (build stamp moved {first} -> {second})"
    );
}

/// Fold: a browser must not serve a cached pre-edit page after a rebuild.
#[test]
fn every_response_is_no_store() {
    let tmp = tempfile::tempdir().unwrap();
    write_min_app(tmp.path());
    let _guard = StopOnDrop(tmp.path().to_path_buf());
    let port = run_json(&["dev", "--background", "--port", "0"], tmp.path())["port"]
        .as_u64()
        .unwrap();
    for path in ["/", "/_runai/status", "/missing.html"] {
        let raw = http_raw(port, "GET", path);
        assert!(
            raw.to_ascii_lowercase().contains("cache-control: no-store"),
            "`{path}` must be `no-store`: {raw}"
        );
    }
}

/// Fold: the runtime is a wasm build; serving it as `application/octet-stream` makes
/// `WebAssembly.instantiateStreaming` refuse it.
#[test]
fn wasm_is_served_as_webassembly() {
    let tmp = tempfile::tempdir().unwrap();
    write_min_app(tmp.path());
    let _guard = StopOnDrop(tmp.path().to_path_buf());
    let port = run_json(&["dev", "--background", "--port", "0"], tmp.path())["port"]
        .as_u64()
        .unwrap();
    std::fs::write(tmp.path().join("dist/app.wasm"), b"\0asm").unwrap();
    let raw = http_raw(port, "GET", "/app.wasm");
    assert!(
        raw.to_ascii_lowercase()
            .contains("content-type: application/wasm"),
        "{raw}"
    );
}

/// I4: `--serve-only` is internal to `--background`. A human running it in a project
/// with a background server must not replace that server's lockfile with its own.
#[test]
fn serve_only_refuses_to_clobber_a_live_lockfile() {
    let tmp = tempfile::tempdir().unwrap();
    write_min_app(tmp.path());
    let _guard = StopOnDrop(tmp.path().to_path_buf());
    let running = run_json(&["dev", "--background", "--port", "0"], tmp.path());
    let port = running["port"].as_u64().unwrap();
    let pid = running["pid"].as_u64().unwrap();
    let before = std::fs::read_to_string(tmp.path().join(".runai/dev.json")).unwrap();

    // Bounded: on the code this test was written against, `--serve-only` serves forever,
    // so a plain `output()` would hang the suite instead of failing it.
    let mut stray = KillOnDrop(
        Command::new(env!("CARGO_BIN_EXE_runai"))
            .args(["dev", "--serve-only", "--port", "0"])
            .current_dir(tmp.path())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap(),
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let status = loop {
        if let Some(status) = stray.0.try_wait().unwrap() {
            break status;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "a stray `--serve-only` kept serving instead of refusing"
        );
        std::thread::sleep(std::time::Duration::from_millis(25));
    };
    assert!(
        !status.success(),
        "a stray `--serve-only` must not start a second server"
    );
    assert_eq!(
        std::fs::read_to_string(tmp.path().join(".runai/dev.json")).unwrap(),
        before,
        "the live server's lockfile must be untouched"
    );
    assert_eq!(http(port, "GET", "/_runai/status").0, 200, "still serving");
    let status: serde_json::Value =
        serde_json::from_str(&http(port, "GET", "/_runai/status").1).unwrap();
    assert_eq!(status["pid"].as_u64().unwrap(), pid);
}

/// NEW-C: an edit made *while a build is running* must be rebuilt on the next request.
///
/// The build marker was taken from `now` **after** `build::run` returned, so a file
/// touched inside the build window had `mtime < built_at` and was never seen as a change
/// again: the pre-edit tree was served forever.
#[test]
fn an_edit_during_a_build_is_rebuilt_on_the_next_request() {
    let tmp = tempfile::tempdir().unwrap();
    write_min_app(tmp.path());
    let _guard = StopOnDrop(tmp.path().to_path_buf());
    let port = run_json(&["dev", "--background", "--port", "0"], tmp.path())["port"]
        .as_u64()
        .unwrap();

    settle();
    // A four-second rebuild, publishing SLOWBUILD if the edit is not picked up.
    std::fs::write(
        tmp.path().join("src/routes/index.page.tsx"),
        slow_route_text("SLOWBUILD", 4000),
    )
    .unwrap();
    let rebuild = std::thread::spawn(move || {
        let _ = http_lenient(port, "GET", "/");
    });

    // The build is now inside its render; edit the route under it.
    std::thread::sleep(std::time::Duration::from_millis(1500));
    std::fs::write(
        tmp.path().join("src/routes/index.page.tsx"),
        route_body("NEWEDIT"),
    )
    .unwrap();
    rebuild.join().unwrap();

    let (code, body) = http(port, "GET", "/");
    assert_eq!(code, 200, "{body}");
    assert!(
        body.contains("<p>NEWEDIT</p>"),
        "an edit made during a build must be rebuilt on the next request, not lost; \
         served: {body}"
    );
}

/// NEW-MA(a): a healthy child whose *initial* build takes seconds answers no status yet,
/// so the parent's 500 ms probe reads `Busy`. That must be polled through to
/// [`STARTUP_TIMEOUT`], not turned into a "busy" failure at half a second.
#[test]
fn background_copes_with_a_slow_initial_build_and_leaves_one_server() {
    let tmp = tempfile::tempdir().unwrap();
    write_min_app(tmp.path());
    // ~3 s inside the prerender: longer than one probe, shorter than the startup timeout.
    std::fs::write(
        tmp.path().join("src/routes/index.page.tsx"),
        slow_route_text("hi", 3000),
    )
    .unwrap();
    let _guard = StopOnDrop(tmp.path().to_path_buf());

    let status = run_json(&["dev", "--background", "--port", "0"], tmp.path());
    assert_eq!(status["running"], true);
    let pid = status["pid"].as_u64().unwrap();
    assert_eq!(
        serve_only_pids(tmp.path()),
        vec![pid],
        "a slow-but-healthy start must leave exactly one server process"
    );
    assert_eq!(http(status["port"].as_u64().unwrap(), "GET", "/").0, 200);
}

/// NEW-MA(b): a start that fails must not leave the child it spawned behind, and must not
/// leave a lockfile naming a server no parent owns. The trigger is the startup timeout: a
/// route whose prerender outlives it, so the parent gives up mid-build.
#[test]
fn a_failed_background_start_kills_the_child_it_spawned() {
    let tmp = tempfile::tempdir().unwrap();
    write_min_app(tmp.path());
    std::fs::write(
        tmp.path().join("src/routes/index.page.tsx"),
        slow_route_text("hi", 9000),
    )
    .unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_runai"))
        .args(["dev", "--background", "--port", "0"])
        .current_dir(tmp.path())
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "a start that never becomes ready must fail: {}",
        String::from_utf8_lossy(&out.stdout)
    );

    // A SIGKILLed child does not get to clean up, so the parent must have done it.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while !serve_only_pids(tmp.path()).is_empty() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    assert_eq!(
        serve_only_pids(tmp.path()),
        Vec::<u64>::new(),
        "a failed `--background` must not leave a server holding the port"
    );
    assert!(
        !tmp.path().join(".runai/dev.json").exists(),
        "a failed `--background` must not leave a lockfile for a server it killed"
    );
}

/// `building` must be true while a rebuild runs, or a client polling `/_runai/status` can
/// never tell why a page request is slow.
#[test]
fn status_reports_that_a_build_is_in_flight() {
    let tmp = tempfile::tempdir().unwrap();
    write_min_app(tmp.path());
    let _guard = StopOnDrop(tmp.path().to_path_buf());
    let port = run_json(&["dev", "--background", "--port", "0"], tmp.path())["port"]
        .as_u64()
        .unwrap();

    settle();
    std::fs::write(
        tmp.path().join("src/routes/index.page.tsx"),
        slow_route(3000),
    )
    .unwrap();
    let rebuild = std::thread::spawn(move || {
        let _ = http_lenient(port, "GET", "/");
    });
    std::thread::sleep(std::time::Duration::from_millis(700));

    let (code, body) = http_lenient(port, "GET", "/_runai/status").expect("status must answer");
    assert_eq!(code, 200, "{body}");
    assert!(
        body.contains("\"building\":true"),
        "a rebuild in flight must be reported: {body}"
    );

    rebuild.join().unwrap();
    let (_, body) = http(port, "GET", "/_runai/status");
    assert!(
        body.contains("\"building\":false"),
        "the flag must clear once the build ends: {body}"
    );
}

/// The pids of this project's `--serve-only` servers, from `/proc`, addressed by pid and
/// by the child's working directory — never by a name pattern, which can match the shell
/// running the test.
fn serve_only_pids(root: &Path) -> Vec<u64> {
    let root = root.canonicalize().unwrap();
    let mut found = Vec::new();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return found;
    };
    for entry in entries.flatten() {
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<u64>() else {
            continue;
        };
        let Ok(command) = std::fs::read_to_string(entry.path().join("cmdline")) else {
            continue;
        };
        if !command.contains("--serve-only") {
            continue;
        }
        match std::fs::read_link(entry.path().join("cwd")) {
            Ok(cwd) if cwd == root => found.push(pid),
            _ => {}
        }
    }
    found.sort_unstable();
    found
}

/// A route file rendering `text`, as the brief's apps write them.
fn route_body(text: &str) -> String {
    format!(
        "\"use static\"\nexport default function P(props: Record<never, never>) {{ return <p>{text}</p> }}\n"
    )
}

/// A static route that busy-waits in `render`, so a rebuild is observable while it is in
/// flight. Same shape as `tests/build.rs`'s `slow_route`: the loop runs inside the
/// prerender child, i.e. after the build has taken its lock.
fn slow_route(millis: u64) -> String {
    slow_route_text("slow", millis)
}

/// [`slow_route`] with a chosen body, so a test can name the tree a slow build publishes.
fn slow_route_text(text: &str, millis: u64) -> String {
    format!(
        "\"use static\"\nexport default function P(props: Record<never, never>) {{\n\
         const until = Date.now() + {millis};\n\
         while (Date.now() < until) {{}}\n\
         return <p>{text}</p>\n}}\n"
    )
}

/// A route that cannot build: `"use static"` with a signal is RA010.
fn broken_route() -> &'static str {
    "\"use static\"\nimport { signal } from \"runai\"\nexport default function P(props: Record<never, never>) { const n = signal(0); return <p>{n()}</p> }\n"
}

/// Wait past the coarse 1-second mtime resolution some filesystems have.
fn settle() {
    std::thread::sleep(std::time::Duration::from_millis(1100));
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
