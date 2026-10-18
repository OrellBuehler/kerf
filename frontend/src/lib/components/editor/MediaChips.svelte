<script lang="ts">
	// What has been done for an asset, at a glance: its preview proxy (building xx% / ready /
	// failed) and one chip per analysis kind (silence, scenes, loudness, rhythm, transcript —
	// done / not run / running / failed / off). The bin's rows and the inspector's header both
	// draw this, from the same statuses (`mediaStatus`), so they cannot disagree. The tooltip
	// of each carries the reason; state is never colour alone (a mark and a border style too). A chip is a word
	// (Silence, Scenes, Loudness, Rhythm, Transcript), not the two-to-four letter tag it used to be (SIL, SCN, LUFS,
	// BPM, TXT): nobody should have to hover to learn what a green box says.
	import Badge from './Badge.svelte';
	import { ui } from '#lib/editor-ui.svelte';
	import { editor } from '#lib/state.svelte';
	import { mediaStatus } from '#lib/media-status.svelte';
	import { analysisChips, analysisMeter, chipsFor, type MediaTraits } from '#lib/analysis-steps';
	import { proxyBadge } from '#lib/proxy-info';
	import type { AnalysisState } from '#lib/types';
	import { tip } from '#lib/tip';

	let {
		assetId,
		proxy = true,
		analysis = true,
		compact = false
	}: {
		assetId: string;
		proxy?: boolean;
		analysis?: boolean;
		/** One pill (`3 of 5 analyzed`, the kinds in its tooltip) instead of a tag per kind: a bin row has no room for five. */
		compact?: boolean;
	} = $props();

	/** What the file can give, so a kind it never could (scenes of a song) is not drawn as waiting. */
	const traits = $derived<MediaTraits>(
		(() => {
			const streams = editor.assets.find((a) => a.id === assetId)?.streams ?? [];
			return {
				audio: streams.some((s) => s.kind === 'audio'),
				image: streams.some((s) => s.image),
				video: streams.some((s) => s.kind === 'video')
			};
		})()
	);
	const chips = $derived(
		chipsFor(analysisChips(mediaStatus.analysis(assetId), ui.analyzingId === assetId ? ui.analysisKind : null), traits)
	);
	const meter = $derived(analysisMeter(chips));
	const badge = $derived(proxyBadge(mediaStatus.proxy(assetId)));

	const MARK: Record<AnalysisState, string> = { done: '✓', not_run: '', running: '', failed: '!', off: '–' };

	// One look per state, from theme tokens only. The pill of `compact` takes the look of its tone.
	const TONE: Record<'done' | 'running' | 'failed' | 'todo', AnalysisState> = {
		done: 'done',
		running: 'running',
		failed: 'failed',
		todo: 'not_run'
	};

	const LOOK: Record<AnalysisState, string> = {
		done: 'color:var(--green-400);background:var(--success-surface);border:var(--line-width) solid transparent',
		running: 'color:var(--agent-300);background:var(--agent-surface);border:var(--line-width) solid transparent',
		failed: 'color:var(--red-400);background:var(--danger-surface);border:var(--line-width) solid transparent',
		not_run: 'color:var(--text-secondary);background:transparent;border:var(--line-width) solid var(--border-strong)',
		off: 'color:var(--text-muted);background:transparent;border:var(--line-width) dashed var(--border-strong)'
	};
</script>

<div style="display:flex;flex-wrap:wrap;gap:4px;align-items:center" data-testid="media-chips">
	{#if proxy && badge}
		<span
			use:tip={badge.title}
			aria-label={badge.title}
			data-testid="proxy-badge"
			data-state={mediaStatus.proxy(assetId)?.state}
		>
			<Badge tone={badge.tone} dot={mediaStatus.proxy(assetId)?.state === 'building'}>{badge.text}</Badge>
		</span>
	{/if}
	{#if analysis && compact}
		{#if meter}
			<span
				use:tip={meter.title}
				data-testid="analysis-meter"
				data-state={TONE[meter.tone]}
				style="display:inline-flex;align-items:center;gap:4px;height:19px;padding:0 7px;border-radius:var(--radius-sm);font-size:var(--text-2xs);font-weight:600;letter-spacing:0.02em;line-height:1;white-space:nowrap;{LOOK[
					TONE[meter.tone]
				]}"
			>
				{#if meter.tone === 'running'}
					<span
						class="kerf-spin"
						style="width:7px;height:7px;border:1.5px solid currentColor;border-top-color:transparent;border-radius:50%"
					></span>
				{:else if meter.tone === 'done'}✓{:else if meter.tone === 'failed'}!{/if}
				{meter.text}
			</span>
		{/if}
	{:else if analysis}
		<span style="display:inline-flex;flex-wrap:wrap;gap:3px" role="list" aria-label="Analysis">
			{#each chips as chip (chip.kind)}
				<span
					role="listitem"
					use:tip={chip.title}
					aria-label={chip.title}
					data-testid="analysis-chip"
					data-kind={chip.kind}
					data-state={chip.state}
					style="display:inline-flex;align-items:center;gap:3px;height:19px;padding:0 6px;border-radius:var(--radius-sm);font-size:var(--text-2xs);font-weight:600;line-height:1;white-space:nowrap;{LOOK[
						chip.state
					]}"
				>
					{#if chip.state === 'running'}
						<span
							class="kerf-spin"
							style="width:7px;height:7px;border:1.5px solid currentColor;border-top-color:transparent;border-radius:50%"
						></span>
					{/if}
					{chip.label}{#if MARK[chip.state]}<span aria-hidden="true">{MARK[chip.state]}</span>{/if}
				</span>
			{/each}
		</span>
	{/if}
</div>
