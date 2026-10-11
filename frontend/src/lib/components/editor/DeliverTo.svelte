<script lang="ts">
	// One cut, every platform: the delivery frames to write as separate files
	// beside the chosen path, each named by shape. The choice lives on `ui`
	// because two places edit it — the export dialog and the Deliver panel — and
	// a shape ticked in one has to be there when the other opens. What each
	// shape is judged for is the platform check's (`Readiness`), which reads the
	// same choice.
	import Chip from './Chip.svelte';
	import CheckRow from './CheckRow.svelte';
	import { ui } from '$lib/editor-ui.svelte';
	import { VARIANT_PRESETS, formatsFor, variantPath } from '$lib/delivery-formats';

	/** `where` finishes "One file per shape, …": the dialog has a destination
	 *  field of its own to point at, the Deliver panel does not. */
	let {
		outputPath = '',
		ext = 'mp4',
		where = 'beside the file above as'
	}: { outputPath?: string; ext?: string; where?: string } = $props();

	const formats = $derived(formatsFor(ui.deliverShapes));
</script>

<div style="display:flex;flex-wrap:wrap;gap:6px;padding:2px 0">
	{#each VARIANT_PRESETS as p (p.id)}
		{@const on = ui.deliverShapes.includes(p.id)}
		<Chip pressed={on} title={p.hint} onclick={() => ui.toggleDeliverShape(p.id)}>{p.label}</Chip>
	{/each}
</div>
{#if formats.length}
	<div style="font-size:11px;color:var(--text-muted);padding:4px 0 2px">
		One file per shape, each shot framed for each — {where}
		{#each formats as f, i (f.width + 'x' + f.height)}{i ? ', ' : ''}<span
				style="font-family:var(--font-mono);color:var(--text-secondary)"
				>{variantPath(outputPath || 'cut.' + ext, f).split(/[\\/]/).pop()}</span
			>{/each}.
	</div>
	<CheckRow checked={ui.deliverSmartCrop} onchange={(e) => (ui.deliverSmartCrop = e.currentTarget.checked)}>
		Smart crop each shot for every shape
	</CheckRow>
{:else}
	<div style="font-size:11px;color:var(--text-muted);padding:4px 0 2px">
		Pick shapes to write one file each — a Reel, a feed post and a YouTube upload from this one cut.
	</div>
{/if}
