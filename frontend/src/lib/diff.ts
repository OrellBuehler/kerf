// Presentation for a `TimelineDiff` — the TS mirror of `TimelineDiff::headline`
// in crates/kerf-core/src/model.rs. The backend sends the entries already
// phrased; what it does not send is the one-line headline, because that is a
// rendering decision the review card and the agent's text summary make
// differently. Keeping the arithmetic here (and tested) is what stops the card
// from disagreeing with the numbers the agent reports.

import type { DiffEntry, DiffKind, TimelineDiff } from './types';

/** `m:ss.d`, the way an editor reads a timeline position. */
export function formatTime(secs: number): string {
	const s = Math.max(0, secs);
	const m = Math.floor(s / 60);
	return `${m}:${(s - m * 60).toFixed(1).padStart(4, '0')}`;
}

function delta(secs: number): string {
	return `${secs >= 0 ? '+' : ''}${secs.toFixed(1)}s`;
}

const ADDED: DiffKind[] = ['track_added', 'clip_added', 'overlay_added', 'marker_added'];
const REMOVED: DiffKind[] = ['track_removed', 'clip_removed', 'overlay_removed', 'marker_removed'];

/** Adds, removes, or alters — the three tints a diff needs. */
export function polarity(kind: DiffKind): 'added' | 'removed' | 'changed' {
	if (ADDED.includes(kind)) return 'added';
	if (REMOVED.includes(kind)) return 'removed';
	return 'changed';
}

/** One line: how many changes, and what they did to the runtime. */
export function diffHeadline(diff: TimelineDiff): string {
	const n = diff.entries.length;
	if (n === 0) return 'No changes';
	let s = `${n} change${n === 1 ? '' : 's'}`;
	if (Math.abs(diff.duration_after - diff.duration_before) > 1e-6) {
		s += ` · ${formatTime(diff.duration_before)} → ${formatTime(diff.duration_after)} (${delta(
			diff.duration_after - diff.duration_before
		)})`;
	} else {
		s += ` · ${formatTime(diff.duration_after)}`;
	}
	if (diff.clips_before !== diff.clips_after) s += ` · ${diff.clips_before} → ${diff.clips_after} clips`;
	return s;
}

/** Entries grouped by what they touch, so a long proposal stays readable. */
export function groupEntries(entries: DiffEntry[]): { label: string; entries: DiffEntry[] }[] {
	const buckets: Record<string, DiffEntry[]> = { Tracks: [], Clips: [], Text: [], Markers: [], Delivery: [] };
	for (const e of entries) {
		if (e.kind.startsWith('track_')) buckets.Tracks.push(e);
		else if (e.kind.startsWith('clip_')) buckets.Clips.push(e);
		else if (e.kind.startsWith('overlay_')) buckets.Text.push(e);
		else if (e.kind.startsWith('marker_')) buckets.Markers.push(e);
		else buckets.Delivery.push(e);
	}
	return Object.entries(buckets)
		.filter(([, list]) => list.length > 0)
		.map(([label, list]) => ({ label, entries: list }));
}
