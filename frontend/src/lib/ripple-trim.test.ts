import { beforeEach, describe, expect, test } from 'bun:test';
import { MIN_CLIP, rippleTrimPreview, trimBounds } from './ripple-trim';
import { addClip, getTimeline, listAssets, revertTo, setRippleMode, trimClip } from './api';
import { trimEdit } from './frames';
import type { Clip, Track } from './types';
import { clipDuration } from './types';

/** A forward clip of `dur` seconds at `start`, cut from `[sourceIn, sourceIn + dur)`. */
function clip(id: string, start: number, dur: number, sourceIn = 0, extra: Partial<Clip> = {}): Clip {
	return {
		id,
		asset_id: 'a',
		source_in: sourceIn,
		source_out: sourceIn + dur,
		timeline_start: start,
		volume: 1,
		fade_in: 0,
		fade_out: 0,
		...extra
	};
}
const lane = (clips: Clip[], locked = false): Track => ({ id: 'v1', kind: 'video', name: 'V1', clips, locked });

// V1: a [0,10) b [10,18) (abutting a) then a 4 s gap, c [22,30). `a` has 6 s of
// source before its in point, 20 s after; `b` has 2 s before, plenty after.
const a = clip('a', 0, 10, 6);
const b = clip('b', 10, 8, 2);
const c = clip('c', 22, 8);
const sorted = [a, b, c];

describe('trimBounds without ripple', () => {
	test('a right edge stops at its neighbour', () => {
		expect(trimBounds(a, 'r', sorted, 100, false, false)).toEqual({ min: MIN_CLIP, max: 10 });
		expect(trimBounds(b, 'r', sorted, 100, false, false)).toEqual({ min: 10 + MIN_CLIP, max: 22 });
	});

	test('the last clip is free to the end of its source', () => {
		expect(trimBounds(c, 'r', sorted, 40, false, false)).toEqual({ min: 22 + MIN_CLIP, max: 30 + 32 });
	});

	test('a left edge stops at the previous clip, at 0, and at its source', () => {
		expect(trimBounds(b, 'l', sorted, 100, false, false)).toEqual({ min: 10, max: 18 - MIN_CLIP });
		const deep = clip('c', 22, 8, 50); // plenty of head: the clip before it is what stops it
		expect(trimBounds(deep, 'l', [a, b, deep], 100, false, false)).toEqual({ min: 18, max: 30 - MIN_CLIP });
		expect(trimBounds(c, 'l', sorted, 100, false, false).min).toBe(22); // no head to draw on
		// `a` has 6 s of head, but starts at 0: it cannot go before the timeline.
		expect(trimBounds(a, 'l', sorted, 100, false, false)).toEqual({ min: 0, max: 10 - MIN_CLIP });
	});

	test('a clip keeps to its source handle when that is closer than the neighbour', () => {
		const lone = clip('x', 20, 5, 1); // 1 s of head
		expect(trimBounds(lone, 'l', [lone], 100, false, false).min).toBe(19);
		expect(trimBounds(lone, 'r', [lone], 8, false, false).max).toBe(25 + 2); // source [1, 6) of 8: 2 s of tail
	});

	test('a still loops: no source limit either way', () => {
		const still = clip('s', 20, 5, 0);
		expect(trimBounds(still, 'l', [still], 5, true, false).min).toBe(0);
		expect(trimBounds(still, 'r', [still], 5, true, false).max).toBe(Infinity);
	});

	test('a reversed clip swaps its handles', () => {
		const r = clip('r', 20, 5, 4, { speed: -1 }); // source [4, 9): 4 s below, asset 20 -> 11 above
		expect(trimBounds(r, 'l', [r], 20, false, false).min).toBe(20 - 11); // left edge = source_out side
		expect(trimBounds(r, 'r', [r], 20, false, false).max).toBe(25 + 4);
	});

	test('speed stretches how far a handle reaches on the timeline', () => {
		const fast = clip('f', 20, 5, 3, { speed: 2, source_out: 13 }); // 10 source s at 2x = 5 s
		expect(trimBounds(fast, 'l', [fast], 100, false, false).min).toBe(20 - 3 / 2);
	});
});

describe('trimBounds with ripple', () => {
	test('the neighbours are no limit: a clip can be stretched over the one beside it', () => {
		expect(trimBounds(a, 'r', sorted, 100, false, true).max).toBe(10 + 84); // its whole tail
		expect(trimBounds(b, 'l', sorted, 100, false, true).min).toBe(10 - 2); // its 2 s of head, not the clip before
	});

	test('the real limits remain: minimum length, source, and 0', () => {
		expect(trimBounds(a, 'r', sorted, 100, false, true).min).toBe(MIN_CLIP);
		expect(trimBounds(b, 'l', sorted, 100, false, true).max).toBe(18 - MIN_CLIP);
		expect(trimBounds(a, 'l', sorted, 100, false, true).min).toBe(0);
		expect(trimBounds(c, 'r', sorted, 40, false, true).max).toBe(30 + 32); // end of its source
		const lone = clip('x', 20, 5, 1);
		expect(trimBounds(lone, 'l', [lone], 100, false, true).min).toBe(19);
	});

	test('a left edge never goes before 0, whatever the head', () => {
		const early = clip('e', 3, 5, 50);
		expect(trimBounds(early, 'l', [early], 100, false, true).min).toBe(0);
	});

	test('a still has no source limit, and the sides it cannot extend are unchanged', () => {
		const still = clip('s', 20, 5, 0);
		expect(trimBounds(still, 'r', [still], 5, true, true).max).toBe(Infinity);
		expect(trimBounds(still, 'r', [still], 5, true, true).min).toBe(20 + MIN_CLIP);
	});
});

describe('rippleTrimPreview', () => {
	const track = () => lane([clip('a', 0, 10, 6), clip('b', 10, 8, 2), clip('c', 22, 8)]);
	const at = (p: ReturnType<typeof rippleTrimPreview>, id: string) => p!.ghosts.find((g) => g.id === id);

	test('lengthening a clip over its neighbour pushes the later clips along, gaps kept', () => {
		const p = rippleTrimPreview(track(), 'a', 'r', 13)!;
		expect(p.ok).toBe(true);
		expect(at(p, 'a')).toEqual({ id: 'a', start: 0, dur: 13 });
		expect(at(p, 'b')).toEqual({ id: 'b', start: 13, dur: 8 });
		expect(at(p, 'c')).toEqual({ id: 'c', start: 25, dur: 8 }); // the 4 s gap before it survives
		expect([...p.shifted].sort()).toEqual(['b', 'c']);
	});

	test('shortening pulls them in', () => {
		const p = rippleTrimPreview(track(), 'b', 'r', 15)!;
		expect(p.ok).toBe(true);
		expect(at(p, 'b')).toEqual({ id: 'b', start: 10, dur: 5 });
		expect(at(p, 'c')).toEqual({ id: 'c', start: 19, dur: 8 });
		expect(at(p, 'a')).toBeUndefined(); // before it: untouched
		expect([...p.shifted]).toEqual(['c']);
	});

	test('a left edge keeps the clip\'s start — it does not hold the right edge still', () => {
		// Drag b's left edge from 10 to 13: ripple keeps b at 10, 5 s long, and c follows it in.
		const p = rippleTrimPreview(track(), 'b', 'l', 13)!;
		expect(p.ok).toBe(true);
		expect(at(p, 'b')).toEqual({ id: 'b', start: 10, dur: 5 });
		expect(at(p, 'c')).toEqual({ id: 'c', start: 19, dur: 8 });
	});

	test('a left edge dragged out grows the clip to the right and pushes the rest', () => {
		// Drag b's left edge from 10 to 8 (it has 2 s of head): b stays at 10, 10 s long.
		const p = rippleTrimPreview(track(), 'b', 'l', 8)!;
		expect(p.ok).toBe(true);
		expect(at(p, 'b')).toEqual({ id: 'b', start: 10, dur: 10 });
		expect(at(p, 'c')).toEqual({ id: 'c', start: 24, dur: 8 });
	});

	test('a trim that changes nothing about timing previews just the clip', () => {
		const p = rippleTrimPreview(track(), 'c', 'r', 30)!;
		expect(p.ghosts).toEqual([{ id: 'c', start: 22, dur: 8 }]);
		expect(p.shifted.size).toBe(0);
	});

	test('the last clip has nothing to push', () => {
		const p = rippleTrimPreview(track(), 'c', 'r', 40)!;
		expect(p.ghosts).toEqual([{ id: 'c', start: 22, dur: 18 }]);
		expect(p.ok).toBe(true);
	});

	test('speed and reverse are the model\'s: the preview follows the same trimEdit the commit does', () => {
		const t = lane([clip('a', 0, 10, 0, { speed: 2, source_out: 20 }), clip('b', 10, 4)]);
		const p = rippleTrimPreview(t, 'a', 'r', 6)!; // a plays 20 s of source at 2x = 10 s; cut it to 6 s
		expect(at(p, 'a')!.dur).toBeCloseTo(6, 9);
		expect(at(p, 'b')!.start).toBeCloseTo(6, 9);
	});

	test('does not touch the track it was given', () => {
		const t = track();
		const copy = JSON.parse(JSON.stringify(t));
		rippleTrimPreview(t, 'a', 'r', 13);
		rippleTrimPreview(t, 'b', 'l', 8);
		expect(t).toEqual(copy);
	});

	test('a ripple the backend would decline leaves the edge over its neighbour — not a drop to make', () => {
		// A locked lane never ripples (and the GUI would not edit it): the stretched clip sits on `b`.
		const p = rippleTrimPreview({ ...track(), locked: true }, 'a', 'r', 13)!;
		expect(p.ok).toBe(false);
	});

	test('is null for a clip that is not on the track', () => {
		expect(rippleTrimPreview(track(), 'zzz', 'r', 5)).toBeNull();
	});
});

// The preview is only worth drawing if it is what the commit does. The harness's
// `trimClip` is the backend's trim + `ripple_from` in miniature, so the two have
// to land every clip in the same place.
describe('rippleTrimPreview agrees with the commit', () => {
	beforeEach(async () => {
		await setRippleMode(true);
		await revertTo(0);
	});

	test('on the harness cut: right and left edges, longer and shorter', async () => {
		const [interview] = await listAssets();
		for (const [clipId, edge, pos] of [
			['c1', 'r', 8],
			['c1', 'r', 16],
			['c2', 'r', 15],
			['c2', 'l', 14],
			['c1', 'l', 4]
		] as const) {
			await revertTo(0);
			await addClip(interview.id, 0, 4, 'v1', 24); // c4 behind a gap
			const timeline = await getTimeline();
			const track = timeline.tracks[0];
			const preview = rippleTrimPreview(track, clipId, edge, pos)!;
			const target = track.clips.find((x) => x.id === clipId)!;
			const e = trimEdit(target, edge, pos);
			const done = (await trimClip(clipId, e.source_in, e.source_out, e.timeline_start)).tracks[0];
			for (const g of preview.ghosts) {
				const real = done.clips.find((x) => x.id === g.id)!;
				expect(real.timeline_start).toBeCloseTo(g.start, 9);
				expect(clipDuration(real)).toBeCloseTo(g.dur, 9);
			}
			// ...and every clip it did not draw stayed where it was.
			for (const real of done.clips) {
				if (preview.ghosts.some((g) => g.id === real.id)) continue;
				expect(real.timeline_start).toBe(track.clips.find((x) => x.id === real.id)!.timeline_start);
			}
			expect(preview.ok).toBe(true);
		}
	});
});
