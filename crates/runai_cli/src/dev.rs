//! `runai dev`: serve `dist/` over HTTP, rebuilding when the sources change.
//!
//! The command exists for the inner loop an agent runs: edit a route, fetch the page,
//! read the diagnostics if it broke. Two properties follow from that, and they are the
//! whole design:
//!
//! - **A broken edit must not take the page down.** A rebuild that fails keeps serving
//!   the `dist/` of the last good build and records the diagnostics for
//!   `GET /_runai/status`; the server never exits over a route that does not compile.
//!   The build's own all-or-nothing publish (see [`crate::build`]) is what makes the
//!   last good tree still there to serve.
//! - **Starting twice is starting once.** `--background` re-executes this binary with a
//!   hidden `--serve-only` flag, detached with all three stdio handles null, and the
//!   child writes `.runai/dev.json`. A second `--background` reads that lockfile and
//!   asks the running server `/_runai/status`; it reuses the instance only when the
//!   answer's `pid` **and** `root` match, so a dead server, a reused port, or a
//!   lockfile copied from another checkout starts a fresh one instead of lying.
//!
//! `dev stop` uses the same verification and then `POST /_runai/shutdown`; it never
//! signals a pid. A stale lockfile whose port now belongs to another program is the
//! case that would otherwise kill an innocent process.
//!
//! # The server is a hand-written HTTP/1.1 reader on purpose
//!
//! The runtime budget is 6144 bytes gzipped and the binary takes no HTTP dependency.
//! It binds `127.0.0.1` only — never `0.0.0.0` — reads one request, answers, and
//! closes. Requests are served one at a time: a rebuild is synchronous, and a browser
//! loading a page issues a handful of requests, so a thread pool would add failure
//! modes without adding anything.
//!
//! # `diagnostics` is the last build's located errors, and `[]` means clean
//!
//! `/_runai/status` carries the error diagnostics of the last build. A build can fail
//! without producing one — a render that threw, a missing runtime, an unreadable file,
//! a refused `--out` — and [`crate::build::run`] reports those as a complete sentence on
//! stderr, not a [`Diagnostic`], because [`Diagnostic`] is a code plus a span and
//! inventing one would lie about a code. In that case the array is empty. The build
//! failure itself is never silent: the dev server also writes
//! `runai dev: the build failed during <reason>` to stderr, and keeps serving the last
//! good `dist/`. Backgrounding sets that stderr to null, so the empty array is the only
//! signal a backgrounded server has for a non-diagnostic failure — a documented boundary
//! rather than an oversight.
//!
//! # Paths are decoded first, then checked
//!
//! The check is on the **decoded** path, never the raw request text, because `%2e%2e`
//! is `..` and checking the encoding would miss it. A decoded path is refused when it
//! is not a normalized relative path inside `dist/`: any segment is `.`, `..`, or
//! starts with `.`; any byte is a control character; it contains a backslash; a
//! segment is a drive prefix; or a `%` escape is malformed. The dot-segment rule is
//! broader than `..` on purpose: the published tree carries `.runai-build`, so
//! `GET /.runai-build` would otherwise answer `runai 0.1.0 build` — a version
//! disclosure and a 200 where a 404 belongs.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime};

use runai_compiler::diagnostic::Diagnostic;

use crate::USAGE;

/// Where the build publishes, and what this serves.
const DIST: &str = "dist";

/// Where the sources live: a change anywhere under here means a rebuild.
const SRC: &str = "src";

/// The config file. It sits beside `src/`, so it is watched separately.
const CONFIG: &str = "runai.json";

/// The lockfile naming the running server, relative to the app root.
const LOCKFILE: &str = ".runai/dev.json";

/// The port used when `--port` is absent.
const DEFAULT_PORT: u16 = 5173;

/// How long a client may take to send a request, so one silent connection cannot
/// wedge the accept loop.
const CLIENT_TIMEOUT: Duration = Duration::from_secs(5);

/// How long the parent waits for a freshly spawned server, and how long `stop` waits
/// for the port to close.
const STARTUP_TIMEOUT: Duration = Duration::from_secs(5);

/// How often the parent re-probes during startup.
const POLL: Duration = Duration::from_millis(25);

/// The largest request head accepted, in bytes. A bind is local and the requests are
/// ours (a browser, or `dev stop`), so a bigger one is not a slow client but a bug.
const MAX_HEAD: usize = 16 * 1024;

/// The control endpoints. Answered from memory, before any file lookup.
const STATUS: &str = "/_runai/status";
const SHUTDOWN: &str = "/_runai/shutdown";

/// The hidden flag that turns this process into the detached server. Not in [`USAGE`]:
/// it is an implementation detail of `--background`, not something to type.
const SERVE_ONLY: &str = "--serve-only";

/// Run `runai dev …` with the arguments that follow `dev`.
///
/// Returns the process exit code: `0` served, started, stopped, or already stopped;
/// `2` the command was misused; `1` the server could not be built, bound, or started
/// within [`STARTUP_TIMEOUT`].
pub fn run(args: &[String]) -> i32 {
    let mut port = DEFAULT_PORT;
    let mut background = false;
    let mut serve_only = false;
    let mut stop = false;
    let mut index = 0;

    while index < args.len() {
        let arg = args[index].as_str();
        index += 1;
        match arg {
            "stop" => stop = true,
            "--background" => background = true,
            SERVE_ONLY => serve_only = true,
            "--port" => {
                let Some(value) = args.get(index) else {
                    eprintln!("error: `--port` needs a number");
                    eprintln!("{USAGE}");
                    return 2;
                };
                index += 1;
                match value.parse::<u16>() {
                    Ok(value) => port = value,
                    Err(_) => return bad_port(value),
                }
            }
            flag if flag.starts_with("--port=") => match flag["--port=".len()..].parse::<u16>() {
                Ok(value) => port = value,
                Err(_) => return bad_port(&flag["--port=".len()..]),
            },
            flag if flag.starts_with('-') && flag != SERVE_ONLY => {
                eprintln!("error: unknown flag `{flag}`");
                eprintln!("{USAGE}");
                return 2;
            }
            value => {
                eprintln!("error: unexpected argument `{value}`");
                eprintln!("{USAGE}");
                return 2;
            }
        }
    }

    if stop {
        return stop_server();
    }

    let root = match std::env::current_dir().and_then(|dir| dir.canonicalize()) {
        Ok(root) => root,
        Err(error) => {
            eprintln!("error: cannot resolve the working directory: {error}");
            return 1;
        }
    };

    // The child half of `--background`. It is checked before `--background` so a
    // spawn that somehow carries both still serves instead of spawning again forever.
    if serve_only {
        return serve(&root, port);
    }

    if background {
        return start_background(&root, port);
    }

    // Foreground: build once so the first request has something to serve, then serve
    // in this process. stdout is the status JSON, matching `--background`.
    let mut state = State::new(&root, port);
    match state.begin_listen() {
        Ok(listener) => {
            state.port = listener
                .local_addr()
                .map(|address| address.port())
                .unwrap_or(port);
            state.rebuild("the initial build");
            state.write_lock();
            println!("{}", state.status_json());
            state.serve(listener);
            0
        }
        Err(message) => {
            eprintln!("error: {message}");
            1
        }
    }
}

/// Refuse a `--port` value that is not a number in range.
fn bad_port(value: &str) -> i32 {
    eprintln!("error: `--port {value}` is not a port number (0..=65535)");
    eprintln!("{USAGE}");
    2
}

/// The live server: what it serves, on what port, and what the last build said.
struct State {
    /// The app root, absolute and canonical: the same value `/_runai/status` reports
    /// and the lockfile carries, so a reused lockfile can be verified against it.
    root: PathBuf,
    port: u16,
    /// When the last rebuild *started*. A file modified after this is a reason to
    /// build again. A failed build moves this forward too, so a broken edit is not
    /// rebuilt on every request while it stays broken.
    last_build: SystemTime,
    /// The error diagnostics of the last failed build; empty when it was clean.
    ///
    /// A failure that is not a located diagnostic — a render that threw, a missing
    /// runtime, an unreadable file — leaves this empty, because [`Diagnostic`] is a
    /// tuple of a code and a span and inventing one would be a lie about a code. Those
    /// reasons are on stderr; see the module docs.
    diagnostics: Vec<Diagnostic>,
}

impl State {
    fn new(root: &Path, port: u16) -> State {
        State {
            root: root.to_path_buf(),
            port,
            last_build: SystemTime::UNIX_EPOCH,
            diagnostics: Vec::new(),
        }
    }

    fn dist(&self) -> PathBuf {
        self.root.join(DIST)
    }

    fn lock_path(&self) -> PathBuf {
        self.root.join(LOCKFILE)
    }

    /// Bind `127.0.0.1:port`; port `0` asks the OS for a free one.
    fn begin_listen(&self) -> Result<TcpListener, String> {
        TcpListener::bind(("127.0.0.1", self.port))
            .map_err(|error| format!("cannot listen on 127.0.0.1:{}: {error}", self.port))
    }

    /// The single accept loop. Ends only by [`std::process::exit`] from the shutdown
    /// endpoint, which is why it returns nothing.
    fn serve(&mut self, listener: TcpListener) {
        for stream in listener.incoming() {
            let mut stream = match stream {
                Ok(stream) => stream,
                // A connection that failed to be accepted is one client's problem; the
                // server must not exit over it.
                Err(_) => continue,
            };
            let _ = stream.set_read_timeout(Some(CLIENT_TIMEOUT));
            let _ = stream.set_write_timeout(Some(CLIENT_TIMEOUT));
            let Some((method, target)) = read_head(&mut stream) else {
                continue;
            };
            if self.handle(&mut stream, &method, &target) {
                return;
            }
        }
    }

    /// Answer one request. Returns `true` when the process should stop.
    fn handle(&mut self, stream: &mut TcpStream, method: &str, target: &str) -> bool {
        // Control endpoints are matched on the raw target: they are literal ASCII
        // paths this server owns, and comparing before decoding keeps the status
        // endpoint reachable even for a request whose encoding is odd. The path
        // safety rule below never applies to a file lookup that is not attempted.
        let path = target.split('?').next().unwrap_or(target);
        if path == STATUS || path == SHUTDOWN {
            return self.control(stream, method, path);
        }

        if method != "GET" && method != "HEAD" {
            write_response(
                stream,
                405,
                "text/plain; charset=utf-8",
                b"method not allowed\n",
                false,
            );
            return false;
        }

        let Some(decoded) = decode_path(path) else {
            not_found(stream, method);
            return false;
        };

        // A rebuild is only worth its cost when the response can be stale HTML. That is
        // a page request: the root, a directory (`/about/` or `/about`), or an explicit
        // `.html`. An asset (`/assets/index.js`) cannot change without a page that
        // references it changing too, and it is exactly what a rebuild would interrupt.
        if page_request(&decoded) {
            self.rebuild_if_stale();
        }

        let Some(file) = resolve_file(&self.dist(), &decoded) else {
            not_found(stream, method);
            return false;
        };
        match std::fs::read(&file) {
            Ok(body) => {
                let kind = content_type(&file);
                write_response(stream, 200, kind, &body, method == "HEAD");
            }
            // A file that vanished between resolution and this read is a rebuild
            // publishing over the tree, not a bug: a 404 is the honest answer.
            Err(_) => not_found(stream, method),
        }
        false
    }

    /// Answer `/_runai/status` or `/_runai/shutdown`. Returns `true` to stop.
    fn control(&mut self, stream: &mut TcpStream, method: &str, path: &str) -> bool {
        if path == STATUS {
            if method != "GET" && method != "HEAD" {
                write_response(
                    stream,
                    405,
                    "text/plain; charset=utf-8",
                    b"method not allowed\n",
                    false,
                );
                return false;
            }
            let body = self.status_json();
            write_response(
                stream,
                200,
                "application/json",
                body.as_bytes(),
                method == "HEAD",
            );
            return false;
        }

        // Shutdown is a POST: a browser prefetch or a stray GET must not stop the
        // server, and `dev stop` is the only intended caller.
        if method != "POST" {
            write_response(
                stream,
                405,
                "text/plain; charset=utf-8",
                b"method not allowed\n",
                false,
            );
            return false;
        }
        write_response(
            stream,
            200,
            "application/json",
            br#"{"stopping":true}"#,
            false,
        );
        let _ = stream.flush();

        // Remove the lockfile here, but only when it still names this process: a
        // successor that already overwrote it (started in the same instant `stop`
        // began) must keep its own lockfile.
        self.remove_own_lock();
        std::process::exit(0);
    }

    /// The status payload. Field order is the contract documented in the brief.
    fn status_json(&self) -> String {
        format!(
            "{{\"running\":true,\"pid\":{},\"port\":{},\"root\":{},\"diagnostics\":{}}}",
            std::process::id(),
            self.port,
            json_string(&self.root.to_string_lossy()),
            Diagnostic::render_all_json(&self.diagnostics),
        )
    }

    /// Write `.runai/dev.json` as `{pid,port,root}`.
    fn write_lock(&self) {
        let path = self.lock_path();
        if let Some(parent) = path.parent()
            && std::fs::create_dir_all(parent).is_err()
        {
            return;
        }
        let body = format!(
            "{{\"pid\":{},\"port\":{},\"root\":{}}}\n",
            std::process::id(),
            self.port,
            json_string(&self.root.to_string_lossy()),
        );
        // Best effort: a read-only checkout still serves, it just cannot be stopped by
        // lockfile and has to be killed. The caller's own process is the authority.
        let _ = std::fs::write(&path, body);
    }

    /// Remove the lockfile if it still names this pid and port.
    fn remove_own_lock(&self) {
        let path = self.lock_path();
        let Ok(text) = std::fs::read_to_string(&path) else {
            return;
        };
        let pid = field_number(&text, "pid");
        let port = field_number(&text, "port");
        if pid == Some(u64::from(std::process::id())) && port == Some(u64::from(self.port)) {
            let _ = std::fs::remove_file(&path);
        }
    }

    /// Rebuild when anything under `src/` or `runai.json` is newer than the last build.
    fn rebuild_if_stale(&mut self) {
        if changed_since(&self.root, self.last_build) {
            self.rebuild("a page request");
        }
    }

    /// Build into `dist/`, recording the outcome.
    ///
    /// The build itself is [`crate::build::run`] — this never reimplements it, so the
    /// served tree is exactly what `runai build` produces. A failure is recorded, not
    /// returned: the previous `dist/` is still there and must keep being served.
    fn rebuild(&mut self, reason: &str) {
        // The marker is taken *before* the build: a file edited while the build runs
        // has an mtime later than this, so the next request rebuilds again rather
        // than serving the tree that missed the edit.
        self.last_build = SystemTime::now();

        let code = crate::build::run(&["--out".to_string(), DIST.to_string()]);
        if code == 0 {
            self.diagnostics.clear();
            return;
        }

        // The build refused. Its stderr went wherever this process's stderr goes (null
        // when backgrounded), so the reason is also captured structurally for the
        // status endpoint. `crate::build::diagnostics` is the same walk and the same
        // `analyze` the build gates on.
        self.diagnostics = crate::build::diagnostics();
        if self.diagnostics.is_empty() {
            eprintln!("runai dev: the build failed during {reason}; serving the last good `dist/`");
        } else {
            eprintln!(
                "runai dev: the build failed during {reason} with {} diagnostic(s); serving the last good `dist/`",
                self.diagnostics.len()
            );
        }
    }
}

/// The child half of `--background`: build, then serve until shutdown.
fn serve(root: &Path, port: u16) -> i32 {
    let mut state = State::new(root, port);
    let listener = match state.begin_listen() {
        Ok(listener) => listener,
        Err(message) => {
            // The parent is polling for the lockfile and will time out with its own
            // message; this one is for a human running `--serve-only` by hand.
            eprintln!("error: {message}");
            return 1;
        }
    };
    // The real port, which differs from the argument when it was `0`.
    state.port = listener
        .local_addr()
        .map(|address| address.port())
        .unwrap_or(port);

    // The lockfile is written before the build so the parent learns the port quickly;
    // the parent re-fetches status with a timeout, so it does not observe a half-built
    // server as ready.
    state.write_lock();
    state.rebuild("startup");
    state.serve(listener);
    0
}

/// `--background`: reuse a verified running server, or spawn one and wait for it.
fn start_background(root: &Path, port: u16) -> i32 {
    if let Some(existing) = verified_server(root) {
        println!("{existing}");
        return 0;
    }

    // Anything in the lockfile is now known stale: it is either unreadable, or its
    // port did not answer, or it answered as a different pid or root. Removing it
    // before the spawn means a crashed child that never got to write cannot leave the
    // parent polling yesterday's file.
    let lock = root.join(LOCKFILE);
    let _ = std::fs::remove_file(&lock);

    let Ok(executable) = std::env::current_exe() else {
        eprintln!("error: cannot find the runai executable to start the server");
        return 1;
    };

    let child = Command::new(executable)
        .args(["dev", SERVE_ONLY, "--port", &port.to_string()])
        .current_dir(root)
        // All three null: the child outlives this process, and leaving a pipe open
        // would make the shell that ran `runai dev --background` wait on the server
        // instead of returning.
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    let mut child = match child {
        Ok(child) => child,
        Err(error) => {
            eprintln!("error: cannot start the dev server: {error}");
            return 1;
        }
    };

    let deadline = SystemTime::now() + STARTUP_TIMEOUT;
    let mut exited = None;
    while SystemTime::now() < deadline {
        if let Ok(Some(status)) = child.try_wait() {
            exited = Some(status);
            break;
        }
        if let Some(lock) = read_lock(root)
            && lock.pid == u64::from(child.id())
            && let Some(status) = fetch_status(lock.port)
            && status.pid == lock.pid
            && same_root(&status.root, &lock.root)
        {
            println!("{}", status.body);
            return 0;
        }
        std::thread::sleep(POLL);
    }

    match exited {
        Some(status) => {
            eprintln!("error: the dev server exited during startup ({status})");
            1
        }
        None => {
            eprintln!(
                "error: the dev server did not answer `/_runai/status` within {}s",
                STARTUP_TIMEOUT.as_secs()
            );
            1
        }
    }
}

/// `dev stop`: verify the lockfile against a live server, ask it to stop, clean up.
///
/// Every path exits `0`. Stopping a server that is not running is success, not an
/// error — `dev stop` in a script must be callable unconditionally. The unverified
/// case is the important one: a lockfile whose port now belongs to another program is
/// deleted, and that program is never signalled.
fn stop_server() -> i32 {
    let root = match std::env::current_dir().and_then(|dir| dir.canonicalize()) {
        Ok(root) => root,
        Err(error) => {
            eprintln!("error: cannot resolve the working directory: {error}");
            return 1;
        }
    };
    let lock = root.join(LOCKFILE);

    let Some(found) = read_lock(&root) else {
        let _ = std::fs::remove_file(&lock);
        return 0;
    };
    // A lockfile copied from another checkout names that checkout as its root. It is
    // stale here no matter what the port answers, and stopping through it would end a
    // server belonging to a different project.
    if !same_root(&found.root, &root.to_string_lossy()) {
        let _ = std::fs::remove_file(&lock);
        return 0;
    }
    let Some(status) = fetch_status(found.port) else {
        // Refused connection, or something that is not our server: stale lockfile.
        let _ = std::fs::remove_file(&lock);
        return 0;
    };
    if status.pid != found.pid || !same_root(&status.root, &found.root) {
        // The port answers, but as a different process (or the same pid in a different
        // checkout): this is exactly the case where a pid signal would kill an
        // innocent program. Drop the lockfile and leave that server alone.
        let _ = std::fs::remove_file(&lock);
        return 0;
    }

    if !post_shutdown(found.port) {
        // The server accepted the connection and then failed to answer; treat it as
        // gone rather than signalling it.
        let _ = std::fs::remove_file(&lock);
        return 0;
    }

    let deadline = SystemTime::now() + STARTUP_TIMEOUT;
    while SystemTime::now() < deadline {
        if TcpStream::connect(("127.0.0.1", found.port)).is_err() {
            break;
        }
        std::thread::sleep(POLL);
    }

    let _ = std::fs::remove_file(&lock);
    0
}

/// The status body of a server that has just been verified for `root`, if there is one.
///
/// All three facts must agree: the lockfile's `pid` is the one `/_runai/status` reports,
/// the served `root` is the one the lockfile names, and that root **is this working
/// directory**. The last comparison is not redundant: without it a `.runai/dev.json`
/// copied from another checkout verifies against itself — its own `root` field is the
/// other project — and `dev stop` there would shut down a server it has no business
/// touching.
fn verified_server(root: &Path) -> Option<String> {
    let found = read_lock(root)?;
    if !same_root(&found.root, &root.to_string_lossy()) {
        return None;
    }
    let status = fetch_status(found.port)?;
    if status.pid != found.pid || !same_root(&status.root, &found.root) {
        return None;
    }
    Some(status.body)
}

/// A lockfile's parsed contents.
struct Lock {
    pid: u64,
    port: u16,
    root: String,
}

/// Read and parse `.runai/dev.json`, or `None` when it is absent or malformed.
///
/// `serde_json` is a dev-dependency only, so the three fields are read with the same
/// small field scanner the status response is parsed with.
fn read_lock(root: &Path) -> Option<Lock> {
    let text = std::fs::read_to_string(root.join(LOCKFILE)).ok()?;
    let pid = field_number(&text, "pid")?;
    let port = field_number(&text, "port")?;
    let root = field_string(&text, "root")?;
    Some(Lock {
        pid,
        port: u16::try_from(port).ok()?,
        root,
    })
}

/// A `/status` answer, parsed enough to verify the server.
struct Status {
    body: String,
    pid: u64,
    root: String,
}

/// `GET /_runai/status` on `port`, or `None` when nothing answers in time.
///
/// The timeout is short because a spawn that bound its port but has not finished the
/// initial build is not ready: the caller polls, and an early answer is better than a
/// slow one.
fn fetch_status(port: u16) -> Option<Status> {
    let body = request(port, "GET", STATUS, Duration::from_millis(500))?;
    let pid = field_number(&body, "pid")?;
    let root = field_string(&body, "root")?;
    Some(Status { body, pid, root })
}

/// `POST /_runai/shutdown` on `port`; whether the request got an answer.
fn post_shutdown(port: u16) -> bool {
    request(port, "POST", SHUTDOWN, Duration::from_secs(1)).is_some()
}

/// One request, returning the response body. `None` on any connection or parse error.
fn request(port: u16, method: &str, path: &str, timeout: Duration) -> Option<String> {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).ok()?;
    stream.set_read_timeout(Some(timeout)).ok()?;
    stream.set_write_timeout(Some(timeout)).ok()?;
    write!(
        stream,
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\nContent-Length: 0\r\n\r\n"
    )
    .ok()?;
    stream.flush().ok()?;

    let mut raw = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(read) => {
                raw.extend_from_slice(&chunk[..read]);
                if raw.len() > MAX_HEAD {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    let text = String::from_utf8_lossy(&raw).into_owned();
    let (head, body) = text.split_once("\r\n\r\n")?;
    if !head.starts_with("HTTP/1.1 200") {
        return Some(String::new());
    }
    Some(body.to_string())
}

/// Read a request line: `(method, target)`.
fn read_head(stream: &mut TcpStream) -> Option<(String, String)> {
    let mut raw = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(read) => raw.extend_from_slice(&chunk[..read]),
            Err(_) => return None,
        }
        if raw.windows(4).any(|window| window == b"\r\n\r\n") || raw.len() > MAX_HEAD {
            break;
        }
    }
    let text = String::from_utf8_lossy(&raw);
    let line = text.lines().next()?;
    let mut parts = line.split_whitespace();
    let method = parts.next()?.to_string();
    let target = parts.next()?.to_string();
    Some((method, target))
}

/// Write one response and close. Headers are lowercase-free and ASCII only.
fn write_response(
    stream: &mut TcpStream,
    code: u16,
    content_type: &str,
    body: &[u8],
    head_only: bool,
) {
    let reason = match code {
        200 => "OK",
        404 => "Not Found",
        405 => "Method Not Allowed",
        _ => "Error",
    };
    let head = format!(
        "HTTP/1.1 {code} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    if !head_only {
        let _ = stream.write_all(body);
    }
    let _ = stream.flush();
}

/// The 404 body. Deliberately not the path: an error page that echoes the request is
/// a reflected-content surface, and the request is often a probe.
fn not_found(stream: &mut TcpStream, method: &str) {
    write_response(
        stream,
        404,
        "text/plain; charset=utf-8",
        b"not found\n",
        method == "HEAD",
    );
}

/// Percent-decode and validate a request path.
///
/// Returns the decoded path with its leading `/`, or `None` when the path is refused.
/// Everything is checked on the **decoded** bytes: `%2e%2e` is `..`, `%00` is a NUL,
/// and `%5c` is a backslash, so checking the raw text would let all three through.
///
/// Refused:
///
/// - a malformed escape (`%zz`, `%2`), because the path was not a legal URI;
/// - a decoded byte that is not valid UTF-8;
/// - a NUL or any other control character, which no file name should carry and which
///   truncates a path at the syscall boundary on some platforms;
/// - a backslash, which is a separator on Windows and would be a name on Linux;
/// - a segment that is `.`, `..`, or starts with `.` — the dot rule is what keeps
///   `.runai-build` inside `dist/` unreadable, and `..` is the traversal case;
/// - a drive prefix (`C:`), which names an absolute path on Windows;
/// - an empty interior segment (`//`), because that is not a normalized path.
fn decode_path(raw: &str) -> Option<String> {
    // A proxy sends the absolute form (`GET http://host/x HTTP/1.1`). Only the
    // origin form is served; anything else is not a path inside `dist/`.
    if !raw.starts_with('/') {
        return None;
    }

    let bytes = raw.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        index += 1;
        if byte != b'%' {
            decoded.push(byte);
            continue;
        }
        let high = hex(*bytes.get(index)?)?;
        let low = hex(*bytes.get(index + 1)?)?;
        index += 2;
        decoded.push(high << 4 | low);
    }

    let path = String::from_utf8(decoded).ok()?;
    if path
        .chars()
        .any(|character| character.is_control() || character == '\\')
    {
        return None;
    }
    if path.contains(':') {
        return None;
    }

    // `/` alone is the root, so the first empty segment is expected; every later empty
    // one (`/a//b`) is not a normalized path.
    let segments: Vec<&str> = path[1..].split('/').collect();
    if segments.len() > 1
        && segments[..segments.len() - 1]
            .iter()
            .any(|part| part.is_empty())
    {
        return None;
    }
    for segment in segments.iter().filter(|segment| !segment.is_empty()) {
        if segment.starts_with('.') {
            return None;
        }
    }

    Some(path)
}

/// The value of one hex digit, or `None`.
fn hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Whether a decoded path could be answered with page HTML, which is the only response
/// a rebuild can make stale.
///
/// The root, a trailing-slash directory, a path with no extension (`/about` is served
/// as `about/index.html`), and an explicit `.html` are pages. `/assets/index.js` is not:
/// a route edit rebuilds the tree and the script is fetched once, so rebuilding on it
/// would interrupt the very request a browser is waiting on for no gain.
fn page_request(path: &str) -> bool {
    let relative = path.trim_start_matches('/');
    if relative.is_empty() || relative.ends_with('/') {
        return true;
    }
    let last = relative.rsplit('/').next().unwrap_or(relative);
    match last.rsplit_once('.') {
        Some((_, extension)) => extension.eq_ignore_ascii_case("html"),
        None => true,
    }
}

/// The file inside `dist/` a decoded path names, or `None` for a 404.
///
/// `/` and `/dir/` are directories, so each maps to its `index.html`; a path naming a
/// file is served as that file. No `..` can reach here — [`decode_path`] refused it —
/// so joining cannot escape the tree.
fn resolve_file(dist: &Path, path: &str) -> Option<PathBuf> {
    let relative = path.trim_start_matches('/');
    let relative = if relative.is_empty() || relative.ends_with('/') {
        format!("{relative}index.html")
    } else {
        relative.to_string()
    };

    let candidate = dist.join(&relative);
    if candidate.is_file() {
        return Some(candidate);
    }
    if candidate.is_dir() {
        let index = candidate.join("index.html");
        if index.is_file() {
            return Some(index);
        }
    }
    None
}

/// A content type guessed from the extension, because `dist/` is written by this tool
/// and its extension set is small and known.
fn content_type(file: &Path) -> &'static str {
    match file
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
    {
        "html" | "htm" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" | "map" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        "txt" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

/// Whether any file under `src/` or `runai.json` was modified after `since`.
///
/// An unreadable entry counts as a change: the build is the thing that must decide,
/// and skipping it would serve a stale page after an edit this walk could not see.
fn changed_since(root: &Path, since: SystemTime) -> bool {
    if newer(&root.join(CONFIG), since) {
        return true;
    }
    directory_changed(&root.join(SRC), since)
}

/// [`changed_since`] for a directory tree.
fn directory_changed(dir: &Path, since: SystemTime) -> bool {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        // No `src/` at all: the build will report that, and it is not a change.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return false,
        Err(_) => return true,
    };
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => return true,
        };
        let path = entry.path();
        match entry.file_type() {
            Ok(kind) if kind.is_dir() => {
                if directory_changed(&path, since) {
                    return true;
                }
            }
            Ok(_) => {
                if newer(&path, since) {
                    return true;
                }
            }
            Err(_) => return true,
        }
    }
    false
}

/// Whether `path` exists and was modified after `since`.
fn newer(path: &Path, since: SystemTime) -> bool {
    match std::fs::metadata(path).and_then(|metadata| metadata.modified()) {
        Ok(modified) => modified > since,
        Err(error) => error.kind() != std::io::ErrorKind::NotFound,
    }
}

/// Whether two root strings name the same directory, comparing canonical paths when
/// both resolve.
fn same_root(left: &str, right: &str) -> bool {
    let left = Path::new(left);
    let right = Path::new(right);
    match (left.canonicalize(), right.canonicalize()) {
        (Ok(left), Ok(right)) => left == right,
        _ => left == right,
    }
}

/// A JSON string literal for one value.
///
/// The binary has no JSON dependency; the two things it writes as JSON are a path and
/// the diagnostics array, and the array comes from the compiler's own serializer.
fn json_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for character in value.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            character if character.is_control() => {
                out.push_str(&format!("\\u{:04x}", character as u32));
            }
            character => out.push(character),
        }
    }
    out.push('"');
    out
}

/// The integer value of `"key":N` in `text`.
fn field_number(text: &str, key: &str) -> Option<u64> {
    let rest = after_key(text, key)?;
    let digits: String = rest
        .trim_start()
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().ok()
}

/// The decoded string value of `"key":"…"` in `text`.
fn field_string(text: &str, key: &str) -> Option<String> {
    let rest = after_key(text, key)?.trim_start();
    let rest = rest.strip_prefix('"')?;
    let mut value = String::new();
    let mut chars = rest.chars();
    while let Some(character) = chars.next() {
        match character {
            '"' => return Some(value),
            '\\' => match chars.next()? {
                '"' => value.push('"'),
                '\\' => value.push('\\'),
                '/' => value.push('/'),
                'n' => value.push('\n'),
                'r' => value.push('\r'),
                't' => value.push('\t'),
                'u' => {
                    let digits: String = chars.by_ref().take(4).collect();
                    let code = u32::from_str_radix(&digits, 16).ok()?;
                    value.push(char::from_u32(code)?);
                }
                _ => return None,
            },
            character => value.push(character),
        }
    }
    None
}

/// The text after `"key"` and its `:`.
fn after_key<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    let marker = format!("\"{key}\"");
    let found = text.find(&marker)?;
    let rest = &text[found + marker.len()..];
    let colon = rest.find(':')?;
    Some(&rest[colon + 1..])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_refuses_escaped_traversal() {
        assert_eq!(decode_path("/%2e%2e/secret"), None);
        assert_eq!(decode_path("/%2E%2E/secret"), None);
        assert_eq!(decode_path("/..%2fsecret"), None);
        assert_eq!(decode_path("/a/../../etc/passwd"), None);
        assert_eq!(decode_path("/../secret.txt"), None);
    }

    #[test]
    fn decode_refuses_dotfiles_control_and_backslashes() {
        assert_eq!(decode_path("/.runai-build"), None);
        assert_eq!(decode_path("/a/.hidden"), None);
        assert_eq!(decode_path("/%00"), None);
        assert_eq!(decode_path("/a\0b"), None);
        assert_eq!(decode_path("/a\\..\\b"), None);
        assert_eq!(decode_path("/C:/win"), None);
        assert_eq!(decode_path("/%zz"), None);
        assert_eq!(decode_path("/%2"), None);
    }

    #[test]
    fn decode_accepts_normal_paths() {
        assert_eq!(decode_path("/").as_deref(), Some("/"));
        assert_eq!(decode_path("/index.html").as_deref(), Some("/index.html"));
        assert_eq!(decode_path("/about/").as_deref(), Some("/about/"));
        assert_eq!(
            decode_path("/assets/index.js").as_deref(),
            Some("/assets/index.js")
        );
        assert_eq!(decode_path("/a%20b.html").as_deref(), Some("/a b.html"));
    }

    #[test]
    fn root_maps_to_index() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("index.html"), "hi").unwrap();
        assert_eq!(
            resolve_file(tmp.path(), "/"),
            Some(tmp.path().join("index.html"))
        );
    }

    #[test]
    fn json_strings_round_trip_through_the_field_scanner() {
        let text = format!("{{\"root\":{}}}", json_string("/a/b\"c\\d"));
        assert_eq!(field_string(&text, "root").as_deref(), Some("/a/b\"c\\d"));
    }
}
