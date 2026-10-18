<script lang="ts">
	// What a composed piece is — the score it was rendered from, drawn as the shape of its sections — with
	// the way back into it (Edit & re-compose opens the Compose dialog on this score and, for a clip, swaps
	// the clip to the new render) and a band-balance readout: where the sound keeps its energy, window by
	// window, in six bands. The measurement decodes the sound, so it is asked for, can be stopped, and is
	// dropped when the clip it measured changes.
	import { untrack } from 'svelte';
	import Btn from './Btn.svelte';
	import Icon from './Icon.svelte';
	import { editor } from '#lib/state.svelte';
	import { ui } from '#lib/editor-ui.svelte';
	import { BAND_ORDER, BAND_SHORT, balanceView } from '#lib/band-view';
	import { BAND_BALANCE_CANCELLED, cancelBandBalance, getBandBalance } from '#lib/api';
	import { composeOffer, fmtLength, hitsLine, sectionContour, styleLabel } from '#lib/compose-form';
	import { scoreDuration, totalBars } from '#lib/music-score';
	import { timeDisplay } from '#lib/time-display.svelte';
	import { failure } from '#lib/failure-copy';
	import type { Asset, BandBalance, Clip, Track } from '#lib/types';
	import { tip } from '#lib/tip';

	let {
		asset,
		clip = undefined,
		track = undefined
	}: {
		asset: Asset;
		/** The clip being inspected; without one the whole asset is what is measured and edited again. */
		clip?: Clip;
		track?: Track;
	} = $props();

	const score = $derived(asset.music_score);
	const contour = $derived(score ? sectionContour(score) : []);
	const offer = $derived(clip ? composeOffer(track, asset, clip) : { show: true, reason: null });

	let balance = $state<BandBalance | null>(null);
	let measuring = $state(false);
	let stopping = $state(false);
	let problem = $state<string | null>(null);
	let ask = 0;
	let live = false;

	/** Forget the measurement, and stop the one still running: nobody is waiting for its answer. */
	function reset() {
		ask++;
		if (live) void cancelBandBalance();
		live = false;
		measuring = false;
		stopping = false;
		balance = null;
		problem = null;
	}

	// What was measured is the clip as it was: another clip, a new render, a trim or a level change makes
	// the answer someone else's, so it goes.
	const measured = $derived(`${asset.id}|${clip?.id ?? ''}|${clip?.source_in}|${clip?.source_out}|${clip?.volume}`);
	$effect(() => {
		void measured;
		untrack(reset);
	});
	$effect(() => () => {
		ask++;
		if (live) void cancelBandBalance();
	});

	const view = $derived(balance ? balanceView(balance, (s) => timeDisplay.fmt(s)) : null);

	async function measure() {
		if (measuring) return;
		const mine = ++ask;
		measuring = true;
		live = true;
		stopping = false;
		problem = null;
		try {
			const result = await getBandBalance(clip ? { clip_id: clip.id } : { asset_id: asset.id });
			if (mine === ask) balance = result;
		} catch (e) {
			const text = e instanceof Error ? e.message : String(e);
			// A stop is the user's own doing; an answer nobody waits for any more is not shown.
			if (mine === ask && text !== BAND_BALANCE_CANCELLED) problem = failure("Couldn't measure the balance", e);
		} finally {
			if (mine === ask) {
				measuring = false;
				live = false;
				stopping = false;
			}
		}
	}

	async function stop() {
		stopping = true;
		await cancelBandBalance();
	}

	const facts = $derived<[string, string][]>(
		score
			? [
					['Style', styleLabel(score.style)],
					['Key', score.key],
					['Tempo', `${Math.round(score.bpm * 10) / 10} BPM`],
					['Sections', String(score.sections.length)],
					['Length', `${fmtLength(scoreDuration(score))} · ${totalBars(score)} bars`],
					['Variation', `#${score.seed}`],
					['Hits', hitsLine(score.hits ?? [])]
				]
			: []
	);
	const energyPct = (e: number) => Math.round(e * 100);
</script>

{#if score}
	<div data-testid="composed-music">
		<div style="display:grid;grid-template-columns:72px minmax(0,1fr);row-gap:3px;column-gap:8px;align-items:baseline">
			{#each facts as [name, value] (name)}
				<span style="font-size:12px;color:var(--text-muted);white-space:nowrap">{name}</span>
				<span data-selectable style="font-family:var(--font-mono);font-size:12px;color:var(--text-secondary);text-align:right;min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap"
					>{value}</span
				>
			{/each}
		</div>

		<div
			role="img"
			aria-label="Sections: {contour.map((b) => `${b.label}, ${b.bars} bars`).join('; ')}"
			style="display:flex;gap:2px;align-items:flex-end;height:44px;margin-top:10px;padding:3px;border-radius:var(--radius-sm);background:var(--surface-inset);border:var(--line-width) solid var(--border-subtle)"
		>
			{#each contour as b, i (i)}
				<div
					data-testid="section-block"
					use:tip={`${b.label} · ${b.bars} ${b.bars === 1 ? 'bar' : 'bars'}\n${timeDisplay.fmt(b.start_s)}–${timeDisplay.fmt(b.end_s)} · energy ${energyPct(b.energy)} %`}
					style="flex:{b.width} 1 0;min-width:2px;height:{18 + b.energy * 82}%;border-radius:2px;background:color-mix(in srgb,var(--kerf-500) {25 + Math.round(b.energy * 55)}%,transparent);border-top:2px solid var(--kerf-400)"
				></div>
			{/each}
		</div>
		<div style="display:flex;gap:2px;padding:2px 3px 0">
			{#each contour as b, i (i)}
				<span
					style="flex:{b.width} 1 0;min-width:2px;overflow:hidden;text-overflow:clip;white-space:nowrap;font-size:var(--text-2xs);color:var(--text-muted)"
					>{b.label}</span
				>
			{/each}
		</div>

		<div style="margin-top:10px">
			<Btn
				size="sm"
				icon="music"
				pending={editor.locked}
				disabled={offer.reason !== null}
				title={offer.reason ??
					(clip
						? 'Change the score and render it again; this clip is swapped to the new piece'
						: 'Open the score in the Compose dialog and add the result as a new piece')}
				onclick={() => ui.openCompose({ score, ...(clip ? { replaceClipId: clip.id } : {}) })}
				>{clip ? 'Edit & re-compose…' : 'Edit as a new piece…'}</Btn
			>
		</div>

		<div style="margin-top:12px;display:flex;align-items:center;gap:8px">
			<span class="k-overline" style="flex:1">Band balance</span>
			{#if measuring}
				<span class="kerf-spin" style="color:var(--kerf-400);display:inline-flex"><Icon n="loader" s={14} /></span>
				<Btn size="sm" variant="ghost" disabled={stopping} onclick={stop}>{stopping ? 'Stopping…' : 'Stop'}</Btn>
			{:else}
				<Btn
					size="sm"
					variant="secondary"
					icon="audio-waveform"
					title="Decode the sound and see where it keeps its energy, section by section, in six bands"
					onclick={measure}>{balance ? 'Measure again' : 'Measure'}</Btn
				>
			{/if}
		</div>

		{#if problem}
			<div role="alert" style="margin-top:6px;font-size:12px;line-height:1.4;color:var(--red-400)">{problem}</div>
		{/if}

		{#if view}
			<div data-testid="band-balance" aria-live="polite" style="margin-top:8px">
				<div style="font-size:12px;color:var(--text-primary)">{view.headline}</div>
				<div style="font-size:11px;color:var(--text-muted);margin-bottom:6px">by {view.basis}</div>
				<div role="table" aria-label="Share of the energy in each band, by window">
					<div
						role="row"
						style="display:grid;grid-template-columns:minmax(40px,0.9fr) repeat(6,minmax(0,1fr)) minmax(48px,0.9fr);column-gap:3px;align-items:end;font-size:var(--text-2xs);color:var(--text-muted);text-align:center"
					>
						<span role="columnheader" style="text-align:left">Window</span>
						{#each BAND_ORDER as band (band)}
							<span role="columnheader">{BAND_SHORT[band]}</span>
						{/each}
						<span role="columnheader" style="text-align:right">LUFS</span>
					</div>
					{#each [view.overall, ...view.rows] as w, i (i)}
						<div
							role="row"
							data-testid={i === 0 ? 'band-overall' : 'band-window'}
							use:tip={w.tip}
							style="display:grid;grid-template-columns:minmax(40px,0.9fr) repeat(6,minmax(0,1fr)) minmax(48px,0.9fr);column-gap:3px;align-items:end;padding:4px 0;{i === 0
								? 'border-bottom:var(--line-width) solid var(--border-subtle);margin-bottom:2px'
								: ''}"
						>
							<span
								role="rowheader"
								style="font-size:11px;color:{i === 0 ? 'var(--text-primary)' : 'var(--text-secondary)'};white-space:nowrap;overflow:hidden;text-overflow:ellipsis;align-self:center"
								>{w.label}</span
							>
							{#each w.bars as b (b.name)}
								<span role="cell" style="display:flex;flex-direction:column;align-items:stretch;justify-content:flex-end;height:34px">
									<span
										data-band={b.name}
										data-dominant={b.dominant ? '' : undefined}
										use:tip={b.tip}
										style="display:block;height:{Math.max(b.height * 100, b.share > 0 ? 4 : 0)}%;min-height:{b.share > 0 ? 2 : 0}px;border-radius:2px 2px 0 0;background:{b.dominant
											? 'var(--kerf-400)'
											: 'color-mix(in srgb,var(--text-muted) 55%,transparent)'}"
									></span>
								</span>
							{/each}
							<span
								role="cell"
								style="font-family:var(--font-mono);font-size:11px;color:var(--text-secondary);text-align:right;align-self:center;white-space:nowrap"
								>{w.loudness.replace(' LUFS', '')}</span
							>
						</div>
					{/each}
				</div>
				{#if balance && balance.notes.length > 0}
					<ul style="margin:8px 0 0;padding:0;list-style:none;display:flex;flex-direction:column;gap:4px">
						{#each balance.notes as n (n)}
							<li style="font-size:11px;line-height:1.45;color:var(--text-secondary)">{n}</li>
						{/each}
					</ul>
				{/if}
			</div>
		{:else if !measuring && !problem}
			<div style="margin-top:4px;font-size:11px;line-height:1.45;color:var(--text-muted)">
				Shows where each section keeps its energy — sub, bass, low mid, mid, high mid, air — and how loud it is.
			</div>
		{/if}
	</div>
{/if}
