<script lang="ts">
	import type { Snippet } from 'svelte';
	import { tip } from '$lib/tip';
	let { title, summary = '', open = false, children }: {
		title: string; summary?: string; open?: boolean; children: Snippet;
	} = $props();
</script>

<details {open}>
	<summary>
		<span class="title">{title}</span>
		{#if summary}<span class="value" use:tip={summary}>{summary}</span>{/if}
	</summary>
	<div class="content">{@render children()}</div>
</details>

<style>
	details { border-top: var(--line-width) solid var(--border-default); margin-top: 6px; }
	/* A flex row, so a long summary ends in an ellipsis beside the title instead of wrapping under it;
	   the disclosure triangle is drawn here (a flex summary has no native marker). */
	summary {
		display: flex;
		align-items: center;
		gap: 8px;
		min-height: 32px;
		cursor: pointer;
		color: var(--text-secondary);
		font-size: var(--text-base);
		list-style: none;
	}
	summary::-webkit-details-marker { display: none; }
	summary::before {
		content: '';
		flex: none;
		width: 5px;
		height: 5px;
		margin: 0 3px 0 2px;
		border-right: 1.5px solid var(--text-muted);
		border-bottom: 1.5px solid var(--text-muted);
		transform: rotate(-45deg);
		transition: transform var(--dur-fast) var(--ease-out);
	}
	details[open] > summary::before { transform: rotate(45deg); }
	summary:hover { color: var(--text-primary); }
	.title { flex: none; font-weight: 600; }
	.value { flex: 0 1 auto; min-width: 0; margin-left: auto; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; font-size: var(--text-sm); font-weight: 400; color: var(--text-muted); }
	.content { padding-bottom: 6px; }
	@media (prefers-reduced-motion: reduce) { summary::before { transition: none; } }
</style>
