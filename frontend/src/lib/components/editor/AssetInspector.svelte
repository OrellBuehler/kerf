<script lang="ts">
	// The Inspector's view of a library asset when one is selected and no clip is: its name and kind, the two
	// ways onto the timeline, a status strip while the file's proxy or analysis is going, waiting or has failed (the
	// wait is felt here, above the fold: what is happening, that editing does not wait, what to do), then one section
	// per subject — the file, its analysis (one row per step: what it found, or what it will give, and Analyze), its
	// proxy — each with its own actions, and the file's Relink / Remove at the end, where a clip keeps its Remove.
	// The facts are the ones the bin's right-click menu used to list as twenty rows.
	import Icon from './Icon.svelte';
	import Badge from './Badge.svelte';
	import Btn from './Btn.svelte';
	import ProgressBar from './ProgressBar.svelte';
	import InspectorSection from './InspectorSection.svelte';
	import ComposedMusic from './ComposedMusic.svelte';
	import TagChip from './TagChip.svelte';
	import TagInput from './TagInput.svelte';
	import AssetPreview from './AssetPreview.svelte';
	import { scoreTitle } from '#lib/music-score';
	import MusicStructure from './MusicStructure.svelte';
	import { musicSummary } from '#lib/music-ui';
	import { VIDEO_THUMB_BG } from './data';
	import { editor } from '#lib/state.svelte';
	import { ui } from '#lib/editor-ui.svelte';
	import { mediaStatus } from '#lib/media-status.svelte';
	import { contextMenu, type MenuItem } from '#lib/context-menu.svelte';
	import { analyzeAsset, analyzeMenuItems } from '#lib/analyze-menu';
	import { ANALYSIS_WHAT, NO_WAIT } from '#lib/analysis-steps';
	import { assetFacts, rowNote, type AnalysisRow, type Fact } from '#lib/asset-facts';
	import { assetWork, workNote, type WorkItem } from '#lib/background-work';
	import { mediaInfo } from '#lib/media-info';
	import { PROXY_NO_WAIT, PROXY_WHAT, proxyActions } from '#lib/proxy-info';
	import { deleteProxy, inTauri, rebuildProxy, revealPath } from '#lib/api';
	import { toast } from '#lib/notifications.svelte';
	import { exportTranscriptSrt, report } from '#lib/ops';
	import { relinkMediaFlow, removeMediaFlow } from '#lib/library-actions';
	import { tip } from '#lib/tip';
	import { settings } from '#lib/settings.svelte';
	import { placementOut } from '#lib/editing-prefs';
	import { tidyReason } from '#lib/failure-copy';
	import type { Asset, TagEdit } from '#lib/types';
	import { untrack } from 'svelte';

	let { asset }: { asset: Asset } = $props();

	const info = $derived(mediaInfo(asset, editor.timeline));
	const facts = $derived(
		assetFacts({
			asset,
			info,
			analysis: editor.analysisFor(asset.id),
			status: mediaStatus.analysis(asset.id),
			analyzing: ui.analyzingId === asset.id,
			busy: ui.analyzing,
			running: ui.analysisKind,
			queued: ui.analysisWaiting.includes(asset.id),
			transcription: ui.transcription?.available ?? true,
			proxy: mediaStatus.proxy(asset.id),
			file: mediaStatus.files[asset.id]
		})
	);
	/** Music with a fitted bar grid: its structure is shown under Analysis. */
	const music = $derived(editor.analysisFor(asset.id)?.music ?? null);
	const proxy = $derived(mediaStatus.proxy(asset.id));
	const act = $derived(proxyActions(proxy));
	const gone = $derived(mediaStatus.isMissing(asset.id));
	/** A pass is running on this asset: its progress takes Analyze's place. */
	const running = $derived(ui.analyzing && ui.analyzingId === asset.id);
	/** The steps that failed, for the strip's retry. */
	const failedKinds = $derived(facts.rows.filter((r) => r.state === 'failed').map((r) => r.kind));
	/** What the proxy and the analysis are doing, going, waiting or failed — the strip under the Add buttons. */
	const work = $derived(
		assetWork({
			proxy,
			run: running ? { label: ui.analysisLabel ?? 'analyzing', queued: ui.analysisQueued } : null,
			waiting: ui.analysisWaiting.includes(asset.id),
			failed: facts.rows.filter((r) => r.state === 'failed').map((r) => ({ label: r.label, reason: r.reason }))
		})
	);
	const note = $derived(workNote(work));
	/** The proxy is not at rest: its sentence under the section's title says whether to wait. */
	const proxyBusy = $derived(proxy?.state === 'building' || proxy?.state === 'queued');
	const TONE_COLOR: Record<WorkItem['tone'], string> = { running: 'var(--kerf-300)', queued: 'var(--text-secondary)', failed: 'var(--red-400)' };

	/** What a strip line's button does: try the failed proxy or the failed analysis steps again; Stop the run. */
	function fix(w: WorkItem) {
		if (w.fix === 'retry-proxy')
			void rebuildProxy(asset.id)
				.then((s) => {
					mediaStatus.noteProxy(s);
					toast.success(`Building the proxy of ${asset.name}`);
				})
				.catch(err);
		else if (w.fix === 'retry-analysis') void analyzeAsset(asset.id, failedKinds);
	}
	const ROW_ITEM = 'flex:1 1 0;min-width:max-content';
	const KIND: Record<string, string> = { video: 'Video', audio: 'Audio', image: 'Still' };

	const err = report;

	function copy(text: string, what: string) {
		void navigator.clipboard
			.writeText(text)
			.then(() => toast.success(`${what} copied`))
			.catch(() => toast.error(`Couldn't copy the ${what.toLowerCase()}`));
	}

	/** A right-click on a fact copies it; the path can also be shown where it is. */
	function factMenu(e: MouseEvent, f: Fact) {
		const path = f.label === 'Path';
		const items: MenuItem[] = [
			{ label: path ? 'Copy path' : `Copy ${f.label.toLowerCase()}`, icon: 'copy', action: () => copy(f.title ?? f.value, f.label) }
		];
		if (path) {
			items.push({ label: 'Copy file name', icon: 'copy', action: () => copy(asset.name, 'File name') });
			items.push({
				label: 'Show in folder',
				icon: 'folder-open',
				disabled: !inTauri(),
				action: () => void revealPath(asset.path).catch(err)
			});
		}
		contextMenu.show(e, items);
	}

	function nameMenu(e: MouseEvent) {
		contextMenu.show(e, [
			{ label: 'Copy file name', icon: 'copy', action: () => copy(asset.name, 'File name') },
			{ label: 'Copy path', icon: 'copy', action: () => copy(asset.path, 'Path') }
		]);
	}

	// ---- tags ----
	const tags = $derived(asset.tags ?? []);
	let draft = $state('');
	let tagError = $state('');
	// Another asset: what was typed, and the refusal of the last change, were about the previous one.
	$effect(() => {
		void asset.id;
		untrack(() => {
			draft = '';
			tagError = '';
		});
	});

	/** The changes asked for, one after the other: a second × while the first is still on its way waits its turn rather
	 *  than being dropped. */
	let queue: Promise<unknown> = Promise.resolve();

	/** One tag put on or taken off, as one `edit_asset_tags`; the engine's refusal (past 64 tags) is said under the field.
	 *  It always lands on the asset it was asked for, but only says anything — and only clears the field — while the
	 *  Inspector is still on that asset. */
	function change(edit: TagEdit): Promise<boolean> {
		const target = asset.id;
		const run = async (): Promise<boolean> => {
			if (asset.id === target) tagError = '';
			try {
				await editor.tagAssets([target], edit);
				return asset.id === target;
			} catch (e) {
				if (asset.id === target) tagError = tidyReason(e instanceof Error ? e.message : String(e));
				return false;
			}
		};
		const next = queue.then(run, run);
		queue = next;
		return next;
	}
</script>

{#snippet analysisRow(r: AnalysisRow)}
	<div data-testid="analysis-row" data-kind={r.kind} data-state={r.state} style="padding:3px 0">
		<div style="display:flex;align-items:center;gap:8px;min-width:0">
			<span aria-hidden="true" style="flex:none;width:12px;display:inline-flex;justify-content:center;color:{r.state === 'done'
				? 'var(--green-400)'
				: r.state === 'failed'
					? 'var(--red-400)'
					: r.state === 'running'
						? 'var(--kerf-300)'
						: 'var(--text-muted)'}">
				{#if r.state === 'done'}<Icon n="check" s={12} />
				{:else if r.state === 'failed'}<Icon n="circle-alert" s={12} />
				{:else if r.state === 'running'}<span class="kerf-spin" style="width:9px;height:9px;border:1.5px solid currentColor;border-top-color:transparent;border-radius:50%"></span>
				{:else if r.state === 'off'}–
				{:else}<span style="width:8px;height:8px;border-radius:50%;border:var(--line-width) solid currentColor"></span>{/if}
			</span>
			<span style="flex:none;font-size:12px;color:var(--text-secondary)">{r.label}</span>
			<span
				data-selectable
				style="flex:1;min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;text-align:right;font-family:var(--font-mono);font-size:12px;color:var(--text-secondary)"
				>{r.state === 'done' ? r.found : r.state === 'running' ? 'running' : r.state === 'failed' ? 'failed' : r.state === 'off' ? 'off' : 'not run'}</span
			>
			{#if r.action}
				<Btn
					size="sm"
					variant="ghost"
					title={r.state === 'failed' ? `Try ${r.label.toLowerCase()} again` : `Run ${r.label.toLowerCase()} on this file`}
					onclick={() => void analyzeAsset(asset.id, [r.kind])}>{r.state === 'failed' ? 'Try again' : 'Run'}</Btn
				>
			{/if}
		</div>
		{#if rowNote(r)}
			<div style="margin:2px 0 0 20px;font-size:11px;line-height:1.45;color:{r.state === 'failed' ? 'var(--text-secondary)' : 'var(--text-muted)'}">{rowNote(r)}</div>
		{/if}
	</div>
{/snippet}

{#snippet row(f: Fact)}
	<div
		role="presentation"
		oncontextmenu={(e) => factMenu(e, f)}
		style="display:flex;align-items:baseline;justify-content:space-between;gap:12px;padding:3px 0"
	>
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
				data-selectable
				data-testid="asset-name"
				role="presentation"
				oncontextmenu={nameMenu}
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
	{#if editor.selectedAssetIds.length > 1}
		<div data-testid="asset-multi" style="margin-top:8px;display:flex;align-items:center;gap:8px;font-size:12px;color:var(--text-secondary)">
			<span style="flex:1;min-width:0">{editor.selectedAssetIds.length - 1} more selected in the bin</span>
			<Btn size="sm" icon="tag" iconSize={12} onclick={() => ui.openTagDialog([...editor.selectedAssetIds], 'bulk')}>
				Tag {editor.selectedAssetIds.length} assets…
			</Btn>
		</div>
	{/if}
	{#if gone}
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
	<AssetPreview {asset} {info} missing={gone} />
	<!-- Two equal buttons that wrap to one per line before either is cut short (the Inspector can be 250 px). -->
	<div style="margin-top:10px;display:flex;flex-wrap:wrap;gap:6px">
		<Btn
			size="sm"
			style={ROW_ITEM}
			icon="plus"
			iconSize={12}
			pending={editor.locked}
			title="Put it on the timeline at the playhead"
			onclick={() =>
				void editor
					.add(asset.id, 0, placementOut(asset, settings.editing.still_seconds), undefined, ui.time)
					.then(() => toast.success(`Added ${asset.name}`))
					.catch(err)}>Add at playhead</Btn
		>
		<Btn
			size="sm"
			style={ROW_ITEM}
			icon="list-plus"
			iconSize={12}
			pending={editor.locked}
			title="Put it at the end of the cut"
			onclick={() =>
				void editor
					.add(asset.id, 0, placementOut(asset, settings.editing.still_seconds), undefined, editor.duration)
					.then(() => toast.success(`Appended ${asset.name}`))
					.catch(err)}>Append</Btn
		>
	</div>
	{#if work.length > 0}
		<!-- The wait, where it is felt: what the proxy and the analysis are doing, that editing does not wait for them, and what to do when one failed. -->
		<div
			role="status"
			data-testid="asset-work"
			style="margin-top:10px;padding:8px 10px;border-radius:var(--radius-sm);background:var(--surface-raised);border:var(--line-width) solid var(--border-default);display:flex;flex-direction:column;gap:7px"
		>
			{#each work as w (w.id)}
				<div data-testid="asset-work-line" data-tone={w.tone} style="display:flex;flex-direction:column;gap:4px;min-width:0">
					<div style="display:flex;align-items:baseline;gap:8px;min-width:0">
						<span style="flex:none;font-size:12px;font-weight:600;color:var(--text-primary)">{w.label}</span>
						<span style="flex:1;min-width:0;font-size:12px;line-height:1.4;color:{TONE_COLOR[w.tone]}">{w.text}</span>
						{#if w.id === 'analysis' && w.tone === 'running'}
							<Btn
								size="sm"
								disabled={ui.stoppingAnalysis}
								title="Stop analyzing — it gives up between steps, and within about a second during transcription"
								onclick={() => ui.stopAnalysis()}>{ui.stoppingAnalysis ? 'Stopping…' : 'Stop'}</Btn
							>
						{:else if w.fix}
							<Btn
								size="sm"
								disabled={w.fix === 'retry-analysis' && ui.analyzing}
								title={w.fix === 'retry-analysis' && ui.analyzing ? 'An analysis is already running' : undefined}
								onclick={() => fix(w)}>Try again</Btn
							>
						{/if}
					</div>
					{#if w.id === 'proxy' && w.tone === 'running'}
						<ProgressBar value={proxy?.fraction ?? null} label="Building the proxy" height={4} />
					{:else if w.id === 'analysis' && w.tone === 'running'}
						<ProgressBar value={ui.analysisStage?.fraction ?? null} label="Analyzing this file" height={4} />
					{/if}
					{#if w.detail}<span style="font-size:11px;line-height:1.45;color:var(--text-secondary)">{w.detail}</span>{/if}
				</div>
			{/each}
			{#if note}<span style="font-size:11px;line-height:1.45;color:var(--text-muted)">{note}</span>{/if}
		</div>
	{/if}

	{#if asset.music_score}
		<InspectorSection title="Composed music" summary={scoreTitle(asset.music_score)} open>
			<ComposedMusic {asset} />
		</InspectorSection>
	{/if}

	<InspectorSection title="Media" summary={`${info.duration}${info.resolution ? ` · ${info.resolution}` : ''}`} open>
		{#each facts.media as f (f.label)}{@render row(f)}{/each}
	</InspectorSection>

	<InspectorSection title="Tags" summary={tags.length === 0 ? 'none' : String(tags.length)} open>
		<div data-testid="asset-tags" style="display:flex;flex-direction:column;gap:8px">
			{#if tags.length > 0}
				<div style="display:flex;flex-wrap:wrap;gap:6px">
					{#each tags as tag (JSON.stringify(tag))}
						<TagChip {tag} onremove={() => void change({ remove: [tag] })} />
					{/each}
				</div>
			{:else}
				<div style="font-size:12px;line-height:1.5;color:var(--text-muted)">
					No tags. A tag like location: Cumbuco finds this later, and lets an agent build a cut by it.
				</div>
			{/if}
			<TagInput bind:value={draft} exclude={tags} error={tagError} oncommit={(tag) => change({ add: [tag] })} />
		</div>
	</InspectorSection>

	<InspectorSection title="Analysis" summary={facts.analyzed} open>
		<p data-testid="analysis-what" style="margin:0 0 6px;font-size:12px;line-height:1.45;color:var(--text-muted)">{ANALYSIS_WHAT} {NO_WAIT}</p>
		{#if facts.offReason}
			<p data-testid="analysis-off" style="margin:0 0 4px;font-size:12px;line-height:1.45;color:var(--text-secondary)">{facts.offReason}. Run does one step by hand.</p>
		{/if}
		{#each facts.rows as r (r.kind)}{@render analysisRow(r)}{/each}
		{#if facts.rows.length === 0}
			<div style="font-size:12px;color:var(--text-muted)">There is nothing in this file to analyze.</div>
		{/if}
		{#each facts.analysis as f (f.label)}{@render row(f)}{/each}
		<div style="margin-top:6px;display:flex;gap:6px;flex-wrap:wrap">
			{#if !running}
				<Btn
					size="sm"
					icon="scan-line"
					iconSize={12}
					onclick={(e: MouseEvent) => {
						const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
						contextMenu.show(new MouseEvent('contextmenu', { clientX: r.left, clientY: r.bottom + 2 }), analyzeMenuItems(asset));
					}}>Analyze</Btn
				>
			{/if}
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
	</InspectorSection>

	{#if music}
		<InspectorSection title="Music structure" summary={musicSummary(music)} open>
			<MusicStructure assetId={asset.id} {music} />
		</InspectorSection>
	{/if}

	{#if facts.proxy.length > 0}
		<InspectorSection title="Proxy" summary={facts.proxy[0].value}>
			{#each facts.proxy as f (f.label)}{@render row(f)}{/each}
			{#if proxy?.state !== 'not_needed'}
				<p data-testid="proxy-what" style="margin:6px 0 0;font-size:12px;line-height:1.45;color:var(--text-muted)">{proxyBusy ? `${PROXY_WHAT} ${PROXY_NO_WAIT}` : PROXY_WHAT}</p>
			{/if}
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
					data-quiet
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

	<div style="margin-top:18px;display:flex;flex-wrap:wrap;gap:6px">
		{#if !gone}
			<Btn size="sm" style={ROW_ITEM} icon="link" iconSize={12} title="Point this media at another file" onclick={() => void relinkMediaFlow(asset)}>Relink…</Btn>
		{/if}
		<Btn
			size="sm"
			style={ROW_ITEM}
			variant="destructive"
			data-quiet
			icon="trash"
			iconSize={12}
			pending={editor.locked}
			title="Take this media out of the project"
			onclick={() => void removeMediaFlow(asset)}>Remove…</Btn
		>
	</div>
</div>
