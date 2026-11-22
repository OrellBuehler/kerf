// What the next export would do, in words and numbers, without opening the dialog: which preset it starts
// on, how many files it writes, what they are. The Deliver panel says it beside its Export button and the
// dialog uses the same answers for the same things (`shapesWritten`, `hasAudioStream`), so the two cannot
// disagree about "Export 3 files". Pure.

import { formatsFor, hasPicture } from './delivery-formats';
import { containerInfo, buildSummary, initialExport, type Preset } from './export-presets';
import { frameKey, restoreDraft, type ExportDraft } from './export-draft';
import type { Asset, Delivery, ExportOptions, Timeline } from './types';

/** Whether any clip on the timeline carries a sound stream: a cut of silent footage has no audio to export. */
export function hasAudioStream(timeline: Timeline, assets: readonly Pick<Asset, 'id' | 'streams'>[]): boolean {
	return timeline.tracks.some((t) =>
		t.clips.some((c) => assets.find((a) => a.id === c.asset_id)?.streams.some((s) => s.kind === 'audio'))
	);
}

/** The shapes an export with `opts` writes a file for: the ticked ones when it is a video export of a cut with a
 *  picture, else none — an audio-only render ignores whatever was ticked, and so does a cut with no video clip. */
export function shapesWritten(opts: Pick<ExportOptions, 'container'>, timeline: Timeline, ticked: readonly string[]): Delivery[] {
	return hasPicture(timeline) && !containerInfo(opts.container).audioOnly ? formatsFor(ticked) : [];
}

/** The preset and options the dialog opens on: what it was last closed on, unless the project has been re-cut for
 *  another frame since, else the frame's own. */
export function plannedExport(draft: ExportDraft | null, format: Delivery | null | undefined): { id: string; opts: ExportOptions } {
	const kept = restoreDraft(draft, frameKey(format));
	return kept ? { id: kept.preset, opts: kept.opts } : initialExport(format);
}

export const filesLine = (n: number): string => (n === 1 ? '1 file' : `${n} files`);

export interface ExportPlan {
	/** `Web 1080p`, or `Custom settings` once anything was changed by hand. */
	preset: string;
	/** How many files the export writes. */
	files: number;
	/** `H.264 · MP4 · CRF 20 · 1920×1080 · AAC 192k`: the settings in one line. */
	summary: string;
}

export function exportPlan(input: {
	id: string;
	opts: ExportOptions;
	presets: readonly Pick<Preset, 'id' | 'label'>[];
	shapes: readonly Delivery[];
	hasVideo: boolean;
	hasAudio: boolean;
}): ExportPlan {
	const { id, opts, presets, shapes, hasVideo, hasAudio } = input;
	return {
		preset: presets.find((p) => p.id === id)?.label ?? 'Custom settings',
		files: Math.max(1, shapes.length),
		summary: buildSummary(opts, hasVideo, hasAudio)
	};
}
