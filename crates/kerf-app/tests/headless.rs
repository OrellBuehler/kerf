//! `kerf-app --headless` as a process: it starts with no display, listens on the port it reports,
//! answers MCP over HTTP behind the token, and stops cleanly on SIGTERM; with `--stdio` it answers
//! on stdin / stdout, which carries nothing but JSON-RPC, and stops when stdin closes.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const INITIALIZE: &str = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"test","version":"0"}}}"#;
const INITIALIZED: &str = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;

/// One POST to `/mcp`: the status, the headers (names lower-cased) and the body read until it holds
/// `until` (or the server closes it).
fn exchange(addr: SocketAddr, headers: &[(&str, &str)], body: &str, until: &str) -> (u16, Vec<(String, String)>, String) {
    let mut request = format!(
        "POST /mcp HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\nAccept: application/json, text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    for (name, value) in headers {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    request.push_str("\r\n");
    request.push_str(body);
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(5)).expect("connect");
    stream.set_read_timeout(Some(Duration::from_millis(500))).unwrap();
    stream.write_all(request.as_bytes()).unwrap();
    let mut seen = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut chunk = [0u8; 4096];
    while Instant::now() < deadline {
        let text = String::from_utf8_lossy(&seen);
        if let Some((head, body)) = text.split_once("\r\n\r\n") {
            let body = if head.to_ascii_lowercase().contains("transfer-encoding: chunked") {
                dechunk(body)
            } else {
                body.to_string()
            };
            // A whole line holds it: an answer is not cut where one read of the stream ended.
            if body.split_inclusive('\n').any(|l| l.ends_with('\n') && l.contains(until)) {
                break;
            }
        }
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => seen.extend_from_slice(&chunk[..n]),
            Err(_) => {}
        }
    }
    let text = String::from_utf8_lossy(&seen).into_owned();
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    let mut lines = head.lines();
    let status = lines
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    let headers: Vec<(String, String)> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
        .collect();
    let chunked = headers
        .iter()
        .any(|(k, v)| k == "transfer-encoding" && v.eq_ignore_ascii_case("chunked"));
    let body = if chunked { dechunk(body) } else { body.to_string() };
    (status, headers, body)
}

/// A chunked body's text, the chunk sizes taken out (a chunk may end inside a word).
fn dechunk(mut body: &str) -> String {
    let mut out = String::new();
    while let Some((size, rest)) = body.split_once("\r\n") {
        let Ok(size) = usize::from_str_radix(size.trim(), 16) else {
            break;
        };
        if size == 0 || rest.len() < size {
            out.push_str(&rest[..rest.len().min(size)]);
            break;
        }
        out.push_str(&rest[..size]);
        body = rest[size..].strip_prefix("\r\n").unwrap_or(&rest[size..]);
    }
    out
}

/// Kills the child if the test fails before it is stopped.
struct Running(Child);

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// `kerf-app --headless` started with `args` (and `dir` as its data directory), and its ready line.
fn spawn(dir: &Path, args: &[&str], token: &str) -> (Running, serde_json::Value) {
    let log = std::fs::File::create(dir.join("stderr.log")).unwrap();
    let mut child = Running(
        Command::new(env!("CARGO_BIN_EXE_kerf-app"))
            .args(["--headless", "--mcp-addr", "127.0.0.1:0", "--data-dir"])
            .arg(dir)
            .args(args)
            .env("KERF_MCP_TOKEN", token)
            .env_remove("DISPLAY")
            .env_remove("WAYLAND_DISPLAY")
            .env_remove("KERF_MCP_ADDR")
            .env_remove("KERF_WORKSPACE")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(log)
            .spawn()
            .expect("spawn kerf-app"),
    );
    let stdout = child.0.stdout.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let _ = BufReader::new(stdout).read_line(&mut line);
        let _ = tx.send(line);
    });
    let stderr = || std::fs::read_to_string(dir.join("stderr.log")).unwrap_or_default();
    let line = rx
        .recv_timeout(Duration::from_secs(120))
        .unwrap_or_else(|_| panic!("no ready line; stderr:\n{}", stderr()));
    let ready = serde_json::from_str(&line).unwrap_or_else(|e| panic!("ready line {line:?}: {e}; stderr:\n{}", stderr()));
    (child, ready)
}

/// An initialized MCP session on `addr`: its `Authorization` and `Mcp-Session-Id`, and the
/// `initialize` answer.
fn session(addr: SocketAddr, token: &str) -> ([(String, String); 2], String) {
    let auth = format!("Bearer {token}");
    let (status, headers, body) = exchange(addr, &[("Authorization", &auth)], INITIALIZE, "\"id\":1");
    assert_eq!(status, 200);
    let session = headers
        .into_iter()
        .find(|(k, _)| k == "mcp-session-id")
        .map(|(_, v)| v)
        .expect("a session id");
    let headers = [("Authorization".to_string(), auth), ("Mcp-Session-Id".to_string(), session)];
    let on_session: Vec<(&str, &str)> = headers.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    assert_eq!(exchange(addr, &on_session, INITIALIZED, "").0, 202);
    (headers, body)
}

/// A `tools/call` on a session: the response body.
fn call(addr: SocketAddr, headers: &[(String, String); 2], id: u32, tool: &str, arguments: serde_json::Value) -> String {
    let body = serde_json::json!({
        "jsonrpc": "2.0", "id": id, "method": "tools/call",
        "params": { "name": tool, "arguments": arguments },
    })
    .to_string();
    let on_session: Vec<(&str, &str)> = headers.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let (status, _, body) = exchange(addr, &on_session, &body, &format!("\"id\":{id}"));
    assert_eq!(status, 200, "{tool}: {body}");
    body
}

/// A fresh directory for one test.
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kerf-headless-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn a_headless_run_serves_mcp_with_no_display_and_stops_on_sigterm() {
    let dir = std::env::temp_dir().join(format!("kerf-headless-it-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let token = "h".repeat(48);
    let (mut child, ready) = spawn(&dir, &[], &token);
    let stderr = || std::fs::read_to_string(dir.join("stderr.log")).unwrap_or_default();
    assert_eq!(ready["kerf"], "listening", "{ready}");
    assert_eq!(ready["token_source"], "env");
    assert_eq!(ready["agent_review"], false, "nobody to review: edits land live");
    assert!(ready["project"].is_null());
    assert!(ready.get("workspace").is_none(), "no workspace, no key");
    let addr: SocketAddr = ready["addr"].as_str().unwrap().parse().unwrap();
    assert_ne!(addr.port(), 0);
    assert_eq!(ready["url"], format!("http://{addr}/mcp"));
    assert!(!stderr().contains(&token), "the token is not printed unless asked");

    let (status, _, _) = exchange(addr, &[], INITIALIZE, "\"id\":1");
    assert_eq!(status, 401, "no token, no entry");
    let (on_session, _) = session(addr, &token);
    let call = |id: u32, tool: &str, arguments: serde_json::Value| {
        let body = call(addr, &on_session, id, tool, arguments);
        assert!(!body.contains("\"isError\":true"), "{tool}: {body}");
        body
    };
    let info = call(2, "project_info", serde_json::json!({}));
    assert!(info.contains(r#"\"in_memory\":true"#), "{info}");
    call(3, "add_marker", serde_json::json!({ "time": 1.5, "name": "headless-mark" }));
    let state = call(4, "get_timeline_state", serde_json::json!({}));
    assert!(state.contains("headless-mark"), "{state}");
    assert!(
        std::fs::read_to_string(dir.join("settings.json")).map_or(true, |s| !s.contains(&token)),
        "the environment's token is never stored"
    );

    #[cfg(unix)]
    {
        let pid = child.0.id().to_string();
        assert!(Command::new("kill").args(["-TERM", &pid]).status().unwrap().success());
        let deadline = Instant::now() + Duration::from_secs(30);
        let status = loop {
            if let Some(status) = child.0.try_wait().unwrap() {
                break status;
            }
            assert!(
                Instant::now() < deadline,
                "still running after SIGTERM; stderr:\n{}",
                stderr()
            );
            std::thread::sleep(Duration::from_millis(100));
        };
        assert_eq!(status.code(), Some(0), "stderr:\n{}", stderr());
        assert!(
            TcpStream::connect_timeout(&addr, Duration::from_millis(300)).is_err(),
            "the port is closed"
        );
        let kept = std::fs::read_dir(dir.join("recovery")).map_or(0, |d| d.count());
        assert_eq!(kept, 1, "the untitled project with a marker is kept for recovery");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

fn headless(args: &[&str], dir: &std::path::Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_kerf-app"));
    command
        .arg("--headless")
        .args(args)
        .arg("--data-dir")
        .arg(dir)
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .env_remove("KERF_MCP_ADDR")
        .env_remove("KERF_MCP_TOKEN");
    command
}

/// A `--stdio` run and its pipe: every stdout line it read, each checked to be JSON-RPC.
struct StdioRun {
    child: Running,
    stdin: Option<std::process::ChildStdin>,
    lines: std::sync::mpsc::Receiver<String>,
    seen: Vec<String>,
    log: std::path::PathBuf,
}

impl StdioRun {
    fn start(args: &[&str], dir: &std::path::Path, log: &str) -> Self {
        let log = dir.join(log);
        let mut child = Running(
            headless(&[&["--stdio"][..], args].concat(), dir)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(std::fs::File::create(&log).unwrap())
                .spawn()
                .expect("spawn kerf-app"),
        );
        let stdout = child.0.stdout.take().unwrap();
        let (tx, lines) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        let stdin = child.0.stdin.take();
        Self {
            child,
            stdin,
            lines,
            seen: Vec::new(),
            log,
        }
    }

    fn stderr(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }

    fn send(&mut self, message: &str) {
        let stdin = self.stdin.as_mut().expect("stdin is open");
        stdin.write_all(message.as_bytes()).unwrap();
        stdin.write_all(b"\n").unwrap();
        stdin.flush().unwrap();
    }

    /// The answer to request `id`, reading past anything else.
    fn answer(&mut self, id: u32) -> serde_json::Value {
        let deadline = Instant::now() + Duration::from_secs(120);
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let line = self
                .lines
                .recv_timeout(left)
                .unwrap_or_else(|_| panic!("no answer to {id}; stderr:\n{}", self.stderr()));
            let message = json_rpc(&line);
            self.seen.push(line);
            if message["id"] == id {
                return message;
            }
        }
    }

    fn initialize(&mut self) -> serde_json::Value {
        self.send(INITIALIZE);
        let init = self.answer(1);
        self.send(INITIALIZED);
        init
    }

    /// `tools/call` of `tool` as request `id`: the result, which must not be an error.
    fn call(&mut self, id: u32, tool: &str, arguments: serde_json::Value) -> serde_json::Value {
        let request = serde_json::json!({
            "jsonrpc": "2.0", "id": id, "method": "tools/call",
            "params": { "name": tool, "arguments": arguments },
        });
        self.send(&request.to_string());
        let answer = self.answer(id);
        assert_ne!(answer["result"]["isError"], true, "{tool}: {answer}");
        assert!(answer.get("error").is_none(), "{tool}: {answer}");
        answer
    }

    /// Wait for the process to end (after `why`): its exit code, every line it wrote checked.
    fn exit(mut self, why: &str) -> Option<i32> {
        let deadline = Instant::now() + Duration::from_secs(30);
        let status = loop {
            if let Some(status) = self.child.0.try_wait().unwrap() {
                break status;
            }
            assert!(
                Instant::now() < deadline,
                "still running after {why}; stderr:\n{}",
                self.stderr()
            );
            std::thread::sleep(Duration::from_millis(100));
        };
        for line in self.seen.iter().cloned().chain(self.lines.try_iter()) {
            assert_eq!(json_rpc(&line)["jsonrpc"], "2.0", "{line}");
        }
        status.code()
    }

    fn close_stdin(mut self) -> Option<i32> {
        self.stdin = None;
        self.exit("stdin closed")
    }
}

fn json_rpc(line: &str) -> serde_json::Value {
    serde_json::from_str(line).unwrap_or_else(|e| panic!("not JSON-RPC on stdout: {line:?}: {e}"))
}

#[test]
fn a_stdio_run_speaks_only_json_rpc_on_stdout_and_ends_with_stdin() {
    let dir = std::env::temp_dir().join(format!("kerf-headless-stdio-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut run = StdioRun::start(&["--tools", "core"], &dir, "stderr.log");

    let init = run.initialize();
    assert_eq!(init["result"]["serverInfo"]["name"], "kerf", "{init}");
    run.send(r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#);
    let list = run.answer(2);
    let names: Vec<&str> = list["result"]["tools"]
        .as_array()
        .unwrap_or_else(|| panic!("{list}"))
        .iter()
        .filter_map(|t| t["name"].as_str())
        .collect();
    assert!(names.contains(&"trim") && names.contains(&"get_editing_guide"), "{names:?}");
    assert!(
        !names.contains(&"export"),
        "--tools core leaves the delivery set out: {names:?}"
    );
    let info = run.call(3, "project_info", serde_json::json!({}));
    assert!(info.to_string().contains(r#"\"in_memory\":true"#), "{info}");
    run.call(4, "add_marker", serde_json::json!({ "time": 1.5, "name": "stdio-mark" }));

    let stderr = run.stderr();
    assert_eq!(run.close_stdin(), Some(0), "stderr:\n{stderr}");
    assert!(stderr.contains("MCP server on stdin / stdout"), "{stderr}");
    let settings = std::fs::read_to_string(dir.join("settings.json")).unwrap_or_default();
    assert!(
        !settings.contains("mcp_token\":\""),
        "no token is made for a pipe: {settings}"
    );
    let kept = std::fs::read_dir(dir.join("recovery")).map_or(0, |d| d.count());
    assert_eq!(kept, 1, "the untitled project with a marker is kept for recovery");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A proposal staged by one stdio run is in the project file with that run's session as its owner;
/// the next run's agent takes it over instead of being told another agent owns it. The first run
/// ends on SIGTERM, the second when stdin closes.
#[test]
fn the_next_stdio_run_takes_over_the_proposal_an_earlier_one_left() {
    let dir = std::env::temp_dir().join(format!("kerf-headless-stdio-again-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("cut.kerf");
    let path = file.to_str().unwrap();

    let mut first = StdioRun::start(&["--review", "--new-project", path], &dir, "first.log");
    first.initialize();
    first.call(2, "add_marker", serde_json::json!({ "time": 1.0, "name": "first-run" }));
    let staged = first.call(3, "staged_diff", serde_json::json!({}));
    assert!(
        staged.to_string().contains("first-run"),
        "the edit waits for review: {staged}"
    );
    #[cfg(unix)]
    {
        let pid = first.child.0.id().to_string();
        assert!(Command::new("kill").args(["-TERM", &pid]).status().unwrap().success());
        let stderr = first.stderr();
        assert_eq!(first.exit("SIGTERM"), Some(0), "stderr:\n{stderr}");
    }
    #[cfg(not(unix))]
    assert_eq!(first.close_stdin(), Some(0));

    let mut second = StdioRun::start(&["--review", "--project", path], &dir, "second.log");
    second.initialize();
    second.call(2, "add_marker", serde_json::json!({ "time": 2.0, "name": "second-run" }));
    let staged = second.call(3, "staged_diff", serde_json::json!({})).to_string();
    assert!(staged.contains("first-run") && staged.contains("second-run"), "{staged}");
    let stderr = second.stderr();
    assert_eq!(second.close_stdin(), Some(0), "stderr:\n{stderr}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A launcher that sent stderr to stdout (`2>&1`) would get the logs in the client's stream: refused.
#[cfg(unix)]
#[test]
fn stdio_refuses_a_stderr_that_is_its_stdout() {
    let dir = std::env::temp_dir().join(format!("kerf-headless-stdio-merged-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let merged = std::fs::File::create(dir.join("merged.log")).unwrap();
    let status = headless(&["--stdio"], &dir)
        .stdin(Stdio::piped())
        .stdout(merged.try_clone().unwrap())
        .stderr(merged)
        .status()
        .expect("run kerf-app");
    let out = std::fs::read_to_string(dir.join("merged.log")).unwrap();
    assert_eq!(status.code(), Some(2), "{out}");
    assert!(out.contains("stderr is the same file"), "{out}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stdio_with_an_http_flag_is_a_usage_error_and_prints_nothing_on_stdout() {
    let dir = std::env::temp_dir().join(format!("kerf-headless-stdio-usage-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for http in [
        &["--mcp-addr", "127.0.0.1:0"][..],
        &["--token-file", "token"],
        &["--show-token"],
    ] {
        let mut args = vec!["--stdio"];
        args.extend_from_slice(http);
        let out = headless(&args, &dir).stdin(Stdio::null()).output().expect("run kerf-app");
        assert_eq!(out.status.code(), Some(2), "{http:?}");
        assert!(out.stdout.is_empty(), "{http:?}: {:?}", String::from_utf8_lossy(&out.stdout));
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("no listener and no token"),
            "{http:?}"
        );
    }
    let help = headless(&["--stdio", "--help"], &dir).stdin(Stdio::null()).output().unwrap();
    assert_eq!(help.status.code(), Some(0));
    assert!(help.stdout.is_empty(), "--help goes to stderr with --stdio");
    assert!(String::from_utf8_lossy(&help.stderr).contains("--stdio"));
    let _ = std::fs::remove_dir_all(&dir);
}

fn aged_file(path: &std::path::Path, bytes: usize, age: Duration) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, vec![1u8; bytes]).unwrap();
    let when = std::time::SystemTime::now() - age;
    std::fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_accessed(when).set_modified(when))
        .unwrap();
}

#[test]
fn a_capped_headless_run_prunes_the_regenerable_caches_in_its_cache_dir_and_nothing_else() {
    let dir = std::env::temp_dir().join(format!("kerf-headless-cap-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let cache = dir.join("volume");
    let week = Duration::from_secs(7 * 86400);
    let idle = [
        cache.join("kerf/stems/s1/vocals.flac"),
        cache.join("kerf/waveforms/w1.bin"),
        cache.join("kerf/proxies/aaaa.mp4"),
        cache.join("kerf/proxies/aaaa.json"),
    ];
    for file in &idle {
        aged_file(file, 5000, 2 * week);
    }
    let kept = [
        cache.join("kerf/models/ggml-base.bin"),
        cache.join("kerf/stitched/lens.mp4"),
        cache.join("kerf/proxies/bbbb.4242.0.part"),
    ];
    aged_file(&kept[0], 5000, 9 * week);
    aged_file(&kept[1], 5000, 9 * week);
    aged_file(&kept[2], 5000, Duration::from_secs(5));

    let log = std::fs::File::create(dir.join("stderr.log")).unwrap();
    let mut child = Running(
        Command::new(env!("CARGO_BIN_EXE_kerf-app"))
            .args(["--headless", "--mcp-addr", "127.0.0.1:0", "--cache-max-gb", "0.000001"])
            .arg("--data-dir")
            .arg(dir.join("data"))
            .arg("--cache-dir")
            .arg(&cache)
            .env("KERF_MCP_TOKEN", "c".repeat(48))
            .env_remove("DISPLAY")
            .env_remove("WAYLAND_DISPLAY")
            .env_remove("KERF_MCP_ADDR")
            .env_remove("KERF_CACHE_MAX_GB")
            .env_remove("KERF_CACHE_DIR")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(log)
            .spawn()
            .expect("spawn kerf-app"),
    );
    let stderr = || std::fs::read_to_string(dir.join("stderr.log")).unwrap_or_default();
    let stdout = child.0.stdout.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let _ = BufReader::new(stdout).read_line(&mut line);
        let _ = tx.send(line);
    });
    rx.recv_timeout(Duration::from_secs(120))
        .unwrap_or_else(|_| panic!("no ready line; stderr:\n{}", stderr()));

    let deadline = Instant::now() + Duration::from_secs(60);
    while idle.iter().any(|f| f.exists()) {
        assert!(
            Instant::now() < deadline,
            "the idle caches were not pruned; stderr:\n{}",
            stderr()
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    for file in &kept {
        assert!(file.exists(), "{} must stay", file.display());
    }
    let text = stderr();
    assert!(text.contains("pruned the cache"), "{text}");
    assert!(text.contains(&cache.display().to_string()), "the cache dir is logged: {text}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_bad_cache_ceiling_is_a_usage_error() {
    for (args, var) in [(vec!["--cache-max-gb", "0"], None), (vec![], Some("plenty"))] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_kerf-app"));
        command
            .arg("--headless")
            .args(&args)
            .env_remove("DISPLAY")
            .env_remove("WAYLAND_DISPLAY")
            .env_remove("KERF_CACHE_MAX_GB")
            .stdin(Stdio::null());
        if let Some(var) = var {
            command.env("KERF_CACHE_MAX_GB", var);
        }
        let out = command.output().expect("run kerf-app");
        assert_eq!(out.status.code(), Some(2), "{args:?} {var:?}");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains("takes a number of GiB above 0"), "{stderr}");
    }
}

/// The answer of a refused call: its JSON-RPC error message.
fn refusal(body: &str) -> String {
    let data = body
        .lines()
        .filter_map(|l| l.strip_prefix("data: "))
        .find(|d| d.starts_with('{'))
        .unwrap_or(body);
    let reply: serde_json::Value = serde_json::from_str(data).unwrap_or_else(|e| panic!("{body}: {e}"));
    assert_eq!(reply["error"]["code"], -32602, "invalid_params: {reply}");
    reply["error"]["message"].as_str().unwrap().to_string()
}

/// The text of a tool's answer, parsed as the JSON it holds.
fn answer(body: &str) -> serde_json::Value {
    let data = body
        .lines()
        .filter_map(|l| l.strip_prefix("data: "))
        .find(|d| d.starts_with('{'))
        .unwrap_or(body);
    let reply: serde_json::Value = serde_json::from_str(data).unwrap_or_else(|e| panic!("{body}: {e}"));
    let text = reply["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("no text: {reply}"));
    serde_json::from_str(text).unwrap_or_else(|_| serde_json::Value::String(text.to_string()))
}

/// A one-second silent 8 kHz mono WAV.
fn tone(path: &Path) {
    let samples = 8000u32;
    let mut wav = Vec::new();
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + samples * 2).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&8000u32.to_le_bytes());
    wav.extend_from_slice(&16000u32.to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&(samples * 2).to_le_bytes());
    wav.resize(wav.len() + samples as usize * 2, 0);
    std::fs::write(path, wav).unwrap();
}

/// A run confined to a workspace: started in it, inputs and outputs outside it refused, relative ones
/// resolved in it and shown back relative to it.
fn workspace_run(name: &str) -> (PathBuf, PathBuf, Running, SocketAddr, [(String, String); 2], String) {
    let dir = scratch(name);
    let ws = dir.join("ws");
    std::fs::create_dir_all(ws.join("media")).unwrap();
    tone(&ws.join("media").join("tone.wav"));
    tone(&dir.join("outside.wav"));
    let token = "w".repeat(48);
    // Kerf's own directory beside the workspace: an output is never written into it.
    let data = dir.join("data");
    std::fs::create_dir_all(&data).unwrap();
    let (child, ready) = spawn(
        &data,
        &["--workspace", ws.to_str().unwrap(), "--new-project", "cut.kerf"],
        &token,
    );
    assert_eq!(ready["kerf"], "listening", "{ready}");
    let shown = ready["workspace"].as_str().expect("the ready line names the workspace");
    assert_eq!(std::fs::canonicalize(shown).unwrap(), std::fs::canonicalize(&ws).unwrap());
    assert!(ws.join("cut.kerf").is_file(), "--new-project resolves against the workspace");
    let addr: SocketAddr = ready["addr"].as_str().unwrap().parse().unwrap();
    let (on_session, init) = session(addr, &token);
    (dir, ws, child, addr, on_session, init)
}

#[test]
fn a_workspace_refuses_paths_outside_it_and_resolves_relative_ones_in_it() {
    let (dir, ws, child, addr, on_session, init) = workspace_run("ws");
    assert!(
        init.contains("give paths relative to it"),
        "the instructions say the rule: {init}"
    );
    let info = answer(&call(addr, &on_session, 2, "project_info", serde_json::json!({})));
    assert_eq!(info["path"], "cut.kerf", "shown relative: {info}");
    assert!(info["workspace"].as_str().is_some_and(|w| !w.is_empty()), "{info}");

    let outside = dir.join("outside.wav");
    for (id, path) in [
        (3, outside.to_str().unwrap()),
        (4, "../outside.wav"),
        (5, "media/../../outside.wav"),
    ] {
        let why = refusal(&call(
            addr,
            &on_session,
            id,
            "import_asset",
            serde_json::json!({ "path": path }),
        ));
        assert!(why.contains("outside the workspace"), "{path}: {why}");
    }
    let why = refusal(&call(
        addr,
        &on_session,
        6,
        "import_asset",
        serde_json::json!({ "path": "media/missing.wav" }),
    ));
    assert!(
        why.contains("does not exist") && why.contains("missing.wav"),
        "resolved in the workspace: {why}"
    );

    call(
        addr,
        &on_session,
        7,
        "add_marker",
        serde_json::json!({ "time": 1.0, "name": "ws-mark" }),
    );
    let why = refusal(&call(
        addr,
        &on_session,
        8,
        "export_markers",
        serde_json::json!({ "output_path": "../markers.csv", "format": "csv" }),
    ));
    assert!(why.contains("outside the workspace"), "{why}");
    assert!(!dir.join("markers.csv").exists());
    let written = answer(&call(
        addr,
        &on_session,
        9,
        "export_markers",
        serde_json::json!({ "output_path": "exports/../markers.csv", "format": "csv" }),
    ));
    assert!(written.get("path").is_some(), "{written}");
    assert!(std::fs::read_to_string(ws.join("markers.csv")).unwrap().contains("ws-mark"));
    let why = refusal(&call(
        addr,
        &on_session,
        10,
        "save_project_as",
        serde_json::json!({ "path": "/tmp/elsewhere.kerf" }),
    ));
    assert!(why.contains("outside the workspace"), "{why}");

    // A model file is a path too: only a named model is taken.
    let why = refusal(&call(
        addr,
        &on_session,
        11,
        "set_speech_model",
        serde_json::json!({ "name": "/etc/passwd" }),
    ));
    assert!(why.contains("model name"), "{why}");
    // A template path is resolved in the workspace, not refused for being relative.
    let why = refusal(&call(
        addr,
        &on_session,
        12,
        "import_template",
        serde_json::json!({ "path": "card.kerftemplate" }),
    ));
    assert!(why.contains("does not exist") && !why.contains("relative"), "{why}");
    // A playlist in the workspace whose entries lie outside it is not handed to FFmpeg, whatever its name.
    let playlist = format!("#EXTM3U\n#EXTINF:1.0,\n{}\n#EXT-X-ENDLIST\n", outside.display());
    std::fs::write(ws.join("media").join("list.mp4"), &playlist).unwrap();
    let why = refusal(&call(
        addr,
        &on_session,
        13,
        "import_asset",
        serde_json::json!({ "path": "media/list.mp4" }),
    ));
    assert!(why.contains("HLS"), "{why}");
    // A project in the workspace that names media outside it is not opened.
    let crafted = kerf_core::Project::open(ws.join("crafted.kerf")).unwrap();
    let asset: kerf_core::Asset = serde_json::from_value(serde_json::json!({
        "id": "00000000-0000-4000-8000-000000000001",
        "path": outside.to_string_lossy(),
        "name": "outside.wav",
        "duration": 1.0,
        "streams": [],
        "imported_at": "2026-01-01T00:00:00Z",
    }))
    .unwrap();
    crafted.insert_or_get_asset(&asset).unwrap();
    crafted.close();
    let why = refusal(&call(
        addr,
        &on_session,
        14,
        "open_project",
        serde_json::json!({ "path": "crafted.kerf" }),
    ));
    assert!(why.contains("outside the workspace") && why.contains("outside.wav"), "{why}");
    let info = answer(&call(addr, &on_session, 15, "project_info", serde_json::json!({})));
    assert_eq!(info["path"], "cut.kerf", "the open project stays: {info}");
    drop(child);

    // Nor at startup, and Kerf's own files may not lie in the workspace.
    let refused = |args: &[&str]| {
        let out = Command::new(env!("CARGO_BIN_EXE_kerf-app"))
            .args(["--headless", "--mcp-addr", "127.0.0.1:0", "--workspace", ws.to_str().unwrap()])
            .args(args)
            .env_remove("DISPLAY")
            .env_remove("KERF_WORKSPACE")
            .env("KERF_MCP_TOKEN", "w".repeat(48))
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert_eq!(
            out.status.code(),
            Some(2),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stderr).into_owned()
    };
    let data = dir.join("data");
    let why = refused(&["--data-dir", data.to_str().unwrap(), "--project", "crafted.kerf"]);
    assert!(why.contains("outside the workspace"), "{why}");
    let why = refused(&["--data-dir", ws.join("kerf-data").to_str().unwrap()]);
    assert!(why.contains("inside --workspace"), "{why}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
#[ignore = "probes the file with the ffprobe binary"]
fn a_workspace_imports_a_relative_path_and_shows_it_relative() {
    let (dir, _, child, addr, on_session, _) = workspace_run("ws-import");
    let asset = answer(&call(
        addr,
        &on_session,
        2,
        "import_asset",
        serde_json::json!({ "path": "media/tone.wav" }),
    ));
    let relative = Path::new("media").join("tone.wav");
    assert_eq!(asset["path"], relative.to_str().unwrap(), "{asset}");
    let listed = call(addr, &on_session, 3, "list_assets", serde_json::json!({}));
    assert!(listed.contains("tone.wav"), "{listed}");
    let again = answer(&call(
        addr,
        &on_session,
        4,
        "import_asset",
        serde_json::json!({ "path": asset["path"] }),
    ));
    assert_eq!(again["id"], asset["id"], "the path shown is one the server takes back");
    drop(child);
    let _ = std::fs::remove_dir_all(&dir);
}

/// `--workspace` holds over `--stdio` as over HTTP: no ready line, so the rule is in the instructions
/// and `project_info`, and a path outside it is refused.
#[test]
fn a_stdio_run_keeps_to_its_workspace_too() {
    let dir = scratch("ws-stdio");
    let (ws, data) = (dir.join("ws"), dir.join("data"));
    std::fs::create_dir_all(&ws).unwrap();
    std::fs::create_dir_all(&data).unwrap();
    tone(&dir.join("outside.wav"));
    let args = ["--workspace", ws.to_str().unwrap(), "--new-project", "cut.kerf"];
    let mut run = StdioRun::start(&args, &data, "stderr.log");
    let init = run.initialize();
    let instructions = init["result"]["instructions"].as_str().unwrap_or_default();
    assert!(instructions.contains("give paths relative to it"), "{init}");
    assert!(ws.join("cut.kerf").is_file(), "--new-project resolves against the workspace");
    let info = run.call(2, "project_info", serde_json::json!({}));
    let info: serde_json::Value = serde_json::from_str(info["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(info["path"], "cut.kerf", "{info}");
    assert!(info["workspace"].as_str().is_some_and(|w| !w.is_empty()), "{info}");
    let outside = dir.join("outside.wav");
    run.send(
        &serde_json::json!({
            "jsonrpc": "2.0", "id": 3, "method": "tools/call",
            "params": { "name": "import_asset", "arguments": { "path": outside.to_str().unwrap() } },
        })
        .to_string(),
    );
    let refused = run.answer(3);
    assert_eq!(refused["error"]["code"], -32602, "{refused}");
    let why = refused["error"]["message"].as_str().unwrap_or_default();
    assert!(why.contains("outside the workspace"), "{refused}");
    let stderr = run.stderr();
    assert_eq!(run.close_stdin(), Some(0), "stderr:\n{stderr}");
    let _ = std::fs::remove_dir_all(&dir);
}
