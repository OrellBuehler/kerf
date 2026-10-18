<script lang="ts" module>
	import { getEnergy } from '$lib/api';

	// The energy of an asset does not change, and the Inspector remounts on every selection: ask once per asset.
	const energy = new Map<string, Promise<number[]>>();
	function energyOf(assetId: string, buckets: number): Promise<number[]> {
		const key = `${assetId}:${buckets}`;
		let p = energy.get(key);
		if (!p) {
			p = getEnergy(assetId, buckets).catch(() => {
				energy.delete(key);
				return [] as number[];
			});
			energy.set(key, p);
		}
		return p;
	}
</script>

<script lang="ts">
	// What the analysis found about a piece of music, laid out in time: the energy curve, a line at every bar (taller
	// every fourth), the intro before the first downbeat and a coloured band wherever a stretch comes back. It is
	// `get_music_structure` for a person — the same grid, bars and phrases an agent reads, so what Fit to video will
	// splice between is visible before it is asked.
	import { timeDisplay } from '$lib/time-display.svelte';
	import { barStart } from '$lib/music-fit';
	import { energyPath, musicStructure, repeatLine, repeatSpans } from '$lib/music-structure';
	import { barLength, musicSummary } from '$lib/music-ui';
	import type { MusicAnalysis } from '$lib/types';

	let { assetId, music }: { assetId: string; music: MusicAnalysis } = $props();

	const BUCKETS = 240;
	const W = 300;
	const H = 54;
	const BAND_H = 6;
	const TINTS = ['var(--agent-400)', 'var(--green-400)', 'var(--orange-400)', 'var(--kerf-300)'];

	const s = $derived(musicStructure(music));
	let curve = $state<number[]>([]);
	$effect(() => {
		const id = assetId;
		void energyOf(id, BUCKETS).then((c) => {
			if (id === assetId) curve = c;
		});
	});
	const x = (t: number) => (s.duration > 0 ? Math.min(W, Math.max(0, (t / s.duration) * W)) : 0);
	const path = $derived(energyPath(curve, W, H - BAND_H - 2));
	// A line per bar; a long piece draws only the beat of every fourth (or more) so they stay lines, not a wash.
	const barLines = $derived.by(() => {
		const step = s.bars > 64 ? 8 : s.bars > 32 ? 4 : 1;
		return Array.from({ length: s.bars }, (_, k) => k)
			.filter((k) => k % step === 0)
			.map((k) => ({ x: x(barStart(music.grid, k)), strong: k % 4 === 0 }));
	});
</script>

<div data-testid="music-structure">
	<div style="font-family:var(--font-mono);font-size:11px;color:var(--text-muted);line-height:1.5">
		{musicSummary(music)} · {barLength(music)} a bar
		{#if s.introEnd > 0.05}· intro {timeDisplay.fmt(s.introEnd)}{/if}
	</div>
	<svg
		viewBox="0 0 {W} {H}"
		role="img"
		aria-label="Energy and bar structure of the music: {s.bars} bars{s.repeats.length > 0 ? `, ${s.repeats.length} repeating ${s.repeats.length === 1 ? 'section' : 'sections'}` : ''}"
		preserveAspectRatio="none"
		style="display:block;width:100%;height:{H}px;margin-top:6px;border-radius:var(--radius-sm);background:var(--surface-inset);border:var(--line-width) solid var(--border-subtle)"
	>
		{#if path}
			<path d={path} fill="var(--kerf-500)" opacity="0.45" />
		{/if}
		{#each barLines as l, i (i)}
			<line x1={l.x} x2={l.x} y1={l.strong ? 0 : H - BAND_H - 8} y2={H - BAND_H} stroke="var(--border-strong)" stroke-width="0.6" opacity={l.strong ? 0.9 : 0.5} vector-effect="non-scaling-stroke" />
		{/each}
		{#if s.introEnd > 0.05}
			<rect x="0" y={H - BAND_H} width={x(s.introEnd)} height={BAND_H} fill="var(--text-muted)" opacity="0.5" />
		{/if}
		{#each s.repeats as r, ri (r.label)}
			{#each repeatSpans(music, r) as span, si (si)}
				<rect x={x(span.start)} y={H - BAND_H} width={Math.max(1, x(span.end) - x(span.start) - 0.6)} height={BAND_H} fill={TINTS[ri % TINTS.length]} opacity="0.85" />
			{/each}
		{/each}
	</svg>
	{#if s.repeats.length > 0}
		<ul data-testid="music-repeats" style="margin:6px 0 0;padding:0;list-style:none;display:flex;flex-direction:column;gap:2px">
			{#each s.repeats as r, ri (r.label)}
				<li style="display:flex;align-items:center;gap:6px;font-size:11px;color:var(--text-secondary)">
					<span style="flex:none;width:9px;height:9px;border-radius:2px;background:{TINTS[ri % TINTS.length]}"></span>
					{repeatLine(r)}
				</li>
			{/each}
		</ul>
	{:else}
		<div style="margin-top:6px;font-size:11px;color:var(--text-muted);line-height:1.45">
			No stretch repeats exactly, so Fit to video has nothing to splice between — it can only cut the end and fade it out.
		</div>
	{/if}
</div>
