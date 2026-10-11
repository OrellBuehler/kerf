/* What the plain gestures say while they are held — pure. Roll, slip and slide always had a number
 * beside the pointer and a frame in the Preview (`trim-tools.ts`); the edge trim, a clip move and
 * the razor, the gestures an edit is mostly made of, had a dashed outline and nothing else. These
 * build the same `Readout` the others use, so the timeline draws them with one component and a
 * limit is amber, a refusal red, the same everywhere. Positions come in already snapped and
 * rounded once (`frames.ts`); nothing here rounds. */

import { MIN_CLIP, type TrimBounds } from './ripple-trim';
import { trimEdit } from './frames';
import { formatTimecode } from './timecode';
import { deltaLabel, edgeFrameTime, type MonitorCell, type Readout, type TrimMonitor } from './trim-tools';
import type { Clip, StreamKind } from './types';

const ZERO = 1e-9;

/** Why an edge dragged to the end of `bounds` went no further: the footage ran out, the clip would
 *  be too short, the cut starts there, or a neighbour is in the way. `strict` is the bounds with
 *  the neighbours as limits, `loose` without them (ripple pushes them along): a limit that is
 *  the same in both is the clip's own. */
export function trimLimitWhy(edge: 'l' | 'r', side: 'min' | 'max', strict: TrimBounds, loose: TrimBounds): string {
	const hit = side === 'min' ? strict.min : strict.max;
	const free = side === 'min' ? loose.min : loose.max;
	if (Math.abs(hit - free) > 1e-6) {
		// A neighbour on the side the edge is moving toward: the previous clip for a left edge
		// moving earlier, the next for a right edge moving later.
		return edge === 'l' ? 'the previous clip is in the way' : 'the next clip is in the way';
	}
	// The edge that cannot go further in: the 0.05 s minimum.
	if ((edge === 'r' && side === 'min') || (edge === 'l' && side === 'max')) return `a clip is at least ${MIN_CLIP} s`;
	if (edge === 'l' && Math.abs(hit) < ZERO) return 'the cut starts here';
	return 'no more footage on that side';
}

/** The pointer-side readout of an edge trim: the change in length, the new length, and, when the
 *  drag is held at a limit, what is holding it. `start` and `end` are the clip's span before
 *  the drag, `pos` where the edge is now. */
export function trimEdgeReadout(o: {
	edge: 'l' | 'r';
	start: number;
	end: number;
	pos: number;
	fps: number;
	/** The pointer asked for somewhere the bounds did not allow, and why. */
	limit: string | null;
}): Readout {
	const was = o.end - o.start;
	const now = o.edge === 'l' ? o.end - o.pos : o.pos - o.start;
	const title = `Trim ${o.edge === 'l' ? 'in' : 'out'} ${deltaLabel(now - was, o.fps)} · length ${formatTimecode(now, o.fps)}`;
	return o.limit
		? { title, detail: o.limit[0].toUpperCase() + o.limit.slice(1), tone: 'limit' }
		: { title, detail: null, tone: 'ok' };
}

/** The readout of a clip (or group) being moved: how far, where its start now is, which lane. A
 *  drop the engine would refuse says why instead of where. */
export function moveReadout(o: {
	delta: number;
	start: number;
	track: string;
	fps: number;
	count: number;
	refused: string | null;
}): Readout {
	const many = o.count > 1 ? ` · ${o.count} clips` : '';
	const title = `Move ${deltaLabel(o.delta, o.fps)} · ${formatTimecode(o.start, o.fps)} · ${o.track}${many}`;
	return o.refused ? { title, detail: o.refused[0].toUpperCase() + o.refused.slice(1), tone: 'refused' } : { title, detail: null, tone: 'ok' };
}

/** The readout of a razor cut about to be made at `at`; `along` names the lanes a linked clip's
 *  partners are cut on too, and a clip too short for a cut on a frame says so. */
export function cutReadout(o: { at: number | null; fps: number; along: readonly string[] }): Readout {
	if (o.at === null) return { title: 'Cut', detail: 'That clip is too short to cut on a frame', tone: 'refused' };
	const along = o.along.length > 0 ? ` · with ${o.along.join(', ')}` : '';
	return { title: `Cut ${formatTimecode(o.at, o.fps)}${along}`, detail: null, tone: 'ok' };
}

/**
 * What the Preview shows while an edge is dragged: the frame at the edge as it would be left — the
 * first shown frame of a left edge, the last of a right one — under the same readout the timeline
 * draws beside the pointer. `null` for sound (no picture to show). Frames come from the source, as
 * the roll / slip / slide monitor's do.
 */
export function trimEdgeMonitor(
	clip: Clip,
	edge: 'l' | 'r',
	pos: number,
	fps: number,
	kind: StreamKind,
	readout: Readout
): TrimMonitor | null {
	if (kind !== 'video') return null;
	const left = trimEdit(clip, edge, pos);
	const after: Clip = { ...clip, ...left };
	const label = edge === 'l' ? 'In' : 'Out';
	const time = edgeFrameTime(after, edge === 'l' ? 'first' : 'last', fps);
	const cell: MonitorCell = {
		key: `${label}:${clip.id}`,
		label,
		clipId: clip.id,
		assetId: clip.asset_id,
		time,
		timecode: formatTimecode(time, fps)
	};
	return { ...readout, cells: [cell] };
}
