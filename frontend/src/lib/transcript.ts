// Transcript lines as the panel shows them: each segment resolved to the
// timeline clip currently carrying it, so a line can seek to itself, be cut
// out, and read as struck through once it is gone.

import type { Clip, Track, TranscriptSegment, Word } from './types';

export type TxLine = {
	t: string;
	s: string;
	start: number;
	end: number;
	clip: Clip | null;
	speaker: string | null;
	/** What cutting the line removes: its words' bounds when it has words (the pauses
	 *  either side stay), else the whole line. */
	cut: { start: number; end: number };
};

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

/** Each segment resolved to the clip showing the most of what cutting it removes (its
 *  words, else the whole line) — null once that is cut out, so a line whose words are gone
 *  but whose pauses remain reads as cut. */
export function transcriptLines(segments: TranscriptSegment[], assetId: string | null, tracks: Track[]): TxLine[] {
	return segments.map((seg) => {
		const words = seg.words ?? [];
		const cut = words.length
			? { start: words[0].start, end: Math.max(words[words.length - 1].end, words[0].start) }
			: { start: seg.start, end: seg.end };
		let clip: Clip | null = null;
		let best = 0;
		for (const tr of tracks) {
			for (const c of tr.clips) {
				if (c.asset_id !== assetId) continue;
				const overlap =
					cut.end > cut.start
						? Math.min(cut.end, c.source_out) - Math.max(cut.start, c.source_in)
						: cut.start > c.source_in && cut.start < c.source_out
							? Number.MIN_VALUE
							: 0;
				if (overlap > best) {
					best = overlap;
					clip = c;
				}
			}
		}
		return {
			t: fmtClock(seg.start),
			s: seg.text,
			start: seg.start,
			end: seg.end,
			clip,
			speaker: seg.speaker ?? null,
			cut
		};
	});
}

/** The speaker labels in a transcript, in order of first appearance. */
export function speakersOf(segments: TranscriptSegment[]): string[] {
	const out: string[] = [];
	for (const s of segments) if (s.speaker && !out.includes(s.speaker)) out.push(s.speaker);
	return out;
}

/** A line's words spread over its span in proportion to their character length,
 *  each marked `estimated`. Mirrors `transcript::estimate_words`. */
export function estimateWords(seg: TranscriptSegment): Word[] {
	const texts = seg.text.split(/\s+/).filter(Boolean);
	const span = seg.end - seg.start;
	if (!texts.length || !(Number.isFinite(seg.start) && Number.isFinite(span) && span > 0)) return [];
	const weights = texts.map((t) => Math.max([...t].length, 1));
	const total = weights.reduce((a, b) => a + b, 0);
	let acc = 0;
	return texts.map((text, i) => {
		const start = seg.start + (span * acc) / total;
		acc += weights[i];
		const end = i + 1 === texts.length ? seg.end : seg.start + (span * acc) / total;
		return { text, start, end, estimated: true, speaker: seg.speaker ?? null };
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
