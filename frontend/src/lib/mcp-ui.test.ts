import { describe, expect, test } from 'bun:test';
import { addressProblem, envOverrideNote, isLoopbackAddr, lastSeenLine, maskSecret, mcpStatus, networkWarning, tokenDisplay } from './mcp-ui';
import { clientHelps } from './mcp-clients';

const info = {
	url: 'http://127.0.0.1:7777/mcp',
	token: 'abc123secret',
	require_token: true,
	claude_code_command: 'claude mcp add --transport http kerf http://127.0.0.1:7777/mcp --header "Authorization: Bearer abc123secret"',
	generic_json_config: '{"mcpServers":{"kerf":{"type":"http","url":"http://127.0.0.1:7777/mcp"}}}'
};

describe('the listener in a line', () => {
	test('off, listening, refused and starting', () => {
		expect(mcpStatus({ enabled: false, running: false, error: null, url: 'u' })).toEqual({ tone: 'off', text: 'Off — no agent can connect' });
		expect(mcpStatus({ enabled: true, running: true, error: null, url: 'http://x/mcp' })).toEqual({ tone: 'ok', text: 'Listening on http://x/mcp' });
		expect(mcpStatus({ enabled: true, running: false, error: 'the port is taken', url: 'u' })).toEqual({
			tone: 'danger',
			text: 'Not listening — the port is taken'
		});
		expect(mcpStatus({ enabled: true, running: false, error: null, url: 'u' }).tone).toBe('warn');
	});
});

describe('addresses', () => {
	test('loopback is this machine only', () => {
		for (const a of ['127.0.0.1:7777', '127.9.9.9:1', 'localhost:80', '[::1]:7777']) expect(isLoopbackAddr(a), a).toBe(true);
		for (const a of ['0.0.0.0:7777', '192.168.1.5:7777', 'example.com:80']) expect(isLoopbackAddr(a), a).toBe(false);
	});

	test('an empty box is the default; the obvious mistakes are said at once', () => {
		expect(addressProblem('', true)).toBeNull();
		expect(addressProblem('  ', false)).toBeNull();
		expect(addressProblem('127.0.0.1:7777', true)).toBeNull();
		expect(addressProblem('[::1]:9000', false)).toBeNull();
		expect(addressProblem('7777', true)).toMatch(/host:port/);
		expect(addressProblem('localhost', true)).toMatch(/host:port/);
		expect(addressProblem('127.0.0.1:0', true)).toMatch(/1 to 65535/);
		expect(addressProblem('127.0.0.1:70000', true)).toMatch(/1 to 65535/);
	});

	test('a network address needs the token', () => {
		expect(addressProblem('0.0.0.0:7777', false)).toMatch(/reachable from the network/);
		expect(addressProblem('0.0.0.0:7777', true)).toBeNull();
		expect(networkWarning('0.0.0.0:7777', true)).toMatch(/not encrypted/);
		expect(networkWarning('127.0.0.1:7777', true)).toBeNull();
		expect(networkWarning(null, true)).toBeNull();
	});

	test('the environment override says it wins', () => {
		expect(envOverrideNote({ env_override: false, url: 'u' })).toBeNull();
		expect(envOverrideNote({ env_override: true, url: 'http://127.0.0.1:9/mcp' })).toContain('KERF_MCP_ADDR');
		expect(envOverrideNote({ env_override: true, url: 'http://127.0.0.1:9/mcp' })).toContain('http://127.0.0.1:9/mcp');
	});
});

describe('the secret', () => {
	test('is dots until it is shown, in the field and in a snippet', () => {
		expect(tokenDisplay(null, true)).toBe('No token yet');
		expect(tokenDisplay('abc', false)).not.toContain('abc');
		expect(tokenDisplay('abc', true)).toBe('abc');
		expect(maskSecret(info.claude_code_command, info.token, false)).not.toContain('abc123secret');
		expect(maskSecret(info.claude_code_command, info.token, true)).toBe(info.claude_code_command);
		expect(maskSecret('no token here', null, false)).toBe('no token here');
	});
});

describe('the other clients', () => {
	test('each gets the endpoint in its own shape, the token in only when it is required', () => {
		const helps = clientHelps(info);
		expect(helps.map((h) => h.id)).toEqual(['claude-code', 'json', 'vscode', 'desktop']);
		expect(helps[0].snippet).toBe(info.claude_code_command);
		expect(helps[1].snippet).toBe(info.generic_json_config);
		const vscode = JSON.parse(helps[2].snippet);
		expect(vscode.servers.kerf).toEqual({ type: 'http', url: info.url, headers: { Authorization: 'Bearer abc123secret' } });
		const desktop = JSON.parse(helps[3].snippet);
		expect(desktop.mcpServers.kerf.args).toEqual(['mcp-remote', info.url, '--header', 'Authorization:${KERF_AUTH}']);
		expect(desktop.mcpServers.kerf.env).toEqual({ KERF_AUTH: 'Bearer abc123secret' });

		const open = clientHelps({ ...info, require_token: false });
		expect(JSON.parse(open[2].snippet).servers.kerf.headers).toBeUndefined();
		expect(JSON.parse(open[3].snippet).mcpServers.kerf.env).toBeUndefined();
		expect(open[3].snippet).not.toContain('abc123secret');
	});
});

describe('when an agent last came', () => {
	test('reads in the unit that fits', () => {
		expect(lastSeenLine(null)).toBe('No agent has used it yet');
		expect(lastSeenLine(2)).toBe('An agent used it just now');
		expect(lastSeenLine(30)).toBe('Last used 30 s ago');
		expect(lastSeenLine(300)).toBe('Last used 5 min ago');
		expect(lastSeenLine(7200)).toBe('Last used 2 h ago');
	});
});
