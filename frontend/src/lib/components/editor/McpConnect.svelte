<script lang="ts">
	// How to connect an agent: the endpoint, then — per client — what to paste, with copy buttons. The token is
	// dots on screen until asked for; what is copied is always the real text. Used by Settings › Agent and the
	// Agent panel's Connect card, so the two cannot tell different stories.
	import Icon from './Icon.svelte';
	import IconBtn from './IconBtn.svelte';
	import Btn from './Btn.svelte';
	import ChoiceChips from './ChoiceChips.svelte';
	import Chip from './Chip.svelte';
	import { toast } from '#lib/notifications.svelte';
	import { clientHelps, type ClientHelp } from '#lib/mcp-clients';
	import { withToolSets } from '#lib/mcp-connect';
	import { maskSecret } from '#lib/mcp-ui';
	import type { McpConnectionInfo } from '#lib/types';

	let {
		info,
		revealed = $bindable(false),
		compact = false
	}: { info: McpConnectionInfo; revealed?: boolean; compact?: boolean } = $props();

	// The tool sets to ask for (`?tools=`); none is every tool. Only the snippets change: it is the client's choice.
	let sets = $state<string[]>([]);
	const shown = $derived(withToolSets(info, sets));
	const helps = $derived(clientHelps(shown));

	function toggle(name: string) {
		const next = sets.includes(name) ? sets.filter((s) => s !== name) : [...sets, name];
		sets = info.tool_sets.map((s) => s.name).filter((s) => next.includes(s));
	}
	let pick = $state<ClientHelp['id']>('claude-code');
	const help = $derived(helps.find((h) => h.id === pick) ?? helps[0]);
	let copied = $state<string | null>(null);

	async function copy(text: string, key: string) {
		try {
			await navigator.clipboard.writeText(text);
			copied = key;
			setTimeout(() => {
				if (copied === key) copied = null;
			}, 1400);
		} catch {
			toast.error("Couldn't copy — select the text and copy manually");
		}
	}
</script>

{#snippet copyRow(shown: string, real: string, key: string, label: string)}
	<div
		style="display:flex;align-items:flex-start;gap:6px;background:var(--surface-inset);border:var(--line-width) solid var(--border-subtle);border-radius:var(--radius-sm);padding:6px 6px 6px 9px"
	>
		<code
			data-selectable
			data-testid="mcp-snippet-{key}"
			style="flex:1;min-width:0;padding-top:4px;font-family:var(--font-mono);font-size:var(--text-sm);line-height:1.45;color:var(--text-secondary);white-space:pre-wrap;overflow-wrap:anywhere"
			>{shown}</code
		>
		<IconBtn title={copied === key ? 'Copied' : label} size={28} onclick={() => copy(real, key)} style="color:{copied === key ? 'var(--green-400)' : ''}">
			<Icon n={copied === key ? 'check' : 'copy'} s={13} />
		</IconBtn>
	</div>
{/snippet}

<div style="display:flex;flex-direction:column;gap:{compact ? '9px' : '12px'}">
	{#if info.token_problem}
		<div role="alert" data-testid="mcp-connect-token-problem" style="display:flex;gap:6px;font-size:12px;line-height:1.5;color:var(--orange-400)">
			<Icon n="alert-triangle" s={13} style="flex:none;margin-top:2px" />
			<span>{info.token_problem}</span>
		</div>
	{/if}
	<div>
		<div style="font-size:12px;color:var(--text-muted);line-height:1.5;margin-bottom:6px">Any MCP client connects to this local endpoint:</div>
		{@render copyRow(shown.url, shown.url, 'url', 'Copy the endpoint')}
	</div>

	{#if !compact && info.tool_sets.length > 0}
		<div>
			<div style="font-size:12px;color:var(--text-muted);line-height:1.5;margin-bottom:6px">
				Every tool by default. Pick sets to list only those and spare the agent's context (the guide, the timeline summary,
				the staged diff and job status always come):
			</div>
			<div role="group" aria-label="Tool sets" style="display:flex;flex-wrap:wrap;gap:6px">
				<Chip pressed={sets.length === 0} data-testid="mcp-tools-all" onclick={() => (sets = [])}>All</Chip>
				{#each info.tool_sets as set (set.name)}
					<Chip pressed={sets.includes(set.name)} title={`${set.description} (${set.tools} tools)`} data-testid="mcp-tools-{set.name}" onclick={() => toggle(set.name)}
						>{set.name}</Chip
					>
				{/each}
			</div>
		</div>
	{/if}

	<div>
		<div style="font-size:12px;color:var(--text-muted);line-height:1.5;margin-bottom:6px">
			Set it up in <span style="color:var(--text-secondary)">your client</span>:
		</div>
		<ChoiceChips
			label="Agent client"
			options={helps.map((h) => ({ value: h.id, label: h.name, hint: h.covers }))}
			value={pick}
			onpick={(id) => (pick = id)}
			testid="mcp-client"
		/>
		<div style="margin:8px 0 6px;font-size:12px;color:var(--text-muted);line-height:1.5">
			<span style="color:var(--text-secondary)">{help.covers}.</span>
			{help.where}
		</div>
		{@render copyRow(maskSecret(help.snippet, info.token, revealed), help.snippet, help.id, `Copy the ${help.name} setup`)}
		{#if info.require_token && info.token}
			<div style="margin-top:6px;display:flex;align-items:center;gap:8px;font-size:12px;color:var(--text-muted);line-height:1.5">
				<span style="flex:1;min-width:0">The setup carries the access token, so treat it like a password.</span>
				<Btn size="sm" variant="ghost" icon={revealed ? 'eye-off' : 'eye'} iconSize={12} aria-pressed={revealed} data-testid="mcp-reveal" onclick={() => (revealed = !revealed)}
					>{revealed ? 'Hide token' : 'Show token'}</Btn
				>
			</div>
		{/if}
	</div>
</div>
