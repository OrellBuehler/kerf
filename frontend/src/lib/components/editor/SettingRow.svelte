<script lang="ts">
	// One setting: its name and what it does on the left, the control on the right (or underneath, for a wide
	// one). It shows only when the settings search lets its `id` through (`settings-index.ts` lists the id).
	import type { Snippet } from 'svelte';
	import { useFilter } from '$lib/settings-filter';

	let {
		id,
		label,
		hint = undefined,
		stacked = false,
		for: control = undefined,
		children
	}: {
		id: string;
		label: string;
		hint?: Snippet | string;
		/** The control goes under the words and takes the row's whole width. */
		stacked?: boolean;
		/** The id of the switch in the row: pressing the name then toggles it. */
		for?: string;
		children?: Snippet;
	} = $props();

	const filter = useFilter();
	const labelId = $props.id();
</script>

{#if filter.shows(id)}
	<div
		data-setting={id}
		role="group"
		aria-labelledby={labelId}
		style="padding:12px 0;border-bottom:var(--line-width) solid var(--border-subtle);display:flex;gap:{stacked ? '9px' : '18px'};{stacked
			? 'flex-direction:column'
			: 'align-items:flex-start;justify-content:space-between'}"
	>
		<div style="min-width:0;{stacked ? '' : 'flex:1'}">
			{#if control}
				<label id={labelId} for={control} style="display:block;font-size:13px;color:var(--text-primary);line-height:1.4;cursor:pointer">{label}</label>
			{:else}
				<div id={labelId} style="font-size:13px;color:var(--text-primary);line-height:1.4">{label}</div>
			{/if}
			{#if hint}
				<div style="margin-top:3px;font-size:12px;line-height:1.55;color:var(--text-secondary)">
					{#if typeof hint === 'string'}{hint}{:else}{@render hint()}{/if}
				</div>
			{/if}
		</div>
		{#if children}
			<div style="{stacked ? 'min-width:0' : 'flex:none;max-width:60%'}">{@render children()}</div>
		{/if}
	</div>
{/if}
