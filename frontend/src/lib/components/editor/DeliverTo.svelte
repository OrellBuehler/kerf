<script lang="ts">
	// One cut, every platform: the delivery frames to write as separate files
	// beside the chosen path, each named by shape. The choice lives on `ui`
	// because two places edit it — the export dialog and the Deliver panel — and
	// a shape ticked in one has to be there when the other opens. Each shape is
	// judged at its own frame: a 9:16 file is a Reel whatever the project is cut
	// in.
	import { ui } from '$lib/editor-ui.svelte';
	import { editor } from '$lib/state.svelte';
	import { platformCheck } from '$lib/api';
	import { VARIANT_PRESETS, formatsFor, variantPath } from '$lib/delivery-formats';
	import type { Delivery, DeliveryCheck } from '$lib/types';

	/** `where` finishes "One file per shape, …": the dialog has a destination
	 *  field of its own to point at, the Deliver panel does not. */
	let {
		outputPath = '',
		ext = 'mp4',
		where = 'beside the file above as'
	}: { outputPath?: string; ext?: string; where?: string } = $props();

	const formats = $derived(formatsFor(ui.deliverShapes));
	const ticked = $derived(VARIANT_PRESETS.filter((p) => ui.deliverShapes.includes(p.id)));

	// Re-judged whenever the cut changes: the panel is docked beside a timeline
	// that is being edited, so a verdict cached when a shape was ticked would go
	// stale as the length moved. Only the newest answer is kept.
	let checks = $state<Record<string, DeliveryCheck[]>>({});
	let seq = 0;
	$effect(() => {
		void editor.timeline;
		const wanted = ticked;
		const mine = ++seq;
		void Promise.all(
			wanted.map(async (p) => {
				const f = p.format as Delivery;
				return [p.id, await platformCheck([f.width, f.height]).catch(() => null)] as const;
			})
		).then((entries) => {
			if (mine !== seq) return;
			const next: Record<string, DeliveryCheck[]> = {};
			for (const [id, c] of entries) if (c) next[id] = c;
			checks = next;
		});
	});

	function readyLabels(c: DeliveryCheck[] | undefined): string {
		if (!c) return '';
		const ready = c.filter((x) => !x.issues.some((i) => i.severity !== 'tip')).map((x) => x.label);
		return ready.length ? `Ready for ${ready.join(' · ')}` : 'Not ready for any target';
	}
</script>

<div style="display:flex;flex-wrap:wrap;gap:6px;padding:2px 0">
	{#each VARIANT_PRESETS as p (p.id)}
		{@const on = ui.deliverShapes.includes(p.id)}
		<button
			title={p.hint}
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
	<div style="display:flex;flex-direction:column;gap:3px;padding:2px 0 4px">
		{#each ticked as p (p.id)}
			<div style="display:flex;align-items:center;gap:6px;font-size:12px">
				<span style="font-family:var(--font-mono);color:var(--text-secondary);width:36px">{p.label}</span>
				<span style="color:{readyLabels(checks[p.id]).startsWith('Ready') ? 'var(--success)' : 'var(--warning)'}"
					>{readyLabels(checks[p.id])}</span
				>
			</div>
		{/each}
	</div>
{:else}
	<div style="font-size:11px;color:var(--text-muted);padding:4px 0 2px">
		Pick shapes to write one file each — a Reel, a feed post and a YouTube upload from this one cut.
	</div>
{/if}
