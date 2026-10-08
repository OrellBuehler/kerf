<script lang="ts">
	// What every strip has in common: a dB fader, the meter beside it on the same
	// scale, and under them the fader's value and the highest peak of the run. The
	// strip decides what a gesture writes; this decides how a level is drawn and
	// worded. The fader's mapping is `mixer.ts`'s, which the timeline header's slider
	// shares, so the same level is the same place on both.
	import MixSlider from './MixSlider.svelte';
	import LevelMeter from './LevelMeter.svelte';
	import {
		FADER_STEP_DB,
		faderTicks,
		faderToGain,
		gainLabel,
		gainToDb,
		gainToFader,
		MAX_GAIN,
		nudgeGain
	} from '$lib/mixer';
	import { clippedAny, peakLabel, strongest, type StereoMeter } from '$lib/meter';
	import type { SliderKey } from '$lib/slider-gesture';

	let {
		value,
		maxGain = MAX_GAIN,
		meter,
		name,
		unityNote = '0 dB',
		accent = 'var(--kerf-400)',
		onpreview,
		oncommit,
		oncancel,
		onreset,
		onclear
	}: {
		/** The fader, linear — the committed value or a gesture's preview of it. */
		value: number;
		/** The top of the fader, linear. */
		maxGain?: number;
		meter: StereoMeter;
		/** What the strip is called, for the labels. */
		name: string;
		unityNote?: string;
		accent?: string;
		onpreview?: (v: number) => void;
		oncommit?: (v: number) => unknown;
		oncancel?: () => void;
		onreset?: () => void;
		/** Clear the held peak and the clip lamp. */
		onclear?: () => void;
	} = $props();

	const maxDb = $derived(gainToDb(maxGain));
	const toPos = (v: number) => gainToFader(v, maxGain);
	const fromPos = (p: number) => faderToGain(p, maxGain);
	const step = (v: number, key: SliderKey) =>
		key.kind === 'edge' ? (key.to === 0 ? 0 : maxGain) : nudgeGain(v, key.dir, key.size, maxGain);
	const ticks = $derived(faderTicks(maxDb).map((t) => ({ pos: t.pos })));

	const peak = $derived(strongest(meter));
	const clipped = $derived(clippedAny(meter));
	const fine = FADER_STEP_DB.fine;
	const tip = $derived(
		`${name} level ${gainLabel(value)} — drag, or ↑ ↓ to nudge ${FADER_STEP_DB.normal} dB (Shift ${FADER_STEP_DB.coarse}, Alt ${fine}); double-click for ${unityNote}`
	);
</script>

<div class="row">
	<MixSlider
		orientation="vertical"
		{value}
		{toPos}
		{fromPos}
		{step}
		label="{name} level"
		valueText={gainLabel}
		{tip}
		{ticks}
		{accent}
		{onpreview}
		{oncommit}
		{oncancel}
		{onreset}
	/>
	<LevelMeter {meter} {maxDb} label="{name} level meter" />
</div>
<div class="readouts">
	<span class="db" title="The fader, in dB">{gainLabel(value)}</span>
	<button
		type="button"
		class="peak"
		class:clipped
		disabled={!onclear}
		title={clipped
			? `${name} reached full scale — click to clear`
			: 'Highest sample peak since playback started (the Measure button reads the true peak) — click to clear'}
		aria-label="{name} highest peak{clipped ? ', clipped' : ''}: {peakLabel(peak)} dBFS. Click to clear."
		onclick={onclear}>{peakLabel(peak)}</button
	>
</div>

<style>
	.row {
		display: flex;
		flex: 1;
		min-height: 0;
		justify-content: center;
		gap: 2px;
	}
	.readouts {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 4px;
	}
	.db,
	.peak {
		font-family: var(--font-mono);
		font-size: 10px;
		font-weight: 500;
		line-height: 1;
		white-space: nowrap;
	}
	.db {
		color: var(--text-secondary);
	}
	.peak {
		min-width: 34px;
		padding: 3px 4px;
		text-align: right;
		cursor: pointer;
		color: var(--text-muted);
		background: var(--surface-inset);
		border: var(--line-width) solid var(--border-default);
		border-radius: var(--radius-xs);
	}
	.peak:hover:not(:disabled) {
		color: var(--text-primary);
	}
	.peak.clipped {
		color: var(--text-on-accent);
		background: var(--red-500);
		border-color: var(--red-500);
	}
</style>
