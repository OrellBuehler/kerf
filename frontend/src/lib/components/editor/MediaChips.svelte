<script lang="ts">
	// What has been done for an asset, at a glance: its preview proxy (queued / building xx% / ready / failed) and one
	// chip per analysis kind (silence, scenes, loudness, rhythm, transcript — done / not run / running / failed / off).
	// The bin's rows and the inspector's header both draw this, from the same statuses (`mediaStatus`), so they cannot
	// disagree. The tooltip of each carries the reason and what it means for the person — whether to wait, what unlocks,
	// what to do (`proxy-info.ts` and `analysis-steps.ts` own those words, so every surface says them alike); state is
	// never colour alone (a mark and a border style too). Work in progress is amber, Kerf's own — cyan is the agent's.
	// A chip is a word (Silence, Scenes, Loudness, Rhythm, Transcript), not the two-to-four letter tag it used to be.
	import Badge from './Badge.svelte';
	import Icon from './Icon.svelte';
	import { ui } from '#lib/editor-ui.svelte';
	import { editor } from '#lib/state.svelte';
	import { mediaStatus } from '#lib/media-status.svelte';
	import { analysisChips, analysisMeter, chipsFor, mediaTraits } from '#lib/analysis-steps';
	import { proxyBadge } from '#lib/proxy-info';
	import { OPEN_QUEUE_HINT, isQueued } from '#lib/proxy-queue';
	import { workspace } from '#lib/workspace.svelte';
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
	const traits = $derived(mediaTraits(editor.assets.find((a) => a.id === assetId)?.streams));
	/** A run is on this asset (it may not have named its step yet). */
	const analyzing = $derived(ui.analyzingId === assetId);
	/** It waits its turn behind the asset being analyzed. */
	const queued = $derived(ui.analysisWaiting.includes(assetId));
	const chips = $derived(chipsFor(analysisChips(mediaStatus.analysis(assetId), analyzing ? ui.analysisKind : null), traits));
	const meter = $derived(analysisMeter(chips, { queued, analyzing }));
	const badge = $derived(proxyBadge(mediaStatus.proxy(assetId)));
	/** A proxy that is building or waiting is in the queue, and its badge opens it. */
	const inQueue = $derived(isQueued(mediaStatus.proxy(assetId)?.state));

	/** The bin row under the badge selects on a click, starts a drag on a press and takes Enter and Space for a select (and
	 *  keeps the key from the button): the badge is its own control, so none of those reach the row. */
	function openQueue(e: MouseEvent) {
		e.stopPropagation();
		workspace.show('proxyQueue');
		// A pointer click does not leave focus on the badge: a focused button swallows Space, the transport's key.
		if (e.detail > 0) (e.currentTarget as HTMLElement | null)?.blur();
	}

	/** What a chip is drawn in; `queued` is the page's own state (waiting behind another file), not the backend's. */
	type Look = AnalysisState | 'queued';

	/** A chip's mark that is not an icon: a kind that is off is a dash. */
	const OFF_MARK = '–';

	// One look per state, from theme tokens only. The pill of `compact` takes the look of its tone.
	const TONE: Record<'done' | 'running' | 'queued' | 'failed' | 'todo', Look> = {
		done: 'done',
		running: 'running',
		queued: 'queued',
		failed: 'failed',
		todo: 'not_run'
	};

	const LOOK: Record<Look, string> = {
		done: 'color:var(--green-400);background:var(--success-surface);border:var(--line-width) solid transparent',
		running: 'color:var(--kerf-200);background:var(--selection-fill);border:var(--line-width) solid transparent',
		queued: 'color:var(--text-secondary);background:transparent;border:var(--line-width) dashed var(--border-strong)',
		failed: 'color:var(--red-400);background:var(--danger-surface);border:var(--line-width) solid transparent',
		not_run: 'color:var(--text-secondary);background:transparent;border:var(--line-width) solid var(--border-strong)',
		off: 'color:var(--text-muted);background:transparent;border:var(--line-width) dashed var(--border-strong)'
	};
</script>

<div style="display:flex;flex-wrap:wrap;gap:4px;align-items:center" data-testid="media-chips">
	{#if proxy && badge}
		{#if inQueue}
			<button
				type="button"
				class="proxy-link"
				use:tip={{ text: badge.title, detail: `${badge.detail} ${OPEN_QUEUE_HINT}` }}
				aria-label="{badge.text}: {badge.title} {badge.detail} {OPEN_QUEUE_HINT}"
				data-testid="proxy-badge"
				data-state={mediaStatus.proxy(assetId)?.state}
				onpointerdown={(e) => e.stopPropagation()}
				onkeydown={(e) => (e.key === 'Enter' || e.key === ' ') && e.stopPropagation()}
				onclick={openQueue}
			>
				<Badge tone={badge.tone} dot={mediaStatus.proxy(assetId)?.state === 'building'}>{badge.text}</Badge>
			</button>
		{:else}
			<span
				use:tip={{ text: badge.title, detail: badge.detail }}
				aria-label="{badge.text}: {badge.title} {badge.detail}"
				data-testid="proxy-badge"
				data-state={mediaStatus.proxy(assetId)?.state}
			>
				<Badge tone={badge.tone}>{badge.text}</Badge>
			</span>
		{/if}
	{/if}
	{#if analysis && compact}
		{#if meter}
			<span
				use:tip={meter.detail ? `${meter.title}\n${meter.detail}` : meter.title}
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
				{:else if meter.tone === 'done'}<Icon n="check" s={10} />{:else if meter.tone === 'failed'}<Icon n="circle-alert" s={10} />{:else if meter.tone === 'queued'}<Icon n="clock" s={10} />{/if}
				{meter.text}
			</span>
		{/if}
	{:else if analysis}
		<span style="display:inline-flex;flex-wrap:wrap;gap:3px" role="list" aria-label="Analysis">
			{#each chips as chip (chip.kind)}
				<span
					role="listitem"
					use:tip={{ text: chip.title, detail: chip.state === 'done' ? undefined : chip.gives }}
					aria-label="{chip.title}. {chip.gives}"
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
					{chip.label}{#if chip.state === 'done'}<Icon n="check" s={10} />{:else if chip.state === 'failed'}<Icon n="circle-alert" s={10} />{:else if chip.state === 'off'}<span aria-hidden="true">{OFF_MARK}</span>{/if}
				</span>
			{/each}
		</span>
	{/if}
</div>

<style>
	/* The proxy badge of a job in the queue: a button with nothing of a button's own, so it reads as the badge it is. */
	.proxy-link {
		display: inline-flex;
		padding: 0;
		border: 0;
		background: none;
		font: inherit;
		border-radius: var(--radius-sm);
		cursor: pointer;
	}
</style>
