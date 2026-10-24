// The browser harness's `search_transcripts` (kerf-core `transcript::search`, simplified: every
// combining mark is dropped and each Han / kana / Thai character is a term).
import type {
	Asset,
	AssetAnalysis,
	CutUse,
	Timeline,
	TranscriptHit,
	TranscriptQuery,
	TranscriptSearch,
	TranscriptSegment,
	Word
} from './types';

const UNSPACED = /[\p{Script=Han}\p{Script=Hiragana}\p{Script=Katakana}\p{Script=Thai}]/u;

export function terms(text: string): string[] {
	const folded = text
		.normalize('NFD')
		.replace(/\p{M}/gu, '')
		.replace(/['‘’ʼ`´′]/g, '')
		.toLowerCase();
	const out: string[] = [];
	for (const run of folded.split(/[^\p{L}\p{N}]+/u)) {
		if (!run) continue;
		let word = '';
		for (const c of run) {
			if (UNSPACED.test(c)) {
				if (word) out.push(word);
				out.push(c);
				word = '';
			} else word += c;
		}
		if (word) out.push(word);
	}
	return out;
}

function wordsOf(seg: TranscriptSegment): Word[] {
	if (seg.words?.length) return seg.words;
	const texts = seg.text.split(/\s+/).filter(Boolean);
	const span = seg.end - seg.start;
	if (!texts.length || !(span > 0)) return [];
	const total = texts.reduce((n, t) => n + Math.max(1, [...t].length), 0);
	let acc = 0;
	return texts.map((text, i) => {
		const start = seg.start + (span * acc) / total;
		acc += Math.max(1, [...text].length);
		const end = i + 1 === texts.length ? seg.end : seg.start + (span * acc) / total;
		return { text, start, end, estimated: true, speaker: seg.speaker };
	});
}

type At = [number, number];

function matches(segments: TranscriptSegment[], query: string[], mode: TranscriptQuery['mode']): [At, At][] {
	const toks: { term: string; at: At }[] = [];
	const lineEnd: number[] = [];
	segments.forEach((seg, s) => {
		wordsOf(seg).forEach((w, i) => terms(w.text).forEach((term) => toks.push({ term, at: [s, i] })));
		lineEnd.push(toks.length);
	});
	const unique = [...new Set(query)];
	const spans: [At, At][] = [];
	if (mode === 'any_word') {
		for (const t of toks) {
			const last = spans.at(-1);
			if (unique.includes(t.term) && !(last && last[0][0] === t.at[0] && last[0][1] === t.at[1])) spans.push([t.at, t.at]);
		}
	} else if (mode === 'all_words') {
		let first = 0;
		for (const last of lineEnd) {
			const hits = toks.slice(first, last).filter((t) => unique.includes(t.term));
			first = last;
			let best: [number, number] | null = null;
			for (let l = 0; l < hits.length; l++) {
				const seen = new Set<string>();
				for (let r = l; r < hits.length; r++) {
					seen.add(hits[r].term);
					if (seen.size === unique.length) {
						if (!best || r - l < best[1] - best[0]) best = [l, r];
						break;
					}
				}
			}
			if (best) spans.push([hits[best[0]].at, hits[best[1]].at]);
		}
	} else {
		for (let i = 0; i + query.length <= toks.length; ) {
			if (query.every((q, j) => toks[i + j].term === q)) {
				spans.push([toks[i].at, toks[i + query.length - 1].at]);
				i += query.length;
			} else i++;
		}
	}
	return spans;
}

function cutUses(timeline: Timeline, assetId: string, start: number, end: number): CutUse[] {
	const out: CutUse[] = [];
	for (const track of timeline.tracks) {
		for (const c of track.clips) {
			if (c.asset_id !== assetId || Math.min(end, c.source_out) <= Math.max(start, c.source_in)) continue;
			const a = Math.max(start, c.source_in);
			const b = Math.min(end, c.source_out);
			const speed = Math.max(Math.abs(c.speed ?? 1), 1e-6);
			const at = (t: number) => c.timeline_start + ((c.speed ?? 1) < 0 ? c.source_out - t : t - c.source_in) / speed;
			const [x, y] = [at(a), at(b)];
			const use: CutUse = { clip_id: c.id, track_id: track.id, start: Math.min(x, y), end: Math.max(x, y) };
			if (a > start || b < end) use.partial = true;
			if (c.enabled === false) use.disabled = true;
			out.push(use);
		}
	}
	return out.sort((p, q) => p.start - q.start);
}

export function searchTranscripts(
	assets: Asset[],
	analyses: Record<string, AssetAnalysis | undefined>,
	timeline: Timeline,
	q: TranscriptQuery
): TranscriptSearch {
	const query = terms(q.query);
	if (!query.length) throw new Error('the query has no words to look for');
	const offset = q.offset ?? 0;
	const limit = Math.min(500, Math.max(1, q.limit ?? 50));
	const named = q.asset_ids ? new Set(q.asset_ids) : null;
	const want = q.speaker?.trim().toLowerCase();
	const all: TranscriptHit[] = [];
	const missing: string[] = [];
	let searched = 0;
	for (const asset of assets.filter((a) => !named || named.has(a.id))) {
		const transcript = analyses[asset.id]?.transcript ?? [];
		const ran = analyses[asset.id]?.ran?.includes('transcript');
		if (!transcript.length && !ran) {
			if (named || asset.streams.some((s) => s.kind === 'audio')) missing.push(asset.id);
			continue;
		}
		searched++;
		const words = transcript.map(wordsOf);
		const found: TranscriptHit[] = [];
		for (const [from, to] of matches(transcript, query, q.mode ?? 'phrase')) {
			const span: Word[] = [];
			for (let s = from[0]; s <= to[0]; s++) {
				span.push(...words[s].slice(s === from[0] ? from[1] : 0, s === to[0] ? to[1] + 1 : undefined));
			}
			const speaker = span[0].speaker ?? transcript[from[0]].speaker ?? undefined;
			if (want && speaker?.toLowerCase() !== want) continue;
			const text = span.map((w) => w.text).join(' ');
			const before = words[from[0]].slice(0, from[1]).map((w) => w.text).join(' ');
			const after = words[to[0]].slice(to[1] + 1).map((w) => w.text).join(' ');
			const start = span[0].start;
			const end = Math.max(...span.map((w) => w.end));
			const hit: TranscriptHit = {
				asset_id: asset.id,
				asset_name: asset.name,
				start,
				end,
				text,
				context: [before, `«${text}»`, after].filter(Boolean).join(' '),
				in_cut: cutUses(timeline, asset.id, start, end)
			};
			if (speaker) hit.speaker = speaker;
			if (span.some((w) => w.estimated)) hit.words_estimated = true;
			found.push(hit);
		}
		all.push(...found.sort((a, b) => a.start - b.start));
	}
	const hits = all.slice(offset, offset + limit);
	const next = offset + hits.length;
	return {
		mode: q.mode ?? 'phrase',
		total: all.length,
		offset,
		returned: hits.length,
		...(next < all.length ? { next_offset: next } : {}),
		searched_assets: searched,
		assets_without_transcript: { count: missing.length, asset_ids: missing.slice(0, 10) },
		hits
	};
}
