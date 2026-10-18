//! The MCP server's lifecycle: where it listens, who may talk to it, and starting, stopping
//! and restarting it when the settings change.
//!
//! The app is a desktop program that holds a person's footage, so the endpoint is not open by
//! default: every request needs `Authorization: Bearer <token>` (`mcp_require_token`, on), a
//! browser's `Origin` is refused outright (native MCP clients send none; a web page the user has
//! open must not be able to drive the editor through the loopback port), and a bind address
//! beyond loopback is refused unless the token is required. A token that changes applies to the
//! next request; a new address or switching the server off restarts the listener.

use std::sync::{Arc, Mutex, OnceLock, RwLock};

use axum::extract::{Request, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use kerf_core::Project;
use rmcp::transport::streamable_http_server::{
    session::local::LocalSessionManager, StreamableHttpServerConfig, StreamableHttpService,
};
use tauri::AppHandle;
use tokio_util::sync::CancellationToken;

use super::KerfMcp;
use crate::settings::Settings;

/// Default localhost bind address for the MCP endpoint; override in Settings or with `KERF_MCP_ADDR`.
pub const DEFAULT_ADDR: &str = "127.0.0.1:7777";

/// The environment override, which wins over the stored address.
pub const ENV_ADDR: &str = "KERF_MCP_ADDR";

/// Shortest token the settings accept; a generated one is 64 hex characters (32 random bytes).
pub const MIN_TOKEN_LEN: usize = 32;

/// What the server runs on, from the stored settings and the environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpConfig {
    pub enabled: bool,
    pub addr: String,
    pub require_token: bool,
    pub token: Option<String>,
}

impl McpConfig {
    pub fn from_settings(settings: &Settings) -> Self {
        Self::resolve(settings, std::env::var(ENV_ADDR).ok())
    }

    /// [`McpConfig::from_settings`] with the environment passed in (so a test need not set it).
    fn resolve(settings: &Settings, env_addr: Option<String>) -> Self {
        let addr = env_addr
            .map(|a| a.trim().to_string())
            .filter(|a| !a.is_empty())
            .or_else(|| {
                settings
                    .mcp_addr
                    .as_deref()
                    .map(str::trim)
                    .filter(|a| !a.is_empty())
                    .map(str::to_string)
            })
            .unwrap_or_else(|| DEFAULT_ADDR.to_string());
        Self {
            enabled: settings.mcp_enabled,
            addr,
            require_token: settings.mcp_require_token,
            token: settings.mcp_token.clone(),
        }
    }
}

/// Whether `KERF_MCP_ADDR` is set to something, i.e. the stored address is not what is in force.
pub fn env_override() -> bool {
    std::env::var(ENV_ADDR).is_ok_and(|a| !a.trim().is_empty())
}

// ---- the address -------------------------------------------------------------

/// The host part of a `host:port` bind address, tolerating a bracketed IPv6
/// literal and a missing port.
pub fn host_of(addr: &str) -> Option<String> {
    if let Ok(sock) = addr.parse::<std::net::SocketAddr>() {
        return Some(sock.ip().to_string());
    }
    let host = match addr.rsplit_once(':') {
        Some((host, port)) if !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) => host,
        _ => addr,
    };
    let host = host.trim().trim_start_matches('[').trim_end_matches(']');
    (!host.is_empty()).then(|| host.to_string())
}

/// Whether `addr` stays on this machine: `localhost` or a loopback IP. A wildcard (`0.0.0.0`,
/// `[::]`) and any other address or name is reachable from the network.
pub fn is_loopback_addr(addr: &str) -> bool {
    let Some(host) = host_of(addr) else { return false };
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    host.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

/// Why `addr` is not an address the server can listen on, or `None` when it is. A name is
/// accepted only as `localhost`; anything else must be an IP literal with a port.
pub fn check_addr(addr: &str) -> Result<(), String> {
    let addr = addr.trim();
    if addr.parse::<std::net::SocketAddr>().is_ok() {
        return Ok(());
    }
    match addr.rsplit_once(':') {
        Some((host, port)) if host.eq_ignore_ascii_case("localhost") && port.parse::<u16>().is_ok() => Ok(()),
        _ => Err(format!(
            "{addr:?} is not an address to listen on: use host:port with an IP address, e.g. 127.0.0.1:7777"
        )),
    }
}

/// A reason the server refuses to bind `addr`: reachable from the network without a token.
pub fn bind_problem(addr: &str, require_token: bool) -> Option<String> {
    (!is_loopback_addr(addr) && !require_token).then(|| {
        format!(
            "refusing to listen on {addr}: it is reachable from the network, and requests would carry no token. \
             Turn the token requirement back on, or listen on 127.0.0.1"
        )
    })
}

/// The `Host` headers this server will accept.
///
/// rmcp validates the inbound `Host` against an allow-list that defaults to
/// loopback, to blunt DNS-rebinding attacks against locally running servers.
/// Moving the server off loopback would make the bare default reject its own
/// clients, so extend the list with whatever the address names: a concrete address
/// is added as-is, while a wildcard bind (`0.0.0.0` / `[::]`) cannot be enumerated
/// at all — the client's `Host` is whichever of this machine's addresses it reached
/// us on — so it yields an empty list, which is rmcp's documented "allow any".
pub fn allowed_hosts(addr: &str) -> Vec<String> {
    let defaults = ["localhost", "127.0.0.1", "::1"];
    let mut hosts: Vec<String> = defaults.iter().map(|h| h.to_string()).collect();
    let Some(host) = host_of(addr) else {
        return hosts;
    };
    if host.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_unspecified()) {
        return Vec::new();
    }
    if !hosts.iter().any(|h| h.eq_ignore_ascii_case(&host)) {
        hosts.push(host);
    }
    hosts
}

// ---- the token ---------------------------------------------------------------

/// A fresh bearer token: 32 random bytes from the OS, in hex.
pub fn generate_token() -> String {
    let mut bytes = [0u8; 32];
    if getrandom::fill(&mut bytes).is_err() {
        // The OS refusing to give randomness is not something to carry on from quietly with a
        // predictable token; two v4 UUIDs draw from the same source and fail the same way, so
        // this is only a last resort that keeps the app starting.
        bytes[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
        bytes[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    }
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Whether `token` is a token the settings may store.
pub fn check_token(token: &str) -> Result<(), String> {
    if token.len() < MIN_TOKEN_LEN {
        return Err(format!("the token must be at least {MIN_TOKEN_LEN} characters"));
    }
    if token.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err("the token must not contain whitespace".to_string());
    }
    Ok(())
}

/// Compare two secrets without stopping at the first byte that differs, so the time a refusal
/// takes does not say how much of a guess was right.
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    let longest = a.len().max(b.len());
    let mut diff = (a.len() ^ b.len()) as u64;
    for i in 0..longest {
        let (x, y) = (a.get(i).copied().unwrap_or(0), b.get(i).copied().unwrap_or(0));
        diff |= u64::from(std::hint::black_box(x ^ y));
    }
    diff == 0
}

/// What the middleware checks each request against; shared with the server so a new token
/// reaches it without a restart.
#[derive(Debug, Clone, Default)]
pub struct Auth {
    pub require_token: bool,
    pub token: Option<String>,
}

impl Auth {
    fn of(config: &McpConfig) -> Self {
        Self {
            require_token: config.require_token,
            token: config.token.clone(),
        }
    }

    /// Whether a request presenting `authorization` (the header's value) may pass.
    pub fn permits(&self, authorization: Option<&str>) -> bool {
        if !self.require_token {
            return true;
        }
        let (Some(token), Some(presented)) = (self.token.as_deref(), authorization.and_then(bearer)) else {
            return false;
        };
        constant_time_eq(presented.as_bytes(), token.as_bytes())
    }
}

/// The token of an `Authorization: Bearer <token>` value (the scheme is case-insensitive).
fn bearer(value: &str) -> Option<&str> {
    let (scheme, rest) = value.trim().split_once(char::is_whitespace)?;
    scheme
        .eq_ignore_ascii_case("bearer")
        .then(|| rest.trim())
        .filter(|t| !t.is_empty())
}

async fn require_bearer(State(auth): State<Arc<RwLock<Auth>>>, request: Request, next: Next) -> Response {
    let permitted = {
        let auth = auth.read().unwrap_or_else(|e| e.into_inner());
        let header = request.headers().get(header::AUTHORIZATION).and_then(|v| v.to_str().ok());
        auth.permits(header)
    };
    if !permitted {
        tracing::warn!(target: "mcp", "request refused: missing or wrong bearer token");
        let mut response = (
            StatusCode::UNAUTHORIZED,
            "Unauthorized: send `Authorization: Bearer <token>`. The token is in Kerf, Settings > Agent.",
        )
            .into_response();
        response
            .headers_mut()
            .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer realm=\"kerf\""));
        return response;
    }
    next.run(request).await
}

// ---- state the rest of the app reads ----------------------------------------

#[derive(Debug, Clone, Default)]
struct Status {
    running: bool,
    error: Option<String>,
    addr: Option<String>,
}

static STATUS: Mutex<Status> = Mutex::new(Status {
    running: false,
    error: None,
    addr: None,
});

fn set_status(running: bool, error: Option<String>, addr: &str) {
    *STATUS.lock().unwrap_or_else(|e| e.into_inner()) = Status {
        running,
        error,
        addr: Some(addr.to_string()),
    };
}

fn status() -> Status {
    STATUS.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// The address the server is configured for (the environment's, else the stored one, else the default).
pub fn bind_addr() -> String {
    status().addr.unwrap_or_else(|| {
        std::env::var(ENV_ADDR)
            .ok()
            .map(|a| a.trim().to_string())
            .filter(|a| !a.is_empty())
            .unwrap_or_else(|| DEFAULT_ADDR.to_string())
    })
}

/// The full URL a client connects to (`http://<addr>/mcp`). The GUI shows this
/// so the user knows where to point their agent.
pub fn endpoint_url() -> String {
    format!("http://{}/mcp", bind_addr())
}

/// Why the server is not listening, if it is not: the port is taken, the address is refused.
pub fn server_error() -> Option<String> {
    status().error
}

/// The sentence the agent panel shows for a server that did not start.
pub fn describe_server_error(addr: &str, error: &anyhow::Error) -> String {
    let in_use = error
        .downcast_ref::<std::io::Error>()
        .is_some_and(|e| e.kind() == std::io::ErrorKind::AddrInUse);
    if in_use {
        format!("The agent endpoint could not start: {addr} is already in use, most likely by another Kerf. Close it, or pick a free address in Settings > Agent.")
    } else {
        format!("The agent endpoint could not start on {addr}: {error}")
    }
}

// ---- what a client is given --------------------------------------------------

/// What the Agent settings page shows: see `mcp_commands::McpConnectionInfo`.
pub struct ConnectionInfo {
    pub enabled: bool,
    pub url: String,
    pub require_token: bool,
    pub token: Option<String>,
    pub running: bool,
    pub error: Option<String>,
    pub configured_addr: Option<String>,
    pub env_override: bool,
    pub agent_review: bool,
    pub claude_code_command: String,
    pub generic_json_config: String,
}

/// The URL a client on this machine connects to for a server bound to `addr`: a wildcard bind is
/// reached through loopback.
pub fn client_url(addr: &str) -> String {
    let Some(host) = host_of(addr) else {
        return format!("http://{addr}/mcp");
    };
    let port = addr
        .rsplit_once(':')
        .map(|(_, p)| p)
        .filter(|p| p.bytes().all(|b| b.is_ascii_digit()));
    let host = match host.parse::<std::net::IpAddr>() {
        Ok(ip) if ip.is_unspecified() => "127.0.0.1".to_string(),
        Ok(std::net::IpAddr::V6(ip)) => format!("[{ip}]"),
        _ => host,
    };
    match port {
        Some(port) => format!("http://{host}:{port}/mcp"),
        None => format!("http://{host}/mcp"),
    }
}

/// The command that adds the server to Claude Code, with the token header when one is required.
pub fn claude_code_command(url: &str, token: Option<&str>, require_token: bool) -> String {
    match token.filter(|_| require_token) {
        Some(token) => format!("claude mcp add --transport http kerf {url} --header \"Authorization: Bearer {token}\""),
        None => format!("claude mcp add --transport http kerf {url}"),
    }
}

/// A `mcpServers` snippet for an MCP client that takes a JSON config.
pub fn generic_json_config(url: &str, token: Option<&str>, require_token: bool) -> String {
    let mut server = serde_json::json!({ "type": "http", "url": url });
    if let Some(token) = token.filter(|_| require_token) {
        server["headers"] = serde_json::json!({ "Authorization": format!("Bearer {token}") });
    }
    serde_json::to_string_pretty(&serde_json::json!({ "mcpServers": { "kerf": server } })).unwrap_or_default()
}

/// The connection details for `settings`, with whether the server is up now.
pub fn connection_info(settings: &Settings) -> ConnectionInfo {
    let config = McpConfig::from_settings(settings);
    let url = client_url(&config.addr);
    let state = status();
    ConnectionInfo {
        enabled: config.enabled,
        claude_code_command: claude_code_command(&url, config.token.as_deref(), config.require_token),
        generic_json_config: generic_json_config(&url, config.token.as_deref(), config.require_token),
        url,
        require_token: config.require_token,
        token: config.token,
        running: state.running,
        error: state.error,
        configured_addr: settings.mcp_addr.clone(),
        env_override: env_override(),
        agent_review: settings.agent_review,
    }
}

// ---- running it --------------------------------------------------------------

static CONTROL: OnceLock<tokio::sync::watch::Sender<McpConfig>> = OnceLock::new();

/// Fill in a token if there is none (the first start), persisting it, and return the settings as
/// they stand.
fn with_token(app: &AppHandle, mut settings: Settings) -> Settings {
    if settings.mcp_token.is_none() {
        let token = generate_token();
        match crate::settings::update(app, &serde_json::json!({ "mcp_token": token })) {
            Ok(stored) => settings = stored,
            Err(e) => {
                tracing::warn!(error = %e, "could not store the MCP token; using one for this run only");
                settings.mcp_token = Some(token);
            }
        }
    }
    settings
}

/// Start the MCP server from the stored settings and keep it in step with them from here on.
/// Called once, from the app's setup hook. The listener comes up on the async runtime; a failure
/// to bind is reported by [`server_error`], not returned.
pub fn start(app: &AppHandle, project: Arc<Mutex<Project>>) {
    let settings = with_token(app, crate::settings::load(app));
    crate::settings::set_agent_review(settings.agent_review);
    project
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .set_agent_review(settings.agent_review);
    let config = McpConfig::from_settings(&settings);
    let (tx, rx) = tokio::sync::watch::channel(config);
    if CONTROL.set(tx).is_err() {
        tracing::warn!("the MCP server was started twice");
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(supervise(app, project, rx));
}

/// Tell the running server the settings moved. A patch that carries no `mcp_*` field changes
/// nothing here. Called by [`crate::settings::update`] once the file is saved.
pub fn settings_changed(settings: &Settings, patch: &serde_json::Value) {
    let touched = patch
        .as_object()
        .is_some_and(|fields| fields.keys().any(|k| k.starts_with("mcp_")));
    if !touched {
        return;
    }
    if let Some(tx) = CONTROL.get() {
        tx.send_replace(McpConfig::from_settings(settings));
    }
}

struct Running {
    stop: CancellationToken,
    task: tauri::async_runtime::JoinHandle<std::io::Result<()>>,
}

impl Running {
    /// Close the listener and every session on it; sessions that do not wind down in a couple of
    /// seconds are cut.
    async fn stop(self) {
        self.stop.cancel();
        let mut task = self.task;
        if tokio::time::timeout(std::time::Duration::from_secs(2), &mut task)
            .await
            .is_err()
        {
            task.abort();
        }
    }
}

async fn supervise(app: AppHandle, project: Arc<Mutex<Project>>, mut rx: tokio::sync::watch::Receiver<McpConfig>) {
    let mut config = rx.borrow_and_update().clone();
    let auth = Arc::new(RwLock::new(Auth::of(&config)));
    loop {
        *auth.write().unwrap_or_else(|e| e.into_inner()) = Auth::of(&config);
        let mut running = None;
        if config.enabled {
            match launch(&config, project.clone(), app.clone(), auth.clone()).await {
                Ok(up) => {
                    set_status(true, None, &config.addr);
                    running = Some(up);
                }
                Err(e) => {
                    tracing::error!(error = %e, addr = %config.addr, "MCP server did not start");
                    set_status(false, Some(describe_server_error(&config.addr, &e)), &config.addr);
                }
            }
        } else {
            set_status(false, None, &config.addr);
        }
        // A token (or the requirement for one) applies live; a new address, or the server being
        // switched on or off, takes a restart. A server that is not up retries on any change.
        loop {
            if rx.changed().await.is_err() {
                if let Some(up) = running {
                    up.stop().await;
                }
                return;
            }
            let next = rx.borrow_and_update().clone();
            *auth.write().unwrap_or_else(|e| e.into_inner()) = Auth::of(&next);
            let restart = next.enabled != config.enabled || next.addr != config.addr || (next.enabled && running.is_none());
            config = next;
            if restart {
                break;
            }
        }
        if let Some(up) = running {
            up.stop().await;
        }
    }
}

async fn launch(
    config: &McpConfig,
    project: Arc<Mutex<Project>>,
    app: AppHandle,
    auth: Arc<RwLock<Auth>>,
) -> anyhow::Result<Running> {
    if let Some(why) = bind_problem(&config.addr, config.require_token) {
        anyhow::bail!(why);
    }
    check_addr(&config.addr).map_err(anyhow::Error::msg)?;
    let stop = CancellationToken::new();
    let service = StreamableHttpService::new(
        move || Ok(KerfMcp::new(project.clone(), app.clone())),
        LocalSessionManager::default().into(),
        StreamableHttpServerConfig::default()
            .with_allowed_hosts(allowed_hosts(&config.addr))
            // No Origin is allowed: a native MCP client sends none, and a page in the user's
            // browser must not be able to reach the editor through the loopback port.
            .enforce_origin_validation()
            .with_cancellation_token(stop.child_token()),
    );
    let router = axum::Router::new()
        .nest_service("/mcp", service)
        .layer(middleware::from_fn_with_state(auth, require_bearer));
    let listener = tokio::net::TcpListener::bind(&config.addr).await?;
    tracing::info!(addr = %config.addr, token = config.require_token, "kerf MCP server listening on http://{}/mcp", config.addr);
    let shutdown = stop.clone();
    let task = tauri::async_runtime::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(shutdown.cancelled_owned())
            .await
    });
    Ok(Running { stop, task })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_binds_keep_the_default_allow_list() {
        let defaults = vec!["localhost".to_string(), "127.0.0.1".to_string(), "::1".to_string()];
        assert_eq!(allowed_hosts("127.0.0.1:7777"), defaults);
        assert_eq!(allowed_hosts("localhost:7777"), defaults);
        assert_eq!(allowed_hosts("[::1]:7777"), defaults);
    }

    #[test]
    fn a_concrete_override_is_allowed() {
        let hosts = allowed_hosts("192.168.1.20:9000");
        assert!(hosts.contains(&"192.168.1.20".to_string()), "{hosts:?}");
        assert!(
            hosts.contains(&"127.0.0.1".to_string()),
            "loopback stays reachable: {hosts:?}"
        );
    }

    #[test]
    fn a_wildcard_bind_disables_host_validation() {
        assert!(allowed_hosts("0.0.0.0:7777").is_empty());
        assert!(allowed_hosts("[::]:7777").is_empty());
    }

    #[test]
    fn odd_addresses_keep_loopback_reachable() {
        let hosts = allowed_hosts("not-an-address");
        assert!(hosts.contains(&"127.0.0.1".to_string()), "{hosts:?}");
        assert_eq!(allowed_hosts(""), vec!["localhost", "127.0.0.1", "::1"]);
    }

    #[test]
    fn only_loopback_stays_on_the_machine() {
        for local in [
            "127.0.0.1:7777",
            "localhost:1",
            "LOCALHOST:7777",
            "[::1]:7777",
            "127.1.2.3:80",
        ] {
            assert!(is_loopback_addr(local), "{local}");
        }
        for open in [
            "0.0.0.0:7777",
            "[::]:7777",
            "192.168.1.20:7777",
            "example.com:80",
            "",
            "10.0.0.1",
        ] {
            assert!(!is_loopback_addr(open), "{open}");
        }
    }

    #[test]
    fn a_network_bind_without_a_token_is_refused_and_with_one_allowed() {
        assert!(bind_problem("0.0.0.0:7777", false).is_some());
        assert!(bind_problem("192.168.1.20:7777", false).is_some());
        assert!(bind_problem("0.0.0.0:7777", true).is_none());
        assert!(bind_problem("127.0.0.1:7777", false).is_none(), "loopback needs no token");
        assert!(bind_problem("localhost:7777", false).is_none());
        let why = bind_problem("0.0.0.0:7777", false).unwrap();
        assert!(why.contains("0.0.0.0:7777") && why.contains("token"), "{why}");
    }

    #[test]
    fn an_address_is_host_and_port_with_an_ip_or_localhost() {
        assert!(check_addr("127.0.0.1:7777").is_ok());
        assert!(check_addr("[::1]:7777").is_ok());
        assert!(check_addr("localhost:8080").is_ok());
        assert!(check_addr("0.0.0.0:7777").is_ok());
        assert!(check_addr("7777").is_err());
        assert!(check_addr("127.0.0.1").is_err());
        assert!(check_addr("example.com:80").is_err());
        assert!(check_addr("").is_err());
        assert!(check_addr("localhost:notaport").is_err());
    }

    #[test]
    fn the_environment_wins_over_the_stored_address() {
        let stored = Settings {
            mcp_addr: Some("127.0.0.1:9000".to_string()),
            ..Settings::default()
        };
        assert_eq!(McpConfig::resolve(&stored, None).addr, "127.0.0.1:9000");
        assert_eq!(
            McpConfig::resolve(&stored, Some("127.0.0.1:9100".to_string())).addr,
            "127.0.0.1:9100"
        );
        assert_eq!(
            McpConfig::resolve(&stored, Some("  ".to_string())).addr,
            "127.0.0.1:9000",
            "an empty override is none"
        );
        assert_eq!(McpConfig::resolve(&Settings::default(), None).addr, DEFAULT_ADDR);
    }

    #[test]
    fn the_defaults_are_on_and_need_a_token() {
        let config = McpConfig::resolve(&Settings::default(), None);
        assert!(config.enabled && config.require_token);
        assert_eq!(config.token, None, "a token is made on the first start, not by a default");
    }

    #[test]
    fn a_generated_token_is_long_random_hex() {
        let (a, b) = (generate_token(), generate_token());
        assert_eq!(a.len(), 64);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
        assert!(check_token(&a).is_ok());
    }

    #[test]
    fn a_stored_token_must_be_long_and_one_word() {
        assert!(check_token("short").is_err());
        assert!(check_token(&"a".repeat(31)).is_err());
        assert!(check_token(&"a".repeat(32)).is_ok());
        assert!(check_token(&format!("{} {}", "a".repeat(20), "b".repeat(20))).is_err());
        assert!(check_token(&format!("{}\n", "a".repeat(40))).is_err());
    }

    #[test]
    fn secrets_compare_equal_only_when_identical() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(constant_time_eq(b"", b""));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"abcd"));
        assert!(!constant_time_eq(b"abcd", b"abc"));
        assert!(!constant_time_eq(b"", b"a"));
    }

    #[test]
    fn a_bearer_header_is_parsed_with_any_case_and_spacing() {
        assert_eq!(bearer("Bearer abc"), Some("abc"));
        assert_eq!(bearer("bearer abc"), Some("abc"));
        assert_eq!(bearer("BEARER   abc  "), Some("abc"));
        assert_eq!(bearer("Basic abc"), None);
        assert_eq!(bearer("Bearer"), None);
        assert_eq!(bearer("Bearer "), None);
        assert_eq!(bearer("abc"), None);
    }

    #[test]
    fn auth_lets_everything_through_only_when_no_token_is_required() {
        let open = Auth {
            require_token: false,
            token: Some("t".repeat(40)),
        };
        assert!(open.permits(None));
        assert!(open.permits(Some("Bearer wrong")));

        let token = "k".repeat(40);
        let shut = Auth {
            require_token: true,
            token: Some(token.clone()),
        };
        assert!(shut.permits(Some(&format!("Bearer {token}"))));
        assert!(shut.permits(Some(&format!("bearer {token}"))));
        assert!(!shut.permits(None));
        assert!(!shut.permits(Some("Bearer wrong")));
        assert!(!shut.permits(Some(&token)), "the scheme is required");
        assert!(!shut.permits(Some(&format!("Bearer {token}x"))));

        let tokenless = Auth {
            require_token: true,
            token: None,
        };
        assert!(
            !tokenless.permits(Some("Bearer anything")),
            "no token to match means no entry"
        );
        assert!(!tokenless.permits(None));
    }

    /// One HTTP request over a plain socket, answering the status code.
    fn status_of(addr: std::net::SocketAddr, headers: &[(&str, &str)]) -> u16 {
        use std::io::{Read, Write};
        let mut request = format!("POST /mcp HTTP/1.1\r\nHost: {addr}\r\nContent-Length: 2\r\nConnection: close\r\n");
        for (name, value) in headers {
            request.push_str(&format!("{name}: {value}\r\n"));
        }
        request.push_str("\r\n{}");
        let mut stream = std::net::TcpStream::connect(addr).expect("connect");
        stream.set_read_timeout(Some(std::time::Duration::from_secs(5))).unwrap();
        stream.write_all(request.as_bytes()).unwrap();
        let mut response = String::new();
        let _ = stream.read_to_string(&mut response);
        response
            .split_whitespace()
            .nth(1)
            .and_then(|code| code.parse().ok())
            .unwrap_or_else(|| panic!("no status in {response:?}"))
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_middleware_refuses_a_request_without_the_token_and_passes_one_with_it() {
        let token = "z".repeat(48);
        let auth = Arc::new(RwLock::new(Auth {
            require_token: true,
            token: Some(token.clone()),
        }));
        let router = axum::Router::new()
            .route("/mcp", axum::routing::post(|| async { "ok" }))
            .layer(middleware::from_fn_with_state(auth.clone(), require_bearer));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await });

        let ask = move |headers: Vec<(String, String)>| {
            tokio::task::spawn_blocking(move || {
                let pairs: Vec<(&str, &str)> = headers.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
                status_of(addr, &pairs)
            })
        };
        assert_eq!(ask(vec![]).await.unwrap(), 401, "no header");
        assert_eq!(
            ask(vec![("Authorization".into(), "Bearer nope".into())]).await.unwrap(),
            401,
            "wrong token"
        );
        assert_eq!(
            ask(vec![("Authorization".into(), token.clone())]).await.unwrap(),
            401,
            "no scheme"
        );
        assert_eq!(
            ask(vec![("Authorization".into(), format!("Bearer {token}"))]).await.unwrap(),
            200,
            "the right token"
        );

        // A new token applies to the very next request, no restart.
        let rotated = "r".repeat(48);
        auth.write().unwrap().token = Some(rotated.clone());
        assert_eq!(
            ask(vec![("Authorization".into(), format!("Bearer {token}"))]).await.unwrap(),
            401,
            "the old token"
        );
        assert_eq!(
            ask(vec![("Authorization".into(), format!("Bearer {rotated}"))])
                .await
                .unwrap(),
            200
        );

        // Switching the requirement off lets a bare request through.
        auth.write().unwrap().require_token = false;
        assert_eq!(ask(vec![]).await.unwrap(), 200);
    }

    #[test]
    fn a_client_is_given_a_url_it_can_reach() {
        assert_eq!(client_url("127.0.0.1:7777"), "http://127.0.0.1:7777/mcp");
        assert_eq!(
            client_url("0.0.0.0:9000"),
            "http://127.0.0.1:9000/mcp",
            "a wildcard is reached through loopback"
        );
        assert_eq!(client_url("[::]:9000"), "http://127.0.0.1:9000/mcp");
        assert_eq!(client_url("[::1]:7777"), "http://[::1]:7777/mcp");
        assert_eq!(client_url("192.168.1.20:7777"), "http://192.168.1.20:7777/mcp");
    }

    #[test]
    fn the_connect_snippets_carry_the_token_only_when_one_is_required() {
        let url = "http://127.0.0.1:7777/mcp";
        let token = "t".repeat(40);
        let with = claude_code_command(url, Some(&token), true);
        assert_eq!(
            with,
            format!("claude mcp add --transport http kerf {url} --header \"Authorization: Bearer {token}\"")
        );
        assert_eq!(
            claude_code_command(url, Some(&token), false),
            format!("claude mcp add --transport http kerf {url}"),
            "no header when none is required"
        );
        let json: serde_json::Value = serde_json::from_str(&generic_json_config(url, Some(&token), true)).unwrap();
        assert_eq!(json["mcpServers"]["kerf"]["url"], url);
        assert_eq!(json["mcpServers"]["kerf"]["type"], "http");
        assert_eq!(
            json["mcpServers"]["kerf"]["headers"]["Authorization"],
            format!("Bearer {token}")
        );
        let bare: serde_json::Value = serde_json::from_str(&generic_json_config(url, Some(&token), false)).unwrap();
        assert!(bare["mcpServers"]["kerf"].get("headers").is_none());
    }

    #[test]
    fn a_taken_port_is_named_as_such() {
        let in_use = anyhow::Error::from(std::io::Error::from(std::io::ErrorKind::AddrInUse));
        let msg = describe_server_error("127.0.0.1:7777", &in_use);
        assert!(msg.contains("127.0.0.1:7777") && msg.contains("already in use"), "{msg}");
        let other = anyhow::anyhow!("permission denied");
        let msg = describe_server_error("127.0.0.1:80", &other);
        assert!(msg.contains("permission denied") && msg.contains("127.0.0.1:80"), "{msg}");
    }
}
