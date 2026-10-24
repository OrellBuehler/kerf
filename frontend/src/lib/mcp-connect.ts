// What a user pastes into an MCP client to connect it to Kerf. The desktop app builds the real ones
// (`mcp_connection_info`); the browser harness builds the same text here so the Agent settings page
// can be worked on without the shell. A change to the wording belongs in
// `crates/kerf-app/src/mcp/server.rs` (`claude_code_command`, `generic_json_config`) too.

import type { McpConnectionInfo } from './types';

/** The one-line command that adds Kerf to Claude Code, with the token header when one is required. A URL with a
 *  query (`?tools=core`) is quoted, since a shell would read `?` as a pattern. */
export function claudeCodeCommand(url: string, token: string | null, requireToken: boolean): string {
	const base = `claude mcp add --transport http kerf ${url.includes('?') ? `"${url}"` : url}`;
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

/** The connection info for a client that asks for only some tool sets (`?tools=core,titles`): the URL and both
 *  snippets carry the selection. No sets is every tool, the info as it is. */
export function withToolSets(info: McpConnectionInfo, sets: readonly string[]): McpConnectionInfo {
	if (sets.length === 0) return info;
	const url = `${info.url}?tools=${sets.join(',')}`;
	return {
		...info,
		url,
		claude_code_command: claudeCodeCommand(url, info.token, info.require_token),
		generic_json_config: genericJsonConfig(url, info.token, info.require_token)
	};
}
