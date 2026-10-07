import { describe, expect, test } from 'bun:test';
import type { SourceLimits } from './edit-modes';
import { linkedTrimBounds, linkedTrimPreview, type LinkedTrimOptions } from './linked-trim';
import { rippleTrimPreview, trimBounds } from './ripple-trim';
import type { Clip, StreamKind, Timeline, Track } from './types';

const clip = (id: string, asset: string, sin: number, sout: number, at: number, link?: string): Clip => ({
	id,
	asset_id: asset,
	source_in: sin,
	source_out: sout,
	timeline_start: at,
	volume: 1,
	fade_in: 0,
	fade_out: 0,
	...(link ? { link_id: link } : {})
});
const lane = (kind: StreamKind, name: string, clips: Clip[], locked = false): Track => ({
	id: name.toLowerCase(),
	kind,
	name,
	clips,
	...(locked ? { locked } : {})
});
const footage: SourceLimits = new Map([['x', 100]]);
const opts = (o: Partial<LinkedTrimOptions> = {}): LinkedTrimOptions => ({ ripple: false, links: true, footage, ...o });
const get = (t: Timeline, id: string) => t.tracks.flatMap((x) => x.clips).find((c) => c.id === id)!;

describe('linkedTrimBounds', () => {
	// V1: a [0,10), b [14,20)     A1: pa [0,10) (linked to a), y [11,15)
	function cut() {
		return {
			tracks: [
				lane('video', 'V1', [clip('a', 'x', 0, 10, 0, 'L'), clip('b', 'x', 30, 36, 14)]),
				lane('audio', 'A1', [clip('pa', 'x', 0, 10, 0, 'L'), clip('y', 'x', 50, 54, 11)])
			]
		} satisfies Timeline;
	}
	const tail = (t: Timeline, ripple: boolean, links = true) => {
		const a = get(t, 'a');
		const alone = trimBounds(a, 'r', t.tracks[0].clips, 100, false, ripple);
		return links ? linkedTrimBounds(t, a, 'r', 100, false, ripple) : alone;
	};

	test('a partner’s neighbour limits the drag, not just the clip’s own', () => {
		const t = cut();
		expect(tail(t, false, false).max).toBe(14); // b
		expect(tail(t, false).max).toBe(11); // y, beside the sound
	});

	test('with ripple the neighbours are pushed, not a limit — only the footage is', () => {
		const t = cut();
		expect(tail(t, true).max).toBe(100); // the 100 s of footage: a's window is 0..10
	});

	test('a partner that does not share the edge is untouched', () => {
		const t = cut();
		t.tracks[1].clips[0].source_out = 9.5; // the sound ends half a second early
		expect(tail(t, false).max).toBe(14);
	});

	test('a partner on a locked track is left to the preview to refuse', () => {
		const t = cut();
		t.tracks[1].locked = true;
		expect(tail(t, false).max).toBe(14);
	});

	test('the partner’s own footage is not a limit: the backend trims it less, it does not refuse', () => {
		const t = cut();
		// The sound has only 10 s of its asset to give; the picture's asset has 100.
		t.tracks[1].clips[0].asset_id = 'short';
		expect(linkedTrimBounds(t, get(t, 'a'), 'r', 100, false, false).max).toBe(11);
	});

	test('the left edge: the partner’s previous clip is the floor', () => {
		const t: Timeline = {
			tracks: [
				lane('video', 'V1', [clip('a', 'x', 10, 20, 10, 'L')]),
				lane('audio', 'A1', [clip('z', 'x', 60, 63, 4), clip('pa', 'x', 10, 20, 10, 'L')])
			]
		};
		const b = linkedTrimBounds(t, get(t, 'a'), 'l', 100, false, false);
		expect(b.min).toBe(7); // the sound's neighbour z ends at 7: that is as far left as both go
		expect(b.max).toBeCloseTo(19.95, 9);
	});

	test('an unlinked clip is exactly `trimBounds`', () => {
		const t = cut();
		const b = get(t, 'b');
		expect(linkedTrimBounds(t, b, 'l', 100, false, false)).toEqual(trimBounds(b, 'l', t.tracks[0].clips, 100, false, false));
	});

	test('the range always holds the edge where it is', () => {
		const t = cut();
		t.tracks[1].clips[1].timeline_start = 9; // y overlaps the sound already: its limit is behind the edge
		const b = linkedTrimBounds(t, get(t, 'a'), 'r', 100, false, false);
		expect(b.max).toBeGreaterThanOrEqual(10);
		expect(b.min).toBeLessThanOrEqual(10);
	});
});

describe('linkedTrimPreview', () => {
	// V1: a [0,10) (L1)  b [10,20) (L2)       A1: pa [0,10) (L1)  pb [10,20) (L2)
	function pairs(): Timeline {
		return {
			tracks: [
				lane('video', 'V1', [clip('a', 'x', 0, 10, 0, 'L1'), clip('b', 'x', 20, 30, 10, 'L2')]),
				lane('audio', 'A1', [clip('pa', 'x', 0, 10, 0, 'L1'), clip('pb', 'x', 20, 30, 10, 'L2')])
			]
		};
	}

	test('a shorter tail takes the partner’s tail with it', () => {
		const p = linkedTrimPreview(pairs(), 'a', 'r', 8, opts())!;
		expect(p.ok).toBe(true);
		expect(p.reason).toBeNull();
		expect(p.ghosts.map((g) => [g.id, g.trackId, g.start, g.dur])).toEqual([
			['a', 'v1', 0, 8],
			['pa', 'a1', 0, 8]
		]);
		expect(p.shifted.size).toBe(0);
	});

	test('with links off only the clip is drawn', () => {
		const p = linkedTrimPreview(pairs(), 'a', 'r', 8, opts({ links: false }))!;
		expect(p.ghosts.map((g) => g.id)).toEqual(['a']);
	});

	test('with ripple the clip behind it moves, and so does the partner of that one', () => {
		// c, an unlinked title, shortened by 2: b ripples left on V1 and — the sync lock — pb follows on A1,
		// a lane nothing was edited on.
		const t = pairs();
		t.tracks[0].clips.unshift(clip('c', 'x', 40, 48, 0));
		t.tracks[0].clips[1].timeline_start = 8; // a
		t.tracks[0].clips = [clip('c', 'x', 40, 48, 0), clip('b', 'x', 20, 30, 8, 'L2')];
		t.tracks[1].clips = [clip('pb', 'x', 20, 30, 8, 'L2')];
		const p = linkedTrimPreview(t, 'c', 'r', 6, opts({ ripple: true }))!;
		expect(p.ok).toBe(true);
		expect(p.ghosts.map((g) => [g.id, g.trackId, g.start])).toEqual([
			['c', 'v1', 0],
			['b', 'v1', 6],
			['pb', 'a1', 6]
		]);
		expect([...p.shifted].sort()).toEqual(['b', 'pb']);
		// …and with links off the sound is left behind, as Alt-trim leaves it.
		const alone = linkedTrimPreview(t, 'c', 'r', 6, opts({ ripple: true, links: false }))!;
		expect(alone.ghosts.map((g) => g.id)).toEqual(['c', 'b']);
	});

	test('a ripple the sound’s lane would not allow refuses, and names what is in the way', () => {
		const t: Timeline = {
			tracks: [
				lane('video', 'V1', [clip('c', 'x', 40, 48, 0), clip('b', 'x', 20, 30, 8, 'L2')]),
				// y sits where pb would have to go: the sync lock cannot move pb past a clip that is
				// not linked to it, so the whole edit is refused with that reason.
				lane('audio', 'A1', [clip('y', 'x', 60, 63, 5), clip('pb', 'x', 20, 30, 8, 'L2')])
			]
		};
		const p = linkedTrimPreview(t, 'c', 'r', 6, opts({ ripple: true }))!;
		expect(p.ok).toBe(false);
		expect(p.reason).toContain('another clip on A1');
		expect(p.reason).toContain('not linked to it');
		expect(p.reason).toContain('hold Alt');
		// With links off, nothing is out of step: they were never carried.
		expect(linkedTrimPreview(t, 'c', 'r', 6, opts({ ripple: true, links: false }))!.ok).toBe(true);
	});

	test('a J/L-cut pair is carried, not refused: the sound that leads its picture still leads it', () => {
		// V1: c [0,8) then b [8,18); A1: pa under c (0..8), pb leads b by 2 s ([6,18)).
		const t: Timeline = {
			tracks: [
				lane('video', 'V1', [clip('c', 'x', 40, 48, 0, 'L1'), clip('b', 'x', 20, 30, 8, 'L2')]),
				lane('audio', 'A1', [clip('pa', 'x', 40, 46, 0, 'L1'), clip('pb', 'x', 18, 30, 6, 'L2')])
			]
		};
		const p = linkedTrimPreview(t, 'c', 'r', 6, opts({ ripple: true }))!;
		expect(p.ok).toBe(true);
		expect(p.reason).toBeNull();
		const at = (id: string) => p.ghosts.find((g) => g.id === id)!;
		expect([at('b').start, at('pb').start]).toEqual([6, 4]);
	});

	test('a sound the trim cuts back to make room is drawn and named, a picture is never cut', () => {
		// V1: c [0,8) then b [8,18); A1: pa under c (0..8), pb leads b by 2 s ([6,18)). Stretching c by 2
		// pushes b to 10 and its sound to 8 — and c's own sound follows the edge to 10, onto pb's lead.
		const t: Timeline = {
			tracks: [
				lane('video', 'V1', [clip('c', 'x', 40, 48, 0, 'L1'), clip('b', 'x', 20, 30, 8, 'L2')]),
				lane('audio', 'A1', [clip('pa', 'x', 40, 48, 0, 'L1'), clip('pb', 'x', 18, 30, 6, 'L2')])
			]
		};
		const p = linkedTrimPreview(t, 'c', 'r', 10, opts({ ripple: true }))!;
		expect(p.ok).toBe(true);
		expect(p.trimmed).toEqual(['A1']);
		const at = (id: string) => p.ghosts.find((g) => g.id === id)!;
		expect(p.ghosts.find((g) => g.id === 'pa')).toBeUndefined(); // stretched to 10, cut back to where it was
		expect(at('pb').start).toBe(8);
		// Nothing trimmed, nothing named.
		expect(linkedTrimPreview(pairs(), 'a', 'r', 8, opts())!.trimmed).toEqual([]);
		// The same cut, named by the *sound* of a J-cut: its picture would be pulled onto the one before.
		const jl: Timeline = {
			tracks: [
				lane('video', 'V1', [clip('x1', 'x', 105, 115, 5, 'L1'), clip('x2', 'x', 215, 225, 15, 'L2')]),
				lane('audio', 'A1', [clip('y1', 'x', 100, 113, 0, 'L1'), clip('y2', 'x', 213, 225, 13, 'L2')])
			]
		};
		const refused = linkedTrimPreview(jl, 'y1', 'r', 10, opts({ ripple: true }))!;
		expect(refused.ok).toBe(false);
		expect(refused.trimmed).toEqual([]);
		expect(refused.reason).toContain('never trimmed');
		expect(refused.reason).toContain('picture');
		expect(refused.reason).toContain('hold Alt');
	});

	test('a locked partner refuses, in the backend’s words', () => {
		const t = pairs();
		t.tracks[1].locked = true;
		const p = linkedTrimPreview(t, 'a', 'r', 8, opts())!;
		expect(p.ok).toBe(false);
		expect(p.reason).toBe('A linked clip is on locked track A1 — unlock it first (or hold Alt to edit this clip on its own)');
	});

	test('a partner the trim would trim away refuses, and says so', () => {
		const t = pairs();
		t.tracks[1].clips[0] = clip('pa', 'x', 0, 3, 0, 'L1'); // shares a's start, but is only 3 s long
		const p = linkedTrimPreview(t, 'a', 'l', 5, opts())!;
		expect(p.ok).toBe(false);
		expect(p.reason).toBe('The linked clip on A1 would be trimmed away by this edit (or hold Alt to edit this clip on its own)');
	});

	test('a lane left overlapping is not ok, but has no reason of its own', () => {
		// Stretch a's tail over b on V1 with ripple off: the clip itself ends up on b.
		const p = linkedTrimPreview(pairs(), 'a', 'r', 13, opts())!;
		expect(p.ok).toBe(false);
		expect(p.reason).toBeNull();
	});

	test('the partner clamps to its footage and the preview shows it', () => {
		const t = pairs();
		t.tracks[1].clips[0] = clip('pa', 'short', 0, 10, 0, 'L1');
		const f: SourceLimits = new Map([
			['x', 100],
			['short', 12]
		]);
		const p = linkedTrimPreview(t, 'a', 'r', 20, opts({ footage: f, ripple: true }))!;
		expect(p.ghosts.find((g) => g.id === 'pa')!.dur).toBe(12);
		expect(p.ghosts.find((g) => g.id === 'a')!.dur).toBe(20);
	});

	test('it never touches the timeline it was given, and works on a reactive one', () => {
		const t = pairs();
		const before = structuredClone(t);
		const reactive = (v: unknown): unknown =>
			v && typeof v === 'object'
				? new Proxy(v as object, { get: (target, key, recv) => reactive(Reflect.get(target, key, recv)) })
				: v;
		expect(() => structuredClone(reactive(t))).toThrow();
		const p = linkedTrimPreview(reactive(t) as Timeline, 'a', 'r', 8, opts({ ripple: true }))!;
		expect(p.ghosts.length).toBeGreaterThan(1);
		expect(t).toEqual(before);
	});

	test('is null for a clip that is not on the timeline', () => {
		expect(linkedTrimPreview(pairs(), 'nope', 'r', 5, opts())).toBeNull();
	});
});

// With links off the preview is the single-lane `rippleTrimPreview` it generalises: the same ghosts,
// the same clips shifted, the same verdict, wherever the edge is dragged.
describe('linkedTrimPreview without links is rippleTrimPreview', () => {
	const lane1 = (): Track =>
		lane('video', 'V1', [
			clip('a', 'x', 6, 16, 0),
			clip('b', 'x', 2, 10, 10),
			clip('c', 'x', 0, 8, 22)
		]);

	test('on every edge, longer and shorter, ripple on', () => {
		for (const id of ['a', 'b', 'c']) {
			for (const edge of ['l', 'r'] as const) {
				for (const pos of [1, 4.5, 9, 12, 15.25, 20, 24, 31, 40]) {
					const track = lane1();
					const old = rippleTrimPreview(track, id, edge, pos);
					const now = linkedTrimPreview({ tracks: [track] }, id, edge, pos, opts({ ripple: true, links: false }));
					expect([id, edge, pos, now?.ok]).toEqual([id, edge, pos, old?.ok]);
					expect(now?.ghosts.map((g) => [g.id, g.start, g.dur])).toEqual(old?.ghosts.map((g) => [g.id, g.start, g.dur]));
					expect([...(now?.shifted ?? [])]).toEqual([...(old?.shifted ?? [])]);
				}
			}
		}
	});
});
