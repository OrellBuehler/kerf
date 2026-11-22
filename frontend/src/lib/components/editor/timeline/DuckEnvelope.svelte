<script lang="ts">
	/* What the export does to a ducked track, drawn quietly over its lane: a shaded fall from the lane's top edge where the rest of the
	   mix speaks and the bed dips (24 dB to the bottom of the shade), a tick along the lane's foot where that speech is — the why — and,
	   under the compressor, where the export stops the track (a compressor ends with its key). It draws nothing for a track that is not
	   ducked, or a mix with nothing to key it, and nothing it cannot say: until the key's peaks have arrived there is no dip.

	   It is a *prediction* from the waveform's peaks (`duck-envelope.ts`: the same `Ducker` the preview plays, run over a power envelope
	   at 100 steps a second), within about half a dB on average, with the edges a hop or two out; the sound is the preview's. One lane's
	   worth of it is mounted in each lane (`<DuckEnvelope {t} {pxPerSec} />`), and it takes no pointer: the lane's clips and gestures are
	   under it. Tokens only. */
	import { untrack } from 'svelte';
	import { editor } from '#lib/state.svelte';
	import { getWaveformRange } from '#lib/api';
	import { duckPlan } from '#lib/duck-plan';
	import { cutAt, dipLine, dipPath, duckView, joinChunks, peakKey, peakRequests, type Peaks } from '#lib/duck-envelope';
	import { clipDuration } from '#lib/types';
	import type { Clip, Track } from '#lib/types';

	let { t, pxPerSec }: { t: Track; pxPerSec: number } = $props();

	const audible = $derived(new Set(editor.assets.filter((a) => a.streams?.some((s) => s.kind === 'audio')).map((a) => a.id)));
	const plan = $derived(duckPlan(editor.timeline, audible));
	const shown = $derived(!!t.duck && plan.active && plan.ducked.includes(t.id));
	/** The clips of the tracks that key the duck, whose peaks the key is read from. */
	const keyClips = $derived(
		shown ? editor.timeline.tracks.filter((k) => plan.keyed.includes(k.id)).flatMap((k) => k.clips.filter((c) => c.enabled !== false)) : []
	);

	/** Peaks of a clip's window by `peakKey`, as they arrive. Replaced, never mutated, so what reads it re-runs. */
	let loaded = $state.raw<Map<string, Peaks>>(new Map());

	$effect(() => {
		if (!shown) return;
		let alive = true;
		for (const c of keyClips) {
			const key = peakKey(c);
			if (untrack(() => loaded.has(key))) continue;
			void peaksOf(c).then((p) => {
				if (alive && p) loaded = new Map(loaded).set(key, p);
			});
		}
		return () => {
			alive = false;
		};
	});

	const end = $derived(editor.duration);
	const view = $derived(shown && end > 0 ? duckView(editor.timeline, plan, (c) => loaded.get(peakKey(c)) ?? null, end) : null);
	const trackEnd = $derived(t.clips.reduce((m, c) => Math.max(m, c.timeline_start + clipDuration(c)), 0));
	const cut = $derived(shown ? cutAt(plan, trackEnd) : null);
	const label = $derived(
		view
			? `Ducked by the rest of the mix: ${plan.mode.kind === 'gate' ? `speech gate, ${plan.mode.depthDb} dB` : 'compressor'}, down to ${view.deepest.toFixed(0)} dB, predicted from the waveform`
			: ''
	);
</script>

<script lang="ts" module>
	/** Peaks by window, over every lane and every redraw: one request chain per window for the session, a failed one remembered (the lane's own
	 *  waveform reports a file it cannot read, once). */
	const cache = new Map<string, Promise<Peaks | null>>();
	const CACHE_CAP = 48;

	function peaksOf(clip: Pick<Clip, 'asset_id' | 'source_in' | 'source_out'>): Promise<Peaks | null> {
		const key = peakKey(clip);
		const hit = cache.get(key);
		if (hit) return hit;
		const load = (async () => {
			try {
				const chunks = [];
				for (const [from, to, buckets] of peakRequests(clip.source_in, clip.source_out)) {
					const r = await getWaveformRange(clip.asset_id, from, to, buckets);
					chunks.push({ buckets: r.buckets, min: r.min, max: r.max });
				}
				return chunks.length > 0 ? joinChunks(clip.source_in, chunks) : null;
			} catch {
				return null;
			}
		})();
		cache.set(key, load);
		if (cache.size > CACHE_CAP) cache.delete(cache.keys().next().value as string);
		return load;
	}
</script>

{#if view && end > 0}
	<div data-duck aria-hidden="true" style="position:absolute;left:0;top:0;width:{end * pxPerSec}px;height:100%;z-index:6;pointer-events:none;overflow:hidden">
		<svg
			role="img"
			aria-label={label}
			viewBox="0 0 {end} 1"
			preserveAspectRatio="none"
			style="position:absolute;left:0;top:0;width:100%;height:78%;overflow:visible"
		>
			<path d={dipPath(view.curve, 0, end)} style="fill:color-mix(in srgb,var(--kerf-400) 20%,transparent);stroke:none" />
			<path
				d={dipLine(view.curve, 0, end)}
				vector-effect="non-scaling-stroke"
				style="fill:none;stroke:color-mix(in srgb,var(--kerf-300) 70%,transparent);stroke-width:1px;stroke-linejoin:round"
			/>
		</svg>
		{#each view.spans as [a, b] (a)}
			<span
				data-duck-key
				style="position:absolute;left:{a * pxPerSec}px;width:{Math.max(2, (b - a) * pxPerSec)}px;bottom:2px;height:2px;border-radius:1px;background:color-mix(in srgb,var(--text-primary) 38%,transparent)"
			></span>
		{/each}
		{#if cut !== null}
			<span
				data-duck-cut
				style="position:absolute;left:{cut * pxPerSec}px;width:{(trackEnd - cut) * pxPerSec}px;top:0;bottom:0;box-sizing:border-box;border-left:var(--line-width) dashed var(--kerf-400);background:repeating-linear-gradient(135deg,color-mix(in srgb,var(--text-primary) 12%,transparent) 0 1px,transparent 1px 6px)"
			>
				{#if (trackEnd - cut) * pxPerSec > 120}
					<span
						style="position:absolute;left:5px;top:3px;padding:1px 5px;border-radius:var(--radius-sm);background:color-mix(in srgb,var(--surface-raised) 85%,transparent);color:var(--text-secondary);font-size:var(--text-2xs);line-height:13px;white-space:nowrap"
						>the export stops here (compressor)</span
					>
				{/if}
			</span>
		{/if}
	</div>
{/if}
