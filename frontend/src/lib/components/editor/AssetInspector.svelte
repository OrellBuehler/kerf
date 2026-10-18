<script lang="ts">
	// The Inspector's view of a library asset when one is selected and no clip is: its name and kind, what
	// has been done to it (the proxy and analysis chips, the same ones the bin draws) and the facts the bin's
	// right-click menu used to list as twenty rows. Read-only; the actions are the buttons here and the menu.
	import Icon from './Icon.svelte';
	import Badge from './Badge.svelte';
	import Btn from './Btn.svelte';
	import MediaChips from './MediaChips.svelte';
	import AnalysisRun from './AnalysisRun.svelte';
	import InspectorSection from './InspectorSection.svelte';
	import ComposedMusic from './ComposedMusic.svelte';
	import { scoreTitle } from '#lib/music-score';
	import MusicStructure from './MusicStructure.svelte';
	import { musicSummary } from '#lib/music-ui';
	import { VIDEO_THUMB_BG } from './data';
	import { editor } from '#lib/state.svelte';
	import { ui } from '#lib/editor-ui.svelte';
	import { mediaStatus } from '#lib/media-status.svelte';
	import { contextMenu } from '#lib/context-menu.svelte';
	import { analyzeMenuItems } from '#lib/analyze-menu';
	import { assetFacts, type Fact } from '#lib/asset-facts';
	import { doneCount } from '#lib/analysis-steps';
	import { mediaInfo, specLine } from '#lib/media-info';
	import { proxyActions } from '#lib/proxy-info';
	import { deleteProxy, rebuildProxy } from '#lib/api';
	import { toast } from '#lib/notifications.svelte';
	import { exportTranscriptSrt, report } from '#lib/ops';
	import { relinkMediaFlow, removeMediaFlow } from '#lib/library-actions';
	import { tip } from '#lib/tip';
	import { settings } from '#lib/settings.svelte';
	import { placementOut } from '#lib/editing-prefs';
	import type { Asset } from '#lib/types';

	let { asset }: { asset: Asset } = $props();

	const info = $derived(mediaInfo(asset, editor.timeline));
	const facts = $derived(
		assetFacts({
			asset,
			info,
			analysis: editor.analysisFor(asset.id),
			status: mediaStatus.analysis(asset.id),
			analyzing: ui.analyzingId === asset.id,
			proxy: mediaStatus.proxy(asset.id)
		})
	);
	const spec = $derived(specLine(info));
	/** Music with a fitted bar grid: its structure is shown under Analysis. */
	const music = $derived(editor.analysisFor(asset.id)?.music ?? null);
	const proxy = $derived(mediaStatus.proxy(asset.id));
	const act = $derived(proxyActions(proxy));
	const KIND: Record<string, string> = { video: 'Video', audio: 'Audio', image: 'Still' };

	const err = report;
</script>

{#snippet row(f: Fact)}
	<div style="display:flex;align-items:baseline;justify-content:space-between;gap:12px;padding:3px 0">
		<span style="flex:none;font-size:12px;color:var(--text-muted);white-space:nowrap">{f.label}</span>
		<span
			data-selectable
			use:tip={f.title}
			style="min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;font-family:var(--font-mono);font-size:12px;color:var(--text-secondary)"
			>{f.value}</span
		>
	</div>
{/snippet}

<div data-testid="asset-inspector">
	<div style="display:flex;gap:9px;align-items:center">
		<div
			style="width:40px;height:28px;border-radius:var(--radius-sm);flex:none;background:{info.kind === 'audio'
				? 'var(--track-audio)'
				: VIDEO_THUMB_BG};display:grid;place-items:center;color:color-mix(in srgb,var(--text-on-video) 80%,transparent)"
		>
			<Icon n={info.kind === 'image' ? 'image' : info.kind === 'audio' ? 'audio-waveform' : 'video'} s={14} />
		</div>
		<div style="flex:1;min-width:0">
			<div
				style="font-size:13px;font-weight:500;color:var(--text-primary);white-space:nowrap;overflow:hidden;text-overflow:ellipsis"
				use:tip={asset.path}
			>
				{asset.name}
			</div>
			<div style="margin-top:3px;display:flex;align-items:center;gap:5px;flex-wrap:wrap">
				<Badge tone="neutral">{KIND[info.kind]} · library</Badge>
				{#if info.projection}<Badge tone="kerf">360</Badge>{/if}
				{#if info.voiceover}<Badge tone="kerf">voiceover</Badge>{/if}
				{#if info.composed}<Badge tone="kerf">composed</Badge>{/if}
			</div>
		</div>
	</div>
	{#if mediaStatus.isMissing(asset.id)}
		<div
			role="alert"
			data-testid="asset-missing"
			style="margin-top:8px;padding:8px 10px;border-radius:var(--radius-sm);background:color-mix(in srgb,var(--danger) 14%,transparent);border:var(--line-width) solid color-mix(in srgb,var(--danger) 40%,transparent);font-size:12px;line-height:1.5;color:var(--text-primary)"
		>
			The file is not at <span style="font-family:var(--font-mono);overflow-wrap:anywhere">{asset.path}</span>. The clips cut from it
			stay on the timeline; previews and export need the file.
			<div style="margin-top:6px"><Btn size="sm" variant="secondary" icon="link" onclick={() => void relinkMediaFlow(asset)}>Relink…</Btn></div>
		</div>
	{/if}
	{#if spec}
		<div style="margin-top:8px;font-size:12px;color:var(--text-muted);line-height:1.4">{spec}</div>
	{/if}

	<div style="margin-top:8px;display:flex;align-items:center;gap:8px;flex-wrap:wrap">
		<MediaChips assetId={asset.id} />
		{#if ui.analyzing && ui.analyzingId === asset.id}
			<div style="flex-basis:100%"><AnalysisRun /></div>
		{:else}
			<Btn
				size="sm"
				variant="secondary"
				icon="scan-line"
				onclick={(e: MouseEvent) => {
					const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
					contextMenu.show(new MouseEvent('contextmenu', { clientX: r.left, clientY: r.bottom + 2 }), analyzeMenuItems(asset));
				}}>Analyze</Btn
			>
		{/if}
	</div>

	<div style="margin-top:10px;display:flex;gap:6px;flex-wrap:wrap">
		<Btn
			size="sm"
			icon="plus"
			iconSize={12}
			pending={editor.locked}
			onclick={() =>
				void editor
					.add(asset.id, 0, placementOut(asset, settings.editing.still_seconds), undefined, ui.time)
					.then(() => toast.success(`Added ${asset.name}`))
					.catch(err)}>Add at playhead</Btn
		>
		<Btn
			size="sm"
			icon="list-plus"
			iconSize={12}
			pending={editor.locked}
			onclick={() =>
				void editor
					.add(asset.id, 0, placementOut(asset, settings.editing.still_seconds), undefined, editor.duration)
					.then(() => toast.success(`Appended ${asset.name}`))
					.catch(err)}>Append</Btn
		>
		{#if mediaStatus.done(asset.id, 'transcript')}
			<Btn
				size="sm"
				icon="file-text"
				iconSize={12}
				title="Write the transcript as a subtitle file, in this media's own time"
				onclick={() => void exportTranscriptSrt(asset.id)}>Export .srt</Btn
			>
		{/if}
	</div>

	<div style="margin-top:6px;display:flex;gap:6px;flex-wrap:wrap">
		<Btn size="sm" variant="ghost" icon="link" iconSize={12} title="Point this media at another file" onclick={() => void relinkMediaFlow(asset)}>Relink…</Btn>
		<Btn
			size="sm"
			variant="ghost"
			icon="trash"
			iconSize={12}
			pending={editor.locked}
			title="Take this media out of the project"
			onclick={() => void removeMediaFlow(asset)}>Remove…</Btn
		>
	</div>

	{#if asset.music_score}
		<InspectorSection title="Composed music" summary={scoreTitle(asset.music_score)} open>
			<ComposedMusic {asset} />
		</InspectorSection>
	{/if}

	<InspectorSection title="Media" summary={`${info.duration}${info.resolution ? ` · ${info.resolution}` : ''}`} open>
		{#each facts.media as f (f.label)}{@render row(f)}{/each}
	</InspectorSection>

	<InspectorSection title="Analysis" summary={doneCount(mediaStatus.analysis(asset.id))} open>
		{#each facts.analysis as f (f.label)}{@render row(f)}{/each}
	</InspectorSection>

	{#if music}
		<InspectorSection title="Music structure" summary={musicSummary(music)} open>
			<MusicStructure assetId={asset.id} {music} />
		</InspectorSection>
	{/if}

	{#if facts.proxy.length > 0}
		<InspectorSection title="Proxy" summary={facts.proxy[0].value}>
			{#each facts.proxy as f (f.label)}{@render row(f)}{/each}
			<div style="margin-top:6px;display:flex;gap:6px;flex-wrap:wrap">
				<Btn
					size="sm"
					icon="refresh-cw"
					iconSize={12}
					disabled={act.rebuild.disabled}
					title={act.rebuild.reason}
					onclick={() =>
						void rebuildProxy(asset.id)
							.then((s) => {
								mediaStatus.noteProxy(s);
								toast.success(`Building the proxy of ${asset.name}`);
							})
							.catch(err)}>{act.rebuild.label}</Btn
				>
				<Btn
					size="sm"
					variant="destructive"
					icon="trash"
					iconSize={12}
					disabled={act.remove.disabled}
					title={act.remove.reason}
					onclick={() =>
						void deleteProxy(asset.id)
							.then((s) => {
								mediaStatus.noteProxy(s);
								ui.refreshPreview();
								toast.success(`Deleted the proxy of ${asset.name}; previews use the original`);
							})
							.catch(err)}>{act.remove.label}</Btn
				>
			</div>
		</InspectorSection>
	{/if}
</div>
