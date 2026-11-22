// Gaps on a lane — pure. The empty stretch between two clips of a track (or between the start of the cut and its
// first clip) is a thing you can point at, select and close: `rippleDeleteGap` moves everything after it up. The
// stretch after a track's last clip is not a gap — there is nothing after it to close up to.
//
// A gap is named by its track and its two ends; it is *only* a selection, never stored, so whatever changes
// the clips on either side makes it a different gap and `gapStillThere` says so.

import type { Timeline, Track } from './types';
import { clipDuration } from './types';

export interface Gap {
	trackId: string;
	/** The end of the clip before it, or 0 at the start of the cut. */
	from: number;
	/** The start of the clip after it. */
	to: number;
}

/** Narrower than this two clips touch: no gap. */
export const GAP_EPS = 1e-6;

/** Every closable gap of `track`, in time order. */
export function gapsOf(track: Pick<Track, 'id' | 'clips'>): Gap[] {
	const lane = [...track.clips].sort((a, b) => a.timeline_start - b.timeline_start);
	const out: Gap[] = [];
	let edge = 0;
	for (const c of lane) {
		if (c.timeline_start - edge > GAP_EPS) out.push({ trackId: track.id, from: edge, to: c.timeline_start });
		edge = Math.max(edge, c.timeline_start + clipDuration(c));
	}
	return out;
}

/** The gap of `track` that `time` is in (inside it, or on either edge), or null. */
export function gapAt(track: Pick<Track, 'id' | 'clips'>, time: number): Gap | null {
	return gapsOf(track).find((g) => time >= g.from - GAP_EPS && time <= g.to + GAP_EPS) ?? null;
}

export const sameGap = (a: Gap | null, b: Gap | null): boolean =>
	a === b || (!!a && !!b && a.trackId === b.trackId && Math.abs(a.from - b.from) < GAP_EPS && Math.abs(a.to - b.to) < GAP_EPS);

/** `gap` as the cut stands now, or null once something has changed on either side of it. */
export function gapStillThere(timeline: Timeline, gap: Gap): Gap | null {
	const track = timeline.tracks.find((t) => t.id === gap.trackId);
	if (!track) return null;
	const now = gapAt(track, (gap.from + gap.to) / 2);
	return sameGap(now, gap) ? now : null;
}

/** The time to ask `ripple_delete_gap` about: the middle, so neither edge is in doubt. */
export const gapPoint = (g: Gap): number => (g.from + g.to) / 2;

export type GapPlan = { ok: true; gap: Gap; track: string } | { ok: false; reason: string };

/**
 * Which gap *Ripple delete gap* closes from the keyboard: the one under the playhead on **the lane last clicked or
 * focused** (`lane`, a track id) — and only there. It never looks at the other lanes: a key that closed "whichever
 * lane has a gap here" would close one the user was not looking at. A locked lane is named, and so is a lane that
 * has no gap there; with no lane yet, it says to click one.
 */
export function planGapClose(timeline: Timeline, o: { lane: string | null; time: number }): GapPlan {
	const track = o.lane ? timeline.tracks.find((t) => t.id === o.lane) : undefined;
	if (!track) return { ok: false, reason: 'Click a track first, then put the playhead in its gap — Ripple delete gap closes the gap on the track you last clicked' };
	if (track.locked) return { ok: false, reason: `Track ${track.name} is locked` };
	const gap = gapAt(track, o.time);
	if (!gap) return { ok: false, reason: `There is no gap under the playhead on ${track.name}` };
	return { ok: true, gap, track: track.name };
}
