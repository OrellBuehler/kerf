import { describe, expect, test } from 'bun:test';
import { claudeCodeCommand, clientUrl, genericJsonConfig } from './mcp-connect';

// The Rust side (`mcp/server.rs`) builds these for the desktop app; these replay its tests so the
// browser harness shows the same text.

describe('claudeCodeCommand', () => {
	const url = 'http://127.0.0.1:7777/mcp';
	const token = 't'.repeat(40);

	test('carries the token header when one is required', () => {
		expect(claudeCodeCommand(url, token, true)).toBe(
			`claude mcp add --transport http kerf ${url} --header "Authorization: Bearer ${token}"`
		);
	});

	test('has no header when none is required, or none exists', () => {
		expect(claudeCodeCommand(url, token, false)).toBe(`claude mcp add --transport http kerf ${url}`);
		expect(claudeCodeCommand(url, null, true)).toBe(`claude mcp add --transport http kerf ${url}`);
	});
});

describe('genericJsonConfig', () => {
	const url = 'http://127.0.0.1:7777/mcp';
	const token = 'k'.repeat(40);

	test('names the server, its URL and the bearer header', () => {
		const config = JSON.parse(genericJsonConfig(url, token, true));
		expect(config.mcpServers.kerf.type).toBe('http');
		expect(config.mcpServers.kerf.url).toBe(url);
		expect(config.mcpServers.kerf.headers.Authorization).toBe(`Bearer ${token}`);
	});

	test('leaves the headers out when no token is required', () => {
		const config = JSON.parse(genericJsonConfig(url, token, false));
		expect(config.mcpServers.kerf.headers).toBeUndefined();
	});
});

describe('clientUrl', () => {
	test('is the address a client can reach', () => {
		expect(clientUrl('127.0.0.1:7777')).toBe('http://127.0.0.1:7777/mcp');
		expect(clientUrl('192.168.1.20:7777')).toBe('http://192.168.1.20:7777/mcp');
		expect(clientUrl('[::1]:7777')).toBe('http://[::1]:7777/mcp');
	});

	test('reaches a wildcard bind through loopback', () => {
		expect(clientUrl('0.0.0.0:9000')).toBe('http://127.0.0.1:9000/mcp');
		expect(clientUrl('[::]:9000')).toBe('http://127.0.0.1:9000/mcp');
	});
});
