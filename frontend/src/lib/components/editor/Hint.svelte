<script lang="ts">
	import type { Snippet } from 'svelte';
	import Icon from './Icon.svelte';

	let { icon = 'lightbulb', children }: { icon?: string; children?: Snippet } = $props();
</script>

<!-- Why a tab's controls are off, or what to do first: the one box the Effects, Transitions and Audio tabs
     say it in. -->
<div
	role="status"
	style="display:flex;gap:7px;align-items:flex-start;padding:8px 10px;border-radius:var(--radius-sm);background:var(--surface-inset);border:var(--line-width) solid var(--border-subtle);font-size:var(--text-sm);line-height:1.45;color:var(--text-muted)"
>
	<span style="margin-top:1px"><Icon n={icon} s={13} color="var(--text-muted)" /></span>
	<span>{@render children?.()}</span>
</div>
