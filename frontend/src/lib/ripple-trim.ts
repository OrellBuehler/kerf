/* Dragging a clip's edge under **ripple mode** — pure. Two things change when the
 * project ripples, and both are decided here so the drag can show them:
 *
 *  - *The bounds.* Without ripple an edge stops at its neighbour (a clip cannot be
 *    stretched over the one beside it). With ripple it need not: lengthening the
 *    clip pushes the later clips along, so the only limits left are the ones that
 *    are real — the footage the source has to give, a clip's minimum length, and
 *    0 for a left edge.
 *  - *The outcome.* Ripple keeps a left-edge trim's **start** and follows its
 *    length (kerf-core's `ripple_from`), so dragging the left edge does not hold
 *    the right edge still, and every later clip on the track moves by the change
 *    in length. The ghost has to be that, not the clip with its edge dragged.
 *    `rippleTrimPreview` computes it the way the commit will: apply the trim to a
 *    scratch copy of the track, run `rippleFrom` over it (the faithful port of the
 *    backend's), and read where everything landed.
 */

import { trimEdit } from './frames';
import { DIFF_EPS, rippleFrom } from './ripple';
import type { Clip, Track } from './types';
import { clipDuration } from './types';

/** Shortest a clip may get when edge-trimming, seconds. */
export const MIN_CLIP = 0.05;

/** The limits of the dragged edge, timeline seconds. */
export interface TrimBounds {
	min: number;
	max: number;
}

/**
 * Where the `edge` of `clip` may be dragged. `clips` is its track, sorted by start
 * (the clip among them); `assetDuration` is the source's length, `still` a looping
 * still (no source limit). With `ripple` the neighbours are not a limit.
 *
 * Unused source on the side being extended: a forward clip's left edge draws on
 * the handle below `source_in`, its right edge on the handle past `source_out`; a
 * reversed clip plays backwards, so the sides swap.
 */
export function trimBounds(
	clip: Clip,
	edge: 'l' | 'r',
	clips: readonly Clip[],
	assetDuration: number | undefined,
	still: boolean,
	ripple: boolean
): TrimBounds {
	const sp = clip.speed ?? 1;
	const mag = Math.max(Math.abs(sp), 0.01);
	const start = clip.timeline_start;
	const end = start + clipDuration(clip);
	const i = clips.findIndex((x) => x.id === clip.id);
	const headHandle = sp < 0 ? Math.max(0, (assetDuration ?? clip.source_out) - clip.source_out) : clip.source_in;
	const tailHandle = sp < 0 ? clip.source_in : Math.max(0, (assetDuration ?? clip.source_out) - clip.source_out);
	if (edge === 'l') {
		const prev = !ripple && i > 0 ? clips[i - 1] : null;
		const prevEnd = prev ? prev.timeline_start + clipDuration(prev) : 0;
		return { min: Math.max(0, prevEnd, still ? 0 : start - headHandle / mag), max: end - MIN_CLIP };
	}
	const nextStart = !ripple && i >= 0 && i < clips.length - 1 ? clips[i + 1].timeline_start : Infinity;
	return { min: start + MIN_CLIP, max: Math.min(nextStart, still ? Infinity : end + tailHandle / mag) };
}

/** One clip of the previewed track as the commit would leave it. */
export interface TrimGhost {
	id: string;
	start: number;
	dur: number;
}

export interface RippleTrimPreview {
	/** Every clip of the track the trim changes — the trimmed clip, and each clip
	 *  ripple moves — where it ends up. */
	ghosts: TrimGhost[];
	/** The clips ripple moves (the trimmed clip not among them). */
	shifted: Set<string>;
	/** Whether the track is legal afterwards: no two clips overlapping. A ripple the
	 *  backend declines (a locked track, a lane it would leave illegal) hands the
	 *  edit back as it was made — an edge dragged over its neighbour then stays
	 *  over it, which is not a drop to make. */
	ok: boolean;
}

/**
 * What dragging `edge` of `clipId` to timeline time `pos` leaves on `track` with
 * ripple on. `pos` is the gesture's one rounded position (what the commit's
 * `trimEdit` is derived from). `null` when the clip is not on the track.
 */
export function rippleTrimPreview(
	track: Track,
	clipId: string,
	edge: 'l' | 'r',
	pos: number
): RippleTrimPreview | null {
	if (!track.clips.some((c) => c.id === clipId)) return null;
	// Only the timing fields are ever written, so a shallow copy of each clip is a scratch copy.
	const copy = (): Track => ({ ...track, clips: track.clips.map((c) => ({ ...c })) });
	const before = copy();
	const after = copy();
	const clip = after.clips.find((c) => c.id === clipId)!;
	const e = trimEdit(clip, edge, pos);
	// What `trim_clip` does with those fields.
	if (e.source_in !== undefined) clip.source_in = e.source_in;
	if (e.source_out !== undefined) clip.source_out = e.source_out;
	if (e.timeline_start !== undefined) {
		clip.timeline_start = Math.max(0, e.timeline_start);
		after.clips.sort((a, b) => a.timeline_start - b.timeline_start);
	}
	const result = rippleFrom({ tracks: [after] }, { tracks: [before] }).tracks[0];

	const was = new Map(before.clips.map((c) => [c.id, { start: c.timeline_start, dur: clipDuration(c) }]));
	const ghosts: TrimGhost[] = [];
	const shifted = new Set<string>();
	for (const c of result.clips) {
		const w = was.get(c.id)!;
		const start = c.timeline_start;
		const dur = clipDuration(c);
		const moved = Math.abs(start - w.start) > 1e-6;
		if (c.id === clipId || moved || Math.abs(dur - w.dur) > 1e-6) ghosts.push({ id: c.id, start, dur });
		if (moved && c.id !== clipId) shifted.add(c.id);
	}
	// One sweep over the lane in start order: a clip starting before the furthest end so far overlaps.
	let reach = -Infinity;
	let ok = true;
	for (const c of [...result.clips].sort((a, b) => a.timeline_start - b.timeline_start)) {
		if (c.timeline_start < reach - DIFF_EPS) ok = false;
		reach = Math.max(reach, c.timeline_start + clipDuration(c));
	}
	return { ghosts, shifted, ok };
}
