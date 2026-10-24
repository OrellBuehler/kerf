/** The transcript of the cut, mirrored from `kerf_core::model::cut_transcript` so the browser
 *  harness reads the cut the way the backend does: every audible clip's transcript in timeline
 *  time, a word belonging to the clip that plays its midpoint. */

import { sourceToTimeline } from './captions';
import { estimateWords } from './transcript';
import type { Clip, CutLine, CutWord, Timeline, TranscriptSegment, Word } from './types';

const SAME_LINE_EPS = 1e-3;

function wordInClip(clip: Clip, w: Word): CutWord | null {
	const text = w.text.trim();
	if (!text || !Number.isFinite(w.start) || !Number.isFinite(w.end)) return null;
	const lo = Math.min(w.start, w.end);
	const hi = Math.max(w.start, w.end);
	const mid = (lo + hi) / 2;
	if (mid < clip.source_in || mid >= clip.source_out) return null;
	const from = Math.max(lo, clip.source_in);
	const to = Math.min(hi, clip.source_out);
	const a = sourceToTimeline(clip, from);
	const b = sourceToTimeline(clip, to);
	const word: CutWord = { text, start: Math.min(a, b), end: Math.max(a, b), source_start: from, source_end: to };
	if (w.estimated) word.estimated = true;
	return word;
}

function lineInClip(clip: Clip, trackId: string, seg: TranscriptSegment): CutLine | null {
	const all = seg.words?.length ? seg.words : estimateWords(seg);
	const words = all.map((w) => wordInClip(clip, w)).filter((w): w is CutWord => w !== null);
	if (!words.length) return null;
	return {
		start: Math.min(...words.map((w) => w.start)),
		end: Math.max(...words.map((w) => w.end)),
		text: words.length === all.length ? seg.text.trim() : words.map((w) => w.text).join(' '),
		speaker: seg.speaker ?? null,
		clip_id: clip.id,
		track_id: trackId,
		asset_id: clip.asset_id,
		source_start: Math.min(...words.map((w) => w.source_start)),
		source_end: Math.max(...words.map((w) => w.source_end)),
		words
	};
}

/** What the cut says, ordered by start. Mirrors `Timeline::cut_transcript`. */
export function cutTranscript(timeline: Timeline, transcripts: Record<string, TranscriptSegment[]>): CutLine[] {
	const soloed = new Set(timeline.tracks.filter((t) => t.solo).map((t) => t.kind));
	const lines: CutLine[] = [];
	for (const track of timeline.tracks) {
		if (track.muted || (soloed.has(track.kind) && !track.solo)) continue;
		for (const clip of track.clips) {
			if (clip.enabled === false || clip.source_audio === false) continue;
			for (const seg of transcripts[clip.asset_id] ?? []) {
				const timed = Number.isFinite(seg.start) && Number.isFinite(seg.end) && seg.end > seg.start;
				const covers = Math.min(seg.end, clip.source_out) > Math.max(seg.start, clip.source_in);
				if (!seg.text.trim() || !timed || !covers) continue;
				const line = lineInClip(clip, track.id, seg);
				if (line) lines.push(line);
			}
		}
	}
	lines.sort((a, b) => a.start - b.start);
	const kept: CutLine[] = [];
	for (const line of lines) {
		let heard = false;
		for (let i = kept.length - 1; i >= 0 && line.start - kept[i].start < SAME_LINE_EPS; i--) {
			const k = kept[i];
			if (k.asset_id === line.asset_id && Math.abs(k.source_start - line.source_start) < SAME_LINE_EPS && k.text === line.text) {
				heard = true;
				break;
			}
		}
		if (!heard) kept.push(line);
	}
	return kept;
}
