<script lang="ts">
	// The master strip: the fader and the limiter on the finished mix, its meter, and
	// the Measure button that reads how loud the cut actually is. The fader and the
	// limiter are edits like any other (one per gesture, in the history); while they
	// are held the preview follows them (`audio.liveMaster`).
	//
	// The preview's limiter is the export's own (`alimiter`, ported sample for sample and run
	// in an AudioWorklet by `audio.ts`); only true peak's oversampling is export-only.
	//
	// The limiter, its ceiling and the duck mode are the strip's FX block: folded by default and remembered
	// (`master-fx.ts`), so the fader gets the strip's height and a limiter that is on still says so.
	//
	// The Duck pair picks how the tracks flagged Duck dip: under the sidechain compressor
	// (the default; it dips by how loud the rest is) or the speech gate (exactly the depth set
	// here while the rest speaks). Export only, like the Duck toggle itself.
	import MixSlider from './MixSlider.svelte';
	import FaderMeter from './FaderMeter.svelte';
	import Badge from './Badge.svelte';
	import Icon from './Icon.svelte';
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
		MASTER_MAX_VOLUME,
		nudgeCeiling,
		nudgeDuck,
		posToCeiling,
		posToDuck
	} from '#lib/levels';
	import type { StereoMeter } from '#lib/meter';
	import type { SliderKey } from '#lib/slider-gesture';
	import { readingText, scopeLabel } from '#lib/levels-view';
	import { timeDisplay } from '#lib/time-display.svelte';
	import type { MasterBus } from '#lib/types';
	import type { MeasureResult } from '#lib/editor-ui.svelte';
	import { fxSummary, loadMasterFxOpen, saveMasterFxOpen } from '#lib/master-fx';
	import { settings } from '#lib/settings.svelte';
	import { tip } from '#lib/tip';

	let {
		master,
		meter,
		measuring,
		stopping,
		result,
		stale,
		onmeasure,
		onstop,
		onclear
	}: {
		master: MasterBus;
		meter: StereoMeter;
		measuring: boolean;
		stopping: boolean;
		result: MeasureResult | null;
		stale: boolean;
		onmeasure: () => void;
		onstop: () => void;
		onclear: () => void;
	} = $props();

	let fxOpen = $state(loadMasterFxOpen());
	const fx = $derived(fxSummary(master));
	function toggleFx() {
		fxOpen = !fxOpen;
		saveMasterFxOpen(fxOpen);
	}

	let faderPreview = $state<number | null>(null);
	let ceilingPreview = $state<number | null>(null);
	let duckPreview = $state<number | null>(null);
	/** The depth the gate had when it was last on, so Compressor → Speech gate returns to it. */
	let lastDepth = $state(DUCK_DEFAULT_DEPTH_DB);

	const volume = $derived(faderPreview ?? master.volume);
	const ceiling = $derived(ceilingPreview ?? master.ceiling_db);
	const read = $derived(result ? readingText(result.levels.master) : null);
	/** The speech gate is on while the master carries a depth; no depth is the compressor. */
	const gate = $derived(master.duck_depth_db != null);
	const depth = $derived(duckPreview ?? master.duck_depth_db ?? lastDepth);
	const anyDucked = $derived(editor.timeline.tracks.some((t) => t.duck));
	$effect(() => {
		if (master.duck_depth_db != null) lastDepth = master.duck_depth_db;
	});

	const err = report;

	const LIMITER_NOTE =
		'The preview plays the export’s own limiter (FFmpeg’s alimiter, sample for sample), so a hot mix sounds and meters here as it will export, short of True peak’s oversampling.';

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
	function toggleTruePeak() {
		void editor.setMasterTruePeak(!master.true_peak).catch(err);
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

	// ---- the duck mode ----------------------------------------------------------
	const DUCK_NOTE =
		'Ducking is applied on export: the preview plays a ducked track at its fader. Turn Duck on for the tracks that should dip (the music bed) in the track header or the strip.';
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
	const duckStep = (v: number, key: SliderKey) =>
		key.kind === 'edge' ? (key.to === 0 ? -40 : -1) : nudgeDuck(v, key.dir, key.size);
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
	function duckReset() {
		duckPreview = null;
		if (master.duck_depth_db !== DUCK_DEFAULT_DEPTH_DB) void duckCommit(DUCK_DEFAULT_DEPTH_DB);
	}
</script>

<section class="strip" aria-label="Master channel strip">
	<header><span class="name">Master</span></header>

	<button
		type="button"
		class="fx-toggle"
		class:active={fx.active}
		aria-expanded={fxOpen}
		aria-controls="master-fx"
		use:tip={fxOpen
			? 'Fold the limiter and duck controls away'
			: 'The limiter and the duck mode — folded so the fader has the strip’s height'}
		onclick={toggleFx}
	>
		<span class="fx-label">FX</span>
		<span class="fx-sum">{fx.text}</span>
		<Icon n={fxOpen ? 'chevron-up' : 'chevron-down'} s={12} />
	</button>

	{#if fxOpen}
		<div id="master-fx" class="fx">
			<button
				type="button"
				class="limiter"
				class:on={master.limiter}
				aria-pressed={master.limiter}
				use:tip={master.limiter
					? `Limiter on — holds the finished mix under ${ceilingLabel(master.ceiling_db)}. ${LIMITER_NOTE}`
					: `Limiter off — turn it on to hold the finished mix under the ceiling. ${LIMITER_NOTE}`}
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
				<span class="ceil-val">{ceilingLabel(ceiling)}</span>
			</div>
			<button
				type="button"
				class="limiter"
				class:on={!!master.true_peak}
				class:dim={!master.limiter}
				aria-pressed={!!master.true_peak}
				use:tip={master.true_peak
					? 'True peak on — the exported limiter runs 4× oversampled, so the peaks between samples stay under the ceiling too (what platforms measure as dBTP). The preview’s limiter is the same either way.'
					: 'True peak off — the exported limiter holds the sample peak; the peaks between samples can read up to about a decibel above the ceiling.'}
				onclick={toggleTruePeak}>True peak</button
			>

			<div class="duck">
				<div class="duck-head">
					<span class="cap">Duck</span>
					{#if gate}<span class="ceil-val">{duckLabel(depth)}</span>{/if}
				</div>
				<select
					class="mode k-select"
					aria-label="Duck mode"
					value={gate ? 'gate' : 'compressor'}
					use:tip={`How tracks flagged Duck dip under the rest of the mix. Compressor: a sidechain compressor dips them by how loud the rest is. Speech gate: they drop by exactly the depth set below while the rest of the mix speaks (easing down in about 50 ms, back up about 300 ms after it stops). ${DUCK_NOTE}`}
					onchange={(e) => void chooseMode(e.currentTarget)}
				>
					<option value="compressor">Compressor</option>
					<option value="gate">Speech gate</option>
				</select>
				{#if gate}
					<div class="depth">
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
							onpreview={(v) => (duckPreview = v)}
							oncommit={duckCommit}
							oncancel={() => (duckPreview = null)}
							onreset={duckReset}
						/>
					</div>
				{/if}
				{#if !anyDucked}
					<span class="hint">No track set to Duck</span>
				{/if}
			</div>
		</div>
	{/if}

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
		disabled={stopping}
		aria-busy={measuring}
		use:tip={measuring
			? 'Stop measuring. The pass reads the whole mix, so on a long cut it can run for minutes; stopping keeps the last result.'
			: 'Measure the loudness of the cut — integrated LUFS, true peak and loudness range, for the finished mix and each track — over the whole cut, or the in / out range when both marks are set. It reads the audio the export would render, so it takes a while on a long cut.'}
		onclick={measuring ? onstop : onmeasure}
	>
		{#if measuring}
			<span class="kerf-spin" style="display:inline-flex"><Icon n="loader" s={13} /></span>{stopping
				? 'Stopping…'
				: 'Stop'}
		{:else}
			<Icon n="audio-waveform" s={13} />Measure
		{/if}
	</button>

	{#if result}
		<div class="scope" class:stale>
			<span use:tip={stale ? 'The cut changed after this was measured — measure again' : undefined}
				>{stale ? 'Out of date · ' : ''}{scopeLabel(result.range, result.levels.duration, (s) => timeDisplay.fmt(s))}</span
			>
			{#if result.levels.estimated}
				<Badge tone="warning" style="height:16px;padding:0 5px;font-size:var(--text-2xs);margin-top:2px"
					>estimated</Badge
				>
			{/if}
		</div>
		{#if read}
			<dl class="read" class:stale>
				<div><dt>LUFS</dt><dd>{read.lufs}</dd></div>
				<div><dt>True peak</dt><dd>{read.truePeak}</dd></div>
				<div><dt>LRA</dt><dd>{read.range}</dd></div>
				{#if read.shortTerm}<div use:tip={'The loudest three seconds'}><dt>Short-term</dt><dd>{read.shortTerm}</dd></div>{/if}
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
	.fx-toggle {
		display: flex;
		align-items: center;
		gap: 5px;
		height: 24px;
		padding: 0 6px;
		cursor: pointer;
		font-family: var(--font-sans);
		font-size: var(--text-2xs);
		color: var(--text-muted);
		background: transparent;
		border: var(--line-width) solid var(--border-default);
		border-radius: var(--radius-xs);
	}
	.fx-toggle:hover {
		color: var(--text-primary);
	}
	.fx-toggle.active {
		color: var(--text-secondary);
	}
	.fx-label {
		flex: none;
		font-weight: 600;
		letter-spacing: var(--tracking-wide);
	}
	.fx-sum {
		flex: 1;
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		text-align: left;
		font-family: var(--font-mono);
	}
	.fx {
		display: flex;
		flex-direction: column;
		gap: 5px;
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
	.ceiling.dim,
	.limiter.dim {
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
	.duck {
		display: flex;
		flex-direction: column;
		gap: 3px;
	}
	.duck-head {
		display: flex;
		align-items: baseline;
		justify-content: space-between;
		gap: 4px;
	}
	.mode {
		height: 24px;
		padding-left: 6px;
		font-size: var(--text-xs);
		font-weight: 600;
		color: var(--text-secondary);
		border-radius: var(--radius-xs);
	}
	.depth {
		display: flex;
		margin: 0 -4px;
	}
	.hint {
		font-size: 10px;
		line-height: 1.3;
		color: var(--text-muted);
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
