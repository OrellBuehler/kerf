<script lang="ts">
	import type { Snippet } from 'svelte';
	let { title, summary = '', open = false, children }: {
		title: string; summary?: string; open?: boolean; children: Snippet;
	} = $props();
</script>

<details {open}>
	<summary>
		<span class="title">{title}</span>
		{#if summary}<span class="value" title={summary}>{summary}</span>{/if}
	</summary>
	<div class="content">{@render children()}</div>
</details>

<style>
	details { border-top: 1px solid var(--border-default); margin-top: 10px; }
	summary { cursor: pointer; min-height: 38px; padding: 10px 0; color: var(--text-secondary); font-size: 13px; }
	summary::marker { color: var(--text-muted); font-size: 11px; }
	summary:hover { color: var(--text-primary); }
	.title { font-weight: 600; }
	.value { float: right; max-width: 48%; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; font-size: 12px; font-weight: 400; color: var(--text-muted); }
	.content { padding-bottom: 6px; }
</style>
