//! `kerf-app --headless` as a process: it starts with no display, listens on the port it reports,
//! answers MCP over HTTP behind the token, and stops cleanly on SIGTERM.

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
