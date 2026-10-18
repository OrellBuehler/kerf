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
	import { tip } from '$lib/tip';
	import { timeDisplay } from '$lib/time-display.svelte';
	import { assetLine, projectFrame, projectLine } from '$lib/status-facts';

	const showLogs = inTauri();

	/** The project: the rate and frame the cut plays and exports at. */
	const project = $derived(projectLine(editor.fps, projectFrame(editor.liveTimeline, editor.assets)));

	/** The library asset that is selected, named — its own rate and size are not the project's. */
	const picked = $derived.by(() => {
		const a = editor.selectedAsset;
		const line = a ? assetLine(a) : null;
		return a && line ? { name: a.name, line } : null;
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
</script>

<div
	style="height:var(--statusbar-h);flex:none;display:flex;align-items:center;gap:12px;padding:0 12px;background:var(--surface-app);border-top:var(--line-width) solid var(--border-default);font-variant-numeric:tabular-nums"
>
	<span
		data-status-project
		use:tip={{ text: 'The project: the frame rate and size the cut plays and exports at', detail: 'Delivery frame, else the first picture on the cut.' }}
		style="font-size:var(--text-xs);color:var(--text-muted)">{project}</span
	>
	<span use:tip={'Length of the cut'} style="font-size:var(--text-xs);color:var(--text-muted)">Cut {timeDisplay.fmt(editor.duration)}</span>
	{#if picked}
		<span style="width:1px;height:12px;background:var(--border-default)"></span>
		<span
			data-status-selected
			use:tip={{ text: `Selected in the library: ${picked.name}`, detail: 'Its own frame rate, size and codec — not the project’s.' }}
			style="min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;font-size:var(--text-xs);color:var(--text-muted)"
			>Selected {picked.name} · {picked.line}</span
		>
	{/if}
	<div style="flex:1"></div>
	{#if sourceNote}
		<!-- Which file this frame was decoded from: the preview proxy, or the original and why. -->
		<span
			data-testid="preview-source"
			data-source={sourceNote.tone}
			use:tip={sourceNote.title}
			style="max-width:46%;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;font-size:var(--text-xs);color:{sourceNote.tone === 'proxy'
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
			use:tip={gpuPreview.reasons.length ? gpuPreview.reasons.join('\n') : 'Drawn by the GPU compositor'}
			style="max-width:46%;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;font-size:var(--text-xs);color:{gpuPreview.renderer === 'gpu' ? 'var(--kerf-300)' : 'var(--text-muted)'}"
		>
			{gpuPreview.label}
		</span>
		<span style="width:1px;height:12px;background:var(--border-default)"></span>
	{/if}
	{#if editor.loading}
		<span style="display:inline-flex;align-items:center;gap:6px;font-size:var(--text-xs);color:var(--text-muted)">
			<span class="kerf-spin" style="width:9px;height:9px;border:1.5px solid var(--text-muted);border-top-color:transparent;border-radius:50%"></span>
			Loading project…
		</span>
	{:else if editor.exportRun}
		<!-- A render keeps going after its dialog is closed; this is where it stays
		     visible and stoppable. -->
		<span style="display:inline-flex;align-items:center;gap:6px;font-size:var(--text-xs);color:var(--kerf-300)">
			<span class="kerf-spin" style="width:9px;height:9px;border:1.5px solid var(--kerf-400);border-top-color:transparent;border-radius:50%"></span>
			{#if editor.exportRun.progress?.waiting}Export {editor.exportRun.progress.waiting}…{:else}Exporting {Math.round(
					(editor.exportRun.progress?.fraction ?? 0) * 100
				)}%{/if}
		</span>
		<button
			type="button"
			disabled={editor.exportRun.cancelling}
			use:tip={'Stop the export and delete the partial file'}
			onclick={() => void editor.stopExport()}
			style="background:none;border:var(--line-width) solid var(--border-strong);border-radius:var(--radius-sm);cursor:pointer;color:var(--text-secondary);font-size:var(--text-xs);height:24px;padding:0 8px"
		>
			{editor.exportRun.cancelling ? 'Stopping…' : 'Stop'}
		</button>
	{:else if ui.analyzing}
		<!-- Analysis is a long commitment (a speech model download, then minutes
		     of inference per asset), so say which step it is on, how much is
		     still queued behind it, and offer the way out. -->
		<span style="display:inline-flex;align-items:center;gap:6px;font-size:var(--text-xs);color:var(--agent-300)">
			<span class="kerf-spin" style="width:9px;height:9px;border:1.5px solid var(--agent-400);border-top-color:transparent;border-radius:50%"></span>
			{ui.analysisLabel ?? 'analyzing'}{ui.analysisQueued > 0 ? ` · ${ui.analysisQueued} queued` : ''}
		</span>
		<button
			type="button"
			disabled={ui.stoppingAnalysis}
			use:tip={'Stop analyzing — it gives up between steps, and within about a second during transcription'}
			onclick={() => ui.stopAnalysis()}
			style="background:none;border:var(--line-width) solid var(--border-strong);border-radius:var(--radius-sm);cursor:pointer;color:var(--text-secondary);font-size:var(--text-xs);height:24px;padding:0 8px"
		>
			{ui.stoppingAnalysis ? 'Stopping…' : 'Stop'}
		</button>
	{:else}
		<span style="font-size:var(--text-xs);color:var(--text-muted)">
			{editor.assets.length} asset{editor.assets.length === 1 ? '' : 's'} · {clipCount} clip{clipCount ===
			1
				? ''
				: 's'}
		</span>
	{/if}
	<!-- Where the project lives; the title bar has its name. -->
	<span style="width:1px;height:12px;background:var(--border-default)"></span>
	<span
		use:tip={editor.currentPath ?? 'In-memory project — not yet saved'}
		style="font-family:var(--font-mono);font-size:var(--text-xs);color:var(--text-muted);max-width:min(320px,40vw);min-width:0;white-space:nowrap;overflow:hidden;text-overflow:ellipsis"
		>{editor.currentPath ?? 'local · in-memory'}</span
	>
	{#if showLogs}
		<span style="width:1px;height:12px;background:var(--border-default)"></span>
		<button
			type="button"
			use:tip={'Show log folder\nAttach kerf.<date>.log when reporting an issue.'}
			onclick={() =>
				revealLogs().catch((e) => toast.error(e instanceof Error ? e.message : String(e)))}
			style="display:inline-flex;align-items:center;gap:5px;background:none;border:none;cursor:pointer;color:var(--text-muted);font-size:var(--text-xs);height:24px;padding:0 6px"
		>
			<Icon n="folder-open" s={11} />
			Logs
		</button>
	{/if}
</div>
