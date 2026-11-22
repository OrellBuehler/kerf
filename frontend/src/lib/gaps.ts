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
 * Which gap *Ripple delete gap* closes: the one selected, if it is still there; else the one the playhead is in,
 * on the tracks of the selected clips (the primary's first), or on every track in the order they are drawn when
 * nothing is selected. A locked track is named, not skipped: that is the reason the key did nothing.
 */
export function planGapClose(
	timeline: Timeline,
	o: { selectedGap: Gap | null; selected: readonly string[]; primary: string | null; time: number; drawn: readonly Track[] }
): GapPlan {
	const named = (id: string) => timeline.tracks.find((t) => t.id === id)?.name ?? id;
	if (o.selectedGap) {
		const g = gapStillThere(timeline, o.selectedGap);
		if (g) {
			const track = timeline.tracks.find((t) => t.id === g.trackId);
			return track?.locked ? { ok: false, reason: `Track ${track.name} is locked` } : { ok: true, gap: g, track: named(g.trackId) };
		}
	}
	const want = new Set(o.selected);
	const holding = timeline.tracks.filter((t) => t.clips.some((c) => want.has(c.id)));
	const lanes = holding.length > 0 ? [...holding].sort((a, b) => Number(b.clips.some((c) => c.id === o.primary)) - Number(a.clips.some((c) => c.id === o.primary))) : o.drawn;
	let lockedAt: string | null = null;
	for (const t of lanes) {
		const g = gapAt(t, o.time);
		if (!g) continue;
		if (t.locked) {
			lockedAt ??= t.name;
			continue;
		}
		return { ok: true, gap: g, track: t.name };
	}
	if (lockedAt) return { ok: false, reason: `Track ${lockedAt} is locked` };
	return { ok: false, reason: 'There is no gap here to close — click one, or put the playhead in one' };
}
