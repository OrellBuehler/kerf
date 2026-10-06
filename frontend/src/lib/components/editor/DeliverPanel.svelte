<script lang="ts">
	// The Deliver workspace's panel: where the cut is going, and the shapes to
	// write it in. It is the export dialog's readiness verdict and its "Deliver
	// to" choice, docked beside the preview — the same components, reading the
	// same `ui.deliverShapes`, so a shape ticked here is ticked in the dialog.
	// The render itself still goes through the full dialog (preset, destination,
	// quality, range), which one button opens.
	import Btn from './Btn.svelte';
	import SectionHead from './SectionHead.svelte';
	import DeliverTo from './DeliverTo.svelte';
	import Readiness from './Readiness.svelte';
	import { editor } from '$lib/state.svelte';
	import { ui } from '$lib/editor-ui.svelte';
	import { formatsFor, presetFor, ratioLabel } from '$lib/delivery-formats';
	import { formatTimecode } from '$lib/timecode';

	const hasClips = $derived(editor.timeline.tracks.some((t) => t.clips.length > 0));
	const shapes = $derived(formatsFor(ui.deliverShapes));
	const frame = $derived(presetFor(editor.timeline.format));
	const rendering = $derived(editor.exportRun !== null);
	const progress = $derived(editor.exportRun?.progress ?? null);
	const clipCount = $derived(editor.timeline.tracks.reduce((n, t) => n + t.clips.length, 0));
</script>

<div style="flex:1;min-height:0;background:var(--surface-panel);display:flex;flex-direction:column;overflow:hidden">
	<div style="flex:1;overflow-y:auto;padding:4px 14px 12px">
		<SectionHead label="This cut" />
		<div style="display:flex;flex-direction:column;gap:5px;font-size:12px;color:var(--text-secondary)">
			<div style="display:flex;justify-content:space-between;gap:8px">
				<span style="color:var(--text-muted)">Length</span>
				<span style="font-family:var(--font-mono)" title="Non-drop timecode"
					>{formatTimecode(editor.duration, editor.fps)}</span
				>
			</div>
			<div style="display:flex;justify-content:space-between;gap:8px">
				<span style="color:var(--text-muted)">Frame</span>
				<span style="font-family:var(--font-mono)"
					>{#if frame.format}{frame.format.width}×{frame.format.height} · {ratioLabel(frame.format.width, frame.format.height)}{:else}follows the footage{/if}</span
				>
			</div>
			<div style="display:flex;justify-content:space-between;gap:8px">
				<span style="color:var(--text-muted)">Clips</span>
				<span style="font-family:var(--font-mono)">{clipCount}</span>
			</div>
		</div>

		{#if !hasClips}
			<div style="margin-top:14px;font-size:12px;color:var(--text-muted);line-height:1.45">
				Nothing to deliver yet — add a clip to the timeline and this panel judges the cut against each platform.
			</div>
		{/if}

		<SectionHead label="Deliver to" />
		<DeliverTo where="named by shape beside the file you choose when you export, as" />

		{#if !shapes.length}
			<Readiness />
		{/if}
	</div>

	<div
		style="flex:none;display:flex;align-items:center;gap:10px;padding:10px 14px;border-top:var(--line-width) solid var(--border-default)"
	>
		{#if rendering}
			<div style="flex:1;display:flex;align-items:center;gap:10px;min-width:0">
				<div style="flex:1;height:6px;border-radius:3px;background:var(--surface-inset);overflow:hidden">
					<div
						style="height:100%;width:{Math.round((progress?.fraction ?? 0) * 100)}%;background:var(--kerf-500);transition:width var(--dur-fast) linear"
					></div>
				</div>
				<span style="font-family:var(--font-mono);font-size:12px;color:var(--text-muted)"
					>{#if progress?.total != null && progress.total > 1}{(progress.variant ?? 0) + 1}/{progress.total} · {/if}{Math.round(
						(progress?.fraction ?? 0) * 100
					)}%</span
				>
			</div>
			<Btn variant="destructive" size="md" disabled={editor.exportRun?.cancelling} onclick={() => editor.stopExport()}>
				{editor.exportRun?.cancelling ? 'Stopping…' : 'Stop'}
			</Btn>
		{:else}
			<span style="flex:1;font-size:11px;color:var(--text-muted);line-height:1.4"
				>Preset, destination, quality and range are in the export dialog.</span
			>
			<Btn variant="primary" size="md" icon="upload" disabled={!hasClips} onclick={() => ui.openExport()}>
				{shapes.length > 1 ? `Export ${shapes.length} files…` : 'Export…'}
			</Btn>
		{/if}
	</div>
</div>
