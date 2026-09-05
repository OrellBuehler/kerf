<script lang="ts">
	import Icon from './Icon.svelte';
	import { VIDEO_THUMB_BG } from './data';
	import Badge from './Badge.svelte';
	import Btn from './Btn.svelte';
	import IconBtn from './IconBtn.svelte';
	import { ui } from '$lib/editor-ui.svelte';
	import { editor } from '$lib/state.svelte';
	import { contextMenu } from '$lib/context-menu.svelte';
	import type { MenuItem } from '$lib/context-menu.svelte';
	import { getFrame, inTauri, revealPath } from '$lib/api';
	import { toast } from '$lib/notifications.svelte';
	import { analysisFacts, mediaInfo, shortPath, specLine, thumbTime, type MediaInfo } from '$lib/media-info';
	import type { Asset } from '$lib/types';

	type BinAsset = { asset: Asset; info: MediaInfo };

	const loaded = $derived(editor.assets.length > 0);

	const assets = $derived<BinAsset[]>(
		editor.assets.map((a) => ({ asset: a, info: mediaInfo(a, editor.timeline) }))
	);

	// One decoded frame per asset, so a row shows the footage rather than an icon.
	// Cached across mounts (the panel is dockable, and a re-dock would otherwise
	// re-decode every asset); `null` in the browser harness, where the icon stays.
	const thumbCache = new Map<string, string | null>();
	const requested = new Set<string>();
	let thumbs = $state<Record<string, string>>({});

	$effect(() => {
		for (const { asset, info } of assets) {
			if (requested.has(asset.id)) continue;
			requested.add(asset.id);
			const cached = thumbCache.get(asset.id);
			if (cached !== undefined) {
				if (cached) thumbs[asset.id] = cached;
				continue;
			}
			if (info.kind === 'audio') {
				thumbCache.set(asset.id, null);
				continue;
			}
			void getFrame(asset.id, thumbTime(asset.duration), 160, false)
				.then((url) => {
					thumbCache.set(asset.id, url);
					if (url) thumbs[asset.id] = url;
				})
				.catch(() => thumbCache.set(asset.id, null));
		}
	});

	async function onImport() {
		if (!inTauri()) {
			toast.info('Importing media is available in the desktop app.');
			return;
		}
		try {
			const { imported, failed } = await editor.importMedia();
			for (const f of failed) toast.error(`Couldn't import ${f.name}: ${f.message}`);
			if (imported.length > 0) {
				toast.success(
					imported.length === 1 ? `Imported ${imported[0].name}` : `Imported ${imported.length} files`
				);
				await ui.analyzeQueue(imported.map((a) => a.id));
			}
		} catch (e) {
			toast.error(e instanceof Error ? e.message : String(e));
		}
	}

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
		const analyzing = ui.analyzingId === asset.id;
		const spherical = !!info.projection;
		const facts = analysisFacts(analysis, asset.duration);
		const imported = new Date(asset.imported_at);

		const items: MenuItem[] = [{ type: 'header', label: asset.name, sub: shortPath(asset.path) }];

		items.push({ type: 'info', label: 'Duration', value: info.duration });
		if (info.resolution)
			items.push({
				type: 'info',
				label: 'Frame',
				value: info.aspect ? `${info.resolution} · ${info.aspect}` : info.resolution
			});
		if (info.fps) items.push({ type: 'info', label: 'Rate', value: info.fps });
		if (info.codec) items.push({ type: 'info', label: 'Codec', value: info.codec });
		if (info.audio) items.push({ type: 'info', label: 'Audio', value: info.audio });
		if (info.projection) items.push({ type: 'info', label: 'Projection', value: info.projection });
		if (info.stitched)
			items.push({
				type: 'info',
				label: 'Stitched',
				value: `${asset.source_paths?.length ?? 0} lens files`,
				title: asset.source_paths?.join('\n')
			});
		items.push({ type: 'info', label: 'Used in', value: info.uses === 1 ? '1 clip' : `${info.uses} clips` });
		if (!Number.isNaN(imported.getTime()))
			items.push({ type: 'info', label: 'Imported', value: imported.toLocaleDateString() });

		items.push({ type: 'separator' });
		if (facts.length) for (const f of facts) items.push({ type: 'info', label: f.label, value: f.value });
		else items.push({ type: 'info', label: 'Analysis', value: analyzing ? 'running…' : 'not analyzed' });

		items.push({ type: 'separator' });
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
		if (info.kind === 'video' && info.audio)
			items.push({
				label: 'Extract audio to a track',
				icon: 'audio-waveform',
				action: () => void editor.extractAudio(asset.id).catch(err)
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

		items.push({ type: 'separator' });
		items.push(
			analyzing
				? { label: 'Stop analysis', icon: 'x', action: () => ui.stopAnalysis() }
				: {
						label: analysis ? 'Re-analyze' : 'Analyze',
						icon: 'scan-line',
						action: () => void ui.runAnalysis(asset.id).catch(err)
					}
		);
		if (info.kind === 'video')
			items.push({
				label: spherical ? 'Mark as flat footage' : 'Mark as 360 (equirect)',
				icon: 'rotate-ccw',
				action: () =>
					void editor
						.setAssetProjection(asset.id, spherical ? 'flat' : 'equirect')
						.then(() => toast.success(spherical ? `${asset.name} is flat` : `${asset.name} is 360 footage`))
						.catch(err)
			});

		items.push({ type: 'separator' });
		items.push({
			label: 'Copy path',
			icon: 'copy',
			action: () =>
				void navigator.clipboard
					.writeText(asset.path)
					.then(() => toast.success('Path copied'))
					.catch(() => toast.error('Could not copy the path'))
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
					onclick={onImport}
					role="button"
					tabindex="0"
					onkeydown={(e) => e.key === 'Enter' && onImport()}
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
							Kerf transcribes & detects locally on import
						</div>
					</div>
					<Btn variant="secondary" size="sm" icon="plus">Import files</Btn>
				</div>
			{/if}
		{:else}
			<!-- asset grid -->
			<div style="display:flex;flex-direction:column;gap:8px">
				<div style="display:flex;align-items:center;justify-content:space-between;margin-bottom:2px">
					<span
						style="font:var(--type-overline);letter-spacing:var(--tracking-caps);text-transform:uppercase;color:var(--text-muted)"
						>{assets.length} assets</span
					>
					{#if editor.importing}
						<span style="display:inline-flex;align-items:center;gap:5px">
							{#if editor.importProgress !== null}
								<span
									style="font-family:var(--font-mono);font-size:10px;color:var(--text-muted)"
									title="Stitching a 360 lens pair"
									>{Math.round(editor.importProgress * 100)}%</span
								>
							{/if}
							<span class="kerf-spin" style="color:var(--kerf-400)"><Icon n="loader" s={14} /></span>
						</span>
					{:else}
						<IconBtn title="Import" size={24} onclick={onImport}><Icon n="plus" s={14} /></IconBtn>
					{/if}
				</div>
				{#each assets as a (a.asset.id)}
					{@const sel = a.asset.id === editor.selectedAssetId}
					{@const spec = specLine(a.info)}
					<div
						role="button"
						tabindex="0"
						draggable={true}
						ondragstart={(e) => onAssetDragStart(e, a)}
						ondragend={onAssetDragEnd}
						oncontextmenu={(e) => onAssetContextMenu(e, a)}
						onclick={() => onSelect(a.asset.id)}
						onkeydown={(e) => e.key === 'Enter' && onSelect(a.asset.id)}
						title="{a.asset.path}&#10;Drag onto a timeline track to add a clip · right-click for details"
						style="display:flex;gap:9px;align-items:center;padding:7px;border-radius:var(--radius-sm);background:{sel ? 'var(--surface-hover)' : 'var(--surface-raised)'};border:1px solid {sel ? 'var(--kerf-500)' : 'var(--border-subtle)'};cursor:grab"
					>
						<div
							style="width:56px;height:36px;border-radius:3px;flex:none;overflow:hidden;background:{a.info
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
							<div
								style="font-size:12px;font-weight:500;color:var(--text-primary);white-space:nowrap;overflow:hidden;text-overflow:ellipsis"
							>
								{a.asset.name}
							</div>
							{#if spec}
								<div
									style="font-size:10.5px;color:var(--text-muted);margin-top:2px;white-space:nowrap;overflow:hidden;text-overflow:ellipsis"
									title={spec}
								>
									{spec}
								</div>
							{/if}
							<div style="display:flex;gap:5px;align-items:center;margin-top:4px;flex-wrap:wrap">
								<span style="font-family:var(--font-mono);font-size:10px;color:var(--text-muted)"
									>{a.info.duration}</span
								>
								{#if a.info.projection}
									<Badge tone="kerf">360</Badge>
								{/if}
								{#if a.info.kind === 'image'}
									<Badge tone="neutral">still</Badge>
								{/if}
								{#if a.info.uses > 0}
									<Badge tone="neutral" style="font-family:var(--font-mono)">×{a.info.uses}</Badge>
								{/if}
								{#if ui.analyzingId === a.asset.id}
									<Badge tone="agent" dot>analyzing</Badge>
								{:else if editor.analysisFor(a.asset.id)}
									<Badge tone="success" dot>analyzed</Badge>
								{/if}
							</div>
						</div>
					</div>
				{/each}
			</div>
		{/if}
	</div>
</div>
