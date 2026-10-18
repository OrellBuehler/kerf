// What a user pastes into an MCP client to connect it to Kerf. The desktop app builds the real ones
// (`mcp_connection_info`); the browser harness builds the same text here so the Agent settings page
// can be worked on without the shell. A change to the wording belongs in
// `crates/kerf-app/src/mcp/server.rs` (`claude_code_command`, `generic_json_config`) too.

/** The one-line command that adds Kerf to Claude Code, with the token header when one is required. */
export function claudeCodeCommand(url: string, token: string | null, requireToken: boolean): string {
	const base = `claude mcp add --transport http kerf ${url}`;
	return requireToken && token ? `${base} --header "Authorization: Bearer ${token}"` : base;
}

/** A `mcpServers` snippet for any MCP client that takes a JSON config. */
export function genericJsonConfig(url: string, token: string | null, requireToken: boolean): string {
	const server: { type: string; url: string; headers?: Record<string, string> } = { type: 'http', url };
	if (requireToken && token) server.headers = { Authorization: `Bearer ${token}` };
	return JSON.stringify({ mcpServers: { kerf: server } }, null, 2);
}

/** The URL a client on this machine connects to for a server bound to `addr`: a wildcard bind is
 *  reached through loopback. */
export function clientUrl(addr: string): string {
	const m = /^(?:\[([^\]]*)\]|([^:]*)):(\d+)$/.exec(addr.trim());
	if (!m) return `http://${addr.trim()}/mcp`;
	const host = m[1] ?? m[2];
	const port = m[3];
	if (host === '0.0.0.0' || host === '::') return `http://127.0.0.1:${port}/mcp`;
	return m[1] !== undefined ? `http://[${host}]:${port}/mcp` : `http://${host}:${port}/mcp`;
}
