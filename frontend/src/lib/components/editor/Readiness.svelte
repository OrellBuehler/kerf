<script lang="ts">
	// Where this cut is going: the platform limits it meets or misses. Judged at
	// the frame a render will actually produce, which is not always the project's
	// — a 9:16 project exported at 1920×1080 is a landscape file, and the panel
	// has to say so — and re-judged as the cut changes, because the Deliver panel
	// stays docked beside a timeline that is being edited.
	//
	// Renders nothing until the backend has answered, heading included.
	import Icon from './Icon.svelte';
	import SectionHead from './SectionHead.svelte';
	import { editor } from '$lib/state.svelte';
	import { platformCheck } from '$lib/api';
	import { ratioLabel } from '$lib/delivery-formats';
	import type { DeliveryCheck } from '$lib/types';

	/** The frame the render will produce; `null` is the project's own. */
	let { frame = null, heading = 'Where it is going' }: { frame?: [number, number] | null; heading?: string } =
		$props();

	let checks = $state<DeliveryCheck[]>([]);
	let seq = 0;
	$effect(() => {
		void editor.timeline;
		const f = frame ?? null;
		const mine = ++seq;
		platformCheck(f)
			.then((c) => {
				if (mine === seq) checks = c;
			})
			.catch(() => {
				if (mine === seq) checks = [];
			});
	});

	/** Targets with nothing but tips against them. */
	const readyFor = $derived(checks.filter((c) => !c.issues.some((i) => i.severity !== 'tip')));
	/** Everything specific enough to be worth its own line — which is everything
	 *  except the shape complaint, since a landscape cut earns one of those from
	 *  every vertical feed and four near-identical lines say nothing four times. */
	const notes = $derived(
		checks.flatMap((c) =>
			c.issues.filter((i) => i.severity !== 'tip' && i.kind !== 'shape').map((i) => ({ label: c.label, ...i }))
		)
	);
	/** The targets this frame would be letterboxed on, collapsed to one line. */
	const wrongShape = $derived(checks.filter((c) => c.issues.some((i) => i.kind === 'shape')).map((c) => c.label));
	/** The frame this render will actually produce, as a ratio. */
	const cutRatio = $derived.by(() => {
		const r = frame ?? (editor.timeline.format ? [editor.timeline.format.width, editor.timeline.format.height] : null);
		return r ? ratioLabel(r[0], r[1]) : null;
	});
	/** The tips, deduplicated — the same advice lands on every target. */
	const tips = $derived([
		...new Set(checks.flatMap((c) => c.issues.filter((i) => i.severity === 'tip').map((i) => i.message)))
	]);

	function listLabels(labels: string[]): string {
		if (labels.length < 2) return labels.join('');
		return `${labels.slice(0, -1).join(', ')} and ${labels[labels.length - 1]}`;
	}
</script>

{#if checks.length}
	<SectionHead label={heading} />
	<div
		style="padding:8px 10px;border-radius:var(--radius-sm);background:var(--surface-inset);border:var(--line-width) solid var(--border-subtle);display:flex;flex-direction:column;gap:6px"
	>
		{#if readyFor.length}
			<div style="display:flex;align-items:center;gap:6px;font-size:12px;color:var(--success)">
				<Icon n="check" s={13} color="var(--success)" />
				<span>Ready for {readyFor.map((c) => c.label).join(' · ')}</span>
			</div>
		{/if}
		{#each notes as note, i (i)}
			<div style="display:flex;align-items:flex-start;gap:6px;font-size:12px;line-height:1.45">
				<span style="flex:none;margin-top:1px">
					<Icon
						n={note.severity === 'error' ? 'x' : 'alert-triangle'}
						s={13}
						color={note.severity === 'error' ? 'var(--danger)' : 'var(--warning)'}
					/>
				</span>
				<span style="color:var(--text-secondary)">
					<span style="color:var(--text-primary);font-weight:600">{note.label}</span>
					— {note.message}
				</span>
			</div>
		{/each}
		{#if wrongShape.length}
			<div style="display:flex;align-items:flex-start;gap:6px;font-size:12px;line-height:1.45">
				<span style="flex:none;margin-top:1px"><Icon n="alert-triangle" s={13} color="var(--warning)" /></span>
				<span style="color:var(--text-secondary)">
					{#if cutRatio}A {cutRatio} cut is letterboxed on{:else}This frame is letterboxed on{/if}
					{listLabels(wrongShape)}. Pick a delivery frame in the toolbar to cut for one of them.
				</span>
			</div>
		{/if}
		{#if tips.length}
			<details>
				<summary style="cursor:pointer;font-size:12px;color:var(--text-muted)">{tips.length} tip{tips.length === 1 ? '' : 's'}</summary>
				{#each tips as tip (tip)}
					<div style="display:flex;align-items:flex-start;gap:6px;margin-top:6px;font-size:12px;line-height:1.45;color:var(--text-muted)">
						<span style="flex:none;margin-top:1px"><Icon n="lightbulb" s={13} color="var(--text-muted)" /></span>
						<span>{tip}</span>
					</div>
				{/each}
			</details>
		{/if}
	</div>
{/if}
