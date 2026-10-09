<script lang="ts">
	import Icon from './Icon.svelte';
	import { ui } from '$lib/editor-ui.svelte';
	import { editor } from '$lib/state.svelte';
	import { gpuPreview } from '$lib/gpu-preview.svelte';
	import { settings } from '$lib/settings.svelte';
	import { mediaStatus } from '$lib/media-status.svelte';
	import { previewSourceNote } from '$lib/proxy-info';
	import { inTauri, revealLogs } from '$lib/api';
	import { toast } from '$lib/notifications.svelte';

	const showLogs = inTauri();

	/** Real metadata of the selected asset: fps · resolution · codec. */
	const meta = $derived.by(() => {
		const a = editor.selectedAsset;
		if (!a) return null;
		const v = a.streams.find((s) => s.kind === 'video');
		const parts: string[] = [];
		if (v?.fps) parts.push(`${v.fps.toFixed(3)} fps`);
		if (v?.width && v?.height) parts.push(`${v.width}×${v.height}`);
		const codec = (v ?? a.streams[0])?.codec;
		if (codec) parts.push(codec);
		return parts.join(' · ') || null;
	});

	const clipCount = $derived(
		editor.timeline.tracks.reduce((n, t) => n + t.clips.length, 0)
	);

	/** Which file the frame under the playhead comes from — the proxy or the original, and why. */
	const sourceNote = $derived(
		previewSourceNote(
			editor.timeline,
			editor.assets,
			mediaStatus.proxies,
			settings.previewSource,
			settings.proxySize,
			ui.time
		)
	);

	function tc(s: number): string {
		const total = Math.max(0, s);
		const h = Math.floor(total / 3600);
		const m = Math.floor((total % 3600) / 60);
		const sec = Math.floor(total % 60);
		const mm = `${m.toString().padStart(2, '0')}:${sec.toString().padStart(2, '0')}`;
		return h > 0 ? `${h}:${mm}` : mm;
	}
</script>

<div
	style="height:var(--statusbar-h);flex:none;display:flex;align-items:center;gap:12px;padding:0 12px;background:var(--surface-app);border-top:var(--line-width) solid var(--border-default)"
>
	{#if meta}
		<span style="font-family:var(--font-mono);font-size:10px;color:var(--text-disabled)">{meta}</span>
	{/if}
	<span style="font-family:var(--font-mono);font-size:10px;color:var(--text-disabled)">
		{tc(editor.duration)}
	</span>
	<div style="flex:1"></div>
	{#if sourceNote}
		<!-- Which file this frame was decoded from: the preview proxy, or the original and why. -->
		<span
			data-testid="preview-source"
			data-source={sourceNote.tone}
			title={sourceNote.title}
			style="max-width:46%;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;font-family:var(--font-mono);font-size:10px;color:{sourceNote.tone === 'proxy'
				? 'var(--kerf-300)'
				: sourceNote.tone === 'waiting'
					? 'var(--orange-400)'
					: 'var(--text-muted)'}"
		>
			{sourceNote.text}
		</span>
		<span style="width:1px;height:12px;background:var(--border-default)"></span>
	{/if}
	{#if settings.gpuPreview && gpuPreview.label}
		<!-- Which renderer made the frame on screen (a dev aid; only with the GPU preview on). -->
		<span
			data-testid="preview-renderer"
			title={gpuPreview.reasons.length ? gpuPreview.reasons.join('\n') : 'Drawn by the GPU compositor'}
			style="max-width:46%;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;font-family:var(--font-mono);font-size:10px;color:{gpuPreview.renderer === 'gpu' ? 'var(--kerf-300)' : 'var(--text-disabled)'}"
		>
			{gpuPreview.label}
		</span>
		<span style="width:1px;height:12px;background:var(--border-default)"></span>
	{/if}
	{#if editor.loading}
		<span style="display:inline-flex;align-items:center;gap:6px;font-size:10px;color:var(--text-muted)">
			<span class="kerf-spin" style="width:9px;height:9px;border:1.5px solid var(--text-muted);border-top-color:transparent;border-radius:50%"></span>
			Loading project…
		</span>
	{:else if editor.exportRun}
		<!-- A render keeps going after its dialog is closed; this is where it stays
		     visible and stoppable. -->
		<span style="display:inline-flex;align-items:center;gap:6px;font-size:10px;color:var(--kerf-300)">
			<span class="kerf-spin" style="width:9px;height:9px;border:1.5px solid var(--kerf-400);border-top-color:transparent;border-radius:50%"></span>
			{#if editor.exportRun.progress?.waiting}Export {editor.exportRun.progress.waiting}…{:else}Exporting {Math.round(
					(editor.exportRun.progress?.fraction ?? 0) * 100
				)}%{/if}
		</span>
		<button
			type="button"
			disabled={editor.exportRun.cancelling}
			title="Stop the export and delete the partial file"
			onclick={() => void editor.stopExport()}
			style="background:none;border:var(--line-width) solid var(--border-strong);border-radius:var(--radius-sm);cursor:pointer;color:var(--text-secondary);font-size:10px;padding:1px 6px"
		>
			{editor.exportRun.cancelling ? 'Stopping…' : 'Stop'}
		</button>
	{:else if ui.analyzing}
		<!-- Analysis is a long commitment (a speech model download, then minutes
		     of inference per asset), so say which step it is on, how much is
		     still queued behind it, and offer the way out. -->
		<span style="display:inline-flex;align-items:center;gap:6px;font-size:10px;color:var(--agent-300)">
			<span class="kerf-spin" style="width:9px;height:9px;border:1.5px solid var(--agent-400);border-top-color:transparent;border-radius:50%"></span>
			{ui.analysisLabel ?? 'analyzing'}{ui.analysisQueued > 0 ? ` · ${ui.analysisQueued} queued` : ''}
		</span>
		<button
			type="button"
			disabled={ui.stoppingAnalysis}
			title="Stop analyzing — it gives up between steps, and within about a second during transcription"
			onclick={() => ui.stopAnalysis()}
			style="background:none;border:var(--line-width) solid var(--border-strong);border-radius:var(--radius-sm);cursor:pointer;color:var(--text-secondary);font-size:10px;padding:1px 6px"
		>
			{ui.stoppingAnalysis ? 'Stopping…' : 'Stop'}
		</button>
	{:else}
		<span style="font-size:10px;color:var(--text-disabled)">
			{editor.assets.length} asset{editor.assets.length === 1 ? '' : 's'} · {clipCount} clip{clipCount ===
			1
				? ''
				: 's'}
		</span>
	{/if}
	<!-- Where the project lives; the title bar has its name. -->
	<span style="width:1px;height:12px;background:var(--border-default)"></span>
	<span
		title={editor.currentPath ?? 'In-memory project — not yet saved'}
		style="font-family:var(--font-mono);font-size:10px;color:var(--text-disabled);max-width:min(320px,40vw);min-width:0;white-space:nowrap;overflow:hidden;text-overflow:ellipsis"
		>{editor.currentPath ?? 'local · in-memory'}</span
	>
	{#if showLogs}
		<span style="width:1px;height:12px;background:var(--border-default)"></span>
		<button
			type="button"
			title="Open the log folder — attach kerf.<date>.log when reporting an issue"
			onclick={() =>
				revealLogs().catch((e) => toast.error(e instanceof Error ? e.message : String(e)))}
			style="display:inline-flex;align-items:center;gap:5px;background:none;border:none;cursor:pointer;color:var(--text-disabled);font-size:10px;padding:0"
		>
			<Icon n="folder-open" s={11} />
			Logs
		</button>
	{/if}
</div>
