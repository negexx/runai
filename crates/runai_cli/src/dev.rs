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
//! # "Could not connect" and "connected but slow" are different facts
//!
//! `dev stop` deletes the lockfile only when it can **positively disprove** a live
//! server: nothing listening, or a server that answers as a different pid/root. A
//! connection that is accepted and then times out is a live server that is busy, and
//! deleting its lockfile strands it — it holds its port, `dev stop` can never reach it
//! again, and a later `--background` starts a second server beside it. So:
//! `TcpStream::connect` failing is `Gone` (safe to delete); a read timeout after a
//! successful connect is `Busy` (lockfile untouched, exit `1`, "try again").
//! [`fetch_status`] carries that distinction as [`Answer`], and [`stop_server`] acts on
//! it. The same rule governs `--background` when it finds an **already running** server:
//! a verified server that is busy is not stale, so a second background start refuses
//! instead of spawning a rival.
//!
//! That rule is *not* applied to the child `--background` has just spawned. A fresh
//! server is in its initial build and answers no status for as long as that build takes,
//! which is the normal case for a real project, so a `Busy` probe during startup keeps
//! polling until [`STARTUP_TIMEOUT`]. A start that gives up for any reason — timeout, an
//! exited child — shuts the child down and removes the lockfile before returning
//! non-zero: an abandoned start that left a server behind would hold a port no
//! `dev stop` could reach.
//!
//! # The control channel does not share a thread with the build
//!
//! One thread per connection ([`Server::serve`]) is what keeps `/_runai/status` and
//! `/_runai/shutdown` answerable while a multi-second build is running. The state they
//! read lives behind their own mutex ([`Shared`]), and they never take the build lock to
//! answer — `status` takes `Shared` for one `format!`, and `shutdown` tries the build
//! lock and answers `503 {"busy":true}` rather than waiting for it. That makes the busy
//! case above rare: it needs a connection that is accepted before a handler thread
//! exists, not merely a slow build.
//!
//! # `--serve-only` is gated on an environment variable
//!
//! It writes the lockfile unconditionally, so a human running it in a project that
//! already has a background server would replace that server's `{pid,port}` with its
//! own and strand it. [`serve`] therefore refuses unless `RUNAI_DEV_SERVE_ONLY` is set,
//! and only the `--background` parent sets it. The alternative — "refuse when a live
//! lockfile verifies" — loses a race against the parent's own startup window, in which
//! the lockfile is written before the first build finishes and a stray invocation would
//! find it not-yet-answering and proceed. The env var has no such window.
//!
//! # A rebuild is triggered by any change, including a deletion
//!
//! [`changed_since`] compares every file's mtime **and every directory's own mtime**,
//! recursively. A creation, deletion, or rename under a directory changes only that
//! directory's mtime and no file's, so a walk that reads only files cannot see
//! `rm src/routes/gone.page.tsx` — that was the defect where a deleted page kept being
//! served from the previous `dist/` forever.
//!
//! A mtime *ahead* of the clock is not treated as "in the future" for the purpose of
//! skipping it, because the file was still touched after the last build. What stops it
//! rebuilding forever is the build marker: when a build starts, `built_at` becomes
//! `max(start, newest source mtime at that instant)`, so a file whose mtime is an hour
//! ahead is covered by the marker that was derived from it ([`Server::rebuild_locked`]).
//!
//! The marker is taken **before** `build::run`, never after, and that is load-bearing: an
//! edit made while the build is running must stay newer than the marker, so the next
//! request rebuilds instead of serving the tree that missed it. Measuring the newest
//! mtime after the build covered the edit permanently — the server served the pre-edit
//! tree until the next unrelated edit.
//!
//! # The server is a hand-written HTTP/1.1 reader on purpose
//!
//! The runtime budget is 6144 bytes gzipped and the binary takes no HTTP dependency.
//! It binds `127.0.0.1` only — never `0.0.0.0` — reads one request, answers, and
//! closes. Each accepted connection gets its own thread; see "The control channel does
//! not share a thread with the build" above for why that is required, not a convenience.
//! Every response carries `Cache-Control: no-store`: a browser reusing a pre-edit page
//! would defeat the whole point of edit-then-fetch.
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
use std::sync::{Arc, Mutex, TryLockError};
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

/// The variable a `--background` parent sets for its child, and the gate [`serve`] checks.
/// It is what keeps a stray `--serve-only` from overwriting a live server's lockfile.
const SERVE_ONLY_ENV: &str = "RUNAI_DEV_SERVE_ONLY";

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
    let server = Arc::new(Server::new(&root, port));
    match TcpListener::bind(("127.0.0.1", port)) {
        Ok(listener) => {
            server.shared().port = listener
                .local_addr()
                .map(|address| address.port())
                .unwrap_or(port);
            {
                let _guard = server
                    .build
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner());
                server.rebuild_locked("the initial build");
            }
            server.write_lock();
            println!("{}", server.status_json());
            server.serve(listener);
            0
        }
        Err(error) => {
            eprintln!("error: cannot listen on 127.0.0.1:{port}: {error}");
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

/// What `/_runai/status` answers, and what a rebuild records.
///
/// Every field here is read by control requests, so it lives behind its own mutex that a
/// build never holds for longer than the two assignments around it. That is the whole
/// point: a rebuild must not be able to block `/_runai/status`, because `dev stop` reads
/// a blocked status as "no server" and would then delete the lockfile of a live one.
struct Shared {
    /// The app root, absolute and canonical: the same value `/_runai/status` reports
    /// and the lockfile carries, so a reused lockfile can be verified against it.
    root: PathBuf,
    port: u16,
    /// When the last rebuild started, clamped forward to the newest mtime that existed at
    /// that instant. A source newer than this is a reason to build again.
    ///
    /// The clamp is what stops a *future* mtime (`touch -d tomorrow`, or clock skew on
    /// an NFS mount) from rebuilding on every request: the marker is
    /// `max(start, newest-at-start)`, so a file whose mtime is ahead of the clock is not
    /// newer than the marker that was derived from it.
    ///
    /// It is taken **before** [`crate::build::run`], never after: a file edited while the
    /// build runs must stay newer than the marker, so the next request rebuilds instead of
    /// serving the tree that missed the edit. Measuring after the build covers the edit
    /// permanently and serves the pre-edit tree until some unrelated change.
    built_at: SystemTime,
    /// The error diagnostics of the last failed build; empty when it was clean.
    ///
    /// A failure that is not a located diagnostic — a render that threw, a missing
    /// runtime, an unreadable file — leaves this empty, because [`Diagnostic`] is a
    /// tuple of a code and a span and inventing one would be a lie about a code. Those
    /// reasons are on stderr; see the module docs.
    diagnostics: Vec<Diagnostic>,
    /// Whether a build is in flight, published so `/_runai/status` can say so under its
    /// own mutex without waiting for the build.
    building: bool,
}

/// The live server. Shared with one thread per connection.
struct Server {
    shared: Mutex<Shared>,
    /// Held for the whole of a build. `/_runai/shutdown` refuses while it is held, so a
    /// stop cannot kill the process between assembling and publishing a tree; that is
    /// reported as "busy" and retried by the caller.
    build: Mutex<()>,
}

impl Server {
    fn new(root: &Path, port: u16) -> Server {
        Server {
            shared: Mutex::new(Shared {
                root: root.to_path_buf(),
                port,
                built_at: SystemTime::UNIX_EPOCH,
                diagnostics: Vec::new(),
                building: false,
            }),
            build: Mutex::new(()),
        }
    }

    /// The shared state, ignoring poisoning: a panic in one handler must not stop the
    /// server from answering the next request.
    fn shared(&self) -> std::sync::MutexGuard<'_, Shared> {
        self.shared
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    fn dist(&self) -> PathBuf {
        self.shared().root.join(DIST)
    }

    fn lock_path(&self) -> PathBuf {
        self.shared().root.join(LOCKFILE)
    }

    /// Accept connections and answer each on its own thread.
    ///
    /// A thread per request is what makes the control channel independent of the build:
    /// one thread can be inside a multi-second `crate::build::run` while another answers
    /// `/_runai/status`. The alternative — one serial loop — is the defect this replaced:
    /// a rebuild blocked the status route, and a timed-out status read was taken for a
    /// dead server, so `dev stop` deleted a live server's lockfile.
    fn serve(self: &Arc<Self>, listener: TcpListener) {
        for stream in listener.incoming() {
            let mut stream = match stream {
                Ok(stream) => stream,
                // A connection that failed to be accepted is one client's problem; the
                // server must not exit over it.
                Err(_) => continue,
            };
            let server = Arc::clone(self);
            // The thread owns one connection and exits with it; there is no pool and no
            // queue, so a slow client holds nothing another client needs.
            let _ = std::thread::Builder::new()
                .name("runai-dev-request".to_string())
                .spawn(move || {
                    let _ = stream.set_read_timeout(Some(CLIENT_TIMEOUT));
                    let _ = stream.set_write_timeout(Some(CLIENT_TIMEOUT));
                    if let Some((method, target)) = read_head(&mut stream) {
                        server.handle(&mut stream, &method, &target);
                    }
                });
        }
    }

    /// Answer one request.
    fn handle(&self, stream: &mut TcpStream, method: &str, target: &str) {
        // Control endpoints are matched on the raw target: they are literal ASCII
        // paths this server owns, and comparing before decoding keeps the status
        // endpoint reachable even for a request whose encoding is odd. The path
        // safety rule below never applies to a file lookup that is not attempted.
        let path = target.split('?').next().unwrap_or(target);
        if path == STATUS || path == SHUTDOWN {
            self.control(stream, method, path);
            return;
        }

        if method != "GET" && method != "HEAD" {
            method_not_allowed(stream);
            return;
        }

        let Some(decoded) = decode_path(path) else {
            not_found(stream, method);
            return;
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
            return;
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
    }

    /// Answer `/_runai/status` or `/_runai/shutdown`.
    ///
    /// Neither path touches the build, and neither holds the build lock while it waits:
    /// `status` takes the shared mutex for one format, shutdown tries the build lock and
    /// answers `503` instead of waiting for it.
    fn control(&self, stream: &mut TcpStream, method: &str, path: &str) {
        if path == STATUS {
            if method != "GET" && method != "HEAD" {
                method_not_allowed(stream);
                return;
            }
            let body = self.status_json();
            write_response(
                stream,
                200,
                "application/json",
                body.as_bytes(),
                method == "HEAD",
            );
            return;
        }

        // Shutdown is a POST: a browser prefetch or a stray GET must not stop the
        // server, and `dev stop` is the only intended caller.
        if method != "POST" {
            method_not_allowed(stream);
            return;
        }

        // A build is publishing. Exiting now would kill the process between assembling
        // and renaming a tree; answering "busy" leaves the lockfile in place and lets
        // the caller retry, which is exactly what `dev stop` reports.
        //
        // The guard is held until `process::exit` below: releasing it first would let a
        // page request begin a publish in the window between the answer and the exit.
        let Some(_guard) = lock_unless_building(&self.build) else {
            write_response(stream, 503, "application/json", br#"{"busy":true}"#, false);
            return;
        };

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
        // `_guard` is still held: releasing it before the exit would let a page request
        // begin a publish in the window between the answer and `process::exit`. (A panic
        // unwinding past this point would poison the lock, which `lock_unless_building`
        // treats as finished — never as busy.)
        std::process::exit(0);
    }

    /// The status payload. Field order is the contract documented in the brief;
    /// `built_at` and `building` are appended so a caller can tell whether a request
    /// rebuilt and whether one is in flight.
    fn status_json(&self) -> String {
        let shared = self.shared();
        format!(
            "{{\"running\":true,\"pid\":{},\"port\":{},\"root\":{},\"built_at\":{},\"building\":{},\"diagnostics\":{}}}",
            std::process::id(),
            shared.port,
            json_string(&shared.root.to_string_lossy()),
            epoch_millis(shared.built_at),
            shared.building,
            Diagnostic::render_all_json(&shared.diagnostics),
        )
    }

    /// Write `.runai/dev.json` as `{pid,port,root}`.
    fn write_lock(&self) {
        let shared = self.shared();
        let path = shared.root.join(LOCKFILE);
        if let Some(parent) = path.parent()
            && std::fs::create_dir_all(parent).is_err()
        {
            return;
        }
        let body = format!(
            "{{\"pid\":{},\"port\":{},\"root\":{}}}\n",
            std::process::id(),
            shared.port,
            json_string(&shared.root.to_string_lossy()),
        );
        // Best effort: a read-only checkout still serves, it just cannot be stopped by
        // lockfile and has to be killed. The caller's own process is the authority.
        let _ = std::fs::write(&path, body);
    }

    /// Remove the lockfile if it still names this pid and port.
    fn remove_own_lock(&self) {
        let path = self.lock_path();
        let port = self.shared().port;
        let Ok(text) = std::fs::read_to_string(&path) else {
            return;
        };
        let pid = field_number(&text, "pid");
        let lock_port = field_number(&text, "port");
        if pid == Some(u64::from(std::process::id())) && lock_port == Some(u64::from(port)) {
            let _ = std::fs::remove_file(&path);
        }
    }

    /// Whether a source file is newer than the last build.
    fn stale(&self) -> bool {
        let built_at = self.shared().built_at;
        changed_since(&self.shared().root, built_at)
    }

    /// Rebuild when anything under `src/` or `runai.json` is newer than the last build.
    ///
    /// The build lock is taken *before* the check, so a page request that arrives while
    /// a build is publishing waits for it and then serves the published tree instead of
    /// racing the rename. It is also what makes two simultaneous page requests run one
    /// build, not two.
    fn rebuild_if_stale(&self) {
        let _guard = self
            .build
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if self.stale() {
            self.rebuild_locked("a page request");
        }
    }

    /// Build into `dist/`, recording the outcome. The caller holds [`Server::build`].
    ///
    /// The build itself is [`crate::build::run`] — this never reimplements it, so the
    /// served tree is exactly what `runai build` produces. A failure is recorded, not
    /// returned: the previous `dist/` is still there and must keep being served.
    fn rebuild_locked(&self, reason: &str) {
        // The marker is published *before* the build, clamped to the newest mtime that
        // exists **now**. Both halves matter: taking it before the build means a file
        // edited while the build runs stays newer than it, so the next request rebuilds;
        // clamping it to the newest mtime that existed at this instant means a future
        // mtime is covered by the marker derived from it and builds exactly once.
        //
        // Never measure `newest` after the build: an edit made inside the build window
        // would then have `mtime < built_at` and be invisible forever.
        let started = SystemTime::now();
        let newest_at_start = newest_modified(&self.shared().root);
        {
            let mut shared = self.shared();
            shared.built_at = started.max(newest_at_start);
            shared.building = true;
        }

        let code = crate::build::run(&["--out".to_string(), DIST.to_string()]);

        let diagnostics = if code == 0 {
            Vec::new()
        } else {
            crate::build::diagnostics()
        };
        {
            let mut shared = self.shared();
            shared.diagnostics = diagnostics;
            // Cleared on both paths: a failed build is finished, not in flight.
            shared.building = false;
        }

        if code == 0 {
            return;
        }
        let count = self.shared().diagnostics.len();
        if count == 0 {
            eprintln!("runai dev: the build failed during {reason}; serving the last good `dist/`");
        } else {
            eprintln!(
                "runai dev: the build failed during {reason} with {count} diagnostic(s); serving the last good `dist/`"
            );
        }
    }
}

/// The child half of `--background`: build, then serve until shutdown.
///
/// It refuses to run unless the parent set [`SERVE_ONLY_ENV`]: `--serve-only` writes the
/// lockfile unconditionally, so a human typing it in a project that already has a
/// background server would replace that server's `{pid,port}` with its own and strand it.
/// The env var is the gate (chosen over "refuse when a live lockfile verifies", which is
/// racy against the parent's own startup window and would need that same env var anyway).
fn serve(root: &Path, port: u16) -> i32 {
    if std::env::var_os(SERVE_ONLY_ENV).is_none() {
        eprintln!(
            "error: `{SERVE_ONLY}` is internal to `dev --background`; run `runai dev` or `runai dev --background`"
        );
        return 2;
    }

    let server = Arc::new(Server::new(root, port));
    let listener = match TcpListener::bind(("127.0.0.1", port)) {
        Ok(listener) => listener,
        Err(error) => {
            // The parent is polling for the lockfile and will time out with its own
            // message; this one is for a human who bypassed the gate deliberately.
            eprintln!("error: cannot listen on 127.0.0.1:{port}: {error}");
            return 1;
        }
    };
    // The real port, which differs from the argument when it was `0`.
    server.shared().port = listener
        .local_addr()
        .map(|address| address.port())
        .unwrap_or(port);

    // The lockfile is written before the build so the parent learns the port quickly;
    // the parent re-fetches status with a timeout, so it does not observe a half-built
    // server as ready.
    server.write_lock();
    {
        let _guard = server
            .build
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        server.rebuild_locked("startup");
    }
    server.serve(listener);
    0
}

/// `--background`: reuse a verified running server, or spawn one and wait for it.
fn start_background(root: &Path, port: u16) -> i32 {
    match verified_server(root) {
        Verified::Running(body) => {
            println!("{body}");
            return 0;
        }
        // A live server we could not get an answer from is busy, not stale. Removing its
        // lockfile here would start a second server beside it and strand both.
        Verified::Busy => return report_busy(),
        Verified::Stale => {}
    }

    // Anything in the lockfile is now known stale: it is unreadable, its port refused
    // the connection, or it answered as a different pid or root. Removing it before the
    // spawn means a crashed child that never got to write cannot leave the parent
    // polling yesterday's file.
    let lock = root.join(LOCKFILE);
    let _ = std::fs::remove_file(&lock);

    let Ok(executable) = std::env::current_exe() else {
        eprintln!("error: cannot find the runai executable to start the server");
        return 1;
    };

    let child = Command::new(executable)
        .args(["dev", SERVE_ONLY, "--port", &port.to_string()])
        .current_dir(root)
        // The gate `serve` checks: only a `--background` parent sets this, so a human
        // typing the hidden flag gets the refusal above instead of a clobbered lockfile.
        .env(SERVE_ONLY_ENV, "1")
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
    // The port the child bound, learned from its lockfile. `None` until it writes one.
    let mut probed = None;
    while SystemTime::now() < deadline {
        if let Ok(Some(status)) = child.try_wait() {
            exited = Some(status);
            break;
        }
        match read_lock(root) {
            Some(lock) if lock.pid == u64::from(child.id()) => {
                probed = Some(lock.port);
                match fetch_status(lock.port) {
                    Answer::Status(status) if status.pid == lock.pid => {
                        println!("{}", status.body);
                        return 0;
                    }
                    // A child that is still in its initial build accepts the connection
                    // and has no status yet: healthy and slow, not failed. Poll until
                    // STARTUP_TIMEOUT rather than calling this "busy" after one probe.
                    Answer::Busy => {}
                    Answer::Status(_) | Answer::Gone => {}
                }
            }
            // The child has not written its lockfile yet; keep waiting.
            _ => {}
        }
        std::thread::sleep(POLL);
    }

    match exited {
        Some(status) => {
            eprintln!("error: the dev server exited during startup ({status})");
            // It is already gone; this only clears a lockfile still naming it.
            remove_lock_naming(root, child.id());
            1
        }
        None => {
            eprintln!(
                "error: the dev server did not answer `/_runai/status` within {}s",
                STARTUP_TIMEOUT.as_secs()
            );
            // The child is alive: whichever reason made this fail, it must not outlive
            // the command that started it. Every abandoned start leaves a server holding
            // a port that no `dev stop` can reach, because the lockfile is about to go.
            stop_spawned_child(&mut child, probed, root);
            1
        }
    }
}

/// Shut down a child this process spawned and could not verify, so a failed
/// `--background` leaves no process and no lockfile behind.
///
/// Preferred: `POST /_runai/shutdown` on the port the child reported, which is the same
/// path `dev stop` uses and lets the child remove its own lockfile. Fallback: kill the pid
/// just spawned, which is this process's own child and nobody else's. Either way the
/// lockfile is then removed, because it names a server that no longer exists.
fn stop_spawned_child(child: &mut std::process::Child, port: Option<u16>, root: &Path) {
    let stopped = match port {
        // The same path `dev stop` uses: the child answers, removes its own lockfile and
        // exits. A `Busy` answer means a build is publishing and the child will not stop,
        // so it falls through to the kill below rather than being waited for.
        Some(port) => matches!(post_shutdown(port), Shutdown::Stopped),
        None => false,
    };

    let mut exited = false;
    if stopped {
        let deadline = SystemTime::now() + STARTUP_TIMEOUT;
        while SystemTime::now() < deadline {
            if let Ok(Some(_)) = child.try_wait() {
                exited = true;
                break;
            }
            std::thread::sleep(POLL);
        }
    }
    if !exited {
        // Its own child, by the pid `spawn` returned: never a pattern, never another
        // process's server.
        let _ = child.kill();
        let _ = child.wait();
    }

    remove_lock_naming(root, child.id());
}

/// Remove `.runai/dev.json` when it names `pid`, so a failed start leaves no lockfile
/// pointing at a server that no longer exists. A successor that started in the meantime
/// owns its own lockfile and is left alone.
fn remove_lock_naming(root: &Path, pid: u32) {
    if let Some(found) = read_lock(root)
        && found.pid == u64::from(pid)
    {
        let _ = std::fs::remove_file(root.join(LOCKFILE));
    }
}

/// `dev stop`: verify the lockfile against a live server, ask it to stop, clean up.
///
/// `0` when stopped, already stopped, or when the lockfile was neither ours nor live.
/// `1` when the server is **busy**: it accepted the connection but did not answer, or
/// answered `503` because a build is publishing. In that case the lockfile is left
/// exactly as it was — a lockfile this process could not positively disprove names a
/// live server, and deleting it strands that server where no `dev stop` can reach it
/// again. The unverified-but-dead case still deletes: a refused connection is proof.
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

    match fetch_status(found.port) {
        Answer::Status(status) => {
            if status.pid != found.pid {
                // The port answers, but as a different process: exactly the case where a
                // pid signal would kill an innocent program. Drop the lockfile and leave
                // that server alone.
                let _ = std::fs::remove_file(&lock);
                return 0;
            }
        }
        // The connection was accepted and then the read timed out: the server is alive
        // and busy. Not proof of death, so the lockfile stays.
        Answer::Busy => return report_busy(),
        // Refused connection: nothing is listening, so the lockfile is genuinely stale.
        Answer::Gone => {
            let _ = std::fs::remove_file(&lock);
            return 0;
        }
    }

    match post_shutdown(found.port) {
        Shutdown::Stopped => {}
        // The server answered `503 {"busy":true}`: a rebuild holds the build lock.
        Shutdown::Busy => return report_busy(),
        // Accepted and then failed to answer, or gone: treat as gone rather than signal.
        Shutdown::Unreachable => {
            let _ = std::fs::remove_file(&lock);
            return 0;
        }
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

/// What `dev stop` says when the server is alive but a build holds it: the lockfile is
/// intact, so the caller can retry, and the exit is non-zero so a script notices.
fn report_busy() -> i32 {
    eprintln!("error: the dev server is busy rebuilding; try `runai dev stop` again in a moment");
    1
}

/// What a lockfile's port answered.
enum Verified {
    /// A live server, verified for `root`: its status body.
    Running(String),
    /// A server that accepted the connection and then did not answer in time.
    Busy,
    /// Nothing verified: no lockfile, a dead server, or a different pid or root.
    Stale,
}

/// The status body of a server that has just been verified for `root`, if there is one.
///
/// All three facts must agree: the lockfile's `pid` is the one `/_runai/status` reports,
/// the served `root` is the one the lockfile names, and that root **is this working
/// directory**. The last comparison is not redundant: without it a `.runai/dev.json`
/// copied from another checkout verifies against itself — its own `root` field is the
/// other project — and `dev stop` there would shut down a server it has no business
/// touching.
fn verified_server(root: &Path) -> Verified {
    let Some(found) = read_lock(root) else {
        return Verified::Stale;
    };
    if !same_root(&found.root, &root.to_string_lossy()) {
        return Verified::Stale;
    }
    match fetch_status(found.port) {
        Answer::Status(status)
            if status.pid == found.pid && same_root(&status.root, &found.root) =>
        {
            Verified::Running(status.body)
        }
        Answer::Status(_) => Verified::Stale,
        Answer::Busy => Verified::Busy,
        Answer::Gone => Verified::Stale,
    }
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

/// What trying to read `/_runai/status` produced.
///
/// The distinction between [`Answer::Busy`] and [`Answer::Gone`] is the whole of C1:
/// `TcpStream::connect` succeeding proves something is listening, so a read timeout after
/// it means a live-but-slow server. That must never be mistaken for a dead one, because
/// `dev stop` deletes the lockfile on `Gone`.
enum Answer {
    /// A parsed status body.
    Status(Status),
    /// The connection was accepted and then nothing arrived in time: alive, busy.
    Busy,
    /// Nothing is listening, or what answered was not this server.
    Gone,
}

/// `GET /_runai/status` on `port`.
///
/// The timeout is short because a spawn that bound its port but has not finished the
/// initial build is not ready: the caller polls, and an early answer is better than a
/// slow one. A timeout is reported as `Busy`, never as `Gone`, whenever the connect
/// itself succeeded.
fn fetch_status(port: u16) -> Answer {
    let mut stream = match TcpStream::connect(("127.0.0.1", port)) {
        Ok(stream) => stream,
        Err(_) => return Answer::Gone,
    };
    match exchange(&mut stream, "GET", STATUS, Duration::from_millis(500)) {
        Outcome::Answered { code, body } if (200..300).contains(&code) => {
            let pid = field_number(&body, "pid");
            let root = field_string(&body, "root");
            match (pid, root) {
                (Some(pid), Some(root)) => Answer::Status(Status { body, pid, root }),
                // The socket is ours but the answer is not a status: not our server.
                _ => Answer::Gone,
            }
        }
        // A non-2xx answer is still an answer from something alive on the port, but it
        // proves nothing about which process it is, so it is not ours to stop.
        Outcome::Answered { .. } => Answer::Gone,
        // The connection was accepted and then went quiet: alive and busy.
        Outcome::TimedOut => Answer::Busy,
        // Accepted then closed with no complete response, or the write failed after a
        // successful connect: indistinguishable from gone for our purposes, and the
        // caller only deletes on this, so it must be conservative. A closed connection
        // is proof the peer closed it, so it is `Gone`; a failed write is not.
        Outcome::Closed => Answer::Gone,
        Outcome::WriteFailed => Answer::Busy,
    }
}

/// What `POST /_runai/shutdown` produced.
enum Shutdown {
    /// `200 {"stopping":true}`: the server is exiting.
    Stopped,
    /// `503 {"busy":true}`: a rebuild holds the server; it is still alive.
    Busy,
    /// Nothing listening, or a complete non-200 answer: treat as gone.
    Unreachable,
}

/// `POST /_runai/shutdown` on `port`.
fn post_shutdown(port: u16) -> Shutdown {
    let mut stream = match TcpStream::connect(("127.0.0.1", port)) {
        Ok(stream) => stream,
        Err(_) => return Shutdown::Unreachable,
    };
    match exchange(&mut stream, "POST", SHUTDOWN, Duration::from_secs(1)) {
        Outcome::Answered { code: 200, .. } => Shutdown::Stopped,
        Outcome::Answered { code: 503, .. } => Shutdown::Busy,
        Outcome::Answered { .. } => Shutdown::Unreachable,
        Outcome::TimedOut | Outcome::WriteFailed => Shutdown::Busy,
        Outcome::Closed => Shutdown::Unreachable,
    }
}

/// How one request ended.
enum Outcome {
    /// A complete response head and its body.
    Answered { code: u16, body: String },
    /// The read timed out after the connection was established.
    TimedOut,
    /// The peer closed without a complete response.
    Closed,
    /// The request could not be sent after the connection was established.
    WriteFailed,
}

/// Send one request on an established connection and read the response.
fn exchange(stream: &mut TcpStream, method: &str, path: &str, timeout: Duration) -> Outcome {
    if stream.set_read_timeout(Some(timeout)).is_err()
        || stream.set_write_timeout(Some(timeout)).is_err()
    {
        return Outcome::WriteFailed;
    }
    if write!(
        stream,
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\nContent-Length: 0\r\n\r\n"
    )
    .is_err()
        || stream.flush().is_err()
    {
        return Outcome::WriteFailed;
    }

    let mut raw = Vec::new();
    let mut chunk = [0u8; 1024];
    let mut timed_out = false;
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(read) => {
                raw.extend_from_slice(&chunk[..read]);
                if raw.len() > MAX_HEAD {
                    break;
                }
            }
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock
                    || error.kind() == std::io::ErrorKind::TimedOut =>
            {
                timed_out = true;
                break;
            }
            // Any other error mid-response: the peer went away.
            Err(_) => break,
        }
    }

    let text = String::from_utf8_lossy(&raw).into_owned();
    let Some((head, body)) = text.split_once("\r\n\r\n") else {
        // No complete head. A timeout means alive-but-slow; a clean close means the peer
        // hung up, which for a live server is its shutdown answering nothing.
        return if timed_out {
            Outcome::TimedOut
        } else {
            Outcome::Closed
        };
    };
    let Some(code) = head.get(9..12).and_then(|code| code.parse().ok()) else {
        return Outcome::Closed;
    };
    Outcome::Answered {
        code,
        body: body.to_string(),
    }
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

/// Try to take the build lock without waiting, for `/_runai/shutdown`.
///
/// `None` means a build is publishing: the caller answers `503` and leaves the restart to
/// `dev stop`. A **poisoned** lock is not busy — the work it guarded is finished or its
/// thread is gone — so it proceeds. Returning `None` on poison made a panic in one build
/// render the server permanently unstoppable: `/_runai/shutdown` answered `503` forever,
/// `dev stop` exited `1` without deleting the lockfile, and the process needed `kill`.
/// [`Server::shared`] and [`Server::rebuild_locked`] already recover from poison
/// deliberately; this is the same decision at the one remaining `try_lock`.
fn lock_unless_building(build: &Mutex<()>) -> Option<std::sync::MutexGuard<'_, ()>> {
    match build.try_lock() {
        Ok(guard) => Some(guard),
        // A build is publishing: that is the one answer `dev stop` must not act on.
        Err(TryLockError::WouldBlock) => None,
        // The guard's work is over; taking the lock is the honest reading.
        Err(TryLockError::Poisoned(poison)) => Some(poison.into_inner()),
    }
}

/// The HTTP reason phrase for a status code this server emits.
fn reason_phrase(code: u16) -> &'static str {
    match code {
        200 => "OK",
        404 => "Not Found",
        405 => "Method Not Allowed",
        503 => "Service Unavailable",
        _ => "Error",
    }
}

/// Write one response and close. Headers are lowercase-free and ASCII only.
fn write_response(
    stream: &mut TcpStream,
    code: u16,
    content_type: &str,
    body: &[u8],
    head_only: bool,
) {
    let reason = reason_phrase(code);
    let head = format!(
        "HTTP/1.1 {code} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    if !head_only {
        let _ = stream.write_all(body);
    }
    let _ = stream.flush();
}

/// The 405 body, shared by the page route and both control endpoints.
///
/// All three sites answer the same bytes, and a single helper is what keeps them
/// the same: a reader reasoning about the control channel only has to read this one
/// place to know what a refused method returns. `head_only` is `false` at every
/// site, including for a `HEAD` request — a `HEAD` never reaches here (`GET` and
/// `HEAD` are the allowed pair on the page and status routes, and the only refused
/// method on shutdown is one that is not `POST`), so there is no body to suppress.
fn method_not_allowed(stream: &mut TcpStream) {
    write_response(
        stream,
        405,
        "text/plain; charset=utf-8",
        b"method not allowed\n",
        false,
    );
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
        "wasm" => "application/wasm",
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
///
/// The directory's **own** mtime is compared, not only its entries'. A creation, a
/// deletion, or a rename under a directory updates that directory's mtime and touches
/// nothing else — no file mtime changes — so a walk that reads only files cannot see
/// `rm src/routes/gone.page.tsx` at all. That was the deletion defect: the deleted page
/// kept being served from the previous `dist/` forever.
fn directory_changed(dir: &Path, since: SystemTime) -> bool {
    let metadata = match std::fs::metadata(dir) {
        Ok(metadata) => metadata,
        // No `src/` at all: the build will report that, and it is not a change.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return false,
        Err(_) => return true,
    };
    // Reading the directory below updates no mtime, so this is the state the walk saw.
    if is_newer(metadata.modified(), since) {
        return true;
    }

    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return true,
    };
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => return true,
        };
        let path = entry.path();
        match entry.file_type() {
            // Recurse: a nested directory's own mtime covers changes made inside it.
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
        Ok(modified) => is_newer(Ok(modified), since),
        Err(error) => error.kind() != std::io::ErrorKind::NotFound,
    }
}

/// The comparison [`newer`] and [`directory_changed`] share, tolerating the clock going
/// backwards between the timestamp's source and this process.
///
/// A *future* mtime is deliberately reported as newer here — the file was still touched
/// after the last build from the build's point of view, and skipping it would serve a
/// stale page. What stops that from rebuilding forever is not this comparison but
/// [`Server::rebuild_locked`], which moves its marker to `max(start, newest mtime at
/// start)` **before** each build: a mtime in the future is then covered by the marker
/// instead of staying ahead of it permanently. Stamping before the build, not after, is
/// what keeps a file edited *during* the build newer than the marker.
fn is_newer(modified: std::io::Result<SystemTime>, since: SystemTime) -> bool {
    match modified {
        Ok(modified) => modified > since,
        Err(_) => true,
    }
}

/// The newest mtime under `src/` or on `runai.json`, or [`SystemTime::UNIX_EPOCH`] when
/// there is neither.
///
/// This is the clamp that makes a future mtime safe: the build marker becomes
/// `max(now, this)`, so a file with a mtime an hour ahead does not look newer than the
/// build that just included it.
fn newest_modified(root: &Path) -> SystemTime {
    let mut newest = SystemTime::UNIX_EPOCH;
    newest = newest.max(modified_or_epoch(&root.join(CONFIG)));
    newest_of(&root.join(SRC), &mut newest);
    newest
}

/// [`newest_modified`] for a directory tree, folding into `newest`.
///
/// Each directory's **own** mtime is folded alongside its files, mirroring
/// [`directory_changed`]. Without it the marker can never cover a directory touched in the
/// future, and the comparison reads that mtime on every request: a future-dated `src/routes`
/// rebuilt once per page request, forever.
fn newest_of(dir: &Path, newest: &mut SystemTime) {
    *newest = (*newest).max(modified_or_epoch(dir));
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        match entry.file_type() {
            Ok(kind) if kind.is_dir() => newest_of(&path, newest),
            Ok(_) => *newest = (*newest).max(modified_or_epoch(&path)),
            Err(_) => {}
        }
    }
}

/// A path's mtime, or the epoch when it cannot be read.
fn modified_or_epoch(path: &Path) -> SystemTime {
    std::fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .unwrap_or(SystemTime::UNIX_EPOCH)
}

/// A `SystemTime` as epoch milliseconds, for the `built_at` field of the status JSON.
fn epoch_millis(time: SystemTime) -> u128 {
    time.duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
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

    /// C1(a), the half the integration test cannot isolate: a server that **accepts the
    /// connection** and then does not answer is alive and busy, not gone. This is a unit
    /// test because the integration tests exercise a threaded server, where the status
    /// route answers even mid-build; the distinction still has to be right, because it is
    /// what protects the lockfile if a build ever does block the control route.
    #[test]
    fn a_socket_that_accepts_and_never_answers_is_busy_not_gone() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        // Hold the connection open without writing: `accept` returns, `read` blocks.
        let held = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            std::thread::sleep(Duration::from_millis(1200));
            drop(stream);
        });

        match fetch_status(port) {
            Answer::Busy => {}
            Answer::Gone => panic!("an accepted connection that timed out is BUSY, not gone"),
            Answer::Status(_) => panic!("nothing was written, so no status can exist"),
        }
        held.join().unwrap();
    }

    /// The 405 response is written from three sites (the page route, `/_runai/status`,
    /// and `/_runai/shutdown`) through one helper. The bytes are the contract: an agent
    /// reading `runai dev`'s output, and any client that retries on a status code, sees
    /// exactly these. Pinning them here is what makes the shared helper a refactor
    /// rather than a rewrite — without it, "the three sites are identical" is only a
    /// claim about the source text.
    ///
    /// Driven through the real `handle`/`control` paths over a loopback pair, not by
    /// calling the helper directly: a test of the helper would pass even if a call site
    /// stopped using it.
    #[test]
    fn the_405_response_is_byte_identical_at_every_site() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join(DIST)).unwrap();
        let server = Arc::new(Server::new(tmp.path(), 0));

        for (method, target) in [
            // The page route: anything but GET/HEAD.
            ("POST", "/index.html"),
            ("PUT", "/"),
            ("DELETE", "/assets/index.js"),
            // `/_runai/status`: anything but GET/HEAD.
            ("POST", STATUS),
            // `/_runai/shutdown`: anything but POST.
            ("GET", SHUTDOWN),
            ("HEAD", SHUTDOWN),
        ] {
            let response = answer_once(&server, method, target);
            assert_eq!(
                response,
                "HTTP/1.1 405 Method Not Allowed\r\n\
                 Content-Type: text/plain; charset=utf-8\r\n\
                 Content-Length: 19\r\n\
                 Cache-Control: no-store\r\n\
                 Connection: close\r\n\
                 \r\n\
                 method not allowed\n",
                "{method} {target} must answer the one 405 body"
            );
        }
    }

    /// Run one request through [`Server::handle`] and return everything written.
    ///
    /// A real socket pair, because `handle` takes a `TcpStream`: this exercises the
    /// writer and the header assembly, which a buffer-shaped refactor would not.
    fn answer_once(server: &Arc<Server>, method: &str, target: &str) -> String {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let client = std::thread::spawn(move || {
            let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
            let mut body = String::new();
            stream.read_to_string(&mut body).unwrap();
            body
        });
        let (mut stream, _) = listener.accept().unwrap();
        server.handle(&mut stream, method, target);
        drop(stream);
        client.join().unwrap()
    }

    /// The other half: nothing listening is gone, which is the only case `dev stop` may
    /// delete a lockfile on.
    #[test]
    fn a_closed_port_is_gone() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        assert!(matches!(fetch_status(port), Answer::Gone));
    }

    /// C2: a deletion changes only the parent directory's mtime, so the walk must compare
    /// the directory itself. Written against the walk directly so the failure names the
    /// mtime comparison rather than a served page.
    #[test]
    fn deleting_a_file_is_seen_as_a_change() {
        let tmp = tempfile::tempdir().unwrap();
        let nested = tmp.path().join(SRC).join("routes");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("gone.page.tsx"), "x").unwrap();
        // Mark the moment after the file exists: nothing is newer than this.
        let after = SystemTime::now();
        std::thread::sleep(Duration::from_millis(20));
        std::fs::remove_file(nested.join("gone.page.tsx")).unwrap();
        assert!(
            changed_since(tmp.path(), after),
            "a deletion must count as a change (directory mtime), or the deleted page is served forever"
        );
    }

    /// I3: a future mtime is clamped by the build marker, so the second comparison says
    /// "not newer". Mirrors what `rebuild_locked` does without needing a real build.
    #[test]
    fn a_future_mtime_is_covered_by_a_clamped_marker() {
        let tmp = tempfile::tempdir().unwrap();
        let route = tmp.path().join(SRC).join("index.page.tsx");
        std::fs::create_dir_all(route.parent().unwrap()).unwrap();
        std::fs::write(&route, "x").unwrap();
        let future = SystemTime::now() + Duration::from_secs(3600);
        std::fs::File::options()
            .write(true)
            .open(&route)
            .unwrap()
            .set_modified(future)
            .unwrap();

        let marker = SystemTime::now().max(newest_modified(tmp.path()));
        assert_eq!(marker, future, "the marker must cover the future mtime");
        assert!(
            !changed_since(tmp.path(), marker),
            "a future mtime must not look newer than the marker derived from it"
        );
    }

    /// N-D: a future mtime on a **directory** must be covered too. `changed_since` reads
    /// every directory's own mtime; the marker folded only file mtimes, so a future-dated
    /// directory was permanently newer and every request rebuilt. The two functions must
    /// fold the same set.
    #[test]
    fn a_future_directory_mtime_is_covered_by_a_clamped_marker() {
        let tmp = tempfile::tempdir().unwrap();
        let routes = tmp.path().join(SRC).join("routes");
        std::fs::create_dir_all(&routes).unwrap();
        std::fs::write(routes.join("index.page.tsx"), "x").unwrap();
        let future = SystemTime::now() + Duration::from_secs(3600);
        std::fs::File::open(&routes)
            .unwrap()
            .set_modified(future)
            .unwrap();

        let marker = SystemTime::now().max(newest_modified(tmp.path()));
        assert_eq!(
            marker, future,
            "the marker must cover a future directory mtime, or `changed_since` sees it forever"
        );
        assert!(
            !changed_since(tmp.path(), marker),
            "a future directory mtime must not look newer than the marker derived from it"
        );
    }

    /// NEW-C, as a unit: the marker must be derived from the sources that existed when the
    /// build **started**, so a file written during the build is newer than it. Measuring
    /// the newest mtime after the build covered that edit permanently.
    ///
    /// The write is separated from the marker by the same 1100 ms the integration tests
    /// use in their `settle` helper: on a 1-second-granularity filesystem a shorter gap
    /// truncates the write's mtime to at or before the marker, and the test fails for a
    /// reason that has nothing to do with the code under test.
    #[test]
    fn a_file_written_during_a_build_stays_newer_than_the_marker() {
        let tmp = tempfile::tempdir().unwrap();
        let route = tmp.path().join(SRC).join("index.page.tsx");
        std::fs::create_dir_all(route.parent().unwrap()).unwrap();
        std::fs::write(&route, "before").unwrap();

        // What `rebuild_locked` now does: stamp before the build.
        let started = SystemTime::now();
        let marker = started.max(newest_modified(tmp.path()));

        // The build runs; the user saves the file while it does.
        std::thread::sleep(Duration::from_millis(1100));
        std::fs::write(&route, "after").unwrap();

        assert!(
            changed_since(tmp.path(), marker),
            "an edit made during a build must be newer than the marker taken before it"
        );
    }

    /// NEW-MB: a poisoned build lock must not make the server unstoppable. A panic inside
    /// one build poisons the mutex; `try_lock` then returns `Err(Poisoned)` forever, and
    /// the old `is_err()` test answered `503` to every shutdown — `dev stop` exited `1`,
    /// kept the lockfile, and only `kill` ended the process.
    #[test]
    fn a_poisoned_build_lock_still_allows_shutdown() {
        let build = Mutex::new(());
        let poisoned = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = build.lock().unwrap();
            panic!("a build panicked while holding the lock");
        }));
        assert!(poisoned.is_err(), "the test must actually poison the lock");

        assert!(
            lock_unless_building(&build).is_some(),
            "a poisoned lock means the build is over, so shutdown must proceed"
        );
    }

    /// The busy half, which must not regress: a lock held by a live build is `None`.
    #[test]
    fn a_held_build_lock_reports_busy() {
        let build = Mutex::new(());
        let _held = build.lock().unwrap();
        assert!(
            lock_unless_building(&build).is_none(),
            "a build in flight must answer `503`, not exit under it"
        );
    }

    /// The status line for `503` read `HTTP/1.1 503 Error`, which no client expects.
    #[test]
    fn every_status_code_has_a_real_reason_phrase() {
        assert_eq!(reason_phrase(503), "Service Unavailable");
        assert_eq!(reason_phrase(404), "Not Found");
        assert_eq!(reason_phrase(200), "OK");
    }
}
