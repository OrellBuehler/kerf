<script lang="ts">
	// One track's channel strip: name, M / S / Duck, pan, the fader and its meter, and —
	// once the cut has been measured — what that track reads. Every control is one
	// edit per gesture; while a fader or the pan is held, the *sound* follows it
	// (`audio.liveTrack`) and the number beside it, but the project is written once,
	// on release, so a drag is one entry in the history and Escape leaves no trace.
	//
	// M, S and Duck are the toolbar's toggle (`ToolbarToggle`), so "on" is one look. A ducked track
	// says, under them, how far the preview is holding it down while it plays (`duckDb`).
	import MixSlider from './MixSlider.svelte';
	import FaderMeter from './FaderMeter.svelte';
	import ToolbarToggle from './ToolbarToggle.svelte';
	import { editor } from '#lib/state.svelte';
	import { audio } from '#lib/audio';
	import { report } from '#lib/ops';
	import { nudgePan, PAN_STEP, panLabel, panSides, panToPos, posToPan, sideLabel } from '#lib/mixer';
	import type { StereoMeter } from '#lib/meter';
	import { stateNote, type MixStrip } from '#lib/mixer-strips';
	import { readingText } from '#lib/levels-view';
	import { duckHint, duckToggleTip, type DuckKind } from '#lib/master-fx';
	import type { SliderKey } from '#lib/slider-gesture';
	import type { LevelReading } from '#lib/types';
	import { settings } from '#lib/settings.svelte';
	import { tip } from '#lib/tip';

	let {
		strip,
		meter,
		reading = null,
		stale = false,
		duckDb = null,
		duckKind = 'pending',
		onclear
	}: {
		strip: MixStrip;
		meter: StereoMeter;
		/** What the last measurement read for this track, if there was one. */
		reading?: LevelReading | null;
		/** The measurement is of a cut that has since changed. */
		stale?: boolean;
		/** How far the duck holds this strip's track down right now, dB (positive), while the preview plays; `null` when it does not. */
		duckDb?: number | null;
		/** Whether the preview ducks at all (`AudioEngine.duckKind`). */
		duckKind?: DuckKind;
		onclear: (id: string) => void;
	} = $props();

	let faderPreview = $state<number | null>(null);
	let panPreview = $state<number | null>(null);

	const volume = $derived(faderPreview ?? strip.volume);
	const pan = $derived(panPreview ?? strip.pan);
	const note = $derived(stateNote(strip));
	const read = $derived(reading ? readingText(reading) : null);
	const sides = $derived(panSides(pan));

	const err = report;

	// ---- the fader ----------------------------------------------------------
	function fadePreview(v: number) {
		faderPreview = v;
		audio.liveTrack(strip.id, { volume: v });
	}
	async function fadeCommit(v: number) {
		faderPreview = v;
		try {
			await editor.setTrackVolume(strip.id, v);
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
		if (strip.volume !== 1) void fadeCommit(1);
		else audio.restoreMix(editor.timeline);
	}

	// ---- the pan ------------------------------------------------------------
	const panStep = (v: number, key: SliderKey) =>
		key.kind === 'edge' ? (key.to === 0 ? -1 : 1) : nudgePan(v, key.dir, key.size);
	function panMove(v: number) {
		panPreview = v;
		audio.liveTrack(strip.id, { pan: v });
	}
	async function panCommit(v: number) {
		panPreview = v;
		try {
			await editor.setTrackPan(strip.id, v);
		} catch (e) {
			err(e);
		} finally {
			panPreview = null;
		}
	}
	function panCancel() {
		panPreview = null;
		audio.restoreMix(editor.timeline);
	}
	function panReset() {
		panPreview = null;
		if (strip.pan !== 0) void panCommit(0);
		else audio.restoreMix(editor.timeline);
	}

</script>

<section class="strip" class:quiet={strip.state !== 'live'} aria-label="{strip.name} channel strip">
	<header>
		<span class="name" use:tip={`${strip.name} — ${strip.clips} clip${strip.clips === 1 ? '' : 's'}${note ? ` · ${note}` : ''}`}
			>{strip.name}</span
		>
		{#if note}<span class="state" use:tip={note}>{strip.state === 'muted' ? 'muted' : 'off'}</span>{/if}
	</header>

	<div class="toggles">
		<ToolbarToggle
			label="{strip.name} mute"
			pressed={strip.muted}
			square
			title={strip.muted
				? strip.kind === 'video'
					? 'Hidden — its sound is off too. Click to show'
					: 'Muted — click to unmute'
				: strip.kind === 'video'
					? 'Hide this track (its sound goes with it)'
					: 'Mute this track'}
			onclick={() => void editor.setTrackMuted(strip.id, !strip.muted).catch(err)}>M</ToolbarToggle
		>
		<ToolbarToggle
			label="{strip.name} solo"
			pressed={strip.solo}
			square
			title={strip.solo ? 'Soloed — click to clear' : `Solo — play only the soloed ${strip.kind} tracks`}
			onclick={() => void editor.setTrackSolo(strip.id, !strip.solo).catch(err)}>S</ToolbarToggle
		>
		{#if strip.kind === 'audio'}
			<ToolbarToggle
				label="{strip.name} duck"
				pressed={strip.duck}
				title={duckToggleTip(strip.duck, duckKind)}
				style="flex:1;min-width:0;padding:0 4px;font-size:var(--text-xs)"
				onclick={() => void editor.setTrackDuck(strip.id, !strip.duck).catch(err)}>DUCK</ToolbarToggle
			>
		{/if}
	</div>
	<div class="hint" class:active={duckDb !== null && duckDb >= 0.5} aria-live="off" aria-hidden={!strip.duck}>
		{strip.duck ? duckHint(duckKind, duckDb) : ''}
	</div>

	<div class="pan">
		<MixSlider
			orientation="horizontal"
			value={pan}
			toPos={panToPos}
			fromPos={posToPan}
			step={panStep}
			label="{strip.name} pan"
			valueText={panLabel}
			tip="Pan {panLabel(pan)} — left {sideLabel(sides.left)}, right {sideLabel(sides.right)}. Drag, or ← → to nudge {PAN_STEP.normal *
				100}% ({settings.modLabel('shift')} {PAN_STEP.coarse * 100}%, {settings.modLabel('alt')} {PAN_STEP.fine * 100}%); double-click to centre"
			detent={0.5}
			fillFrom={0.5}
			accent="var(--text-muted)"
			onpreview={panMove}
			oncommit={panCommit}
			oncancel={panCancel}
			onreset={panReset}
		/>
		<span class="pan-label">{panLabel(pan) === 'centre' ? 'C' : panLabel(pan)}</span>
	</div>

	<FaderMeter
		value={volume}
		meter={meter}
		name={strip.name}
		onpreview={fadePreview}
		oncommit={fadeCommit}
		oncancel={fadeCancel}
		onreset={fadeReset}
		onclear={() => onclear(strip.id)}
	/>

	{#if read}
		<dl class="read" class:stale use:tip={stale ? 'Measured before the last edit' : 'What the last measurement read for this track'}>
			<div><dt>LUFS</dt><dd>{read.lufs}</dd></div>
			<div><dt>TP</dt><dd>{read.truePeak}</dd></div>
			<div><dt>LRA</dt><dd>{read.range}</dd></div>
		</dl>
	{/if}
</section>

<style>
	.strip {
		flex: none;
		width: 108px;
		display: flex;
		flex-direction: column;
		gap: 5px;
		padding: 7px 7px 8px;
		box-sizing: border-box;
		/* As tall as the panel allows, never shorter than its controls need. */
		min-height: 268px;
		background: var(--surface-raised);
		border: var(--line-width) solid var(--border-default);
		border-radius: var(--radius-md);
		transition: opacity var(--dur-normal) var(--ease-out);
	}
	/* A muted or empty strip dims what it shows, not what it offers: the M / S / D buttons — the way back — and
	   the name's own red "muted" stay at full strength. */
	.strip.quiet > :global(:not(header):not(.toggles)),
	.strip.quiet .name {
		opacity: 0.6;
	}
	header {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 4px;
		min-height: 16px;
	}
	.name {
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		font-family: var(--font-mono);
		font-size: 12px;
		font-weight: 600;
		color: var(--text-secondary);
	}
	.state {
		flex: none;
		font: var(--type-overline);
		letter-spacing: var(--tracking-wide);
		text-transform: uppercase;
		color: var(--red-400);
	}
	.toggles {
		display: flex;
		gap: 3px;
	}
	.hint {
		min-height: 10px;
		margin: -2px 0 -1px;
		font-family: var(--font-mono);
		font-size: var(--text-2xs);
		line-height: 1;
		text-align: center;
		color: var(--text-muted);
	}
	/* The duck is holding the track down: said in the accent, so a quiet bed that is *ducked* reads apart from one that is turned down. */
	.hint.active {
		color: var(--kerf-300);
	}
	.pan {
		display: flex;
		align-items: center;
		gap: 2px;
		margin: 0 -4px;
	}
	.pan-label {
		flex: none;
		width: 24px;
		text-align: left;
		font-family: var(--font-mono);
		font-size: 10px;
		color: var(--text-secondary);
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
