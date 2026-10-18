// What the Agent settings say about the embedded MCP server, as words and checks. Pure, so the sentences and the
// address rules are held by a test (`mcp-ui.test.ts`); the server (`crates/kerf-app/src/mcp/server.rs`) judges an
// address for real, and what is checked here only saves a round trip for the obvious mistakes.

import type { McpConnectionInfo } from './types';

export type StatusTone = 'ok' | 'warn' | 'danger' | 'off';

export interface McpStatus {
	tone: StatusTone;
	text: string;
}

/** One line for the state of the listener. */
export function mcpStatus(info: Pick<McpConnectionInfo, 'enabled' | 'running' | 'error' | 'url'>): McpStatus {
	if (!info.enabled) return { tone: 'off', text: 'Off — no agent can connect' };
	if (info.running) return { tone: 'ok', text: `Listening on ${info.url}` };
	if (info.error) return { tone: 'danger', text: `Not listening — ${info.error}` };
	return { tone: 'warn', text: 'Starting…' };
}

/** `127.0.0.1:7777`, `[::1]:7777` or `localhost:7777` — reachable from this machine only. */
export function isLoopbackAddr(addr: string): boolean {
	return /^(127\.\d{1,3}\.\d{1,3}\.\d{1,3}|localhost|\[::1\]):/i.test(addr.trim());
}

/** What is wrong with a typed address before it is sent, or `null`. Empty means the default and is fine. */
export function addressProblem(text: string, requireToken: boolean): string | null {
	const addr = text.trim();
	if (addr === '') return null;
	const m = /^(\[[^\]]+\]|[^:\s/]+):(\d{1,5})$/.exec(addr);
	if (!m) return 'Write it as host:port, e.g. 127.0.0.1:7777';
	const port = Number(m[2]);
	if (port < 1 || port > 65535) return 'The port is a number from 1 to 65535';
	if (!isLoopbackAddr(addr) && !requireToken)
		return `${addr} is reachable from the network. Turn the access token on first, or listen on 127.0.0.1.`;
	return null;
}

/** Said under the address when a network address is in use: it is the one setting that opens the door wider. */
export function networkWarning(addr: string | null | undefined, requireToken: boolean): string | null {
	if (!addr || isLoopbackAddr(addr)) return null;
	return requireToken
		? 'This address is reachable from the network. Every request needs the access token, and traffic is not encrypted — use it on a network you trust.'
		: null;
}

/** `KERF_MCP_ADDR` in the environment wins over the address saved here. */
export function envOverrideNote(info: Pick<McpConnectionInfo, 'env_override' | 'url'>): string | null {
	return info.env_override
		? `KERF_MCP_ADDR is set in the environment and wins over this address — Kerf is listening on ${info.url}. Unset it and restart Kerf to use the address saved here.`
		: null;
}

/** The token as it is drawn: dots until it is asked for. */
export function tokenDisplay(token: string | null, shown: boolean): string {
	if (!token) return 'No token yet';
	return shown ? token : '•'.repeat(24);
}

/** A snippet for the screen: the secret is dots until it is shown. What is copied is the real text. */
export function maskSecret(text: string, token: string | null, shown: boolean): string {
	if (!token || shown) return text;
	return text.split(token).join('•'.repeat(12));
}

/** When an agent last spoke to the endpoint, for the line under the status. A streamable-HTTP client holds no
 *  connection between calls, so "connected" is a judgement from the age. */
export function lastSeenLine(secs: number | null): string {
	if (secs === null) return 'No agent has used it yet';
	if (secs < 5) return 'An agent used it just now';
	if (secs < 90) return `Last used ${Math.round(secs)} s ago`;
	if (secs < 5400) return `Last used ${Math.round(secs / 60)} min ago`;
	return `Last used ${Math.round(secs / 3600)} h ago`;
}

/** Where the listener has got to after a change, or `null` while it has not moved yet: it restarts on its own after
 *  a new address, a switch or a token change, a moment after the setting is saved. `up`: listening; `off`: switched off
 *  and down; `failed`: it did not start, with `error` saying why. */
export function listenerSettled(info: Pick<McpConnectionInfo, 'enabled' | 'running' | 'error'>): 'up' | 'off' | 'failed' | null {
	if (!info.enabled) return info.running ? null : 'off';
	if (info.running) return 'up';
	return info.error ? 'failed' : null;
}

export interface SettleOptions {
	/** Reads after the first; 20 at 250 ms is five seconds. */
	tries?: number;
	delayMs?: number;
	sleep?: (ms: number) => Promise<void>;
}

/** Read the connection state again until the listener has settled after a change (`first` is what the change
 *  answered), for a bounded time. Resolves to the last state read — settled or not — and never rejects: a read that
 *  fails stops the wait with what is known. */
export async function awaitListener(
	read: () => Promise<McpConnectionInfo>,
	first: McpConnectionInfo,
	opts: SettleOptions = {}
): Promise<McpConnectionInfo> {
	const tries = opts.tries ?? 20;
	const delay = opts.delayMs ?? 250;
	const sleep = opts.sleep ?? ((ms: number) => new Promise<void>((r) => setTimeout(r, ms)));
	let info = first;
	for (let i = 0; i < tries && listenerSettled(info) === null; i++) {
		await sleep(delay);
		try {
			info = await read();
		} catch {
			break;
		}
	}
	return info;
}

export interface AddressNotice {
	tone: 'success' | 'info' | 'error';
	title: string;
	description?: string;
}

/** What to tell the user once an address was saved and the listener has (or has not) followed. `wanted` is what was
 *  typed (`null`: the default). */
export function addressNotice(
	info: Pick<McpConnectionInfo, 'enabled' | 'running' | 'error' | 'url' | 'env_override'>,
	wanted: string | null
): AddressNotice {
	if (info.env_override)
		return {
			tone: 'info',
			title: 'Address saved',
			description: `KERF_MCP_ADDR is set in the environment and wins — Kerf ${info.running ? 'is listening' : 'will listen'} on ${info.url}.`
		};
	if (!info.enabled)
		return { tone: 'success', title: 'Address saved', description: 'The server is off; it listens there once you turn it on.' };
	if (info.running)
		return { tone: 'success', title: wanted === null ? 'Listening on the default address' : `Listening on ${wanted}`, description: info.url };
	if (info.error) return { tone: 'error', title: 'Couldn’t listen on that address', description: info.error };
	return { tone: 'info', title: 'Address saved', description: 'The server is still starting — the status above shows when it is up.' };
}

/** Whether a patch makes the listener restart. */
export function restartsListener(patch: { mcp_enabled?: boolean; mcp_addr?: string | null; mcp_require_token?: boolean }): boolean {
	return patch.mcp_enabled !== undefined || 'mcp_addr' in patch || patch.mcp_require_token !== undefined;
}
