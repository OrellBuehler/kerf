// How to point the MCP clients people use at Kerf. `McpConnectionInfo` carries the two snippets the backend builds
// (the Claude Code command and a generic `mcpServers` JSON); the rest are the same endpoint in the shape each other
// client asks for, built here from the URL and the token.

import type { McpConnectionInfo } from './types';

export interface ClientHelp {
	id: 'claude-code' | 'json' | 'vscode' | 'desktop';
	/** The chip. */
	name: string;
	/** The clients it covers, for the line under the chips. */
	covers: string;
	/** Where to put the snippet, or where to run it. */
	where: string;
	/** What to paste (the real text, token included). */
	snippet: string;
}

const header = (token: string | null, require: boolean): Record<string, string> | undefined =>
	require && token ? { Authorization: `Bearer ${token}` } : undefined;

/** The help for every client, the Claude Code command first. */
export function clientHelps(info: Pick<McpConnectionInfo, 'url' | 'token' | 'require_token' | 'claude_code_command' | 'generic_json_config'>): ClientHelp[] {
	const headers = header(info.token, info.require_token);
	const vscode = JSON.stringify({ servers: { kerf: { type: 'http', url: info.url, ...(headers ? { headers } : {}) } } }, null, 2);
	// mcp-remote reads a header's value from the environment, which keeps the space in `Bearer <token>` out of an argument.
	const bridge = JSON.stringify(
		{
			mcpServers: {
				kerf: {
					command: 'npx',
					args: ['mcp-remote', info.url, ...(headers ? ['--header', 'Authorization:${KERF_AUTH}'] : [])],
					...(headers ? { env: { KERF_AUTH: `Bearer ${info.token}` } } : {})
				}
			}
		},
		null,
		2
	);
	return [
		{
			id: 'claude-code',
			name: 'Claude Code',
			covers: 'Claude Code (the claude command line)',
			where: 'Run this once in a terminal:',
			snippet: info.claude_code_command
		},
		{
			id: 'json',
			name: 'Cursor & others',
			covers: 'Cursor, Windsurf, Cline, Zed and any client with an mcpServers JSON file',
			where: 'Add this to the client’s MCP config (Cursor: ~/.cursor/mcp.json):',
			snippet: info.generic_json_config
		},
		{
			id: 'vscode',
			name: 'VS Code',
			covers: 'VS Code with GitHub Copilot agent mode',
			where: 'Add this to .vscode/mcp.json (or run “MCP: Add Server” and paste the URL):',
			snippet: vscode
		},
		{
			id: 'desktop',
			name: 'Claude Desktop',
			covers: 'Claude Desktop, which reaches a local HTTP server through the mcp-remote bridge',
			where: 'Add this to claude_desktop_config.json (it needs Node.js for npx):',
			snippet: bridge
		}
	];
}
