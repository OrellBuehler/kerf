<script lang="ts">
	import Icon from './Icon.svelte';
	import Badge from './Badge.svelte';
	import Btn from './Btn.svelte';
	import IconBtn from './IconBtn.svelte';
	import { ui } from '$lib/editor-ui.svelte';
	import { editor } from '$lib/state.svelte';
	import { inTauri } from '$lib/api';
	import { toast } from 'svelte-sonner';

	type BinAsset = { id: string; name: string; dur: string; kind: 'video' | 'audio'; tag: string };

	let tab = $state<'bin' | 'tx'>('bin');

	const loaded = $derived(editor.assets.length > 0);

	function fmt(s: number): string {
		const m = Math.floor(s / 60);
		const sec = Math.floor(s % 60);
		return `${m.toString().padStart(2, '0')}:${sec.toString().padStart(2, '0')}`;
	}

	const assets = $derived<BinAsset[]>(
		editor.assets.map((a) => {
			const hasVideo = a.streams.some((s) => s.kind === 'video');
			const primary = a.streams[0];
			return {
				id: a.id,
				name: a.name,
				dur: fmt(a.duration),
				kind: hasVideo ? 'video' : 'audio',
				tag: primary?.codec ?? (hasVideo ? 'video' : 'audio')
			};
		})
	);

	const txLines = $derived(
		(editor.selectedMetadata?.analysis?.transcript ?? []).map((seg) => ({
			t: fmt(seg.start),
			s: seg.text
		}))
	);

	const tabs = $derived([
		{ id: 'bin' as const, label: 'Media', count: assets.length || undefined },
		{ id: 'tx' as const, label: 'Transcript', count: txLines.length || undefined }
	]);

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
				for (const a of imported) await ui.runAnalysis(a.id);
				toast.success('Analysis complete');
			}
		} catch (e) {
			toast.error(e instanceof Error ? e.message : String(e));
		}
	}

	function onSelect(assetId: string) {
		void editor.select(assetId);
	}

	// Drag an asset out of the bin; the timeline lanes accept the drop and add a
	// clip. `ui.dndAsset` carries the payload (dataTransfer is opaque on dragover).
	function onAssetDragStart(e: DragEvent, a: BinAsset) {
		const asset = editor.assets.find((x) => x.id === a.id);
		if (!asset) return;
		ui.dndAsset = { id: a.id, kind: a.kind, duration: asset.duration };
		if (e.dataTransfer) {
			e.dataTransfer.effectAllowed = 'copy';
			// A custom MIME so an off-target drop on a text field doesn't paste the
			// name; the drop is coordinated entirely via `ui.dndAsset`.
			e.dataTransfer.setData('application/x-kerf-asset', a.id);
		}
	}

	function onAssetDragEnd() {
		ui.dndAsset = null;
	}
</script>

<div
	style="width:var(--sidebar-w);flex:none;background:var(--surface-panel);border-right:1px solid var(--border-default);display:flex;flex-direction:column;overflow:hidden"
>
	<!-- tab bar -->
	<div style="display:flex;border-bottom:1px solid var(--border-default);flex:none;padding:0 6px">
		{#each tabs as t (t.id)}
			{@const on = t.id === tab}
			<button
				onclick={() => (tab = t.id)}
				style="position:relative;display:inline-flex;align-items:center;gap:6px;padding:10px;background:none;border:none;cursor:pointer;font-family:var(--font-sans);font-size:13px;font-weight:500;color:{on
					? 'var(--text-primary)'
					: 'var(--text-muted)'}"
			>
				{t.label}
				{#if t.count != null}
					<span
						style="font-family:var(--font-mono);font-size:10px;color:{on
							? 'var(--kerf-400)'
							: 'var(--text-disabled)'}">{t.count}</span
					>
				{/if}
				<span
					style="position:absolute;left:4px;right:4px;bottom:-1px;height:2px;background:{on
						? 'var(--kerf-500)'
						: 'transparent'}"
				></span>
			</button>
		{/each}
	</div>

	<div style="flex:1;overflow-y:auto;padding:12px">
		{#if tab === 'bin'}
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
								Probing streams locally
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
							<span class="kerf-spin" style="color:var(--kerf-400)"><Icon n="loader" s={14} /></span>
						{:else}
							<IconBtn title="Import" size={24} onclick={onImport}><Icon n="plus" s={14} /></IconBtn>
						{/if}
					</div>
					{#each assets as a (a.id)}
						{@const sel = a.id === editor.selectedAssetId}
						<div
							role="button"
							tabindex="0"
							draggable={true}
							ondragstart={(e) => onAssetDragStart(e, a)}
							ondragend={onAssetDragEnd}
							onclick={() => onSelect(a.id)}
							onkeydown={(e) => e.key === 'Enter' && onSelect(a.id)}
							title="Drag onto a timeline track to add a clip"
							style="display:flex;gap:9px;align-items:center;padding:7px;border-radius:var(--radius-sm);background:{sel ? 'var(--surface-hover)' : 'var(--surface-raised)'};border:1px solid {sel ? 'var(--kerf-500)' : 'var(--border-subtle)'};cursor:grab"
						>
							<div
								style="width:46px;height:30px;border-radius:3px;flex:none;background:{a.kind ===
								'audio'
									? 'var(--track-audio)'
									: 'linear-gradient(135deg,#22303f,#33424f)'};display:grid;place-items:center;color:rgba(255,255,255,.8)"
							>
								<Icon n={a.kind === 'audio' ? 'audio-waveform' : 'video'} s={14} />
							</div>
							<div style="flex:1;min-width:0">
								<div
									style="font-size:12px;font-weight:500;color:var(--text-primary);white-space:nowrap;overflow:hidden;text-overflow:ellipsis"
								>
									{a.name}
								</div>
								<div style="display:flex;gap:6px;align-items:center;margin-top:3px">
									<span style="font-family:var(--font-mono);font-size:10px;color:var(--text-muted)">{a.dur}</span>
									{#if ui.analyzingId === a.id}
										<Badge tone="agent" dot>analyzing</Badge>
									{:else}
										<Badge tone="neutral">{a.tag}</Badge>
									{/if}
								</div>
							</div>
						</div>
					{/each}
				</div>
			{/if}
		{:else if tab === 'tx'}
			{#if txLines.length === 0}
				<div
					style="display:flex;flex-direction:column;align-items:center;gap:10px;padding:40px 16px;color:var(--text-disabled);text-align:center"
				>
					<Icon n="captions" s={22} /><span style="font-size:12px">Transcript appears after analysis</span>
				</div>
			{:else}
				<div data-selectable style="display:flex;flex-direction:column;gap:2px">
					{#each txLines as l, i (i)}
						<div style="display:flex;gap:8px;padding:7px 8px;border-radius:var(--radius-sm)">
							<span
								style="font-family:var(--font-mono);font-size:10px;color:var(--text-disabled);flex:none;padding-top:1px"
								>{l.t}</span
							>
							<span style="font-size:12px;line-height:1.45;color:var(--text-secondary)">{l.s}</span>
						</div>
					{/each}
				</div>
			{/if}
		{/if}
	</div>
</div>
