/**
 * The beat grid: an asset's cached tempo analysis mapped onto the timeline, and
 * the cut alignment built on it. Mirrors `model.rs` (`Timeline::beat_grid`,
 * `Track::align_cuts_to_beats`) so the ruler's ticks, the drag snapping and the
 * browser harness's "cut to the beat" all agree with what the backend does.
 */
import type { Clip, Tempo, Timeline } from './types';

/** Tempo estimates below this confidence are ignored. */
export const BEAT_MIN_CONFIDENCE = 0.25;

/** Shortest clip an alignment may leave behind, seconds. */
export const MIN_BEAT_CLIP = 0.05;

/** Where a source timestamp of a clip lands on the timeline, honoring speed and reverse. */
export function sourceToTimeline(c: Clip, source: number): number {
	const speed = c.speed ?? 1;
	const mag = Math.max(Math.abs(speed), 0.01);
	return c.timeline_start + (speed < 0 ? c.source_out - source : source - c.source_in) / mag;
}

/** The marks of the audio tracks (`marksOf` picks which list of an asset's tempo) in timeline
 *  seconds, ascending and de-duplicated. */
function trackGrid(
	timeline: Timeline,
	tempoFor: (assetId: string) => Tempo | null | undefined,
	marksOf: (tempo: Tempo) => readonly number[]
): number[] {
	const times: number[] = [];
	for (const track of timeline.tracks) {
		if (track.kind !== 'audio') continue;
		for (const clip of track.clips) {
			const tempo = tempoFor(clip.asset_id);
			if (!tempo || tempo.confidence < BEAT_MIN_CONFIDENCE || tempo.bpm <= 0) continue;
			for (const mark of marksOf(tempo)) {
				if (mark >= clip.source_in && mark <= clip.source_out) times.push(sourceToTimeline(clip, mark));
			}
		}
	}
	times.sort((a, b) => a - b);
	// Overlapping clips of one asset repeat the same marks; drop the copies.
	return times.filter((b, i) => i === 0 || b - times[i - 1] > 0.005);
}

/** Beat times of the audio tracks in timeline seconds, ascending and de-duplicated. */
export function beatGrid(timeline: Timeline, tempoFor: (assetId: string) => Tempo | null | undefined): number[] {
	return trackGrid(timeline, tempoFor, (t) => t.beats);
}

/** Bar starts of the audio tracks in timeline seconds: `beatGrid` over the tempos' `downbeats`
 *  (`Timeline::bar_grid`). Empty when no music has a fitted bar grid. */
export function barGrid(timeline: Timeline, tempoFor: (assetId: string) => Tempo | null | undefined): number[] {
	return trackGrid(timeline, tempoFor, (t) => t.downbeats ?? []);
}

/** The beat nearest `time` within `tolerance`, or null. */
export function nearestBeat(beats: number[], time: number, tolerance: number): number | null {
	if (tolerance <= 0) return null;
	let best: number | null = null;
	for (const b of beats) {
		const d = Math.abs(b - time);
		if (d <= tolerance && (best === null || d < Math.abs(best - time))) best = b;
	}
	return best;
}

/**
 * What the beat grid pulls a gesture towards. `edges` are the offsets from `time` of the points that
 * should land on the grid — `[0]` for a cut or a trimmed edge, `[0, length]` for a span whose head
 * and tail may both be set on it — and the answer is the value of `time` that puts one of them on
 * a grid mark (the candidates for a snap to choose the nearest from).
 *
 * Bars win over beats: when any edge has a bar within `tolerance`, only the bars are offered — a
 * beat that happens to be nearer does not pull the clip off the downbeat — and otherwise the
 * beats within reach are. A bar is also a beat (they come from one grid), so with a single edge
 * this only changes the answer where the grid is sparse enough, or the tolerance wide enough,
 * for a beat to sit nearer than the bar. Marks exactly `tolerance` away do not count, like
 * `nearestWithin`'s.
 */
export function gridMagnets(
	time: number,
	edges: readonly number[],
	beats: readonly number[],
	bars: readonly number[],
	tolerance: number
): number[] {
	if (!(tolerance > 0)) return [];
	const within = (marks: readonly number[]) => {
		const out: number[] = [];
		for (const offset of edges) {
			for (const m of marks) if (Math.abs(m - (time + offset)) < tolerance) out.push(m - offset);
		}
		return out;
	};
	const onBars = within(bars);
	return onBars.length > 0 ? onBars : within(beats);
}

/** Half the median beat interval — every cut then moves to the beat it is nearest. */
export function defaultBeatTolerance(beats: number[]): number {
	const gaps = beats.slice(1).map((b, i) => b - beats[i]);
	if (gaps.length === 0) return 0;
	gaps.sort((a, b) => a - b);
	return gaps[Math.floor(gaps.length / 2)] / 2;
}

/** Shortest interval in the grid, seconds — Infinity when there is no grid. */
export function beatPeriod(beats: number[]): number {
	return beats.slice(1).reduce((min, b, i) => Math.min(min, b - beats[i]), Infinity);
}

/**
 * Ripple every cut of `clips` (one track, in place) onto the nearest beat,
 * retrimming each clip at its outgoing edge and keeping the gaps. `limitFor`
 * caps an asset's source time (Infinity for a still, which loops). Returns how
 * many cuts moved.
 */
export function alignCutsToBeats(
	clips: Clip[],
	beats: number[],
	tolerance: number,
	limitFor: (assetId: string) => number
): number {
	clips.sort((a, b) => a.timeline_start - b.timeline_start);
	let moved = 0;
	let cursor = 0; // end of the previous clip after alignment
	let previousEnd = 0; // ...and where it ended before
	for (const clip of clips) {
		const speed = clip.speed ?? 1;
		const mag = Math.max(Math.abs(speed), 0.01);
		const gap = Math.max(clip.timeline_start - previousEnd, 0);
		previousEnd = clip.timeline_start + (clip.source_out - clip.source_in) / mag;

		let start = cursor + gap;
		if (gap > 1e-6) {
			const beat = nearestBeat(beats, start, tolerance);
			if (beat !== null && Math.abs(Math.max(beat, cursor) - start) > 1e-6) {
				moved += 1;
				start = Math.max(beat, cursor);
			}
		}

		let duration = (clip.source_out - clip.source_in) / mag;
		const beat = nearestBeat(beats, start + duration, tolerance);
		if (beat !== null) {
			const left = speed < 0 ? clip.source_out : Math.max(limitFor(clip.asset_id) - clip.source_in, 0);
			const available = Math.max(left / mag, MIN_BEAT_CLIP);
			const wanted = Math.min(Math.max(beat - start, MIN_BEAT_CLIP), available);
			if (Math.abs(wanted - duration) > 1e-6) {
				moved += 1;
				duration = wanted;
			}
		}

		clip.timeline_start = start;
		if (speed < 0) clip.source_in = Math.max(clip.source_out - duration * mag, 0);
		else clip.source_out = clip.source_in + duration * mag;
		cursor = start + duration;
	}
	return moved;
}
