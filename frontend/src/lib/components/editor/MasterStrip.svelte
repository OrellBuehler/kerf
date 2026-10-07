<script lang="ts">
	// The master strip: the fader and the limiter on the finished mix, its meter, and
	// the Measure button that reads how loud the cut actually is. The fader and the
	// limiter are edits like any other (one per gesture, in the history); while they
	// are held the preview follows them (`audio.liveMaster`).
	//
	// The preview's limiter is an approximation — a compressor with a hard knee at the
	// ceiling — and the toggle's tooltip says so; Measure reads the real thing.
	import MixSlider from './MixSlider.svelte';
	import FaderMeter from './FaderMeter.svelte';
	import Badge from './Badge.svelte';
	import Icon from './Icon.svelte';
	import { editor } from '$lib/state.svelte';
	import { audio } from '$lib/audio';
	import { toast } from '$lib/notifications.svelte';
	import {
		CEILING_STEP,
		ceilingLabel,
		ceilingToPos,
		MASTER_DEFAULT_CEILING_DB,
		MASTER_MAX_VOLUME,
		nudgeCeiling,
		posToCeiling
	} from '$lib/levels';
	import type { StereoMeter } from '$lib/meter';
	import type { SliderKey } from '$lib/slider-gesture';
	import { readingText, scopeLabel } from '$lib/levels-view';
	import type { MasterBus } from '$lib/types';
	import type { MeasureResult } from '$lib/editor-ui.svelte';

	let {
		master,
		meter,
		measuring,
		result,
		stale,
		onmeasure,
		onclear
	}: {
		master: MasterBus;
		meter: StereoMeter;
		measuring: boolean;
		result: MeasureResult | null;
		stale: boolean;
		onmeasure: () => void;
		onclear: () => void;
	} = $props();

	let faderPreview = $state<number | null>(null);
	let ceilingPreview = $state<number | null>(null);

	const volume = $derived(faderPreview ?? master.volume);
	const ceiling = $derived(ceilingPreview ?? master.ceiling_db);
	const read = $derived(result ? readingText(result.levels.master) : null);

	function err(e: unknown) {
		toast.error(e instanceof Error ? e.message : String(e));
	}

	const APPROX =
		'The export runs a lookahead limiter (alimiter). The preview plays an approximation — a compressor with a hard knee at the ceiling — so a very hot mix can read a little over it. Measure reads the real thing.';

	// ---- the fader ----------------------------------------------------------
	function fadePreview(v: number) {
		faderPreview = v;
		audio.liveMaster({ ...master, volume: v });
	}
	async function fadeCommit(v: number) {
		faderPreview = v;
		try {
			await editor.setMasterVolume(v);
		} catch (e) {
			err(e);
		} finally {
			faderPreview = null;
		}
	}
	function fadeCancel() {
		faderPreview = null;
		audio.restoreMix(editor.timeline);
	}
	function fadeReset() {
		faderPreview = null;
		if (master.volume !== 1) void fadeCommit(1);
		else audio.restoreMix(editor.timeline);
	}

	// ---- the limiter ----------------------------------------------------------
	function toggleLimiter() {
		void editor.setMasterLimiter(!master.limiter).catch(err);
	}
	const ceilingStep = (v: number, key: SliderKey) =>
		key.kind === 'edge' ? (key.to === 0 ? -24 : 0) : nudgeCeiling(v, key.dir, key.size);
	function ceilingMove(v: number) {
		ceilingPreview = v;
		audio.liveMaster({ ...master, ceiling_db: v });
	}
	async function ceilingCommit(v: number) {
		ceilingPreview = v;
		try {
			await editor.setMasterLimiter(master.limiter, v);
		} catch (e) {
			err(e);
		} finally {
			ceilingPreview = null;
		}
	}
	function ceilingCancel() {
		ceilingPreview = null;
		audio.restoreMix(editor.timeline);
	}
	function ceilingReset() {
		ceilingPreview = null;
		if (master.ceiling_db !== MASTER_DEFAULT_CEILING_DB) void ceilingCommit(MASTER_DEFAULT_CEILING_DB);
		else audio.restoreMix(editor.timeline);
	}
</script>

<section class="strip" aria-label="Master channel strip">
	<header><span class="name">Master</span></header>

	<button
		type="button"
		class="limiter"
		class:on={master.limiter}
		aria-pressed={master.limiter}
		title={master.limiter
			? `Limiter on — holds the finished mix under ${ceilingLabel(master.ceiling_db)}. ${APPROX}`
			: `Limiter off — turn it on to hold the finished mix under the ceiling. ${APPROX}`}
		onclick={toggleLimiter}>Limiter</button
	>
	<div class="ceiling" class:dim={!master.limiter}>
		<span class="cap">Ceiling</span>
		<div class="ceiling-row">
			<MixSlider
				orientation="horizontal"
				value={ceiling}
				toPos={ceilingToPos}
				fromPos={posToCeiling}
				step={ceilingStep}
				label="Limiter ceiling"
				valueText={ceilingLabel}
				tip="Limiter ceiling {ceilingLabel(ceiling)} — the level the finished mix is held under. Drag, or ← → to nudge {CEILING_STEP.normal} dB (Shift {CEILING_STEP.coarse}, Alt {CEILING_STEP.fine}); double-click for {ceilingLabel(
					MASTER_DEFAULT_CEILING_DB
				)}. {APPROX}"
				detent={ceilingToPos(MASTER_DEFAULT_CEILING_DB)}
				accent="var(--text-muted)"
				onpreview={ceilingMove}
				oncommit={ceilingCommit}
				oncancel={ceilingCancel}
				onreset={ceilingReset}
			/>
		</div>
		<span class="ceil-val">{ceilingLabel(ceiling)}</span>
	</div>

	<FaderMeter
		value={volume}
		maxGain={MASTER_MAX_VOLUME}
		meter={meter}
		name="Master"
		onpreview={fadePreview}
		oncommit={fadeCommit}
		oncancel={fadeCancel}
		onreset={fadeReset}
		{onclear}
	/>

	<button
		type="button"
		class="measure"
		disabled={measuring}
		aria-busy={measuring}
		title="Measure the loudness of the cut — integrated LUFS, true peak and loudness range, for the finished mix and each track — over the whole cut, or the in / out range when both marks are set. It reads the audio the export would render, so it takes a while on a long cut."
		onclick={onmeasure}
	>
		{#if measuring}
			<span class="kerf-spin" style="display:inline-flex"><Icon n="loader" s={13} /></span>Measuring…
		{:else}
			<Icon n="audio-waveform" s={13} />Measure
		{/if}
	</button>

	{#if result}
		<div class="scope" class:stale>
			<span title={stale ? 'The cut changed after this was measured — measure again' : undefined}
				>{stale ? 'Out of date · ' : ''}{scopeLabel(result.range, result.levels.duration)}</span
			>
			{#if result.levels.estimated}
				<Badge tone="warning" style="height:16px;padding:0 5px;font-size:9px;margin-top:2px"
					>estimated</Badge
				>
			{/if}
		</div>
		{#if read}
			<dl class="read" class:stale>
				<div><dt>LUFS</dt><dd>{read.lufs}</dd></div>
				<div><dt>True peak</dt><dd>{read.truePeak}</dd></div>
				<div><dt>LRA</dt><dd>{read.range}</dd></div>
				{#if read.shortTerm}<div title="The loudest three seconds"><dt>Short-term</dt><dd>{read.shortTerm}</dd></div>{/if}
			</dl>
		{/if}
	{/if}
</section>

<style>
	.strip {
		flex: none;
		width: 132px;
		display: flex;
		flex-direction: column;
		gap: 5px;
		padding: 7px 8px 8px;
		box-sizing: border-box;
		min-height: 268px;
		background: var(--surface-raised);
		border: var(--line-width) solid var(--border-strong);
		border-radius: var(--radius-md);
	}
	header {
		display: flex;
		align-items: center;
		min-height: 16px;
	}
	.name {
		font: var(--type-overline);
		letter-spacing: var(--tracking-caps);
		text-transform: uppercase;
		color: var(--text-secondary);
	}
	.limiter,
	.measure {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		gap: 6px;
		height: 24px;
		padding: 0 8px;
		cursor: pointer;
		font-family: var(--font-sans);
		font-size: 11px;
		font-weight: 600;
		color: var(--text-secondary);
		background: transparent;
		border: var(--line-width) solid var(--border-strong);
		border-radius: var(--radius-xs);
	}
	.limiter:hover,
	.measure:hover:not(:disabled) {
		color: var(--text-primary);
	}
	.limiter.on {
		color: var(--text-on-accent);
		background: var(--kerf-500);
		border-color: var(--kerf-500);
	}
	.measure {
		background: var(--surface-hover);
	}
	.measure:disabled {
		cursor: progress;
	}
	.ceiling {
		display: grid;
		grid-template-columns: 1fr auto;
		grid-template-areas:
			'cap val'
			'row row';
		align-items: baseline;
		gap: 0 4px;
	}
	.ceiling.dim {
		opacity: 0.6;
	}
	.cap {
		grid-area: cap;
		font: var(--type-overline);
		letter-spacing: var(--tracking-wide);
		text-transform: uppercase;
		color: var(--text-muted);
	}
	.ceil-val {
		grid-area: val;
		font-family: var(--font-mono);
		font-size: 10px;
		color: var(--text-secondary);
	}
	.ceiling-row {
		grid-area: row;
		display: flex;
		margin: 0 -4px;
	}
	.scope {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		justify-content: space-between;
		gap: 2px 4px;
		font-size: 10px;
		line-height: 1.2;
		color: var(--text-muted);
	}
	.scope.stale {
		color: var(--orange-400);
	}
	.read {
		margin: 0;
		display: flex;
		flex-direction: column;
		gap: 2px;
		padding-top: 5px;
		border-top: var(--line-width) solid var(--border-subtle);
		font-family: var(--font-mono);
		font-size: 10px;
		color: var(--text-secondary);
	}
	.read.stale {
		opacity: 0.5;
	}
	.read div {
		display: flex;
		justify-content: space-between;
		gap: 4px;
	}
	.read dt {
		color: var(--text-muted);
	}
	.read dd {
		margin: 0;
	}
</style>
