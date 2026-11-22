<script lang="ts">
	// The Deliver workspace's panel: the project's frame, where the cut is going and what an export of it will do.
	// The frame is set here (`ProjectFrame`), where all of it is in view; the shapes to write and the platform check
	// are the export dialog's own components reading the same `ui.deliverShapes`, so a shape ticked here is ticked
	// in the dialog. The render itself still goes through the full dialog (preset, destination, quality, range),
	// which one button opens, and what it leaves behind — the files, or why not — is kept here (`ExportOutcomes`).
	import Btn from './Btn.svelte';
	import ProgressBar from './ProgressBar.svelte';
	import SectionHead from './SectionHead.svelte';
	import ScrollArea from './ScrollArea.svelte';
	import InspectorSection from './InspectorSection.svelte';
	import ProjectFrame from './ProjectFrame.svelte';
	import ExportOutcomes from './ExportOutcomes.svelte';
	import DeliverTo from './DeliverTo.svelte';
	import Readiness from './Readiness.svelte';
	import ThumbnailSection from './ThumbnailSection.svelte';
	import { editor } from '#lib/state.svelte';
	import { ui } from '#lib/editor-ui.svelte';
	import { knownHwEncoders } from '#lib/api';
	import { hasPicture } from '#lib/delivery-formats';
	import { lengthClock } from '#lib/platform-verdict';
	import { exportPlan, filesLine, hasAudioStream, plannedExport, shapesWritten } from '#lib/export-plan';
	import { presetsFor } from '#lib/export-presets';
	import { progressLine } from '#lib/export-progress';
	import { timeDisplay } from '#lib/time-display.svelte';
	import { timecodeRate } from '#lib/timecode';
	import { tip } from '#lib/tip';

	const hasClips = $derived(editor.timeline.tracks.some((t) => t.clips.length > 0));
	// Shapes are for a picture: the dialog ignores them for an audio-only cut, so
	// the panel does not offer them, and the button never promises several files.
	const picture = $derived(hasPicture(editor.timeline));
	const rendering = $derived(editor.exportRun !== null);
	const line = $derived(progressLine(editor.exportRun?.progress));
	const lengthTip = $derived(
		timeDisplay.mode === 'seconds' ? 'Minutes and seconds' : timecodeRate(editor.fps).drop > 0 ? 'Drop-frame timecode' : 'Non-drop timecode'
	);
	const clipCount = $derived(editor.timeline.tracks.reduce((n, t) => n + t.clips.length, 0));

	// What the Export button will do: the dialog opens on what it was last closed on, else on the frame's own
	// preset, and writes a file for each ticked shape (a video cut only).
	const planned = $derived(plannedExport(ui.exportDraft, editor.timeline.format));
	const plan = $derived(
		exportPlan({
			id: planned.id,
			opts: planned.opts,
			presets: presetsFor(knownHwEncoders()),
			shapes: shapesWritten(planned.opts, editor.timeline, ui.deliverShapes),
			hasVideo: picture,
			hasAudio: hasAudioStream(editor.timeline, editor.assets)
		})
	);
</script>

<div style="flex:1;min-height:0;background:var(--surface-panel);display:flex;flex-direction:column;overflow:hidden">
	<ScrollArea padding="4px 14px 12px">
		<SectionHead label="This cut" />
		<div style="display:flex;align-items:baseline;gap:8px;font-size:12px;color:var(--text-secondary)">
			<span style="color:var(--text-muted)">Length</span>
			<span style="font-family:var(--font-mono)" use:tip={lengthTip}>{timeDisplay.fmt(editor.duration)}</span>
			<span style="font-family:var(--font-mono);color:var(--text-muted)" use:tip={'How the platform check and a platform’s limits say it'}
				>({lengthClock(editor.duration)})</span
			>
			<span style="flex:1"></span>
			<span style="font-family:var(--font-mono)">{clipCount} {clipCount === 1 ? 'clip' : 'clips'}</span>
		</div>

		<SectionHead label="Project frame" />
		<ProjectFrame />

		{#if !hasClips}
			<div style="margin-top:14px;font-size:12px;color:var(--text-muted);line-height:1.45">
				Nothing to deliver yet — add a clip to the timeline and this panel shows where it fits.
			</div>
		{/if}

		{#if picture}
			<SectionHead label="Shapes to write" />
			<DeliverTo where="named by shape beside the file you choose when you export" />
			<Readiness />
		{/if}

		<!-- Below the frame and the shapes, which are what this panel is for: the newest export on its own, the rest folded. -->
		{#if ui.exportOutcomes.length}
			<SectionHead label="Recent exports" />
			<ExportOutcomes outcomes={ui.exportOutcomes.slice(0, 1)} dense />
			{#if ui.exportOutcomes.length > 1}
				<div style="margin-top:6px">
					<InspectorSection title="Earlier" summary={`${ui.exportOutcomes.length - 1}`}>
						<ExportOutcomes outcomes={ui.exportOutcomes.slice(1)} dense />
					</InspectorSection>
				</div>
			{/if}
		{/if}

		{#if picture}
			<SectionHead label="Thumbnails" />
			<ThumbnailSection />
		{/if}
	</ScrollArea>

	<div
		style="flex:none;display:flex;align-items:center;gap:10px;padding:10px 14px;border-top:var(--line-width) solid var(--border-default)"
	>
		{#if rendering}
			<div style="flex:1;display:flex;align-items:center;gap:10px;min-width:0">
				<ProgressBar value={line.value} label="Exporting" />
				<span style="font-family:var(--font-mono);font-size:12px;color:var(--text-muted)">{line.text}</span>
			</div>
			<Btn variant="destructive" size="md" disabled={editor.exportRun?.cancelling} onclick={() => editor.stopExport()}>
				{editor.exportRun?.cancelling ? 'Stopping…' : 'Stop'}
			</Btn>
		{:else}
			<div style="flex:1;min-width:0;display:flex;flex-direction:column;gap:2px">
				{#if hasClips}
					<span style="font-size:12px;color:var(--text-secondary)">{plan.preset} · {filesLine(plan.files)}</span>
					<span
						use:tip={plan.summary}
						style="font-family:var(--font-mono);font-size:11px;color:var(--text-muted);overflow:hidden;text-overflow:ellipsis;white-space:nowrap"
						>{plan.summary}</span
					>
				{:else}
					<span style="font-size:12px;color:var(--text-muted)">Nothing to export yet.</span>
				{/if}
			</div>
			<Btn variant="primary" size="md" icon="upload" disabled={!hasClips} onclick={() => ui.openExport()}>
				{plan.files > 1 ? `Export ${plan.files} files…` : 'Export…'}
			</Btn>
		{/if}
	</div>
</div>
