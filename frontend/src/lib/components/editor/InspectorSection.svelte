<script lang="ts">
	import type { Snippet } from 'svelte';
	import { tip } from '#lib/tip';
	import { workspace } from '#lib/workspace.svelte';
	import { inspectorMemory, sectionId } from '#lib/inspector-state';
	// `open` is the section's own default; the workspace may ask the other way, and what the user has opened or
	// closed in this workspace beats both (`inspector-state.ts`). The panel is rebuilt on every workspace switch, so
	// that is kept there and not in the `<details>`.
	let { title, id = sectionId(title), summary = '', open = false, children }: {
		title: string; id?: string; summary?: string; open?: boolean; children: Snippet;
	} = $props();
</script>

<!-- Keyed by the workspace: a section that is still on screen when the workspace changes (a window of its own)
     starts again from that workspace's say instead of keeping what the last one left in the element. -->
{#key workspace.shown}
	<details
		open={inspectorMemory.isOpen(workspace.shown, id, open)}
		data-section={id}
		ontoggle={(e) => inspectorMemory.set(workspace.shown, id, e.currentTarget.open, open)}
	>
		<summary>
			<span class="title">{title}</span>
			{#if summary}<span class="value" use:tip={summary}>{summary}</span>{/if}
		</summary>
		<div class="content">{@render children()}</div>
	</details>
{/key}

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
