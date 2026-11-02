//! `kerf-app --headless` as a process: it starts with no display, listens on the port it reports,
//! answers MCP over HTTP behind the token, and stops cleanly on SIGTERM; with `--stdio` it answers
//! on stdin / stdout, which carries nothing but JSON-RPC, and stops when stdin closes.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream};
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
        if let Some((_, body)) = text.split_once("\r\n\r\n") {
            if body.contains(until) {
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
    let headers = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
        .collect();
    (status, headers, body.to_string())
}

/// Kills the child if the test fails before it is stopped.
struct Running(Child);

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn a_headless_run_serves_mcp_with_no_display_and_stops_on_sigterm() {
    let dir = std::env::temp_dir().join(format!("kerf-headless-it-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let token = "h".repeat(48);
    let log = std::fs::File::create(dir.join("stderr.log")).unwrap();
    let mut child = Running(
        Command::new(env!("CARGO_BIN_EXE_kerf-app"))
            .args(["--headless", "--mcp-addr", "127.0.0.1:0", "--data-dir"])
            .arg(&dir)
            .env("KERF_MCP_TOKEN", &token)
            .env_remove("DISPLAY")
            .env_remove("WAYLAND_DISPLAY")
            .env_remove("KERF_MCP_ADDR")
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
    let line = rx
        .recv_timeout(Duration::from_secs(120))
        .unwrap_or_else(|_| panic!("no ready line; stderr:\n{}", stderr()));
    let ready: serde_json::Value =
        serde_json::from_str(&line).unwrap_or_else(|e| panic!("ready line {line:?}: {e}; stderr:\n{}", stderr()));
    assert_eq!(ready["kerf"], "listening", "{ready}");
    assert_eq!(ready["token_source"], "env");
    assert_eq!(ready["agent_review"], false, "nobody to review: edits land live");
    assert!(ready["project"].is_null());
    let addr: SocketAddr = ready["addr"].as_str().unwrap().parse().unwrap();
    assert_ne!(addr.port(), 0);
    assert_eq!(ready["url"], format!("http://{addr}/mcp"));
    assert!(!stderr().contains(&token), "the token is not printed unless asked");

    let (status, _, _) = exchange(addr, &[], INITIALIZE, "\"id\":1");
    assert_eq!(status, 401, "no token, no entry");
    let auth = format!("Bearer {token}");
    let (status, headers, _) = exchange(addr, &[("Authorization", &auth)], INITIALIZE, "\"id\":1");
    assert_eq!(status, 200);
    let session = headers
        .into_iter()
        .find(|(k, _)| k == "mcp-session-id")
        .map(|(_, v)| v)
        .expect("a session id");
    let on_session = [("Authorization", auth.as_str()), ("Mcp-Session-Id", session.as_str())];
    assert_eq!(exchange(addr, &on_session, INITIALIZED, "").0, 202);

    let call = |id: u32, tool: &str, arguments: serde_json::Value| {
        let body = serde_json::json!({
            "jsonrpc": "2.0", "id": id, "method": "tools/call",
            "params": { "name": tool, "arguments": arguments },
        })
        .to_string();
        let (status, _, body) = exchange(addr, &on_session, &body, &format!("\"id\":{id}"));
        assert_eq!(status, 200, "{tool}: {body}");
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
