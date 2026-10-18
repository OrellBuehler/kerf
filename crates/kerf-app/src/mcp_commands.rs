//! The webview's side of the agent connection: what to paste into an MCP client, the token, and
//! the switches for the server (see `mcp/server.rs`). Thin: the rules live in `settings` and
//! `mcp::server`.

use serde::Serialize;
use tauri::AppHandle;

use crate::mcp::{self, ConnectionInfo};
use crate::settings;

type CmdResult<T> = Result<T, String>;

/// What the Agent settings page shows and the user copies into a client.
#[derive(Debug, Clone, Serialize)]
pub struct McpConnectionInfo {
    /// Whether the server is switched on.
    pub enabled: bool,
    /// Where a client connects, e.g. `http://127.0.0.1:7777/mcp`.
    pub url: String,
    /// Whether requests must carry the token.
    pub require_token: bool,
    /// The bearer token (always present once the app has started once; sent even when it is not
    /// required so the page can show it before the user turns the requirement back on).
    pub token: Option<String>,
    /// Whether the listener is up now.
    pub running: bool,
    /// Why it is not, when it should be.
    pub error: Option<String>,
    /// The address stored in the settings (`null` is the default); what the field edits.
    pub configured_addr: Option<String>,
    /// Whether `KERF_MCP_ADDR` is set and so wins over `configured_addr`.
    pub env_override: bool,
    /// Whether an agent's edits wait for the user's review.
    pub agent_review: bool,
    /// The one-line command that adds Kerf to Claude Code.
    pub claude_code_command: String,
    /// A JSON snippet (`mcpServers`) for any other MCP client.
    pub generic_json_config: String,
}

impl From<ConnectionInfo> for McpConnectionInfo {
    fn from(c: ConnectionInfo) -> Self {
        Self {
            enabled: c.enabled,
            url: c.url,
            require_token: c.require_token,
            token: c.token,
            running: c.running,
            error: c.error,
            configured_addr: c.configured_addr,
            env_override: c.env_override,
            agent_review: c.agent_review,
            claude_code_command: c.claude_code_command,
            generic_json_config: c.generic_json_config,
        }
    }
}

fn info(app: &AppHandle) -> McpConnectionInfo {
    mcp::connection_info(&settings::load(app)).into()
}

/// The endpoint, token and ready-made client configuration, and whether the server is up.
#[tauri::command(async)]
pub fn mcp_connection_info(app: AppHandle) -> McpConnectionInfo {
    info(&app)
}

/// Make a new token. The old one stops working at once; every client has to be given the new one.
#[tauri::command(async)]
pub fn regenerate_mcp_token(app: AppHandle) -> CmdResult<String> {
    let token = mcp::generate_token();
    settings::update(&app, &serde_json::json!({ "mcp_token": token }))?;
    Ok(token)
}

/// The agent switches: `{ mcp_enabled, mcp_addr, mcp_require_token, agent_review }`, any of
/// them. Anything else in the patch is refused, so this cannot be used to write the layout or the
/// token. The server restarts by itself when the address or the switch moves. Answers the
/// connection info as it is after.
#[tauri::command(async)]
pub fn set_mcp_settings(app: AppHandle, patch: serde_json::Value) -> CmdResult<McpConnectionInfo> {
    const ALLOWED: [&str; 4] = ["mcp_enabled", "mcp_addr", "mcp_require_token", "agent_review"];
    let serde_json::Value::Object(fields) = &patch else {
        return Err("an agent settings patch must be an object".to_string());
    };
    if let Some(other) = fields.keys().find(|k| !ALLOWED.contains(&k.as_str())) {
        return Err(format!("`{other}` is not an agent setting"));
    }
    settings::update(&app, &patch)?;
    Ok(info(&app))
}
