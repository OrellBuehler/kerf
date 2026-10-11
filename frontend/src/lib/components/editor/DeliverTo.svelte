<script lang="ts">
	// One cut, every platform: the delivery frames to write as separate files
	// beside the chosen path, each named by shape. The choice lives on `ui`
	// because two places edit it — the export dialog and the Deliver panel — and
	// a shape ticked in one has to be there when the other opens. What each
	// shape is judged for is the platform check's (`Readiness`), which reads the
	// same choice.
	import { ui } from '$lib/editor-ui.svelte';
	import { VARIANT_PRESETS, formatsFor, variantPath } from '$lib/delivery-formats';
	import { tip } from '$lib/tip';

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
		<button
			use:tip={p.hint}
			aria-pressed={on}
			onclick={() => ui.toggleDeliverShape(p.id)}
			style="padding:5px 10px;border-radius:999px;font-size:12px;cursor:pointer;white-space:nowrap;border:var(--line-width) solid {on
				? 'var(--kerf-500)'
				: 'var(--border-strong)'};background:{on
				? 'color-mix(in srgb,var(--kerf-500) 22%,transparent)'
				: 'var(--surface-inset)'};color:{on ? 'var(--text-primary)' : 'var(--text-secondary)'}"
			>{p.label}</button
		>
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
	<label style="display:flex;align-items:center;justify-content:space-between;gap:8px;padding:4px 0">
		<span style="font-size:12px;color:var(--text-muted)">Smart crop each shot for every shape</span>
		<input
			type="checkbox"
			checked={ui.deliverSmartCrop}
			onchange={(e) => (ui.deliverSmartCrop = e.currentTarget.checked)}
			style="accent-color:var(--kerf-500);width:15px;height:15px"
		/>
	</label>
{:else}
	<div style="font-size:11px;color:var(--text-muted);padding:4px 0 2px">
		Pick shapes to write one file each — a Reel, a feed post and a YouTube upload from this one cut.
	</div>
{/if}
