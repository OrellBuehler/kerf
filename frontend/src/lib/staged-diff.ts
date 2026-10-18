/* A staged proposal, drawn where it is reviewed — pure. While the agent's proposed cut is on the
 * timeline, the Agent panel lists what changed in words; this is the same list as geometry, so the
 * clips that changed *look* changed: added clips and titles in `--diff-add`, moved, trimmed and
 * adjusted ones in `--diff-shift`, and what the proposal removes as a hatched `--diff-remove`
 * ghost where it stood. It reads the backend's `TimelineDiff` entries (the single source of truth
 * of what changed, matched by id) and the two cuts — the proposal on screen and the live one the
 * editor keeps parked — for the geometry of what is gone. Nothing here decides what counts as a
 * change. */

import { seekTime, stepPoint } from './frames';
import { svgDisplayName } from './svg-titles';
import type { DiffEntry, Timeline } from './types';
import { clipDuration } from './types';

/** How a clip or title on the proposed cut differs from the live one. */
export type ChangeMark = 'added' | 'shifted';

/** Something the proposal removes, where it stood on the live cut. */
export interface RemovedGhost {
	id: string;
	trackId: string;
	start: number;
	dur: number;
}

export interface RemovedTitle {
	id: string;
	start: number;
	end: number;
	text: string;
	/** An animated SVG title rather than text: its `text` is its display name. */
	svg?: boolean;
}

export interface DiffView {
	/** By clip id on the proposed cut. */
	clips: Map<string, ChangeMark>;
	/** By title id on the proposed cut — text overlays and animated SVG titles alike (their ids never collide). */
	titles: Map<string, ChangeMark>;
	/** Tracks the proposal adds or alters, by id. */
	tracks: Map<string, ChangeMark>;
	/** Clips the proposal removes, from tracks that survive it. */
	removed: RemovedGhost[];
	removedTitles: RemovedTitle[];
}

/** The marks for `entries`, with `live` supplying the geometry of whatever the proposal removes. */
export function diffView(entries: readonly DiffEntry[], proposed: Timeline, live: Timeline): DiffView {
	const view: DiffView = { clips: new Map(), titles: new Map(), tracks: new Map(), removed: [], removedTitles: [] };
	const mark = (m: Map<string, ChangeMark>, id: string, to: ChangeMark) => {
		// A clip both added and adjusted in one proposal is still added.
		if (m.get(id) !== 'added') m.set(id, to);
	};
	const lanes = new Set(proposed.tracks.map((t) => t.id));
	for (const e of entries) {
		switch (e.kind) {
			case 'clip_added':
				if (e.clip_id) mark(view.clips, e.clip_id, 'added');
				break;
			case 'clip_moved':
			case 'clip_retrimmed':
			case 'clip_changed':
				if (e.clip_id) mark(view.clips, e.clip_id, 'shifted');
				break;
			case 'clip_removed': {
				const track = live.tracks.find((t) => t.id === e.track_id);
				const clip = track?.clips.find((c) => c.id === e.clip_id);
				if (track && clip && lanes.has(track.id)) {
					view.removed.push({ id: clip.id, trackId: track.id, start: clip.timeline_start, dur: clipDuration(clip) });
				}
				break;
			}
			case 'overlay_added':
				if (e.overlay_id) mark(view.titles, e.overlay_id, 'added');
				break;
			case 'overlay_changed':
				if (e.overlay_id) mark(view.titles, e.overlay_id, 'shifted');
				break;
			case 'overlay_removed': {
				// `overlay_*` entries name a text overlay or an animated SVG title by the same field.
				const o = (live.overlays ?? []).find((x) => x.id === e.overlay_id);
				const s = o ? undefined : (live.svg_overlays ?? []).find((x) => x.id === e.overlay_id);
				if (o) view.removedTitles.push({ id: o.id, start: o.start, end: o.end, text: o.text });
				else if (s) view.removedTitles.push({ id: s.id, start: s.start, end: s.end, text: svgDisplayName(s), svg: true });
				break;
			}
			case 'track_added':
				if (e.track_id) mark(view.tracks, e.track_id, 'added');
				break;
			case 'track_changed':
				if (e.track_id) mark(view.tracks, e.track_id, 'shifted');
				break;
			default:
				break;
		}
	}
	return view;
}

/** Whether `view` marks anything on the timeline at all. */
export const isEmptyView = (v: DiffView): boolean =>
	v.clips.size === 0 && v.titles.size === 0 && v.tracks.size === 0 && v.removed.length === 0 && v.removedTitles.length === 0;

// ---- walking the changes -------------------------------------------------------------

/** The times the changes sit at (`DiffEntry.at`), ascending and distinct — what `[` and `]` step through. */
export function changePoints(entries: readonly DiffEntry[]): number[] {
	const at = entries.map((e) => e.at).filter((t): t is number => typeof t === 'number' && Number.isFinite(t));
	return [...new Set(at)].sort((a, b) => a - b);
}

/** The change after (`dir` 1) or before (-1) `time`, or null when there is none that way. A change is
 *  the start of something, so the playhead goes to the first frame at or after it (`up`) and a step
 *  from there is measured against that. */
export function stepChange(points: readonly number[], time: number, dir: 1 | -1, fps: number, duration: number): number | null {
	return stepPoint(points, time, dir, fps, duration, 'up');
}

/** Which change the playhead is at or has just passed, 1-based, for "2 of 4"; 0 before the first. */
export function changeIndex(points: readonly number[], time: number, fps: number, duration: number): number {
	let n = 0;
	for (const p of points) if (seekTime(p, fps, duration, { snap: 'up' }) <= time + 1e-4) n++;
	return n;
}
