<script lang="ts">
	// One row of a form: its name in one column, its control in the other, and what the control means under
	// the control — never beside it and never under the name — so the export dialog and the Deliver panel read
	// the same way down the page and every control starts at the same x. The control is a snippet that is
	// handed the id its label points at and the id of the hint, for `aria-describedby`.
	import type { Snippet } from 'svelte';
	import { FORM_GAP, FORM_LABEL_W } from '#lib/form-rows';

	let {
		label,
		hint = undefined,
		top = false,
		control
	}: {
		label: string;
		/** What the control means or what it will do, in a line or two; a snippet for a hint with something in it. */
		hint?: string | Snippet;
		/** The control is more than one line tall (chips that wrap, a list): the name sits at its top. */
		top?: boolean;
		control: Snippet<[id: string, describedby: string | undefined]>;
	} = $props();

	const uid = $props.id();
	const id = `${uid}-control`;
	const hintId = $derived(hint ? `${uid}-hint` : undefined);
</script>

<div
	style="display:grid;grid-template-columns:{FORM_LABEL_W}px minmax(0,1fr);column-gap:{FORM_GAP}px;row-gap:3px;align-items:{top
		? 'start'
		: 'center'};padding:4px 0"
>
	<label
		for={id}
		style="font-size:var(--text-sm);line-height:{top ? 'var(--control-sm)' : '1.3'};color:var(--text-secondary)"
		>{label}</label
	>
	<div style="min-width:0;display:flex;flex-direction:column;align-items:flex-start">{@render control(id, hintId)}</div>
	{#if hint}
		<div
			id={hintId}
			style="grid-column:2;font-size:var(--text-sm);line-height:1.45;color:var(--text-muted)"
		>
			{#if typeof hint === 'string'}{hint}{:else}{@render hint()}{/if}
		</div>
	{/if}
</div>
