<script lang="ts">
	// What every strip has in common: a dB fader with a unity detent, the meter beside it on
	// the same scale, and under them the fader's value — a field to type a level into — and the
	// highest peak of the run, which is also the clip lamp's reset. The strip decides what a
	// gesture writes; this decides how a level is drawn and worded. The fader's mapping is
	// `mixer.ts`'s, which the timeline header's slider shares, so the same level is the same
	// place on both.
	//
	// A typed level is one edit, like a drag: Enter or leaving the field writes it, Escape puts
	// the old one back, and what is written goes where the strip sent it — the strip is the track
	// the field was made for, so a selection that moved on does not take it elsewhere.
	import MixSlider from './MixSlider.svelte';
	import LevelMeter from './LevelMeter.svelte';
	import {
		FADER_STEP_DB,
		faderTicks,
		faderToGain,
		gainLabel,
		gainToDb,
		gainToFader,
		levelEditable,
		MAX_GAIN,
		nudgeGain,
		parseLevelEntry
	} from '#lib/mixer';
	import { clippedAny, peakLabel, strongest, type StereoMeter } from '#lib/meter';
	import type { SliderKey } from '#lib/slider-gesture';
	import { settings } from '#lib/settings.svelte';
	import { tip as tooltip } from '#lib/tip';

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

	/** The place unity sits on the travel: a line across the rail, the one a drag is caught on. */
	const unity = $derived(gainToFader(1, maxGain));

	// ---- the typed level -------------------------------------------------------
	let editing = $state(false);
	let text = $state('');
	let dirty = false;
	function enter(e: FocusEvent & { currentTarget: HTMLInputElement }) {
		editing = true;
		dirty = false;
		text = levelEditable(value);
		e.currentTarget.value = text;
		e.currentTarget.select();
	}
	function leave(e: FocusEvent & { currentTarget: HTMLInputElement }) {
		editing = false;
		const typed = e.currentTarget.value;
		const was = dirty;
		dirty = false;
		if (!was) return;
		const gain = parseLevelEntry(typed, maxGain);
		// Not a level, or no change: the field goes back to the fader's own.
		if (gain === null || Math.abs(gain - value) < 1e-9) return;
		void oncommit?.(gain);
	}
	function typed(e: KeyboardEvent & { currentTarget: HTMLInputElement }) {
		if (e.key === 'Enter') {
			e.currentTarget.blur();
		} else if (e.key === 'Escape') {
			// Only a field with something typed in it is Escape's: otherwise it falls through to the page.
			if (!dirty) return;
			dirty = false;
			text = levelEditable(value);
			e.currentTarget.value = text;
			e.currentTarget.blur();
			e.stopPropagation();
		}
	}

	const peak = $derived(strongest(meter));
	const clipped = $derived(clippedAny(meter));
	const fine = FADER_STEP_DB.fine;
	const tip = $derived(
		`${name} level ${gainLabel(value)} — drag, or ↑ ↓ to nudge ${FADER_STEP_DB.normal} dB (${settings.modLabel('shift')} ${FADER_STEP_DB.coarse}, ${settings.modLabel('alt')} ${fine}); double-click for ${unityNote}; or type a level below`
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
		detent={unity}
		{accent}
		{onpreview}
		{oncommit}
		{oncancel}
		{onreset}
	/>
	<LevelMeter {meter} {maxDb} label="{name} level meter" />
</div>
<div class="readouts">
	<input
		class="db"
		type="text"
		inputmode="decimal"
		autocomplete="off"
		spellcheck="false"
		aria-label="{name} level in dB"
		use:tooltip={'The fader in dB: click to type a level (−6, +3, −inf), Enter to set it, Esc to leave it'}
		value={editing ? text : gainLabel(value)}
		onfocus={enter}
		onblur={leave}
		oninput={(e) => {
			dirty = true;
			text = e.currentTarget.value;
		}}
		onkeydown={typed}
	/>
	<button
		type="button"
		class="peak"
		class:clipped
		disabled={!onclear}
		use:tooltip={clipped
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
		flex: 1 1 0;
		min-width: 0;
		width: 0;
		padding: 3px 4px;
		margin-left: -4px;
		box-sizing: border-box;
		color: var(--text-secondary);
		background: transparent;
		border: var(--line-width) solid transparent;
		border-radius: var(--radius-xs);
	}
	.db:hover {
		color: var(--text-primary);
		border-color: var(--border-default);
	}
	.db:focus {
		color: var(--text-primary);
		background: var(--surface-inset);
		border-color: var(--kerf-400);
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
