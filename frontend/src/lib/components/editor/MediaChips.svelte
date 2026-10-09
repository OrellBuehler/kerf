<script lang="ts">
	// What has been done for an asset, at a glance: its preview proxy (building xx% / ready /
	// failed) and one chip per analysis kind (silence, scenes, loudness, rhythm, transcript —
	// done / not run / running / failed / off). The bin's rows and the inspector's header both
	// draw this, from the same statuses (`mediaStatus`), so they cannot disagree. The tooltip
	// of each carries the reason; state is never colour alone (a mark and a border style too).
	import Badge from './Badge.svelte';
	import { ui } from '$lib/editor-ui.svelte';
	import { mediaStatus } from '$lib/media-status.svelte';
	import { analysisChips } from '$lib/analysis-steps';
	import { proxyBadge } from '$lib/proxy-info';
	import type { AnalysisState } from '$lib/types';

	let {
		assetId,
		proxy = true,
		analysis = true
	}: { assetId: string; proxy?: boolean; analysis?: boolean } = $props();

	const chips = $derived(
		analysisChips(mediaStatus.analysis(assetId), ui.analyzingId === assetId ? ui.analysisKind : null)
	);
	const badge = $derived(proxyBadge(mediaStatus.proxy(assetId)));

	const MARK: Record<AnalysisState, string> = { done: '✓', not_run: '', running: '', failed: '!', off: '–' };

	// One look per state, from theme tokens only.
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
			title={badge.title}
			aria-label={badge.title}
			data-testid="proxy-badge"
			data-state={mediaStatus.proxy(assetId)?.state}
		>
			<Badge tone={badge.tone} dot={mediaStatus.proxy(assetId)?.state === 'building'}>{badge.text}</Badge>
		</span>
	{/if}
	{#if analysis}
		<span style="display:inline-flex;flex-wrap:wrap;gap:3px" role="list" aria-label="Analysis">
			{#each chips as chip (chip.kind)}
				<span
					role="listitem"
					title={chip.title}
					aria-label={chip.title}
					data-testid="analysis-chip"
					data-kind={chip.kind}
					data-state={chip.state}
					style="display:inline-flex;align-items:center;gap:2px;height:17px;padding:0 4px;border-radius:var(--radius-sm);font-family:var(--font-mono);font-size:9px;font-weight:600;letter-spacing:0.02em;line-height:1;{LOOK[
						chip.state
					]}"
				>
					{#if chip.state === 'running'}
						<span
							class="kerf-spin"
							style="width:7px;height:7px;border:1.5px solid currentColor;border-top-color:transparent;border-radius:50%"
						></span>
					{/if}
					{chip.short}{MARK[chip.state]}
				</span>
			{/each}
		</span>
	{/if}
</div>
