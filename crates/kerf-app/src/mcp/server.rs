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
#[derive(Clone, PartialEq, Eq)]
pub struct McpConfig {
    pub enabled: bool,
    pub addr: String,
    pub require_token: bool,
    pub token: Option<String>,
}

/// A token is a secret: it never reaches a log line through a `{:?}`.
impl std::fmt::Debug for McpConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpConfig")
            .field("enabled", &self.enabled)
            .field("addr", &self.addr)
            .field("require_token", &self.require_token)
            .field("token", &self.token.as_ref().map(|_| "<redacted>"))
            .finish()
    }
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
            token: settings.mcp_token.clone().or_else(run_token),
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

/// A fresh bearer token: 32 random bytes from the OS, in hex. The OS refusing to give randomness
/// (tried three times) is an error and not a quiet fall back to something guessable — and not a
/// panic either: `uuid` draws from the same source and would panic when it fails.
pub fn generate_token() -> Result<String, String> {
    let mut bytes = [0u8; 32];
    let mut last = None;
    for _ in 0..3 {
        match getrandom::fill(&mut bytes) {
            Ok(()) => return Ok(bytes.iter().map(|b| format!("{b:02x}")).collect()),
            Err(e) => last = Some(e),
        }
    }
    Err(format!(
        "the operating system gave no random bytes for the agent token{}",
        last.map(|e| format!(": {e}")).unwrap_or_default()
    ))
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

/// What the middleware checks each request against; shared with the server and set again
/// whenever the settings move (the server also restarts on a changed token, which ends every session).
#[derive(Clone, Default)]
pub struct Auth {
    pub require_token: bool,
    pub token: Option<String>,
}

impl std::fmt::Debug for Auth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Auth")
            .field("require_token", &self.require_token)
            .field("token", &self.token.as_ref().map(|_| "<redacted>"))
            .finish()
    }
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
        note_refused();
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

/// Refusals are logged at most once every [`REFUSED_LOG_EVERY`] seconds, with how many were left
/// out: a client with a stale token retries in a loop, and a log line each would bury the rest.
const REFUSED_LOG_EVERY: i64 = 10;

fn note_refused() {
    static LAST: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);
    static SKIPPED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let now = super::unix_now();
    let last = LAST.load(std::sync::atomic::Ordering::Relaxed);
    if now - last >= REFUSED_LOG_EVERY
        && LAST
            .compare_exchange(
                last,
                now,
                std::sync::atomic::Ordering::Relaxed,
                std::sync::atomic::Ordering::Relaxed,
            )
            .is_ok()
    {
        let skipped = SKIPPED.swap(0, std::sync::atomic::Ordering::Relaxed);
        tracing::warn!(target: "mcp", skipped, "request refused: missing or wrong bearer token");
    } else {
        SKIPPED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
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
    /// The token could not be made or kept: what to tell the user ("this token works until Kerf
    /// quits", "no token could be made, so the server stays off").
    pub token_problem: Option<String>,
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

/// Whether the listener is up and what stopped it, for the address now configured. The status is for
/// the address the supervisor last started on: until it has restarted for a new one, that listener
/// (or its error) is the old address's and says nothing about this one.
fn listener_state(state: &Status, addr: &str) -> (bool, Option<String>) {
    let current = state.addr.as_deref() == Some(addr);
    (state.running && current, state.error.clone().filter(|_| current))
}

/// The connection details for `settings`, with whether the server is up now.
pub fn connection_info(settings: &Settings) -> ConnectionInfo {
    let config = McpConfig::from_settings(settings);
    let url = client_url(&config.addr);
    let (running, error) = listener_state(&status(), &config.addr);
    ConnectionInfo {
        enabled: config.enabled,
        claude_code_command: claude_code_command(&url, config.token.as_deref(), config.require_token),
        generic_json_config: generic_json_config(&url, config.token.as_deref(), config.require_token),
        url,
        require_token: config.require_token,
        token: config.token,
        token_problem: token_problem(),
        running,
        error,
        configured_addr: settings.mcp_addr.clone(),
        env_override: env_override(),
        agent_review: settings.agent_review,
    }
}

// ---- running it --------------------------------------------------------------

static CONTROL: OnceLock<tokio::sync::watch::Sender<McpConfig>> = OnceLock::new();

/// A token that could not be written to the settings file, kept for this run so the server and
/// what the Agent page shows agree (the page must not say "no token" while the server asks for
/// one).
static RUN_TOKEN: Mutex<Option<String>> = Mutex::new(None);

/// What is wrong with the token, for the Agent page: it was not kept, or could not be made.
static TOKEN_PROBLEM: Mutex<Option<String>> = Mutex::new(None);

fn run_token() -> Option<String> {
    RUN_TOKEN.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

fn token_problem() -> Option<String> {
    TOKEN_PROBLEM.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

fn set_token_problem(problem: Option<String>) {
    *TOKEN_PROBLEM.lock().unwrap_or_else(|e| e.into_inner()) = problem;
}

/// Fill in a token if there is none (the first start), persisting it, and return the settings as
/// they stand. A token that cannot be made leaves none (the server then refuses to start while one
/// is required); one that cannot be saved is kept for this run and the problem is reported.
fn with_token(app: &AppHandle, mut settings: Settings) -> Settings {
    if settings.mcp_token.is_some() {
        return settings;
    }
    match generate_token() {
        Ok(token) => match crate::settings::update(app, &serde_json::json!({ "mcp_token": token })) {
            Ok(stored) => settings = stored,
            Err(e) => {
                tracing::warn!(error = %e, "could not store the MCP token; using one for this run only");
                set_token_problem(Some(format!(
                    "The token could not be saved ({e}). It works until Kerf quits; the next start makes a new one."
                )));
                *RUN_TOKEN.lock().unwrap_or_else(|e| e.into_inner()) = Some(token.clone());
                settings.mcp_token = Some(token);
            }
        },
        Err(e) => {
            tracing::error!(error = %e, "could not make an MCP token");
            set_token_problem(Some(format!("No token could be made: {e}.")));
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
    super::activity::pump_project();
    let app = app.clone();
    tauri::async_runtime::spawn(supervise(move || KerfMcp::new(project.clone(), app.clone()), rx));
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

/// The session tables of the listeners that are up (there is one in the app; a test may start more).
static SESSIONS: Mutex<Vec<Arc<LocalSessionManager>>> = Mutex::new(Vec::new());

/// Whether a listener has ever come up in this process: before the first, no session can be judged gone.
static EVER_LISTENED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn register_sessions(sessions: &Arc<LocalSessionManager>) {
    SESSIONS.lock().unwrap_or_else(|e| e.into_inner()).push(sessions.clone());
    EVER_LISTENED.store(true, std::sync::atomic::Ordering::SeqCst);
}

fn forget_sessions(sessions: &Arc<LocalSessionManager>) {
    SESSIONS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .retain(|s| !Arc::ptr_eq(s, sessions));
}

/// Session ids that have made a call, newest last (a bounded list): the ones whose absence from a
/// listener means something. A proposal's owner is always one of these, so an id never seen — a test's,
/// or a client that reached the project some other way — is never judged gone.
static SEEN: Mutex<std::collections::VecDeque<String>> = Mutex::new(std::collections::VecDeque::new());
const SEEN_MAX: usize = 64;

/// Note that the session `id` made a call (it exists, and from here on its disappearance is evidence).
pub fn note_session_seen(id: &str) {
    let mut seen = SEEN.lock().unwrap_or_else(|e| e.into_inner());
    if !seen.iter().any(|s| s == id) {
        seen.push_back(id.to_string());
        if seen.len() > SEEN_MAX {
            seen.pop_front();
        }
    }
}

/// Whether the session `id` is on a listener that is up. A table being written counts as present.
fn session_exists(id: &str) -> bool {
    if !EVER_LISTENED.load(std::sync::atomic::Ordering::SeqCst) {
        return true;
    }
    SESSIONS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .any(|s| s.sessions.try_read().map(|table| table.contains_key(id)).unwrap_or(true))
}

/// Whether the MCP session `id` should be treated as still there. A session that made a call
/// ([`note_session_seen`]) and is no longer on any listener — its connection closed, it expired, or
/// the server restarted with a new token — is gone, and a proposal it owned is released
/// ([`kerf_core::Project::release_orphaned_staging`]). Ownership is only ever let go of on that
/// evidence: an id never seen, or asked about before a listener has come up, is alive.
pub fn session_alive(id: &str) -> bool {
    let seen = SEEN.lock().unwrap_or_else(|e| e.into_inner()).iter().any(|s| s == id);
    !seen || session_exists(id)
}

struct Running {
    sessions: Arc<LocalSessionManager>,
    stop: CancellationToken,
    task: tauri::async_runtime::JoinHandle<std::io::Result<()>>,
}

impl Running {
    /// Close the listener and every session on it; sessions that do not wind down in a couple of
    /// seconds are cut.
    async fn stop(self) {
        self.stop.cancel();
        // Every session of this listener ends with it.
        forget_sessions(&self.sessions);
        let mut task = self.task;
        if tokio::time::timeout(std::time::Duration::from_secs(2), &mut task)
            .await
            .is_err()
        {
            task.abort();
        }
    }
}

/// Whether the listener has to be stopped and started again for `next` (it is up now if `running`).
/// A new address or the server switched on or off does, and so does a different token or the
/// requirement for one switched: the sessions opened under the old credentials end with the
/// listener, and a session id from before no longer answers. A server that is not up retries on any
/// change.
fn needs_restart(config: &McpConfig, next: &McpConfig, running: bool) -> bool {
    next.enabled != config.enabled
        || next.addr != config.addr
        || next.token != config.token
        || next.require_token != config.require_token
        || (next.enabled && !running)
}

async fn supervise<S, F>(factory: F, mut rx: tokio::sync::watch::Receiver<McpConfig>)
where
    S: rmcp::ServerHandler + Send + 'static,
    F: Fn() -> S + Clone + Send + Sync + 'static,
{
    let mut config = rx.borrow_and_update().clone();
    let auth = Arc::new(RwLock::new(Auth::of(&config)));
    loop {
        *auth.write().unwrap_or_else(|e| e.into_inner()) = Auth::of(&config);
        let mut running = None;
        if config.enabled {
            match launch(&config, factory.clone(), auth.clone()).await {
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
        // A new address, a new token (or the requirement for one) and the server being switched on
        // or off all restart the listener. A server that is not up retries on any change.
        loop {
            if rx.changed().await.is_err() {
                if let Some(up) = running {
                    up.stop().await;
                }
                return;
            }
            let next = rx.borrow_and_update().clone();
            *auth.write().unwrap_or_else(|e| e.into_inner()) = Auth::of(&next);
            let restart = needs_restart(&config, &next, running.is_some());
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

/// The listener's settings for rmcp: who may name which `Host`, and no browser `Origin` at all.
fn http_config(addr: &str) -> StreamableHttpServerConfig {
    StreamableHttpServerConfig::default()
        .with_allowed_hosts(allowed_hosts(addr))
        // No Origin is allowed: a native MCP client sends none, and a page in the user's
        // browser must not be able to reach the editor through the loopback port.
        .enforce_origin_validation()
}

async fn launch<S, F>(config: &McpConfig, factory: F, auth: Arc<RwLock<Auth>>) -> anyhow::Result<Running>
where
    S: rmcp::ServerHandler + Send + 'static,
    F: Fn() -> S + Send + Sync + 'static,
{
    if let Some(why) = bind_problem(&config.addr, config.require_token) {
        anyhow::bail!(why);
    }
    check_addr(&config.addr).map_err(anyhow::Error::msg)?;
    if config.require_token && config.token.is_none() {
        anyhow::bail!(
            "there is no token to require, so the server stays off; turn the token requirement off or make a new token"
        );
    }
    let stop = CancellationToken::new();
    let sessions = Arc::new(LocalSessionManager::default());
    let service = StreamableHttpService::new(
        move || Ok(factory()),
        sessions.clone(),
        http_config(&config.addr).with_cancellation_token(stop.child_token()),
    );
    let router = axum::Router::new()
        .nest_service("/mcp", service)
        .layer(middleware::from_fn_with_state(auth, require_bearer));
    let listener = tokio::net::TcpListener::bind(&config.addr).await?;
    register_sessions(&sessions);
    tracing::info!(addr = %config.addr, token = config.require_token, "kerf MCP server listening on http://{}/mcp", config.addr);
    let shutdown = stop.clone();
    let task = tauri::async_runtime::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(shutdown.cancelled_owned())
            .await
    });
    Ok(Running { sessions, stop, task })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_listener_is_only_reported_up_for_the_address_it_was_started_on() {
        let up = Status {
            running: true,
            error: None,
            addr: Some("127.0.0.1:7777".into()),
        };
        assert_eq!(listener_state(&up, "127.0.0.1:7777"), (true, None));
        assert_eq!(
            listener_state(&up, "127.0.0.1:9000"),
            (false, None),
            "not yet restarted for the new one"
        );
        let failed = Status {
            running: false,
            error: Some("in use".into()),
            addr: Some("127.0.0.1:7777".into()),
        };
        assert_eq!(listener_state(&failed, "127.0.0.1:7777"), (false, Some("in use".into())));
        assert_eq!(
            listener_state(&failed, "127.0.0.1:9000"),
            (false, None),
            "the old address's error is not this one's"
        );
        assert_eq!(listener_state(&Status::default(), "127.0.0.1:7777"), (false, None));
    }

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
        let (a, b) = (generate_token().unwrap(), generate_token().unwrap());
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
        assert!(!constant_time_eq(b"abc", b"abx"));
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
        try_status_of(addr, headers).unwrap_or_else(|| panic!("no answer from {addr}"))
    }

    /// [`status_of`] for a server that may be down (or restarting): `None` when nothing answers.
    fn try_status_of(addr: std::net::SocketAddr, headers: &[(&str, &str)]) -> Option<u16> {
        use std::io::{Read, Write};
        let mut request = "POST /mcp HTTP/1.1\r\nContent-Length: 2\r\nConnection: close\r\n".to_string();
        if !headers.iter().any(|(name, _)| name.eq_ignore_ascii_case("host")) {
            request.push_str(&format!("Host: {addr}\r\n"));
        }
        for (name, value) in headers {
            request.push_str(&format!("{name}: {value}\r\n"));
        }
        request.push_str("\r\n{}");
        let mut stream = std::net::TcpStream::connect_timeout(&addr, std::time::Duration::from_secs(2)).ok()?;
        stream.set_read_timeout(Some(std::time::Duration::from_secs(5))).ok()?;
        stream.write_all(request.as_bytes()).ok()?;
        let mut response = String::new();
        let _ = stream.read_to_string(&mut response);
        response.split_whitespace().nth(1).and_then(|code| code.parse().ok())
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
    fn a_changed_token_or_requirement_restarts_the_listener_and_other_settings_do_not() {
        let base = McpConfig {
            require_token: true,
            ..config_for("127.0.0.1:7777", &"a".repeat(40), true)
        };
        assert!(!needs_restart(&base, &base.clone(), true), "nothing moved");
        let other_token = McpConfig {
            token: Some("b".repeat(40)),
            ..base.clone()
        };
        assert!(needs_restart(&base, &other_token, true), "a new token ends the old sessions");
        let loose = McpConfig {
            require_token: false,
            ..base.clone()
        };
        assert!(needs_restart(&base, &loose, true));
        assert!(needs_restart(&loose, &base, true));
        let elsewhere = McpConfig {
            addr: "127.0.0.1:7778".to_string(),
            ..base.clone()
        };
        assert!(needs_restart(&base, &elsewhere, true));
        let off = McpConfig {
            enabled: false,
            ..base.clone()
        };
        assert!(needs_restart(&base, &off, true));
        assert!(!needs_restart(&off, &off.clone(), false), "off stays off");
        assert!(needs_restart(&base, &base.clone(), false), "a server that is not up retries");
    }

    /// One request over a plain socket: the status and the headers (names lower-cased), read as
    /// far as the end of the headers.
    fn exchange(addr: std::net::SocketAddr, headers: &[(&str, &str)], body: &str) -> Option<(u16, Vec<(String, String)>)> {
        use std::io::{Read, Write};
        let mut request = format!(
            "POST /mcp HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\nAccept: application/json, text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n",
            body.len()
        );
        for (name, value) in headers {
            request.push_str(&format!("{name}: {value}\r\n"));
        }
        request.push_str("\r\n");
        request.push_str(body);
        let mut stream = std::net::TcpStream::connect_timeout(&addr, std::time::Duration::from_secs(2)).ok()?;
        stream.set_read_timeout(Some(std::time::Duration::from_secs(5))).ok()?;
        stream.write_all(request.as_bytes()).ok()?;
        let mut head = Vec::new();
        let mut byte = [0u8; 1];
        while !head.ends_with(b"\r\n\r\n") {
            if stream.read(&mut byte).ok()? == 0 {
                break;
            }
            head.push(byte[0]);
        }
        let text = String::from_utf8_lossy(&head).into_owned();
        let mut lines = text.lines();
        let status = lines.next()?.split_whitespace().nth(1)?.parse().ok()?;
        let headers = lines
            .filter_map(|l| l.split_once(':'))
            .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
            .collect();
        Some((status, headers))
    }

    const INITIALIZE: &str = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"test","version":"0"}}}"#;
    const INITIALIZED: &str = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;

    #[test]
    fn a_token_never_reaches_a_debug_print() {
        let token = "q".repeat(48);
        let config = mcp_config_with(&token);
        let shown = format!(
            "{config:?} {:?}",
            Auth {
                require_token: true,
                token: Some(token.clone()),
            }
        );
        assert!(!shown.contains(&token), "{shown}");
        assert!(shown.contains("<redacted>") && shown.contains("127.0.0.1:7777"), "{shown}");
    }

    fn mcp_config_with(token: &str) -> McpConfig {
        McpConfig {
            require_token: true,
            ..config_for("127.0.0.1:7777", token, true)
        }
    }

    #[test]
    fn a_session_that_never_made_a_call_is_never_judged_gone() {
        assert!(session_alive("an-id-nobody-has-used"), "no evidence it ever existed");
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

    /// A server with no tools: enough for rmcp to answer, so the listener's own rules can be tried.
    #[derive(Clone)]
    struct Nothing;
    impl rmcp::ServerHandler for Nothing {}

    fn free_port() -> u16 {
        std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    }

    /// Whether anything accepts connections at `addr`.
    fn listening(addr: &str) -> bool {
        std::net::TcpStream::connect_timeout(&addr.parse().unwrap(), std::time::Duration::from_millis(300)).is_ok()
    }

    async fn until(what: &str, mut condition: impl FnMut() -> bool) {
        for _ in 0..100 {
            if condition() {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        panic!("timed out waiting for {what}");
    }

    fn config_for(addr: &str, token: &str, enabled: bool) -> McpConfig {
        McpConfig {
            enabled,
            addr: addr.to_string(),
            require_token: true,
            token: Some(token.to_string()),
        }
    }

    /// The listener restarts by itself when the address moves or the switch is turned off, and a
    /// new token applies to the very next request without a restart.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_server_follows_its_settings_live() {
        let (a, b) = (format!("127.0.0.1:{}", free_port()), format!("127.0.0.1:{}", free_port()));
        let (tx, rx) = tokio::sync::watch::channel(config_for(&a, &"1".repeat(40), true));
        let task = tokio::spawn(supervise(|| Nothing, rx));
        let ask = |addr: &str, token: Option<&str>| {
            let addr: std::net::SocketAddr = addr.parse().unwrap();
            let header = token.map(|t| format!("Bearer {t}"));
            move || {
                let headers: Vec<(&str, &str)> = header.iter().map(|h| ("Authorization", h.as_str())).collect();
                status_of(addr, &headers)
            }
        };

        until("the server to come up", || listening(&a)).await;
        assert!(server_error().is_none());
        assert_eq!(tokio::task::spawn_blocking(ask(&a, None)).await.unwrap(), 401, "no token");
        assert_ne!(
            tokio::task::spawn_blocking(ask(&a, Some(&"1".repeat(40)))).await.unwrap(),
            401,
            "the token opens the door"
        );

        // A new token: the listener restarts (see `a_new_token_ends_the_sessions_opened_under_the_old_one`),
        // and from then on only the new one gets in.
        tx.send_replace(config_for(&a, &"2".repeat(40), true));
        let (old_token, new_token) = ("1".repeat(40), "2".repeat(40));
        let (old_at, new_at) = (a.parse().unwrap(), a.parse().unwrap());
        until("the new token to apply", || {
            let new_in = try_status_of(new_at, &[("Authorization", &format!("Bearer {new_token}"))]);
            let old_out = try_status_of(old_at, &[("Authorization", &format!("Bearer {old_token}"))]);
            matches!(new_in, Some(code) if code != 401) && old_out == Some(401)
        })
        .await;
        assert_ne!(ask(&a, Some(&"2".repeat(40)))(), 401);

        // A new address: the old one stops, the new one starts.
        tx.send_replace(config_for(&b, &"2".repeat(40), true));
        until("the new address to listen", || listening(&b)).await;
        until("the old address to stop", || !listening(&a)).await;
        assert_eq!(bind_addr(), b);

        // Switched off: nothing listens, and that is not an error.
        tx.send_replace(config_for(&b, &"2".repeat(40), false));
        until("the server to stop", || !listening(&b)).await;
        assert!(server_error().is_none());

        // On again.
        tx.send_replace(config_for(&b, &"2".repeat(40), true));
        until("the server to come back", || listening(&b)).await;

        // An address it will not serve is reported, and the server stays down until one it will.
        tx.send_replace(McpConfig {
            require_token: false,
            ..config_for("0.0.0.0:7777", &"2".repeat(40), true)
        });
        until("the refusal to be reported", || server_error().is_some()).await;
        assert!(server_error().unwrap().contains("refusing to listen"), "{:?}", server_error());
        assert!(!listening(&b));
        tx.send_replace(config_for(&b, &"2".repeat(40), true));
        until("the server to recover", || listening(&b) && server_error().is_none()).await;

        drop(tx);
        task.await.unwrap();
        assert!(!listening(&b), "the server stops with its supervisor");
    }

    /// One MCP `initialize` over a plain socket: the session id the server hands out, if it lets us in.
    async fn open_session(addr: std::net::SocketAddr, token: String) -> Option<String> {
        tokio::task::spawn_blocking(move || {
            let auth = format!("Bearer {token}");
            let (status, headers) = exchange(addr, &[("Authorization", auth.as_str())], INITIALIZE)?;
            (status == 200)
                .then(|| headers.into_iter().find(|(k, _)| k == "mcp-session-id").map(|(_, v)| v))
                .flatten()
        })
        .await
        .unwrap()
    }

    /// A notification on an existing session: the status the server answers, `None` when it is down.
    async fn ping_session(addr: std::net::SocketAddr, token: String, session: String) -> Option<u16> {
        tokio::task::spawn_blocking(move || {
            let auth = format!("Bearer {token}");
            exchange(
                addr,
                &[("Authorization", auth.as_str()), ("Mcp-Session-Id", session.as_str())],
                INITIALIZED,
            )
            .map(|r| r.0)
        })
        .await
        .unwrap()
    }

    /// The real listener, the real middleware and a real session table: a session opened with one
    /// token stops answering once the token is replaced — its id is gone with the listener, so a
    /// client holding it has to start over with the new token — and the old token opens nothing.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_new_token_ends_the_sessions_opened_under_the_old_one() {
        let text = format!("127.0.0.1:{}", free_port());
        let addr: std::net::SocketAddr = text.parse().unwrap();
        let (old, new) = ("a".repeat(48), "b".repeat(48));
        let (tx, rx) = tokio::sync::watch::channel(config_for(&text, &old, true));
        let task = tokio::spawn(supervise(|| Nothing, rx));
        until("the server to come up", || listening(&text)).await;

        let session = open_session(addr, old.clone()).await.expect("the token opens a session");
        note_session_seen(&session);
        assert!(session_alive(&session), "a session of the listener that is up");
        // This session opens a proposal; it owns it.
        let shared = Mutex::new(Project::open_in_memory().unwrap());
        {
            let mine = super::super::lock_agent_edit(&shared, Some(&session)).expect("the first edit opens the proposal");
            mine.add_overlay("Title".to_string(), 0.0, 2.0).unwrap();
            assert_eq!(mine.staged().unwrap().unwrap().owner.as_deref(), Some(session.as_str()));
        }
        assert_eq!(ping_session(addr, old.clone(), session.clone()).await, Some(202));
        assert_eq!(
            ping_session(addr, new.clone(), session.clone()).await,
            Some(401),
            "wrong token"
        );

        // The token is replaced: the listener restarts, and the session it had is gone.
        tx.send_replace(config_for(&text, &new, true));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while ping_session(addr, new.clone(), session.clone()).await != Some(404) {
            assert!(
                std::time::Instant::now() < deadline,
                "the old session still answers after the token changed"
            );
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        assert!(!session_alive(&session), "the restarted listener has no such session");
        assert_eq!(
            ping_session(addr, old.clone(), session.clone()).await,
            Some(401),
            "the old token is refused"
        );
        assert!(open_session(addr, old).await.is_none(), "and opens no new session");
        let fresh = open_session(addr, new.clone()).await.expect("the new token opens one");
        note_session_seen(&fresh);
        assert!(session_alive(&fresh));
        // The agent that reconnected is not told another agent owns its own proposal: the session
        // that made it is gone, so the new one takes it over.
        {
            let again = super::super::lock_agent_edit(&shared, Some(&fresh)).expect("a gone session's proposal is released");
            again.add_overlay("Another".to_string(), 3.0, 4.0).unwrap();
            let staged = again.staged().unwrap().unwrap();
            assert_eq!(staged.owner.as_deref(), Some(fresh.as_str()));
            assert_eq!(staged.edits.len(), 2, "the earlier steps are still in it");
        }
        // A session still on the listener keeps its proposal against a stranger.
        let stranger = open_session(addr, new.clone()).await.expect("another session");
        note_session_seen(&stranger);
        assert!(super::super::lock_agent_edit(&shared, Some(&stranger)).is_err());
        assert_eq!(ping_session(addr, new, fresh).await, Some(202));

        drop(tx);
        task.await.unwrap();
    }

    /// A page in the user's browser must not be able to reach the editor; a native client, which
    /// sends no `Origin`, is not turned away on that ground.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_browser_origin_is_refused_and_a_native_client_is_not() {
        let auth = Arc::new(RwLock::new(Auth::default()));
        let addr = format!("127.0.0.1:{}", free_port());
        let config = McpConfig {
            enabled: true,
            addr: addr.clone(),
            require_token: false,
            token: None,
        };
        let up = launch(&config, || Nothing, auth).await.expect("bind");
        let socket: std::net::SocketAddr = addr.parse().unwrap();
        let ask =
            move |headers: Vec<(&'static str, &'static str)>| tokio::task::spawn_blocking(move || status_of(socket, &headers));
        assert_eq!(
            ask(vec![("Origin", "https://evil.example")]).await.unwrap(),
            403,
            "a web page"
        );
        assert_eq!(ask(vec![("Origin", "null")]).await.unwrap(), 403, "a sandboxed page");
        assert_eq!(
            ask(vec![("Origin", "http://localhost:3000")]).await.unwrap(),
            403,
            "even a local page"
        );
        let native = ask(vec![("Content-Type", "application/json")]).await.unwrap();
        assert_ne!(native, 403, "no Origin is a native client");
        assert_eq!(
            ask(vec![("Host", "evil.example")]).await.unwrap(),
            403,
            "a rebinding page names its own Host"
        );
        up.stop().await;
    }

    /// The activity stream and nothing else: what `KerfMcp` does with its listener.
    #[derive(Clone, Default)]
    struct Streaming(super::super::activity::Listener);

    impl rmcp::ServerHandler for Streaming {
        fn get_info(&self) -> rmcp::model::ServerConfig {
            let mut info = rmcp::model::ServerConfig::default();
            info.capabilities = rmcp::model::ServerCapabilities::builder()
                .enable_resources()
                .enable_resources_subscribe()
                .build();
            info
        }

        async fn on_initialized(&self, context: rmcp::service::NotificationContext<rmcp::service::RoleServer>) {
            self.0.start(context.peer);
        }

        #[allow(deprecated)]
        async fn subscribe(
            &self,
            request: rmcp::model::SubscribeRequestParams,
            _context: rmcp::service::RequestContext<rmcp::service::RoleServer>,
        ) -> Result<(), rmcp::ErrorData> {
            self.0.subscribe(&request.uri)
        }
    }

    /// Open the session's standalone stream (`GET /mcp`) and read it until a line holds every one of
    /// `needles` or `wait` runs out: the line, or the status when the stream was refused (200 when it
    /// was opened but nothing matched).
    fn stream_until(
        addr: std::net::SocketAddr,
        headers: &[(&str, &str)],
        needles: &[&str],
        wait: std::time::Duration,
    ) -> Result<String, u16> {
        use std::io::{BufRead, BufReader, Write};
        let mut request = format!("GET /mcp HTTP/1.1\r\nHost: {addr}\r\nAccept: text/event-stream\r\n");
        for (name, value) in headers {
            request.push_str(&format!("{name}: {value}\r\n"));
        }
        request.push_str("\r\n");
        let mut stream = std::net::TcpStream::connect_timeout(&addr, std::time::Duration::from_secs(2)).map_err(|_| 0u16)?;
        stream
            .set_read_timeout(Some(std::time::Duration::from_millis(500)))
            .map_err(|_| 0u16)?;
        stream.write_all(request.as_bytes()).map_err(|_| 0u16)?;
        let mut reader = BufReader::new(stream);
        let mut status = String::new();
        reader.read_line(&mut status).map_err(|_| 0u16)?;
        let code: u16 = status.split_whitespace().nth(1).and_then(|c| c.parse().ok()).unwrap_or(0);
        if code != 200 {
            return Err(code);
        }
        let deadline = std::time::Instant::now() + wait;
        let mut line = String::new();
        while std::time::Instant::now() < deadline {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) => break,
                Ok(_) if needles.iter().all(|n| line.contains(n)) => return Ok(line.trim().to_string()),
                _ => {}
            }
        }
        Err(200)
    }

    /// One JSON-RPC message on a session: the status it is answered with.
    async fn send_on(addr: std::net::SocketAddr, token: String, session: String, body: &'static str) -> Option<u16> {
        tokio::task::spawn_blocking(move || {
            let auth = format!("Bearer {token}");
            exchange(
                addr,
                &[("Authorization", auth.as_str()), ("Mcp-Session-Id", session.as_str())],
                body,
            )
            .map(|r| r.0)
        })
        .await
        .unwrap()
    }

    /// The real listener, middleware and session: a session that subscribed to `kerf://jobs` is told on
    /// its standalone stream when a job moves, and a stream opened without the token is refused.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_subscribed_session_hears_a_job_move_on_its_stream() {
        use super::super::activity::{publish, Event};
        let text = format!("127.0.0.1:{}", free_port());
        let addr: std::net::SocketAddr = text.parse().unwrap();
        let token = "s".repeat(48);
        let (tx, rx) = tokio::sync::watch::channel(config_for(&text, &token, true));
        let task = tokio::spawn(supervise(Streaming::default, rx));
        until("the server to come up", || listening(&text)).await;

        let session = open_session(addr, token.clone()).await.expect("a session");
        assert_eq!(send_on(addr, token.clone(), session.clone(), INITIALIZED).await, Some(202));
        const SUBSCRIBE: &str = r#"{"jsonrpc":"2.0","id":2,"method":"resources/subscribe","params":{"uri":"kerf://jobs"}}"#;
        assert_eq!(send_on(addr, token.clone(), session.clone(), SUBSCRIBE).await, Some(200));

        let refused = {
            let session = session.clone();
            tokio::task::spawn_blocking(move || {
                stream_until(
                    addr,
                    &[("Mcp-Session-Id", session.as_str())],
                    &["kerf://jobs"],
                    std::time::Duration::from_secs(1),
                )
            })
            .await
            .unwrap()
        };
        assert_eq!(refused, Err(401), "the stream asks for the token like every request");

        let reading = {
            let (session, auth) = (session.clone(), format!("Bearer {token}"));
            tokio::task::spawn_blocking(move || {
                stream_until(
                    addr,
                    &[("Authorization", auth.as_str()), ("Mcp-Session-Id", session.as_str())],
                    &["notifications/resources/updated", "\"kerf://jobs\""],
                    std::time::Duration::from_secs(20),
                )
            })
        };
        // The stream opens on its own thread: keep the job moving until it has been heard.
        while !reading.is_finished() {
            publish(Event::Job("job-stream".to_string()));
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        let heard = reading.await.unwrap().expect("the update reached the stream");
        assert!(heard.starts_with("data:"), "{heard}");

        drop(tx);
        task.await.unwrap();
    }
}
