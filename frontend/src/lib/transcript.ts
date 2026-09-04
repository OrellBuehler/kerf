// Transcript lines as the panel shows them: each segment resolved to the
// timeline clip currently carrying it, so a line can seek to itself, be cut
// out, and read as struck through once it is gone.

import type { Clip, Track, TranscriptSegment } from './types';

export type TxLine = { t: string; s: string; start: number; end: number; clip: Clip | null };

export function fmtClock(s: number): string {
	const m = Math.floor(s / 60);
	const sec = Math.floor(s % 60);
	return `${m.toString().padStart(2, '0')}:${sec.toString().padStart(2, '0')}`;
}

/** Timeline time of a source point within a clip (mirrors the timeline's
 *  mapping, honoring speed and reverse). */
export function srcToTimeline(c: Clip, src: number): number {
	const sp = c.speed ?? 1;
	const mag = Math.max(Math.abs(sp), 0.01);
	const off = sp < 0 ? c.source_out - src : src - c.source_in;
	return c.timeline_start + Math.max(0, off) / mag;
}

/** Each segment resolved (by its midpoint) to the clip showing it — null once
 *  cut out. */
export function transcriptLines(segments: TranscriptSegment[], assetId: string | null, tracks: Track[]): TxLine[] {
	return segments.map((seg) => {
		const mid = (seg.start + seg.end) / 2;
		let clip: Clip | null = null;
		outer: for (const tr of tracks) {
			for (const c of tr.clips) {
				if (c.asset_id === assetId && mid > c.source_in && mid < c.source_out) {
					clip = c;
					break outer;
				}
			}
		}
		return { t: fmtClock(seg.start), s: seg.text, start: seg.start, end: seg.end, clip };
	});
}

/** Index of the line under the playhead, or -1. */
export function activeLineIndex(lines: TxLine[], time: number): number {
	for (let i = 0; i < lines.length; i++) {
		const l = lines[i];
		if (!l.clip) continue;
		const a = srcToTimeline(l.clip, Math.max(l.start, l.clip.source_in));
		const b = srcToTimeline(l.clip, Math.min(l.end, l.clip.source_out));
		if (time >= Math.min(a, b) && time < Math.max(a, b)) return i;
	}
	return -1;
}
