<script lang="ts">
	import Icon from './Icon.svelte';
	import Badge from './Badge.svelte';
	import Btn from './Btn.svelte';
	import IconBtn from './IconBtn.svelte';
	import { ui } from '$lib/editor-ui.svelte';
	import { editor } from '$lib/state.svelte';
	import { inTauri } from '$lib/api';
	import { toast } from 'svelte-sonner';
	import { MOCK_ASSETS, TRANSCRIPT, FX, type MockAsset } from './data';

	let tab = $state<'bin' | 'tx' | 'fx'>('bin');

	const loaded = $derived(ui.phase !== 'empty');

	function fmt(s: number): string {
		const m = Math.floor(s / 60);
		const sec = Math.floor(s % 60);
		return `${m.toString().padStart(2, '0')}:${sec.toString().padStart(2, '0')}`;
	}

	/* Real assets when present (import works in the desktop app); otherwise the
	   design's mock bin so the browser preview always has content. */
	const assets = $derived<MockAsset[]>(
		editor.assets.length
			? editor.assets.map((a) => {
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
			: MOCK_ASSETS
	);

	const txLines = $derived(
		editor.selectedMetadata?.analysis?.transcript?.length
			? editor.selectedMetadata.analysis.transcript.map((seg) => ({
					t: fmt(seg.start),
					s: seg.text,
					cut: false,
					sil: false
				}))
			: TRANSCRIPT
	);

	const tabs = [
		{ id: 'bin' as const, label: 'Media', count: undefined as number | undefined },
		{ id: 'tx' as const, label: 'Transcript', count: 412 },
		{ id: 'fx' as const, label: 'Effects', count: undefined }
	];

	async function onImport() {
		if (inTauri()) {
			try {
				const asset = await editor.importMedia();
				if (asset) toast.success(`Imported ${asset.name}`);
			} catch (e) {
				toast.error(e instanceof Error ? e.message : String(e));
			}
		} else {
			toast.info('Kerf transcribes & detects locally on import (desktop app).');
		}
		ui.startAnalyze();
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
			{:else}
				<!-- asset grid -->
				<div style="display:flex;flex-direction:column;gap:8px">
					<div style="display:flex;align-items:center;justify-content:space-between;margin-bottom:2px">
						<span
							style="font:var(--type-overline);letter-spacing:var(--tracking-caps);text-transform:uppercase;color:var(--text-muted)"
							>{assets.length} assets</span
						>
						<IconBtn title="Import" size={24} onclick={onImport}><Icon n="plus" s={14} /></IconBtn>
					</div>
					{#each assets as a, i (a.id)}
						<div
							style="display:flex;gap:9px;align-items:center;padding:7px;border-radius:var(--radius-sm);background:var(--surface-raised);border:1px solid var(--border-subtle)"
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
									{#if ui.phase === 'analyzing' && i < 3}
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
			{#if !loaded}
				<div
					style="display:flex;flex-direction:column;align-items:center;gap:10px;padding:40px 16px;color:var(--text-disabled);text-align:center"
				>
					<Icon n="captions" s={22} /><span style="font-size:12px">Transcript appears after analysis</span>
				</div>
			{:else}
				<div style="display:flex;flex-direction:column;gap:2px">
					{#each txLines as l, i (i)}
						<div
							style="display:flex;gap:8px;padding:7px 8px;border-radius:var(--radius-sm);background:{l.cut
								? 'var(--diff-remove-surface)'
								: 'transparent'};border:1px solid {l.cut ? 'rgba(229,84,75,.25)' : 'transparent'}"
						>
							<span
								style="font-family:var(--font-mono);font-size:10px;color:{l.cut
									? 'var(--red-400)'
									: 'var(--text-disabled)'};flex:none;padding-top:1px">{l.t}</span
							>
							<span
								style="font-size:12px;line-height:1.45;color:{l.sil
									? 'var(--text-disabled)'
									: l.cut
										? 'var(--text-muted)'
										: 'var(--text-secondary)'};text-decoration:{l.cut && !l.sil
									? 'line-through'
									: 'none'};font-style:{l.sil ? 'italic' : 'normal'}">{l.s}</span
							>
						</div>
					{/each}
				</div>
			{/if}
		{:else}
			<div style="display:flex;flex-direction:column;gap:6px">
				{#each FX as f (f)}
					<div
						style="display:flex;align-items:center;gap:8px;padding:9px 10px;border-radius:var(--radius-sm);background:var(--surface-raised);border:1px solid var(--border-subtle);font-size:12px;color:var(--text-secondary)"
					>
						<Icon n="sliders-horizontal" s={14} color="var(--text-muted)" />{f}
					</div>
				{/each}
			</div>
		{/if}
	</div>
</div>
