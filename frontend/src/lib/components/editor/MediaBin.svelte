<script lang="ts">
	import { untrack } from 'svelte';
	import Icon from './Icon.svelte';
	import { VIDEO_THUMB_BG } from './data';
	import Badge from './Badge.svelte';
	import MediaChips from './MediaChips.svelte';
	import Btn from './Btn.svelte';
	import { ui } from '$lib/editor-ui.svelte';
	import { editor } from '$lib/state.svelte';
	import { mediaStatus } from '$lib/media-status.svelte';
	import { analyzeSubmenuItem } from '$lib/analyze-menu';
		import { proxyActions } from '$lib/proxy-info';
	import { deleteProxy, rebuildProxy } from '$lib/api';
	import { contextMenu } from '$lib/context-menu.svelte';
	import type { MenuItem } from '$lib/context-menu.svelte';
	import { getFrame, inTauri, revealPath } from '$lib/api';
	import { importMedia } from '$lib/import-media';
	import { toast } from '$lib/notifications.svelte';
	import { thumbnails } from '$lib/thumbnails';
	import { audioExtraction, mediaInfo, shortPath, specLine, thumbTime, type MediaInfo } from '$lib/media-info';
	import type { Asset } from '$lib/types';
	import { tip } from '$lib/tip';

	type BinAsset = { asset: Asset; info: MediaInfo };

	const loaded = $derived(editor.assets.length > 0);

	const assets = $derived<BinAsset[]>(
		editor.assets.map((a) => ({ asset: a, info: mediaInfo(a, editor.timeline) }))
	);

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

	function onSelect(assetId: string) {
		void editor.select(assetId);
	}

	function err(e: unknown) {
		toast.error(e instanceof Error ? e.message : String(e));
	}

	// Drag an asset out of the bin; the timeline lanes accept the drop and add a
	// clip. `ui.dndAsset` carries the payload (dataTransfer is opaque on dragover).
	function onAssetDragStart(e: DragEvent, { asset, info }: BinAsset) {
		ui.dndAsset = { id: asset.id, kind: info.kind === 'audio' ? 'audio' : 'video', duration: asset.duration };
		if (e.dataTransfer) {
			e.dataTransfer.effectAllowed = 'copy';
			// A custom MIME so an off-target drop on a text field doesn't paste the
			// name; the drop is coordinated entirely via `ui.dndAsset`.
			e.dataTransfer.setData('application/x-kerf-asset', asset.id);
		}
	}

	function onAssetDragEnd() {
		ui.dndAsset = null;
	}

	function onAssetContextMenu(e: MouseEvent, { asset, info }: BinAsset) {
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
					.add(asset.id, 0, asset.duration, undefined, ui.time)
					.then(() => toast.success(`Added ${asset.name}`))
					.catch(err)
		});
		items.push({
			label: 'Append to timeline',
			icon: 'list-plus',
			action: () =>
				void editor
					.add(asset.id, 0, asset.duration, undefined, editor.duration)
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

		items.push({ type: 'separator' });
		if (asset.voiceover) {
			const vo = asset.voiceover;
			items.push({
				label: 'Regenerate voiceover…',
				icon: 'mic',
				action: () => ui.openVoiceover({ text: vo.text, voice: vo.voice, speed: vo.speed })
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
				<div style="display:flex;justify-content:center;margin-top:8px">
					<Btn variant="ghost" size="sm" icon="mic" onclick={() => ui.openVoiceover()}>or write a voiceover…</Btn>
				</div>
			{/if}
		{:else}
			<!-- asset list; the count and the import actions are the library's header -->
			<div style="display:flex;flex-direction:column;gap:8px">
				{#each assets as a (a.asset.id)}
					{@const sel = a.asset.id === editor.selectedAssetId}
					{@const spec = specLine(a.info)}
					<div
						role="button"
						tabindex="0"
						aria-pressed={sel}
						draggable={true}
						ondragstart={(e) => onAssetDragStart(e, a)}
						ondragend={onAssetDragEnd}
						oncontextmenu={(e) => onAssetContextMenu(e, a)}
						onclick={() => onSelect(a.asset.id)}
						onkeydown={(e) => {
							if (e.key === 'Enter' || e.key === ' ') {
								e.preventDefault();
								onSelect(a.asset.id);
							}
						}}
						use:tip={`${a.asset.path}
Drag onto a timeline track to add a clip · right-click for details`}
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
							{#if a.info.kind === 'image'}
								<Badge tone="neutral">still</Badge>
							{/if}
							{#if a.info.uses > 0}
								<span use:tip={`Used in ${a.info.uses === 1 ? '1 clip' : `${a.info.uses} clips`} on the timeline`}>
									<Badge tone="neutral" style="font-family:var(--font-mono)">×{a.info.uses}</Badge>
								</span>
							{/if}
						</div>
					</div>
				{/each}
			</div>
		{/if}
	</div>
</div>
