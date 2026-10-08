<script lang="ts">
	// The Mixer panel: one channel strip per track that can be heard, and the master.
	//
	// What it shows is the project (`editor.timeline` — faders, pans, mute / solo /
	// duck, the master and its limiter) and, while the preview plays, what is actually
	// coming out of it: `audio.meters()` reads the analysers on each track's bus and on
	// the master, and a frame loop here steps `meter.ts`'s ballistics. The loop runs
	// only while playback does; stopped, the bars drop to nothing and what the run
	// reached (the held peak, a clip lamp) stays to be read.
	//
	// Two things are *not* the preview's: ducking happens on export (Web Audio's
	// compressor has no sidechain input without an AudioWorklet), and the limiter is an
	// approximation — each says so where you switch it on. The Measure button reads
	// the real mix.
	import { untrack } from 'svelte';
	import MixerStrip from './MixerStrip.svelte';
	import MasterStrip from './MasterStrip.svelte';
	import Badge from './Badge.svelte';
	import { editor } from '$lib/state.svelte';
	import { ui } from '$lib/editor-ui.svelte';
	import { audio } from '$lib/audio';
	import { masterOf } from '$lib/levels';
	import { mixerStrips } from '$lib/mixer-strips';
	import { clearStereo, IDLE_STEREO, SILENCE, settleStereo, stepStereo, type StereoMeter } from '$lib/meter';
	import { isStale, noteTone, verdict } from '$lib/levels-view';

	/** Assets that carry sound, rebuilt only when the bin changes. */
	const audible = $derived(
		new Set(editor.assets.filter((a) => a.streams?.some((s) => s.kind === 'audio')).map((a) => a.id))
	);
	const strips = $derived(mixerStrips(editor.timeline, audible));
	const master = $derived(masterOf(editor.timeline));

	// ---- the meters ------------------------------------------------------------

	/** Each strip's meter by track id, and the master's under `master`. */
	let meters = $state<Record<string, StereoMeter>>({});
	/** Whether the panel is on screen. A panel in a background tab stays mounted but has
	 *  nobody looking at its meters, so the frame loop idles instead of reading them. */
	let root = $state<HTMLDivElement | null>(null);
	let visible = true;
	$effect(() => {
		if (!root) return;
		const io = new IntersectionObserver((entries) => (visible = entries.at(-1)?.isIntersecting ?? true));
		io.observe(root);
		return () => io.disconnect();
	});
	const meterOf = (id: string): StereoMeter => meters[id] ?? IDLE_STEREO;

	$effect(() => {
		if (!ui.playing) {
			// Stopped: the bars fall, the run's high-water marks stay.
			untrack(() => {
				meters = Object.fromEntries(Object.entries(meters).map(([id, m]) => [id, settleStereo(m)]));
			});
			return;
		}
		// A fresh run starts with the marks clear (a restart for an edit does not: the
		// effect only re-runs when playback itself changes).
		untrack(() => (meters = {}));
		let raf = 0;
		let last = performance.now();
		const frame = (now: number) => {
			const dt = now - last;
			last = now;
			const read = visible ? audio.meters() : null;
			if (read) {
				const next: Record<string, StereoMeter> = {};
				for (const s of untrack(() => strips)) {
					const r = read.tracks.get(s.id) ?? { l: SILENCE, r: SILENCE };
					next[s.id] = stepStereo(untrack(() => meterOf(s.id)), r.l, r.r, dt);
				}
				next.master = stepStereo(untrack(() => meterOf('master')), read.master.l, read.master.r, dt);
				meters = next;
			}
			raf = requestAnimationFrame(frame);
		};
		raf = requestAnimationFrame(frame);
		return () => cancelAnimationFrame(raf);
	});

	const clear = (id: string) => (meters = { ...meters, [id]: clearStereo(meterOf(id)) });

	// ---- the loudness measurement ---------------------------------------------

	const measure = $derived(ui.measure);
	const result = $derived(measure.result);
	/** The project as it is now, to tell a measurement of an older cut from a current one. */
	const now = $derived({ seq: editor.history.find((r) => r.current)?.seq ?? null, path: editor.currentPath });
	const stale = $derived(result ? isStale(result.stamp, now) : false);
	const summary = $derived(result ? verdict(result.levels) : null);
	const readingOf = (id: string) => result?.levels.tracks.find((t) => t.track_id === id)?.level ?? null;
</script>

<div class="mixer" bind:this={root}>
	<div class="row">
		<div class="strips" role="group" aria-label="Track strips">
			{#each strips as strip (strip.id)}
				<MixerStrip {strip} meter={meterOf(strip.id)} reading={readingOf(strip.id)} {stale} onclear={clear} />
			{:else}
				<p class="empty">
					Nothing with sound on the timeline yet. Add a clip that has audio — or an audio track — and its strip
					appears here.
				</p>
			{/each}
		</div>
		<MasterStrip
			{master}
			meter={meterOf('master')}
			measuring={measure.running}
			stopping={measure.stopping}
			{result}
			{stale}
			onmeasure={() => void ui.measureLevels()}
			onstop={() => ui.stopMeasure()}
			onclear={() => clear('master')}
		/>
	</div>

	{#if result && summary}
		<section class="loudness" class:stale aria-label="Loudness">
			<div class="verdict {summary.tone}">
				<span class="dot"></span>
				<span class="text">{summary.text}</span>
				{#if stale}<Badge tone="warning">out of date</Badge>{/if}
				{#if result.levels.estimated}
					<Badge tone="warning"
						>estimated</Badge
					>
				{/if}
			</div>
			<ul>
				{#each result.levels.notes as note (note)}
					<li class={noteTone(note)}>{note}</li>
				{/each}
			</ul>
			{#if result.levels.estimated}
				<p class="caption">
					There is no audio analyzer in the browser, so these numbers are estimated from the sample analysis. The
					desktop app measures the real mix.
				</p>
			{/if}
		</section>
	{/if}
	{#if measure.error}
		<p class="error" role="alert">Couldn't measure the loudness — {measure.error}</p>
	{/if}

	<p class="foot">
		Meters read what the preview plays (sample peak). Ducking and the exact limiter are applied on export.
	</p>
</div>

<style>
	.mixer {
		flex: 1;
		min-height: 0;
		min-width: 0;
		display: flex;
		flex-direction: column;
		gap: 8px;
		padding: 8px;
		box-sizing: border-box;
		overflow: auto;
		background: var(--surface-panel);
	}
	.row {
		flex: 1 0 auto;
		display: flex;
		gap: 8px;
		min-height: 0;
	}
	.strips {
		flex: 1;
		min-width: 0;
		display: flex;
		gap: 6px;
		overflow-x: auto;
		overflow-y: hidden;
		/* A strip is as tall as the panel lets it be, but never shorter than its controls. */
		align-items: stretch;
	}
	.empty {
		align-self: center;
		margin: 0 auto;
		max-width: 260px;
		font-size: 12px;
		line-height: 1.45;
		text-align: center;
		color: var(--text-muted);
	}
	.loudness {
		flex: none;
		display: flex;
		flex-direction: column;
		gap: 6px;
		padding: 8px 10px;
		background: var(--surface-raised);
		border: var(--line-width) solid var(--border-default);
		border-radius: var(--radius-md);
	}
	.loudness.stale {
		opacity: 0.7;
	}
	.verdict {
		display: flex;
		align-items: center;
		gap: 8px;
		font-family: var(--font-mono);
		font-size: 12px;
		font-weight: 600;
		color: var(--text-primary);
	}
	.verdict .dot {
		flex: none;
		width: 8px;
		height: 8px;
		border-radius: 50%;
		background: var(--text-muted);
	}
	.verdict.ok .dot {
		background: var(--success);
	}
	.verdict.warn .dot {
		background: var(--warning);
	}
	.verdict .text {
		min-width: 0;
	}
	ul {
		margin: 0;
		padding: 0;
		list-style: none;
		display: flex;
		flex-direction: column;
		gap: 4px;
	}
	li {
		position: relative;
		padding-left: 12px;
		font-size: 12px;
		line-height: 1.4;
		color: var(--text-secondary);
	}
	li::before {
		content: '';
		position: absolute;
		left: 0;
		top: 0.55em;
		width: 5px;
		height: 5px;
		border-radius: 50%;
		background: var(--text-muted);
	}
	li.ok::before {
		background: var(--success);
	}
	li.warn::before {
		background: var(--warning);
	}
	.caption,
	.foot,
	.error {
		margin: 0;
		font-size: 11px;
		line-height: 1.4;
		color: var(--text-muted);
	}
	.error {
		color: var(--red-400);
	}
	.foot {
		flex: none;
		padding: 0 2px;
	}
</style>
