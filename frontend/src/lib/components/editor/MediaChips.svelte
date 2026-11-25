<script lang="ts">
	// What has been done for an asset, at a glance: its preview proxy (queued / building xx% / ready / failed) and one
	// chip per analysis kind (silence, scenes, loudness, rhythm, transcript — done / not run / running / failed / off).
	// The bin's rows and the inspector's header both draw this, from the same statuses (`mediaStatus`), so they cannot
	// disagree. The tooltip of each carries the reason and what it means for the person — whether to wait, what unlocks,
	// what to do (`proxy-info.ts` and `analysis-steps.ts` own those words, so every surface says them alike); state is
	// never colour alone (a mark and a border style too). Work in progress is amber, Kerf's own — cyan is the agent's. A state is a
	// round pill with an icon; an attribute of the file (360, voiceover, how many clips use it) is a square label with none. A
	// narrow bin row (`container: binrow`, under 240 px of content) draws the two states as one — `bin-status.ts` — so the row
	// keeps to one line of them.
	// A chip is a word (Silence, Scenes, Loudness, Rhythm, Transcript), not the two-to-four letter tag it used to be.
	import Icon from './Icon.svelte';
	import { ui } from '#lib/editor-ui.svelte';
	import { editor } from '#lib/state.svelte';
	import { mediaStatus } from '#lib/media-status.svelte';
	import { analysisChips, analysisMeter, chipsFor, mediaTraits } from '#lib/analysis-steps';
	import { proxyBadge, previewMode } from '#lib/proxy-info';
	import { analysisStatePill, mergedStatus, proxyPill, type PillLook, type StatePill } from '#lib/bin-status';
	import { settings } from '#lib/settings.svelte';
	import { OPEN_QUEUE_HINT } from '#lib/proxy-queue';
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
	const badge = $derived(proxyBadge(mediaStatus.proxy(assetId), previewMode(settings.previewSource, settings.proxySize)));
	const pills = $derived({
		proxy: proxy ? proxyPill(badge, mediaStatus.proxy(assetId)) : null,
		analysis: analysis ? analysisStatePill(meter) : null
	});
	const merged = $derived(mergedStatus([pills.proxy, pills.analysis]));

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

	// One look per state, from theme tokens only (the chips of the Inspector's header; the pills are the `.pill` rules below).
	const LOOK: Record<Look, string> = {
		done: 'color:var(--green-400);background:var(--success-surface);border:var(--line-width) solid transparent',
		running: 'color:var(--kerf-200);background:var(--selection-fill);border:var(--line-width) solid transparent',
		queued: 'color:var(--text-secondary);background:transparent;border:var(--line-width) dashed var(--border-strong)',
		failed: 'color:var(--red-400);background:var(--danger-surface);border:var(--line-width) solid transparent',
		not_run: 'color:var(--text-secondary);background:transparent;border:var(--line-width) solid var(--border-strong)',
		off: 'color:var(--text-muted);background:transparent;border:var(--line-width) dashed var(--border-strong)'
	};
</script>

{#snippet mark(look: PillLook)}
	{#if look === 'running'}<span class="kerf-spin ring"></span>
	{:else if look === 'done'}<Icon n="check" s={10} />
	{:else if look === 'failed'}<Icon n="circle-alert" s={10} />
	{:else if look === 'queued'}<Icon n="clock" s={10} />
	{:else}<span class="dot" data-look={look}></span>{/if}
{/snippet}

<!-- One state pill: round, with its mark. The proxy's, while it is in the queue, is a button into it. -->
{#snippet pill(p: StatePill, testid: string, state: string | undefined)}
	{#if p.queue}
		<button
			type="button"
			class="proxy-link"
			use:tip={{ text: p.title, detail: `${p.detail} ${OPEN_QUEUE_HINT}` }}
			aria-label="{p.text}: {p.title} {p.detail} {OPEN_QUEUE_HINT}"
			data-testid={testid}
			data-state={state}
			onpointerdown={(e) => e.stopPropagation()}
			onkeydown={(e) => (e.key === 'Enter' || e.key === ' ') && e.stopPropagation()}
			onclick={openQueue}
		>
			<span class="pill" data-look={p.look}>{@render mark(p.look)}{p.text}</span>
		</button>
	{:else}
		<span class="hold" use:tip={p.detail ? `${p.title}\n${p.detail}` : p.title} data-testid={testid} data-state={state}>
			<span class="pill" data-look={p.look}>{@render mark(p.look)}{p.text}</span>
		</span>
	{/if}
{/snippet}

<div class="chips" data-testid="media-chips">
	{#if compact}
		<span class="wide">
			{#if pills.proxy}{@render pill(pills.proxy, 'proxy-badge', mediaStatus.proxy(assetId)?.state)}{/if}
			{#if pills.analysis}{@render pill(pills.analysis, 'analysis-meter', pills.analysis.look)}{/if}
		</span>
		{#if merged}
			<!-- A narrow row: the neediest state, and a count of the other. -->
			<span class="narrow" data-testid="status-merged">
				{@render pill(merged.head, 'status-head', merged.head.look)}
				{#if merged.more > 0}
					<span use:tip={{ text: merged.others.map((o) => o.text).join(' · '), detail: merged.others.map((o) => o.title).join('\n') }} class="more" aria-label="and {merged.more} more: {merged.others.map((o) => o.text).join(', ')}">+{merged.more}</span>
				{/if}
			</span>
		{/if}
	{:else}
		{#if proxy && pills.proxy}{@render pill(pills.proxy, 'proxy-badge', mediaStatus.proxy(assetId)?.state)}{/if}
		{#if analysis}
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
	{/if}
</div>

<style>
	.chips {
		display: flex;
		flex-wrap: wrap;
		gap: 4px;
		align-items: center;
	}
	/* The wide form lays its pills in the row's own flow; the narrow one is hidden until the row is under 240 px of content
	   (the nearest `binrow` container: `MediaBin`'s row). A chip list outside a bin row has no container and keeps the wide form. */
	.wide,
	.narrow {
		display: contents;
	}
	.narrow {
		display: none;
	}
	@container binrow (max-width: 239px) {
		.wide {
			display: none;
		}
		.narrow {
			display: contents;
		}
	}
	/* The proxy pill of a job in the queue: a button with nothing of a button's own, so it reads as the pill it is. */
	.hold {
		display: inline-flex;
	}
	.proxy-link {
		display: inline-flex;
		padding: 0;
		border: 0;
		background: none;
		font: inherit;
		border-radius: 999px;
		cursor: pointer;
	}
	/* A state: round, with its mark. An attribute (`Badge`) is square and has none. */
	.pill {
		display: inline-flex;
		align-items: center;
		gap: 4px;
		height: 19px;
		padding: 0 8px 0 6px;
		border-radius: 999px;
		box-sizing: border-box;
		font-size: var(--text-2xs);
		font-weight: 600;
		letter-spacing: 0.02em;
		line-height: 1;
		white-space: nowrap;
		border: var(--line-width) solid transparent;
	}
	.pill[data-look='done'] {
		color: var(--green-400);
		background: var(--success-surface);
	}
	.pill[data-look='running'] {
		color: var(--kerf-200);
		background: var(--selection-fill);
	}
	.pill[data-look='failed'] {
		color: var(--red-400);
		background: var(--danger-surface);
	}
	.pill[data-look='queued'] {
		color: var(--text-secondary);
		border-style: dashed;
		border-color: var(--border-strong);
	}
	.pill[data-look='idle'] {
		color: var(--text-secondary);
		border-color: var(--border-strong);
	}
	.pill[data-look='off'] {
		color: var(--text-muted);
		border-style: dashed;
		border-color: var(--border-strong);
	}
	.ring {
		width: 7px;
		height: 7px;
		border: 1.5px solid currentColor;
		border-top-color: transparent;
		border-radius: 50%;
	}
	.dot {
		width: 6px;
		height: 6px;
		border-radius: 50%;
		border: var(--line-width) solid currentColor;
		box-sizing: border-box;
	}
	.dot[data-look='off'] {
		border-style: dashed;
	}
	/* "and one more": the other state, counted. */
	.more {
		display: inline-flex;
		align-items: center;
		height: 19px;
		padding: 0 6px;
		border-radius: 999px;
		font-size: var(--text-2xs);
		font-weight: 600;
		line-height: 1;
		color: var(--text-secondary);
		border: var(--line-width) solid var(--border-strong);
	}
</style>
