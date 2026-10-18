<script lang="ts">
	// Agent: whether an AI agent may connect (the embedded MCP server), where, with what token, and whether its
	// edits wait for the user's review. Every control writes through `setMcpSettings`; the listener restarts by
	// itself when the address or the switch moves, and what comes back is what is in force.
	import SettingRow from './SettingRow.svelte';
	import McpConnect from './McpConnect.svelte';
	import Btn from './Btn.svelte';
	import Icon from './Icon.svelte';
	import { settings } from '$lib/settings.svelte';
	import { agentStatus, confirmAction, mcpConnectionInfo, regenerateMcpToken, setMcpSettings } from '$lib/api';
	import { CONFIRM } from '$lib/confirm-copy';
	import { toast } from '$lib/notifications.svelte';
	import { addressProblem, envOverrideNote, lastSeenLine, mcpStatus, networkWarning, tokenDisplay } from '$lib/mcp-ui';
	import type { McpConnectionInfo, McpSettingsPatch } from '$lib/types';

	let info = $state<McpConnectionInfo | null>(null);
	let loadError = $state<string | null>(null);
	let lastSeen = $state<number | null>(null);
	let revealed = $state(false);
	let addr = $state('');
	let addrDirty = $state(false);
	let copied = $state(false);
	let busy = $state(false);

	async function load() {
		try {
			const next = await mcpConnectionInfo();
			info = next;
			loadError = null;
			if (!addrDirty) addr = next.configured_addr ?? '';
		} catch (e) {
			loadError = e instanceof Error ? e.message : String(e);
		}
		try {
			lastSeen = (await agentStatus()).last_seen_secs;
		} catch {
			/* the status line is a courtesy */
		}
	}

	// Read when the section shows, again every few seconds (the listener's state and the last request move
	// by themselves), and when a setting changed from outside (an agent's `set_settings`).
	$effect(() => {
		void settings.externalChanges;
		void load();
		const t = setInterval(() => void load(), 5000);
		return () => clearInterval(t);
	});

	/** Write one or more of the agent switches; what comes back replaces what is shown. */
	async function change(patch: McpSettingsPatch): Promise<boolean> {
		busy = true;
		try {
			const next = await setMcpSettings(patch);
			info = next;
			settings.agentReview = next.agent_review;
			if (!addrDirty) addr = next.configured_addr ?? '';
			return true;
		} catch (e) {
			toast.error("Couldn't change the agent setting", { description: e instanceof Error ? e.message : String(e) });
			await load();
			return false;
		} finally {
			busy = false;
		}
	}

	async function setReview(on: boolean, box: HTMLInputElement) {
		if (!on) {
			const c = CONFIRM.agentReviewOff;
			if (!(await confirmAction(c.message, c.title, c))) {
				box.checked = true;
				return;
			}
		}
		if (!(await change({ agent_review: on }))) box.checked = !on;
	}

	const problem = $derived(info ? addressProblem(addr, info.require_token) : null);
	const unchanged = $derived(info ? addr.trim() === (info.configured_addr ?? '') : true);

	async function applyAddr() {
		if (problem || unchanged) return;
		const wanted = addr.trim();
		addrDirty = false;
		if (await change({ mcp_addr: wanted === '' ? null : wanted })) toast.success(wanted === '' ? 'Listening on the default address' : `Listening on ${wanted}`);
	}

	async function copyToken() {
		if (!info?.token) return;
		try {
			await navigator.clipboard.writeText(info.token);
			copied = true;
			setTimeout(() => (copied = false), 1400);
		} catch {
			toast.error("Couldn't copy — show the token and copy it manually");
		}
	}

	async function newToken() {
		const c = CONFIRM.newToken;
		if (!(await confirmAction(c.message, c.title, c))) return;
		busy = true;
		try {
			await regenerateMcpToken();
			await load();
			toast.success('Made a new access token', { description: 'Give it to every agent that connects.' });
		} catch (e) {
			toast.error("Couldn't make a new token", { description: e instanceof Error ? e.message : String(e) });
		} finally {
			busy = false;
		}
	}

	const status = $derived(info ? mcpStatus(info) : null);
	const TONE: Record<string, string> = {
		ok: 'var(--green-400)',
		warn: 'var(--orange-400)',
		danger: 'var(--red-400)',
		off: 'var(--text-muted)'
	};
	const override = $derived(info ? envOverrideNote(info) : null);
	const network = $derived(info ? networkWarning(info.configured_addr, info.require_token) : null);
</script>

<SettingRow id="agent-review" label="Review the agent’s edits before they land">
	{#snippet hint()}
		On, everything an agent changes waits as a proposal: you look at it on the timeline and apply, pick or discard it.
		Off, its edits land as they are made (each one still a step you can undo).
	{/snippet}
	<input
		type="checkbox"
		role="switch"
		aria-label="Review the agent’s edits before they land"
		data-testid="agent-review"
		checked={settings.agentReview}
		disabled={busy}
		onchange={(e) => void setReview(e.currentTarget.checked, e.currentTarget)}
		style="accent-color:var(--kerf-500);cursor:pointer;width:16px;height:16px"
	/>
</SettingRow>

<SettingRow id="mcp-enabled" label="Let agents connect">
	{#snippet hint()}
		Kerf runs an MCP server on this computer so an AI agent — Claude Code, Cursor and others — can look at your project
		and edit it through the same engine the buttons use. Nothing leaves your machine unless you point it at a network
		address.
		{#if loadError}
			<span style="display:block;margin-top:6px;color:var(--red-400)">Couldn't read the server's state: {loadError}</span>
		{:else if status && info}
			<span data-testid="mcp-status" data-tone={status.tone} style="display:flex;align-items:center;gap:6px;margin-top:8px;color:{TONE[status.tone]}">
				<Icon n={status.tone === 'ok' ? 'circle-check' : status.tone === 'danger' ? 'circle-alert' : 'plug'} s={13} />
				{status.text}
			</span>
			{#if info.enabled && info.running}
				<span style="display:block;margin-top:3px;color:var(--text-muted)">{lastSeenLine(lastSeen)}</span>
			{/if}
		{/if}
	{/snippet}
	{#if info}
		<input
			type="checkbox"
			role="switch"
			aria-label="Let agents connect"
			data-testid="mcp-enabled"
			checked={info.enabled}
			disabled={busy}
			onchange={(e) => void change({ mcp_enabled: e.currentTarget.checked })}
			style="accent-color:var(--kerf-500);cursor:pointer;width:16px;height:16px"
		/>
	{/if}
</SettingRow>

<SettingRow id="mcp-address" label="Address" stacked>
	{#snippet hint()}
		Where the server listens, as host:port. Empty is the default, 127.0.0.1:7777 — reachable from this computer only. The
		server restarts by itself when this changes.
	{/snippet}
	{#if info}
		<div style="display:flex;gap:8px;align-items:center">
			<input
				type="text"
				bind:value={addr}
				oninput={() => (addrDirty = true)}
				onkeydown={(e) => e.key === 'Enter' && void applyAddr()}
				placeholder="127.0.0.1:7777"
				aria-label="Server address"
				aria-invalid={problem ? 'true' : undefined}
				data-testid="mcp-address"
				spellcheck="false"
				style="flex:1;min-width:0;height:28px;padding:0 8px;background:var(--surface-inset);color:var(--text-primary);border:var(--line-width) solid {problem
					? 'var(--red-400)'
					: 'var(--border-default)'};border-radius:var(--radius-sm);font-family:var(--font-mono);font-size:12px"
			/>
			<Btn variant="secondary" size="sm" disabled={busy || unchanged || !!problem} data-testid="mcp-address-apply" onclick={applyAddr}>Apply</Btn>
		</div>
		{#if problem}
			<div role="alert" style="margin-top:6px;font-size:12px;color:var(--red-400)">{problem}</div>
		{/if}
		{#if override}
			<div data-testid="mcp-env-note" style="margin-top:8px;display:flex;gap:6px;font-size:12px;line-height:1.5;color:var(--orange-400)">
				<Icon n="info" s={13} style="flex:none;margin-top:2px" />
				<span>{override}</span>
			</div>
		{/if}
		{#if network}
			<div style="margin-top:6px;font-size:12px;line-height:1.5;color:var(--text-muted)">{network}</div>
		{/if}
	{/if}
</SettingRow>

<SettingRow id="mcp-token" label="Access token" stacked>
	{#snippet hint()}
		With the token required, an agent has to send it with every request — another program on this computer cannot
		drive Kerf without it. Turn it off only on a machine nobody else uses.
	{/snippet}
	{#if info}
		<label style="display:flex;align-items:center;gap:8px;font-size:12px;color:var(--text-primary);margin-bottom:8px;cursor:pointer">
			<input
				type="checkbox"
				role="switch"
				data-testid="mcp-require-token"
				checked={info.require_token}
				disabled={busy}
				onchange={(e) => void change({ mcp_require_token: e.currentTarget.checked })}
				style="accent-color:var(--kerf-500);cursor:pointer;width:16px;height:16px"
			/>
			Require the access token
		</label>
		<div style="display:flex;gap:6px;align-items:center">
			<code
				data-selectable
				data-testid="mcp-token-value"
				style="flex:1;min-width:0;padding:6px 8px;border-radius:var(--radius-sm);background:var(--surface-inset);border:var(--line-width) solid var(--border-subtle);font-family:var(--font-mono);font-size:12px;color:var(--text-secondary);overflow:hidden;text-overflow:ellipsis;white-space:nowrap"
				>{tokenDisplay(info.token, revealed)}</code
			>
			<Btn size="sm" variant="secondary" icon={revealed ? 'eye-off' : 'eye'} iconSize={12} aria-pressed={revealed} onclick={() => (revealed = !revealed)}
				>{revealed ? 'Hide' : 'Show'}</Btn
			>
			<Btn size="sm" variant="secondary" icon={copied ? 'check' : 'copy'} iconSize={12} disabled={!info.token} data-testid="mcp-token-copy" onclick={copyToken}
				>{copied ? 'Copied' : 'Copy'}</Btn
			>
			<Btn size="sm" variant="secondary" icon="refresh-cw" iconSize={12} disabled={busy} data-testid="mcp-token-new" onclick={newToken}>New token…</Btn>
		</div>
	{/if}
</SettingRow>

<SettingRow id="mcp-connect" label="Connect an agent" stacked>
	{#snippet hint()}
		Paste one of these into your agent’s setup. The agent edits the project you have open{settings.agentReview
			? ' and proposes changes you review'
			: ' — review is off, so its edits land directly'}.
	{/snippet}
	{#if info}
		{#if info.enabled}
			<McpConnect {info} bind:revealed />
		{:else}
			<div style="font-size:12px;color:var(--text-muted)">Agents cannot connect while the server is off.</div>
		{/if}
	{/if}
</SettingRow>
