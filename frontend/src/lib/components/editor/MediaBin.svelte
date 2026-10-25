<script lang="ts">
	import { untrack } from 'svelte';
	import Icon from './Icon.svelte';
	import { VIDEO_THUMB_BG } from './data';
	import Badge from './Badge.svelte';
	import MediaChips from './MediaChips.svelte';
	import AnalysisRun from './AnalysisRun.svelte';
	import Btn from './Btn.svelte';
	import TagChip from './TagChip.svelte';
	import { ui } from '#lib/editor-ui.svelte';
	import { editor } from '#lib/state.svelte';
	import { mediaStatus } from '#lib/media-status.svelte';
	import { analyzeSubmenuItem } from '#lib/analyze-menu';
	import { appendAllMedia, exportTranscriptSrt, report } from '#lib/ops';
		import { proxyActions } from '#lib/proxy-info';
	import { deleteProxy, rebuildProxy } from '#lib/api';
	import { contextMenu } from '#lib/context-menu.svelte';
	import type { MenuItem } from '#lib/context-menu.svelte';
	import { getFrame, inTauri, revealPath } from '#lib/api';
	import { importMedia } from '#lib/import-media';
	import { relinkMediaFlow, removeMediaFlow } from '#lib/library-actions';
	import { missingLine } from '#lib/library-copy';
	import { toast } from '#lib/notifications.svelte';
	import { thumbnails } from '#lib/thumbnails';
	import { audioExtraction, mediaInfo, shortPath, specLine, thumbTime, type MediaInfo } from '#lib/media-info';
	import type { Asset } from '#lib/types';
	import { tip } from '#lib/tip';
	import { settings } from '#lib/settings.svelte';
	import { placementOut } from '#lib/editing-prefs';
	import { tagsMatch } from '#lib/tags';
	import { rowChips, termLabel, toggleTerm } from '#lib/tag-ui';

	type BinAsset = { asset: Asset; info: MediaInfo };

	const loaded = $derived(editor.assets.length > 0);

	const assets = $derived<BinAsset[]>(
		editor.assets.map((a) => ({ asset: a, info: mediaInfo(a, editor.timeline) }))
	);

	/** What the tag filter lets through, in the bin's order: what the bin draws, what Shift-click runs across. */
	const shown = $derived(assets.filter((a) => tagsMatch(ui.tagFilter, a.asset.tags ?? [])));
	const filtered = $derived(ui.tagFilter.length > 0);
	const order = $derived(shown.map((a) => a.asset.id));

	// One decoded frame per asset, so a row shows the footage rather than an icon.
	// The frames live in `thumbnails`, outside this component: the library remounts
	// the bin on every tab switch, unfold and workspace switch, and each remount
	// would otherwise decode every asset again. `null` in the browser harness,
	// where the icon stays. A remount starts from what is already cached, so the
	// rows never flash back to icons.
	let thumbs = $state<Record<string, string>>(
		Object.fromEntries(
			untrack(() => editor.assets).flatMap((a) => {
				const url = thumbnails.peek(a.id);
				return url ? [[a.id, url]] : [];
			})
		)
	);
	const requested = new Set<string>();

	$effect(() => {
		// Frames of assets that were removed are dropped with them.
		thumbnails.prune(assets.map((a) => a.asset.id));
		for (const { asset, info } of assets) {
			if (requested.has(asset.id)) continue;
			requested.add(asset.id);
			if (info.kind === 'audio') {
				thumbnails.none(asset.id);
				continue;
			}
			void thumbnails.load(asset.id, () => getFrame(asset.id, thumbTime(asset.duration), 160, false)).then((url) => {
				if (url) thumbs[asset.id] = url;
			});
		}
	});

	/** A click or Enter / Space on a row: replaces the selection; Ctrl/Cmd toggles, Shift extends from the primary. */
	function onSelect(e: MouseEvent | KeyboardEvent, assetId: string) {
		void editor.pickAsset(assetId, e, order);
	}

	const err = report;

	// Drag an asset out of the bin; the timeline lanes accept the drop and add a
	// clip. A pointer drag, not HTML5 drag-and-drop: on Windows, Tauri's file-drop
	// handler (kept on for files from the OS) answers an in-page drag with "no
	// drop", so WebView2 never delivers its dragover / drop to the page.
	const DRAG_SLOP = 4;
	function onAssetPointerDown(e: PointerEvent, { asset, info }: BinAsset) {
		if (e.button !== 0) return;
		e.preventDefault();
		const payload = { id: asset.id, kind: info.kind === 'audio' ? ('audio' as const) : ('video' as const), duration: placementOut(asset, settings.editing.still_seconds) };
		const [x0, y0] = [e.clientX, e.clientY];
		const root = document.documentElement;
		const cursor = root.style.cursor;
		const end = () => {
			window.removeEventListener('pointermove', move);
			window.removeEventListener('pointerup', up);
			window.removeEventListener('pointercancel', end);
			window.removeEventListener('keydown', key);
			window.removeEventListener('blur', end);
			root.style.cursor = cursor;
			ui.dndAsset = null;
			ui.dndPoint = null;
		};
		const move = (m: PointerEvent) => {
			if (!ui.dndAsset) {
				if (Math.hypot(m.clientX - x0, m.clientY - y0) < DRAG_SLOP) return;
				ui.dndAsset = payload;
				root.style.cursor = 'grabbing';
			}
			ui.dndPoint = { x: m.clientX, y: m.clientY };
		};
		const up = (u: PointerEvent) => {
			const dragging = !!ui.dndAsset;
			end();
			if (dragging) ui.assetDropTarget?.(payload, u.clientX, u.clientY);
		};
		const key = (k: KeyboardEvent) => {
			if (k.key === 'Escape') end();
		};
		window.addEventListener('pointermove', move);
		window.addEventListener('pointerup', up);
		window.addEventListener('pointercancel', end);
		window.addEventListener('keydown', key);
		window.addEventListener('blur', end);
	}

	/** The menu of several selected assets: what can be done to all of them at once. */
	function onSelectionContextMenu(e: MouseEvent) {
		const ids = [...editor.selectedAssetIds];
		contextMenu.show(e, [
			{ type: 'header', label: `${ids.length} assets`, sub: 'Selected in the bin' },
			{ label: `Tag ${ids.length} assets…`, icon: 'tag', action: () => ui.openTagDialog(ids, 'bulk') },
			{ type: 'separator' },
			{
				label: 'Clear selection',
				icon: 'x',
				shortcut: settings.shortcut('edit.clearSelection'),
				action: () => void editor.collapseAssetSelection()
			}
		]);
	}

	function onAssetContextMenu(e: MouseEvent, { asset, info }: BinAsset) {
		// A right-click inside a selection of several acts on all of them; anywhere else it selects what it is on.
		if (editor.selectedAssetIds.length > 1 && editor.selectedAssetSet.has(asset.id)) return onSelectionContextMenu(e);
		void editor.select(asset.id);
		const analysis = editor.analysisFor(asset.id);
		const spherical = !!info.projection;

		const items: MenuItem[] = [{ type: 'header', label: asset.name, sub: shortPath(asset.path) }];

		// The facts — length, frame, codec, where it is used, what analysis found, the proxy — are the Inspector's
		// view of the asset (`AssetInspector`); this menu is what can be done to it.
		items.push({
			label: 'Add at playhead',
			icon: 'plus',
			action: () =>
				void editor
					.add(asset.id, 0, placementOut(asset, settings.editing.still_seconds), undefined, ui.time)
					.then(() => toast.success(`Added ${asset.name}`))
					.catch(err)
		});
		items.push({
			label: 'Append to timeline',
			icon: 'list-plus',
			action: () =>
				void editor
					.add(asset.id, 0, placementOut(asset, settings.editing.still_seconds), undefined, editor.duration)
					.then(() => toast.success(`Appended ${asset.name}`))
					.catch(err)
		});
		// The label says what will happen (`audioExtraction` mirrors the backend's two operations): the
		// asset's own sound is detached (`extract_audio`) from every clip on the timeline still playing
		// it — so it is heard once — and only an asset with none such has its whole audio appended to an
		// audio track (`add_asset_audio`, a separate operation the backend never falls back to).
		const extraction = info.kind === 'video' ? audioExtraction(asset, editor.timeline) : null;
		if (extraction)
			items.push({
				label: extraction.label,
				icon: 'audio-waveform',
				action: () => {
					if (extraction.mode === 'append') {
						void editor
							.addAssetAudio(asset.id)
							.then(() =>
								toast.success(`Added the audio of ${asset.name}`, {
									action: { label: 'Undo', onClick: () => void editor.undo() }
								})
							)
							.catch(err);
						return;
					}
					void editor
						.extractAudio(asset.id)
						.then((done) =>
							toast.success(
								`Detached audio from ${done.detached} ${done.detached === 1 ? 'clip' : 'clips'}` +
									(done.skipped.length > 0 ? ` — ${done.skipped.length} skipped (${done.skipped[0].reason})` : ''),
								{ action: { label: 'Undo', onClick: () => void editor.undo() } }
							)
						)
						.catch(err);
				}
			});
		items.push({
			label: 'Remove silences',
			icon: 'Scissors',
			disabled: !analysis?.silence_segments.length,
			action: () =>
				void editor
					.removeSilence(asset.id)
					.then(() => toast.success(`Removed ${analysis?.silence_segments.length} silent gaps`))
					.catch(err)
		});
		// Drums / bass / other / vocals into the library; the clip menu lays them under a clip instead.
		if (info.audio)
			items.push({
				label: 'Separate stems…',
				icon: 'layers',
				action: () => ui.openStems(asset.id)
			});

		// The transcript as a subtitle file, in the asset's own time; off, with the reason, until there is one.
		const hasTranscript = (analysis?.transcript.length ?? 0) > 0;
		items.push({
			label: 'Export transcript as .srt…',
			icon: 'file-text',
			disabled: !hasTranscript,
			reason: hasTranscript ? undefined : 'Transcribe it first (Analyze › Transcribe speech)',
			action: () => void exportTranscriptSrt(asset.id)
		});
		if (editor.assets.length > 1)
			items.push({
				label: `Append all ${editor.assets.length} files to the timeline`,
				icon: 'list-plus',
				disabled: editor.locked,
				reason: editor.locked ? 'Exit the proposal preview to edit' : undefined,
				action: () => void appendAllMedia()
			});

		items.push({ label: 'Tag…', icon: 'tag', action: () => ui.openTagDialog([asset.id], 'bulk') });

		items.push({ type: 'separator' });
		if (asset.voiceover) {
			const vo = asset.voiceover;
			items.push({
				label: 'Regenerate voiceover…',
				icon: 'mic',
				action: () => ui.openVoiceover({ text: vo.text, voice: vo.voice, speed: vo.speed })
			});
		}
		if (asset.music_score) {
			const score = asset.music_score;
			items.push({
				label: 'Edit & compose again…',
				icon: 'music',
				action: () => ui.openCompose({ score })
			});
		}
		items.push(analyzeSubmenuItem(asset, { x: e.clientX, y: e.clientY }));
		if (info.kind === 'video')
			items.push({
				label: spherical ? 'Mark as flat footage' : 'Mark as 360 (equirect)',
				icon: spherical ? 'video' : 'globe',
				action: () =>
					void editor
						.setAssetProjection(asset.id, spherical ? 'flat' : 'equirect')
						.then(() => toast.success(spherical ? `${asset.name} is flat` : `${asset.name} is 360 footage`))
						.catch(err)
			});
		if (info.kind === 'video') {
			const act = proxyActions(mediaStatus.proxy(asset.id));
			items.push({ type: 'separator' });
			items.push({
				label: act.rebuild.label,
				icon: 'refresh-cw',
				disabled: act.rebuild.disabled,
				reason: act.rebuild.reason,
				action: () =>
					void rebuildProxy(asset.id)
						.then((s) => {
							mediaStatus.noteProxy(s);
							toast.success(`Building the proxy of ${asset.name}`);
						})
						.catch(err)
			});
			items.push({
				label: act.remove.label,
				icon: 'trash',
				danger: true,
				disabled: act.remove.disabled,
				reason: act.remove.reason,
				action: () =>
					void deleteProxy(asset.id)
						.then((s) => {
							mediaStatus.noteProxy(s);
							ui.refreshPreview();
							toast.success(`Deleted the proxy of ${asset.name}; previews use the original`);
						})
						.catch(err)
			});
		}

		const gone = mediaStatus.isMissing(asset.id);
		items.push({ type: 'separator' });
		items.push({
			label: gone ? 'Relink media…' : 'Relink to another file…',
			icon: 'link',
			action: () => void relinkMediaFlow(asset)
		});
		items.push({
			label: 'Remove from project…',
			icon: 'trash',
			danger: true,
			disabled: editor.locked,
			reason: editor.locked ? 'Exit the proposal preview to edit' : undefined,
			action: () => void removeMediaFlow(asset)
		});
		items.push({ type: 'separator' });
		items.push({
			label: 'Copy path',
			icon: 'copy',
			action: () =>
				void navigator.clipboard
					.writeText(asset.path)
					.then(() => toast.success('Path copied'))
					.catch(() => toast.error("Couldn't copy the path"))
		});
		items.push({
			label: 'Show in folder',
			icon: 'folder-open',
			disabled: !inTauri(),
			action: () => void revealPath(asset.path).catch(err)
		});

		contextMenu.show(e, items);
	}
</script>

<div
	style="flex:1;min-height:0;background:var(--surface-panel);display:flex;flex-direction:column;overflow:hidden"
>
	{#if loaded && filtered}
		<!-- The tag filter in force: every term must hold. The header's tag button adds terms; each is taken off here. -->
		<div
			data-testid="tag-filter-bar"
			style="flex:none;display:flex;align-items:center;gap:6px;flex-wrap:wrap;padding:6px 12px;border-bottom:var(--line-width) solid var(--border-subtle)"
		>
			{#each ui.tagFilter as term (termLabel(term))}
				<span
					data-tag-term
					style="display:inline-flex;align-items:center;gap:2px;min-width:0;max-width:100%;height:22px;padding:0 3px 0 8px;border-radius:var(--radius-sm);border:var(--line-width) solid var(--kerf-500);background:color-mix(in srgb,var(--kerf-500) 18%,transparent);font-size:12px;color:var(--text-primary)"
				>
					<span style="min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap">{termLabel(term)}</span>
					<button
						type="button"
						class="k-ibtn"
						aria-label="Stop filtering by {termLabel(term)}"
						use:tip={`Stop filtering by ${termLabel(term)}`}
						onclick={() => ui.setTagFilter(toggleTerm(ui.tagFilter, term))}
						style="flex:none;width:16px;height:16px;border-radius:var(--radius-sm)"><Icon n="x" s={10} /></button
					>
				</span>
			{/each}
			<Btn variant="ghost" size="sm" style="margin-left:auto" data-testid="tag-filter-clear" onclick={() => ui.setTagFilter([])}>Clear</Btn>
		</div>
	{/if}
	<div style="flex:1;overflow-y:auto;padding:12px">
		{#if !loaded}
			{#if editor.importing}
				<!-- import in flight -->
				<div
					style="border:1.5px dashed var(--border-strong);border-radius:var(--radius-md);padding:32px 16px;display:flex;flex-direction:column;align-items:center;gap:12px;background:var(--surface-inset);text-align:center"
				>
					<span class="kerf-spin" style="color:var(--kerf-400)"><Icon n="loader" s={22} /></span>
					<div>
						<div style="font:var(--type-ui);color:var(--text-primary)">Importing media…</div>
						<div style="font-size:12px;color:var(--text-muted);margin-top:3px">
							{#if editor.importProgress !== null}
								Stitching 360 lens pair · {Math.round(editor.importProgress * 100)}%
							{:else}
								Probing streams locally
							{/if}
						</div>
					</div>
					<Btn variant="secondary" size="sm" icon="x" pending={editor.stoppingImport} onclick={() => void editor.stopImport()}>
						{editor.stoppingImport ? 'Stopping…' : 'Stop'}
					</Btn>
				</div>
			{:else}
				<!-- dropzone -->
				<div
					onclick={importMedia}
					role="button"
					tabindex="0"
					onkeydown={(e) => (e.key === 'Enter' || e.key === ' ') && (e.preventDefault(), void importMedia())}
					style="border:1.5px dashed var(--border-strong);border-radius:var(--radius-md);padding:32px 16px;display:flex;flex-direction:column;align-items:center;gap:12px;cursor:pointer;background:var(--surface-inset);text-align:center"
				>
					<div
						style="width:40px;height:40px;border-radius:var(--radius-md);display:grid;place-items:center;background:var(--surface-hover);color:var(--text-muted)"
					>
						<Icon n="film" s={20} />
					</div>
					<div>
						<div style="font:var(--type-ui);color:var(--text-primary)">Drop media to start</div>
						<div style="font-size:12px;color:var(--text-muted);margin-top:3px">
							Kerf analyzes new media on this computer. Choose what in Settings › Analysis.
						</div>
					</div>
					<Btn variant="secondary" size="sm" icon="plus">Import media…</Btn>
				</div>
				<div style="display:flex;justify-content:center;flex-wrap:wrap;gap:4px;margin-top:8px">
					<Btn variant="ghost" size="sm" icon="mic" onclick={() => ui.openVoiceover()}>or write a voiceover…</Btn>
					<Btn variant="ghost" size="sm" icon="music" onclick={() => ui.openCompose()}>or compose music…</Btn>
				</div>
			{/if}
		{:else}
			<!-- asset list; the count and the import actions are the library's header -->
			{#if mediaStatus.missingList.length > 0}
				<!-- Files the project expects and the disk does not have: said once, over the list, with the way out. -->
				<div
					role="alert"
					data-testid="missing-banner"
					style="margin-bottom:8px;padding:8px 10px;border-radius:var(--radius-sm);background:color-mix(in srgb,var(--danger) 14%,transparent);border:var(--line-width) solid color-mix(in srgb,var(--danger) 40%,transparent);display:flex;align-items:center;gap:8px;font-size:12px;color:var(--text-primary)"
				>
					<Icon n="alert-triangle" s={13} color="var(--danger)" />
					<span style="flex:1;min-width:0">{missingLine(mediaStatus.missingList)}</span>
					<Btn size="sm" variant="secondary" onclick={() => ui.openRelink()}>Relink…</Btn>
				</div>
			{/if}
			{#if shown.length === 0}
				<div
					data-testid="tag-filter-empty"
					style="padding:20px 12px;text-align:center;font-size:12px;line-height:1.5;color:var(--text-muted)"
				>
					No asset has {ui.tagFilter.length === 1 ? 'that tag' : 'all of those tags'}.
					<div style="margin-top:8px"><Btn variant="secondary" size="sm" onclick={() => ui.setTagFilter([])}>Clear the filter</Btn></div>
				</div>
			{/if}
			<div style="display:flex;flex-direction:column;gap:8px">
				{#each shown as a (a.asset.id)}
					{@const sel = editor.selectedAssetSet.has(a.asset.id)}
					{@const chips = rowChips(a.asset.tags)}
					{@const spec = specLine(a.info)}
					<div
						role="button"
						tabindex="0"
						aria-pressed={sel}
						onpointerdown={(e) => onAssetPointerDown(e, a)}
						oncontextmenu={(e) => onAssetContextMenu(e, a)}
						onclick={(e) => onSelect(e, a.asset.id)}
						onkeydown={(e) => {
							if (e.key === 'Enter' || e.key === ' ') {
								e.preventDefault();
								onSelect(e, a.asset.id);
							}
						}}
						use:tip={`${a.asset.path}
Drag onto a timeline track to add a clip · ${settings.modLabel('primary')}-click or ${settings.modLabel('shift')}-click to select several · right-click for actions`}
						style="display:flex;flex-direction:column;gap:6px;padding:7px;border-radius:var(--radius-sm);background:{sel ? 'var(--surface-hover)' : 'var(--surface-raised)'};border:var(--line-width) solid {sel ? 'var(--kerf-500)' : 'var(--border-subtle)'};cursor:grab"
					>
						<div style="display:flex;gap:9px;align-items:center">
							<div
								style="width:56px;height:36px;border-radius:var(--radius-sm);flex:none;overflow:hidden;background:{a.info
									.kind === 'audio'
									? 'var(--track-audio)'
									: VIDEO_THUMB_BG};display:grid;place-items:center;color:color-mix(in srgb,var(--text-on-video) 80%,transparent)"
							>
								{#if thumbs[a.asset.id]}
									<img
										src={thumbs[a.asset.id]}
										alt=""
										draggable={false}
										style="width:100%;height:100%;object-fit:cover;display:block"
									/>
								{:else}
									<Icon
										n={a.info.kind === 'image' ? 'image' : a.info.kind === 'audio' ? 'audio-waveform' : 'video'}
										s={14}
									/>
								{/if}
							</div>
							<div style="flex:1;min-width:0">
								<div style="display:flex;align-items:baseline;gap:8px">
									<span
										style="flex:1;min-width:0;font-size:12px;font-weight:500;color:var(--text-primary);white-space:nowrap;overflow:hidden;text-overflow:ellipsis"
										>{a.asset.name}</span
									>
									<span
										style="flex:none;font-family:var(--font-mono);font-size:11px;font-variant-numeric:tabular-nums;color:{sel ? 'var(--text-secondary)' : 'var(--text-muted)'}"
										>{a.info.duration}</span
									>
								</div>
								{#if spec}
									<div
										style="font-size:11px;color:{sel ? 'var(--text-secondary)' : 'var(--text-muted)'};margin-top:2px;line-height:1.4;overflow-wrap:anywhere"
										use:tip={spec}
									>
										{spec}
									</div>
								{/if}
							</div>
						</div>
						<div style="display:flex;gap:5px;align-items:center;flex-wrap:wrap">
							<MediaChips assetId={a.asset.id} compact />
							{#if a.info.projection}
								<Badge tone="kerf">360</Badge>
							{/if}
							{#if a.info.voiceover}
								<Badge tone="kerf">voiceover</Badge>
							{/if}
							{#if a.info.composed}
								<Badge tone="kerf">composed</Badge>
							{/if}
							{#if a.info.kind === 'image'}
								<Badge tone="neutral">still</Badge>
							{/if}
							{#if mediaStatus.isMissing(a.asset.id)}
								<span use:tip={`The file is not at ${a.asset.path}. Right-click to relink it.`}>
									<Badge tone="danger">missing</Badge>
								</span>
							{/if}
							{#if a.info.uses > 0}
								<span use:tip={`Used in ${a.info.uses === 1 ? '1 clip' : `${a.info.uses} clips`} on the timeline`}>
									<Badge tone="neutral" style="font-family:var(--font-mono)">×{a.info.uses}</Badge>
								</span>
							{/if}
						</div>
						{#if chips.shown.length > 0}
							<div data-testid="row-tags" style="display:flex;gap:4px;align-items:center;flex-wrap:wrap;min-width:0">
								{#each chips.shown as tag (JSON.stringify(tag))}
									<TagChip {tag} compact />
								{/each}
								{#if chips.hidden > 0}
									<span
										data-testid="row-tags-more"
										use:tip={{ text: `${chips.shown.length + chips.hidden} tags`, detail: chips.all }}
										style="display:inline-flex;align-items:center;height:17px;padding:0 5px;border-radius:var(--radius-sm);font-size:10px;line-height:1;color:var(--text-muted);border:var(--line-width) dashed var(--border-default)"
										>+{chips.hidden}</span
									>
								{/if}
							</div>
						{/if}
						{#if ui.analyzing && ui.analyzingId === a.asset.id}
							<!-- What the run is on and how to stop it, on the card the run is for; a press here is Stop's, not the card's. -->
							<div role="presentation" onclick={(e) => e.stopPropagation()} onkeydown={(e) => e.stopPropagation()}>
								<AnalysisRun compact />
							</div>
						{/if}
					</div>
				{/each}
			</div>
		{/if}
	</div>
</div>
