<script lang="ts">
	// The master's FX, in a popover beside the master strip: the limiter, its ceiling and true-peak, and how the
	// tracks flagged Duck dip. They used to stack above the master fader and left it about 70 px of travel
	// against 190 for a track's at 1366x768; here the strip keeps its height and the controls open beside it,
	// over the track strips, so the master's meter stays in view while the ceiling is moved (`MixerPopover.svelte` is the box).
	//
	// Every control is the edit it always was — one per gesture, in the history — and the sound follows a held one
	// (`audio.liveMaster`): the ceiling, and the speech gate's depth, which the preview's worklet glides to.
	import MixerPopover from './MixerPopover.svelte';
	import MixSlider from './MixSlider.svelte';
	import ToolbarToggle from './ToolbarToggle.svelte';
	import { editor } from '#lib/state.svelte';
	import { audio } from '#lib/audio';
	import { report } from '#lib/ops';
	import {
		CEILING_STEP,
		ceilingLabel,
		ceilingToPos,
		DUCK_DEFAULT_DEPTH_DB,
		DUCK_STEP,
		duckLabel,
		duckToPos,
		MASTER_DEFAULT_CEILING_DB,
		nudgeCeiling,
		nudgeDuck,
		posToCeiling,
		posToDuck
	} from '#lib/levels';
	import type { SliderKey } from '#lib/slider-gesture';
	import { duckNote, limiterNote, type DuckKind, type LimiterKind } from '#lib/master-fx';
	import { settings } from '#lib/settings.svelte';
	import { tip } from '#lib/tip';
	import type { MasterBus } from '#lib/types';

	let {
		master,
		anchor,
		limiterKind,
		duckKind,
		onclose
	}: {
		master: MasterBus;
		/** The button that opened it: the popover is set beside it, and a press there closes it. */
		anchor: HTMLElement;
		limiterKind: LimiterKind;
		duckKind: DuckKind;
		onclose: () => void;
	} = $props();

	const err = report;
	const LIMITER_NOTE = $derived(limiterNote(limiterKind));
	const DUCK_NOTE = $derived(duckNote(duckKind));

	let ceilingPreview = $state<number | null>(null);
	let duckPreview = $state<number | null>(null);
	/** The depth the gate had when it was last on, so Compressor → Speech gate returns to it. */
	let lastDepth = $state(DUCK_DEFAULT_DEPTH_DB);
	const ceiling = $derived(ceilingPreview ?? master.ceiling_db);
	/** The speech gate is on while the master carries a depth; no depth is the compressor. */
	const gate = $derived(master.duck_depth_db != null);
	const depth = $derived(duckPreview ?? master.duck_depth_db ?? lastDepth);
	const anyDucked = $derived(editor.timeline.tracks.some((t) => t.duck));
	$effect(() => {
		if (master.duck_depth_db != null) lastDepth = master.duck_depth_db;
	});

	// ---- the limiter ---------------------------------------------------------
	const toggleLimiter = () => void editor.setMasterLimiter(!master.limiter).catch(err);
	const toggleTruePeak = () => void editor.setMasterTruePeak(!master.true_peak).catch(err);
	const ceilingStep = (v: number, key: SliderKey) => (key.kind === 'edge' ? (key.to === 0 ? -24 : 0) : nudgeCeiling(v, key.dir, key.size));
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

	// ---- the duck ------------------------------------------------------------
	/** The mode the select asks for. The select shows what the master holds, so a refusal puts it back. */
	async function chooseMode(select: HTMLSelectElement) {
		const wantsGate = select.value === 'gate';
		if (wantsGate === gate) return;
		try {
			await editor.setMasterDuck(wantsGate ? lastDepth : null);
		} catch (e) {
			err(e);
			select.value = gate ? 'gate' : 'compressor';
		}
	}
	const duckStep = (v: number, key: SliderKey) => (key.kind === 'edge' ? (key.to === 0 ? -40 : -1) : nudgeDuck(v, key.dir, key.size));
	function duckMove(v: number) {
		duckPreview = v;
		audio.liveMaster({ ...master, duck_depth_db: v });
	}
	async function duckCommit(v: number) {
		duckPreview = v;
		try {
			await editor.setMasterDuck(v);
		} catch (e) {
			err(e);
		} finally {
			duckPreview = null;
		}
	}
	function duckCancel() {
		duckPreview = null;
		audio.restoreMix(editor.timeline);
	}
	function duckReset() {
		duckPreview = null;
		if (master.duck_depth_db !== DUCK_DEFAULT_DEPTH_DB) void duckCommit(DUCK_DEFAULT_DEPTH_DB);
		else audio.restoreMix(editor.timeline);
	}
</script>

<MixerPopover label="Master FX" {anchor} {onclose}>

	<section aria-label="Limiter">
		<div class="line">
			<ToolbarToggle
				pressed={master.limiter}
				title={master.limiter
					? `Limiter on — holds the finished mix under ${ceilingLabel(master.ceiling_db)}. ${LIMITER_NOTE}`
					: `Limiter off — turn it on to hold the finished mix under the ceiling. ${LIMITER_NOTE}`}
				label="Limiter"
				onclick={toggleLimiter}>Limiter</ToolbarToggle
			>
			<ToolbarToggle
				pressed={!!master.true_peak}
				disabled={!master.limiter}
				title={master.true_peak
					? 'True peak on — the exported limiter runs 4× oversampled, so the peaks between samples stay under the ceiling too (what platforms measure as dBTP). The preview’s limiter is the same either way.'
					: master.limiter
						? 'True peak off — the exported limiter holds the sample peak; the peaks between samples can read up to about a decibel above the ceiling.'
						: 'True peak limits the peaks between samples as well. It needs the limiter on.'}
				label="True peak"
				onclick={toggleTruePeak}>True peak</ToolbarToggle
			>
		</div>
		<div class="slide" class:dim={!master.limiter}>
			<span class="cap">Ceiling</span>
			<span class="val">{ceilingLabel(ceiling)}</span>
			<div class="track">
				<MixSlider
					orientation="horizontal"
					value={ceiling}
					toPos={ceilingToPos}
					fromPos={posToCeiling}
					step={ceilingStep}
					label="Limiter ceiling"
					valueText={ceilingLabel}
					tip="Limiter ceiling {ceilingLabel(ceiling)} — the level the finished mix is held under. Drag, or ← → to nudge {CEILING_STEP.normal} dB ({settings.modLabel('shift')} {CEILING_STEP.coarse}, {settings.modLabel('alt')} {CEILING_STEP.fine}); double-click for {ceilingLabel(
						MASTER_DEFAULT_CEILING_DB
					)}. {LIMITER_NOTE}"
					detent={ceilingToPos(MASTER_DEFAULT_CEILING_DB)}
					accent="var(--text-muted)"
					onpreview={ceilingMove}
					oncommit={ceilingCommit}
					oncancel={ceilingCancel}
					onreset={ceilingReset}
				/>
			</div>
		</div>
	</section>

	<section aria-label="Duck">
		<div class="slide">
			<span class="cap">Duck</span>
			{#if gate}<span class="val">{duckLabel(depth)}</span>{/if}
			<select
				class="mode k-select"
				aria-label="Duck mode"
				value={gate ? 'gate' : 'compressor'}
				use:tip={`How tracks flagged Duck dip under the rest of the mix. Compressor: a sidechain compressor dips them by how loud the rest is (8:1 over −26 dBFS, 20 ms in, 400 ms out), and ends them where the rest of the mix ends, as the export does. Speech gate: they drop by exactly the depth set below while the rest of the mix speaks (easing down in about 50 ms, back up about 300 ms after it stops). ${DUCK_NOTE}`}
				onchange={(e) => void chooseMode(e.currentTarget)}
			>
				<option value="compressor">Compressor</option>
				<option value="gate">Speech gate</option>
			</select>
			{#if gate}
				<div class="track">
					<MixSlider
						orientation="horizontal"
						value={depth}
						toPos={duckToPos}
						fromPos={posToDuck}
						step={duckStep}
						label="Speech gate depth"
						valueText={duckLabel}
						tip={`Speech gate depth ${duckLabel(depth)} — how far a ducked track drops while the rest of the mix speaks. Drag, or ← → to nudge ${DUCK_STEP.normal} dB (${settings.modLabel('shift')} ${DUCK_STEP.coarse}); double-click for ${duckLabel(DUCK_DEFAULT_DEPTH_DB)}. ${DUCK_NOTE}`}
						detent={duckToPos(DUCK_DEFAULT_DEPTH_DB)}
						accent="var(--text-muted)"
						onpreview={duckMove}
						oncommit={duckCommit}
						oncancel={duckCancel}
						onreset={duckReset}
					/>
				</div>
			{/if}
		</div>
		{#if !anyDucked}
			<p class="hint">No track is set to Duck yet — turn DUCK on for the music bed, in its strip or its track header.</p>
		{:else if duckKind === 'unavailable'}
			<p class="hint">This preview cannot duck, so a ducked track plays at its fader here. The export still dips it.</p>
		{:else}
			<p class="hint">
				{gate
					? 'The preview dips a ducked track by exactly this while the rest of the mix speaks.'
					: 'The preview dips a ducked track the way the export does, by how loud the rest of the mix is.'}
			</p>
		{/if}
	</section>
</MixerPopover>

<style>
	section {
		display: flex;
		flex-direction: column;
		gap: 6px;
	}
	.line {
		display: flex;
		gap: 6px;
	}
	.line :global(.k-tt) {
		flex: 1;
	}
	.slide {
		display: grid;
		grid-template-columns: 1fr auto;
		grid-template-areas:
			'cap val'
			'ctl ctl';
		align-items: baseline;
		gap: 2px 6px;
	}
	.slide.dim {
		opacity: 0.6;
	}
	.cap {
		grid-area: cap;
		font: var(--type-overline);
		letter-spacing: var(--tracking-wide);
		text-transform: uppercase;
		color: var(--text-muted);
	}
	.val {
		grid-area: val;
		font-family: var(--font-mono);
		font-size: var(--text-2xs);
		color: var(--text-secondary);
	}
	.mode {
		grid-area: ctl;
		height: 24px;
		padding-left: 6px;
		font-size: var(--text-xs);
		font-weight: 600;
		color: var(--text-secondary);
		border-radius: var(--radius-xs);
	}
	.track {
		grid-column: 1 / -1;
		display: flex;
		margin: 0 -4px;
	}
	.slide .mode + .track {
		grid-row: 3;
	}
	.hint {
		margin: 0;
		font-size: var(--text-2xs);
		line-height: 1.35;
		color: var(--text-muted);
	}
</style>
