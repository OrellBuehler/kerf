<script lang="ts">
	// The master strip: the fader on the finished mix, its meter, the Measure button that reads how loud the cut
	// actually is, and — once it has — that reading against what a delivery is held to: two lines (the loudness and the true peak,
	// each with a dot that says whether it is on its line) that open the delivery popover (`MasterDelivery.svelte`) with how far each
	// is from its line and what to do when it is over. The fader is an edit like any other (one per gesture, in the history);
	// while it is held the preview follows it (`audio.liveMaster`).
	//
	// The limiter, its ceiling and the duck are the FX button's popover (`MasterFx.svelte`), so the fader keeps the strip's
	// height: stacked above it they left it under half a track's at 1366x768, and the result's lines under it did the same. The button
	// says what is on, so a closed popover does not hide a limiter that is switched on (`fxSummary`).
	//
	// The preview's limiter is the export's own (`alimiter`, ported sample for sample and run in an AudioWorklet by
	// `audio.ts`) on the preview's mix — or, where the worklet cannot run, an approximation; the tooltips say which.
	import FaderMeter from './FaderMeter.svelte';
	import Icon from './Icon.svelte';
	import { editor } from '#lib/state.svelte';
	import { audio } from '#lib/audio';
	import { report } from '#lib/ops';
	import { MASTER_MAX_VOLUME } from '#lib/levels';
	import type { StereoMeter } from '#lib/meter';
	import { readingText, type DeliveryRow } from '#lib/levels-view';
	import type { MasterBus } from '#lib/types';
	import type { MeasureResult } from '#lib/editor-ui.svelte';
	import { fxSummary } from '#lib/master-fx';
	import { tip } from '#lib/tip';

	let {
		master,
		meter,
		measuring,
		stopping,
		result,
		stale,
		rows,
		fxOpen,
		fxButton = $bindable(null),
		onfx,
		deliveryOpen,
		deliveryButton = $bindable(null),
		ondelivery,
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
		/** The cut against its delivery's lines (`deliveryCheck`), or `null` when there is nothing to judge. */
		rows: DeliveryRow[] | null;
		/** Whether the FX popover is open, and the button that opens it (the popover returns focus there). */
		fxOpen: boolean;
		fxButton?: HTMLButtonElement | null;
		onfx: () => void;
		/** The same for the delivery popover (the measured result against its lines). */
		deliveryOpen: boolean;
		deliveryButton?: HTMLButtonElement | null;
		ondelivery: () => void;
		onmeasure: () => void;
		onstop: () => void;
		onclear: () => void;
	} = $props();

	const fx = $derived(fxSummary(master));
	let faderPreview = $state<number | null>(null);

	const volume = $derived(faderPreview ?? master.volume);
	const read = $derived(result ? readingText(result.levels.master) : null);
	const err = report;

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
</script>

<section class="strip" aria-label="Master channel strip">
	<header><span class="name">Master</span></header>

	<button
		bind:this={fxButton}
		type="button"
		class="fx-toggle"
		class:active={fx.active}
		class:open={fxOpen}
		aria-expanded={fxOpen}
		aria-haspopup="true"
		use:tip={fxOpen
			? 'Close the master FX'
			: 'The limiter, its ceiling and true peak, and how the tracks flagged Duck dip — they open beside the strip, so the fader keeps its height'}
		onclick={onfx}
	>
		<span class="fx-label">FX</span>
		<span class="fx-sum">{fx.text}</span>
		<Icon n={fxOpen ? 'chevron-left' : 'chevron-right'} s={12} />
	</button>

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
		{#if rows}
			<!-- The measured mix against the lines a delivery is held to, in two lines: a dot for whether each is on its line. -->
			<button
				bind:this={deliveryButton}
				type="button"
				class="delivery"
				class:stale
				class:open={deliveryOpen}
				aria-expanded={deliveryOpen}
				aria-haspopup="true"
				aria-label="{stale ? 'Out of date: the cut changed after this was measured. ' : ''}The cut against its delivery: {rows.map((r) => `${r.label} ${r.value}, ${r.delta}`).join('; ')}"
				use:tip={`${stale ? 'Out of date — measured before the last edit. ' : ''}${rows.some((r) => r.tone === 'warn') ? 'Off its line — click for how far, and what to do' : 'On its lines — click for the check'}`}
				onclick={ondelivery}
			>
				{#each rows as row (row.id)}
					<span class="metric {row.tone}"><span class="dot" aria-hidden="true"></span>{row.value}</span>
				{/each}
				{#if result.levels.estimated}<span class="est">estimated</span>{/if}
			</button>
		{:else if read}
			<dl class="read" class:stale>
				<div><dt>LUFS</dt><dd>{read.lufs}</dd></div>
				<div><dt>True peak</dt><dd>{read.truePeak}</dd></div>
			</dl>
		{/if}
	{/if}
</section>

<style>
	.strip {
		flex: none;
		width: var(--master-w, 132px);
		display: flex;
		flex-direction: column;
		gap: 5px;
		padding: 7px 8px 8px;
		box-sizing: border-box;
		min-height: 268px;
		background: var(--surface-raised);
		border: var(--line-width) solid var(--border-strong);
		border-radius: var(--radius-md);
		/* A panel docked narrower than its strips scrolls them under this one: a shade on its edge says there is more behind it. */
		box-shadow: -8px 0 8px -8px color-mix(in srgb, var(--shadow-color) 70%, transparent);
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
	.fx-toggle.open {
		color: var(--kerf-300);
		border-color: var(--kerf-400);
		background: var(--selection-fill);
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
	.measure {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		gap: 6px;
		height: 24px;
		padding: 0 8px;
		cursor: pointer;
		font-family: var(--font-sans);
		font-size: var(--text-xs);
		font-weight: 600;
		color: var(--text-secondary);
		background: var(--surface-hover);
		border: var(--line-width) solid var(--border-strong);
		border-radius: var(--radius-xs);
	}
	.measure:hover:not(:disabled) {
		color: var(--text-primary);
	}
	.measure:disabled {
		cursor: progress;
	}
	/* The result: two lines on the strip, the rest in the popover it opens. */
	.delivery {
		display: flex;
		flex-direction: column;
		gap: 2px;
		padding: 4px 6px;
		cursor: pointer;
		font-family: var(--font-mono);
		font-size: var(--text-2xs);
		text-align: left;
		color: var(--text-primary);
		background: transparent;
		border: var(--line-width) solid var(--border-default);
		border-radius: var(--radius-xs);
	}
	.delivery:hover {
		border-color: var(--border-strong);
	}
	.delivery.open {
		border-color: var(--kerf-400);
		background: var(--selection-fill);
	}
	.delivery.stale {
		opacity: 0.55;
	}
	.metric {
		display: flex;
		align-items: center;
		gap: 5px;
		white-space: nowrap;
	}
	.dot {
		flex: none;
		width: 6px;
		height: 6px;
		border-radius: 50%;
		background: var(--success);
	}
	.metric.warn .dot {
		background: var(--warning);
	}
	.est {
		font-family: var(--font-sans);
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
		font-size: var(--text-2xs);
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
	dt {
		color: var(--text-muted);
	}
	dd {
		margin: 0;
	}
</style>
