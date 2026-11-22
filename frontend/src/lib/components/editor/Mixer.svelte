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
	// Ducking is the preview's too (`audio.ts`: the export's compressor or speech gate in a worklet,
	// keyed by the rest of the mix), and while it plays a ducked strip says how far it is down. The
	// limiter is the export's own. The peak readout says the meters are the preview's sample peak;
	// the Measure button reads the real mix.
	import { untrack } from 'svelte';
	import { intersectionObserverFor } from '#lib/realm';
	import { windows, type FrameHandle } from '#lib/windows.svelte';
	import MixerStrip from './MixerStrip.svelte';
	import MasterStrip from './MasterStrip.svelte';
	import MasterFx from './MasterFx.svelte';
	import MasterDelivery from './MasterDelivery.svelte';
	import { editor } from '#lib/state.svelte';
	import { ui } from '#lib/editor-ui.svelte';
	import { audio } from '#lib/audio';
	import { masterOf } from '#lib/levels';
	import { mixerStrips } from '#lib/mixer-strips';
	import { clearStereo, IDLE_STEREO, SILENCE, settleStereo, stepStereo, type StereoMeter } from '#lib/meter';
	import { deliveryCheck, deliveryTarget, isStale, scopeLabel, verdict } from '#lib/levels-view';
	import { timeDisplay } from '#lib/time-display.svelte';
	import { reductionDb } from '#lib/duck';

	/** Assets that carry sound, rebuilt only when the bin changes. */
	const audible = $derived(
		new Set(editor.assets.filter((a) => a.streams?.some((s) => s.kind === 'audio')).map((a) => a.id))
	);
	const strips = $derived(mixerStrips(editor.timeline, audible));
	const master = $derived(masterOf(editor.timeline));

	// ---- the meters ------------------------------------------------------------

	/** Each strip's meter by track id, and the master's under `master`. */
	let meters = $state<Record<string, StereoMeter>>({});
	/** Which limiter the preview plays, kept as the engine learns it (the FX popover's tooltips say which). */
	let limiterKind = $state(audio.limiterKind());
	$effect(() => audio.onLimiterKind((kind) => (limiterKind = kind)));
	/** Which popover beside the master strip is open, and the buttons that open them (a popover hands the focus back there). */
	let pop = $state<'fx' | 'delivery' | null>(null);
	let fxButton = $state<HTMLButtonElement | null>(null);
	let deliveryButton = $state<HTMLButtonElement | null>(null);
	const togglePop = (which: 'fx' | 'delivery') => (pop = pop === which ? null : which);
	/** Whether the preview ducks (`audio.duckKind`): the worklet loads the first time a ducked mix plays. */
	let duckKind = $state(audio.duckKind());
	$effect(() => audio.onDuckKind((kind) => (duckKind = kind)));
	/** How far the duck is holding the ducked tracks down, dB (positive), while the preview plays; `null` when it is not ducking. */
	let duck = $state<number | null>(null);
	/** Whether the panel is on screen. A panel in a background tab stays mounted but has
	 *  nobody looking at its meters, so the frame loop idles instead of reading them. */
	let root = $state<HTMLDivElement | null>(null);
	let visible = true;
	$effect(() => {
		if (!root) return;
		// Re-run when the panel moves to another window, and observe from that window: an
		// observer made by the editor window's constructor reports an element in a
		// detached one as not intersecting, for good.
		void windows.version;
		const io = intersectionObserverFor(root, (entries) => (visible = entries.at(-1)?.isIntersecting ?? true));
		io.observe(root);
		return () => io.disconnect();
	});
	const meterOf = (id: string): StereoMeter => meters[id] ?? IDLE_STEREO;

	$effect(() => {
		if (!ui.playing) {
			// Stopped: the bars fall, the run's high-water marks stay.
			untrack(() => {
				meters = Object.fromEntries(Object.entries(meters).map(([id, m]) => [id, settleStereo(m)]));
				duck = null;
			});
			return;
		}
		// A fresh run starts with the marks clear (a restart for an edit does not: the
		// effect only re-runs when playback itself changes).
		untrack(() => (meters = {}));
		let raf: FrameHandle | null = null;
		let last = performance.now();
		// Frames come from whichever window is showing, whose timestamps count from its own
		// start: the editor's own clock is read instead.
		const frame = () => {
			const now = performance.now();
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
				duck = read.duck === null ? null : reductionDb(read.duck);
			}
			raf = windows.requestFrame(frame);
		};
		raf = windows.requestFrame(frame);
		return () => windows.cancelFrame(raf);
	});

	// A popover opens at the top of the panel, where it is held to the panel's height: a panel scrolled down would hide it.
	$effect(() => {
		if (pop && root) root.scrollTop = 0;
	});

	const clear = (id: string) => (meters = { ...meters, [id]: clearStereo(meterOf(id)) });

	// ---- the loudness measurement ---------------------------------------------

	const measure = $derived(ui.measure);
	const result = $derived(measure.result);
	/** The project as it is now, to tell a measurement of an older cut from a current one. */
	const now = $derived({ seq: editor.history.find((r) => r.current)?.seq ?? null, path: editor.currentPath });
	const stale = $derived(result ? isStale(result.stamp, now) : false);
	const summary = $derived(result ? verdict(result.levels) : null);
	/** The loudness the cut is held to: the one chosen in Export (the options its dialog was last closed on), else the streaming target the engine measures by. */
	const target = $derived(deliveryTarget(ui.exportDraft?.opts, result?.levels.target_lufs));
	/** The measured cut against its delivery's lines: the strip's two lines, and the popover they open. */
	const rows = $derived(result ? deliveryCheck(result.levels, master, target) : null);
	const readingOf = (id: string) => result?.levels.tracks.find((t) => t.track_id === id)?.level ?? null;
</script>

<div class="mixer" bind:this={root}>
	<div class="row">
		<div class="strips" role="group" aria-label="Track strips">
			{#each strips as strip (strip.id)}
				<MixerStrip {strip} meter={meterOf(strip.id)} reading={readingOf(strip.id)} {stale} duckDb={strip.duck ? duck : null} {duckKind} onclear={clear} />
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
			{rows}
			fxOpen={pop === 'fx'}
			bind:fxButton
			onfx={() => togglePop('fx')}
			deliveryOpen={pop === 'delivery'}
			bind:deliveryButton
			ondelivery={() => togglePop('delivery')}
			onmeasure={() => void ui.measureLevels()}
			onstop={() => ui.stopMeasure()}
			onclear={() => clear('master')}
		/>
		{#if pop === 'fx' && fxButton}
			<MasterFx {master} anchor={fxButton} {limiterKind} {duckKind} onclose={() => (pop = null)} />
		{:else if pop === 'delivery' && deliveryButton && result && rows}
			<MasterDelivery
				{rows}
				{target}
				scope={scopeLabel(result.range, result.levels.duration, (s) => timeDisplay.fmt(s))}
				{stale}
				estimated={!!result.levels.estimated}
				verdict={summary}
				notes={result.levels.notes}
				anchor={deliveryButton}
				onfx={() => (pop = 'fx')}
				onclose={() => (pop = null)}
			/>
		{/if}
	</div>

	{#if measure.error}
		<p class="error" role="alert">Couldn't measure the loudness — {measure.error}</p>
	{/if}
</div>

<style>
	.mixer {
		/* The master's width: two strips (108 px each) and it fit the 370 px the Audio workspace gives the panel at 1366 px. */
		--master-w: 120px;
		/* A size container, so a popover can be held to the height the panel shows (`cqh`), not the height of the strips it scrolls. */
		container-type: size;
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
		/* The master FX popover is set against the row, over the strips to the master's left. */
		position: relative;
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
		/* A panel docked narrower than its strips scrolls them, and a strip lands whole rather than cut: the master stays put. */
		scroll-snap-type: x proximity;
		scrollbar-width: thin;
		/* A strip is as tall as the panel lets it be, but never shorter than its controls. */
		align-items: stretch;
	}
	.strips > :global(.strip) {
		scroll-snap-align: start;
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
	.error {
		margin: 0;
		font-size: 11px;
		line-height: 1.4;
		color: var(--red-400);
	}
</style>
