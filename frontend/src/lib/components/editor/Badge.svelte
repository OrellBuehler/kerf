<script lang="ts">
	import type { Snippet } from 'svelte';

	let {
		tone = 'neutral',
		dot = false,
		style = '',
		children
	}: {
		tone?: 'neutral' | 'kerf' | 'agent' | 'success' | 'warning' | 'danger';
		dot?: boolean;
		style?: string;
		children?: Snippet;
	} = $props();

	const tones = {
		neutral: { fg: 'var(--neutral-300)', bg: 'var(--surface-hover)' },
		kerf: { fg: 'var(--kerf-300)', bg: 'rgba(226,157,46,0.14)' },
		agent: { fg: 'var(--agent-300)', bg: 'var(--agent-surface)' },
		success: { fg: 'var(--green-400)', bg: 'var(--success-surface)' },
		warning: { fg: 'var(--orange-400)', bg: 'var(--warning-surface)' },
		danger: { fg: 'var(--red-400)', bg: 'var(--danger-surface)' }
	} as const;

	const t = $derived(tones[tone]);
</script>

<span
	style="display:inline-flex;align-items:center;gap:5px;height:19px;padding:0 7px;border-radius:var(--radius-sm);font-family:var(--font-sans);font-size:10px;font-weight:600;letter-spacing:0.02em;line-height:1;color:{t.fg};background:{t.bg};border:{tone ===
	'neutral'
		? '1px solid var(--border-strong)'
		: '1px solid transparent'};{style}"
>
	{#if dot}<span style="width:5px;height:5px;border-radius:50%;background:{t.fg}"></span>{/if}
	{@render children?.()}
</span>
