import { describe, expect, test } from 'bun:test';
import { splitAllTracks } from './range-edit';
import { cutAllLanes, cutAllSaid, cutAllWhy, gapSaid, lockTrimmedLine, notCutLine, planMarkedRange, rangeSaid, seconds, skippedLine } from './range-edit-ui';
import type { Clip, CutAll, GapClosed, RangeEdit, Timeline, Track } from './types';

const fmt = (s: number) => `T${s.toFixed(2)}`;
const FPS = 30;

describe('the marked range', () => {
	test('both marks, each on the frame it is nearest', () => {
		expect(planMarkedRange(3, 5, FPS, 'lift')).toEqual({ ok: true, from: 3, to: 5 });
		const p = planMarkedRange(3.004, 5.0166, FPS, 'extract');
		expect(p.ok).toBe(true);
		if (p.ok) {
			expect(p.from).toBeCloseTo(3, 9);
			expect(p.to).toBeCloseTo(5.0, 9);
			// On frames: the engine is never asked for a sub-frame remainder by the gesture's own rounding.
			expect(Math.abs(p.from * FPS - Math.round(p.from * FPS))).toBeLessThan(1e-9);
			expect(Math.abs(p.to * FPS - Math.round(p.to * FPS))).toBeLessThan(1e-9);
		}
	});

	test('none says what to do and, given the keys, which keys', () => {
		const none = planMarkedRange(null, null, FPS, 'lift');
		expect(none).toEqual({ ok: false, reason: 'There is no range to lift: set an in point and an out point first' });
		const keyed = planMarkedRange(null, null, FPS, 'extract', { in: 'I', out: 'O' });
		expect(keyed.ok ? '' : keyed.reason).toBe('There is no range to extract: set an in point and an out point first (I and O at the playhead)');
	});

	test('one mark is said apart from the other', () => {
		const a = planMarkedRange(2, null, FPS, 'lift', { in: 'I', out: 'O' });
		expect(a.ok ? '' : a.reason).toBe('The range has an in point but no out point: set one to lift it (O)');
		const b = planMarkedRange(null, 2, FPS, 'lift', { in: 'I', out: 'O' });
		expect(b.ok ? '' : b.reason).toBe('The range has an out point but no in point: set one to lift it (I)');
	});

	test('a range inside one frame is refused here, not by the engine', () => {
		const r = planMarkedRange(3.0, 3.01, FPS, 'lift');
		expect(r.ok).toBe(false);
		expect(r.ok ? '' : r.reason).toContain('shorter than a frame');
		expect(planMarkedRange(5, 3, FPS, 'lift').ok).toBe(false);
		expect(planMarkedRange(3, 3, FPS, 'lift').ok).toBe(false);
	});

	test('a single frame is a range', () => {
		expect(planMarkedRange(1, 1 + 1 / FPS, FPS, 'lift').ok).toBe(true);
	});
});

describe('what a locked track, a whole clip and the sync lock say', () => {
	test('locked tracks left alone', () => {
		expect(skippedLine([])).toBeNull();
		expect(skippedLine([{ track_id: 'a2', name: 'A2' }])).toBe('A2 is locked and was left alone');
		expect(skippedLine([{ track_id: 'a2', name: 'A2' }, { track_id: 'v2', name: 'V2' }])).toBe('A2 and V2 are locked and were left alone');
		expect(skippedLine([1, 2, 3].map((i) => ({ track_id: `t${i}`, name: `T${i}` })))).toBe('T1, T2 and T3 are locked and were left alone');
	});

	test('clips left whole', () => {
		expect(notCutLine([])).toBeNull();
		expect(notCutLine(['intro.mp4'])).toBe('1 clip left whole — a half would have been under a frame: intro.mp4');
		expect(notCutLine(['a', 'b'])).toBe('2 clips left whole — one half would have been under a frame: a and b');
		expect(notCutLine(['a', 'b', 'c', 'd'])).toBe('4 clips left whole — one half would have been under a frame');
	});

	test('what the sync lock trimmed back, to the centisecond', () => {
		expect(lockTrimmedLine([])).toBeNull();
		expect(lockTrimmedLine([{ track_id: 'a1', track: 'A1', clip_id: 'x', seconds: 0.125 }])).toBe('The sync lock trimmed 0.12 s off a clip on A1 to make room');
		expect(seconds(2.4)).toBe('2.40 s');
		expect(seconds(-1)).toBe('0.00 s');
	});
});

const cutAll = (extra: Partial<CutAll> = {}): CutAll => ({ at: 3, cuts: [], skipped: [], not_cut: [], ...extra });
const range = (extra: Partial<RangeEdit> = {}): RangeEdit => ({
	from: 3,
	to: 5,
	removed: 0,
	trimmed: 0,
	split: 0,
	moved: 0,
	closed: 0,
	tracks: [],
	skipped: [],
	lock_trimmed: [],
	...extra
});

describe('the sentence after each edit', () => {
	test('a cut through every track: how many, where, and what it did not touch', () => {
		const cuts = [1, 2, 3].map((i) => ({ track_id: `t${i}`, left: {} as never, right: {} as never }));
		const clean = cutAllSaid(cutAll({ cuts }), fmt);
		expect(clean).toEqual({ text: 'Cut 3 clips at T3.00', detail: undefined, warn: false });
		const one = cutAllSaid(cutAll({ cuts: cuts.slice(0, 1) }), fmt);
		expect(one.text).toBe('Cut 1 clip at T3.00');
		const said = cutAllSaid(cutAll({ cuts, skipped: [{ track_id: 'a2', name: 'A2' }], not_cut: ['c9'] }), fmt, (id) => (id === 'c9' ? 'broll.mp4' : id));
		expect(said.warn).toBe(true);
		expect(said.detail).toBe('A2 is locked and was left alone. 1 clip left whole — a half would have been under a frame: broll.mp4');
	});

	test('a lift: the range and what happened to the clips in it', () => {
		const said = rangeSaid('lift', range({ removed: 2, trimmed: 1, split: 1 }), fmt);
		expect(said.text).toBe('Lifted T3.00 to T5.00: 2 clips removed, 1 clip trimmed, 1 clip split');
		expect(said.warn).toBe(false);
		expect(rangeSaid('lift', range(), fmt).text).toBe('Lifted T3.00 to T5.00: nothing on the tracks was in it');
		expect(rangeSaid('lift', range({ removed: 1 }), fmt).text).toBe('Lifted T3.00 to T5.00: 1 clip removed');
	});

	test('an extract also says how far the rest moved up', () => {
		const said = rangeSaid('extract', range({ removed: 1, split: 1, moved: 4, closed: 2 }), fmt);
		expect(said.text).toBe('Extracted T3.00 to T5.00: 1 clip removed, 1 clip split, 4 clips moved up 2.00 s');
		expect(rangeSaid('extract', range({ moved: 1, closed: 2 }), fmt).text).toBe('Extracted T3.00 to T5.00: nothing on the tracks was in it, 1 clip moved up 2.00 s');
	});

	test('locked tracks and the sync lock make it a warning, with both lines', () => {
		const said = rangeSaid(
			'extract',
			range({ removed: 1, moved: 2, closed: 2, skipped: [{ track_id: 'a2', name: 'A2' }], lock_trimmed: [{ track_id: 'a1', track: 'A1', clip_id: 'x', seconds: 0.5 }] }),
			fmt
		);
		expect(said.warn).toBe(true);
		expect(said.detail).toBe('A2 is locked and was left alone. The sync lock trimmed 0.50 s off a clip on A1 to make room');
	});

	test('a closed gap: its length, its track, how many moved, and the sync lock', () => {
		const g: GapClosed = { track_id: 'v1', from: 6, to: 8.5, moved: 3, lock_trimmed: [] };
		expect(gapSaid(g, 'V1')).toEqual({ text: 'Closed a 2.50 s gap on V1 — 3 clips moved up', detail: undefined, warn: false });
		const one = gapSaid({ ...g, moved: 1, lock_trimmed: [{ track_id: 'a1', track: 'A1', clip_id: 'x', seconds: 0.04 }] }, 'V1');
		expect(one.text).toBe('Closed a 2.50 s gap on V1 — 1 clip moved up');
		expect(one.warn).toBe(true);
		expect(one.detail).toBe('The sync lock trimmed 0.04 s off a clip on A1 to make room');
	});
});


const clip = (id: string, start: number, dur: number, extra: Partial<Clip> = {}): Clip => ({
	id,
	asset_id: 'a',
	source_in: 0,
	source_out: dur,
	timeline_start: start,
	volume: 1,
	fade_in: 0,
	fade_out: 0,
	...extra
});
const track = (id: string, clips: Clip[], extra: Partial<Track> = {}): Track => ({ id, kind: 'video', name: id.toUpperCase(), clips, ...extra });

describe('which lanes a cut through every track would go through', () => {
	const tl: Timeline = {
		tracks: [
			track('v1', [clip('c1', 0, 10)]),
			track('v2', [clip('c2', 4, 2)]),
			track('a1', [clip('c3', 0, 10)], { kind: 'audio', locked: true }),
			track('a2', [clip('c4', 12, 3)], { kind: 'audio' })
		]
	};

	test('every unlocked lane with a clip it is inside; the locked ones are named, not cut', () => {
		expect(cutAllLanes(tl, 5, 30)).toEqual({ cut: ['v1', 'v2'], locked: ['A1'], blocked: null });
		expect(cutAllLanes(tl, 2, 30)).toEqual({ cut: ['v1'], locked: ['A1'], blocked: null });
	});

	test('a time on a clip edge, or in a gap, goes through nothing', () => {
		expect(cutAllLanes(tl, 4, 30).cut).toEqual(['v1']);
		expect(cutAllLanes(tl, 11, 30)).toEqual({ cut: [], locked: [], blocked: null });
		expect(cutAllLanes(tl, 10, 30)).toEqual({ cut: [], locked: [], blocked: null });
	});

	test('a time within a frame of a clip’s edge is not cut there (a half under a frame is never made)', () => {
		expect(cutAllLanes(tl, 4 + 0.01, 30).cut).toEqual(['v1']);
		expect(cutAllLanes(tl, 4 + 1 / 30, 30).cut).toEqual(['v1', 'v2']);
	});

	test('a linked partner is cut with its clip, unless links are off', () => {
		const linked: Timeline = {
			tracks: [track('v1', [clip('c1', 0, 10, { link_id: 'L' })]), track('a1', [clip('c3', 0, 10, { link_id: 'L' })], { kind: 'audio' }), track('a2', [clip('c4', 0, 10)], { kind: 'audio', locked: true })]
		};
		expect(cutAllLanes(linked, 5, 30).cut).toEqual(['v1', 'a1']);
		// Both are under the time themselves, so Alt changes nothing about *which* lanes: each is cut by its own clip.
		expect(cutAllLanes(linked, 5, 30, false).cut).toEqual(['v1', 'a1']);
	});

	test('a partner that is not under the time on a lane of its own is carried along only with links on', () => {
		// The picture is 0-10; its sound is 6-10, so 5 is inside the picture only. The sound is not cut at 5 (the time is
		// not inside it): lanes are what the clips under the time are on.
		const offset: Timeline = { tracks: [track('v1', [clip('c1', 0, 10, { link_id: 'L' })]), track('a1', [clip('c3', 6, 4, { link_id: 'L' })], { kind: 'audio' })] };
		expect(cutAllLanes(offset, 5, 30).cut).toEqual(['v1']);
	});
});

describe('a locked track that holds a linked partner refuses the whole cut', () => {
	// The picture on V1 and its sound on locked A1 are one link group; V2 has a clip of its own under the time.
	const tl: Timeline = {
		tracks: [
			track('v1', [clip('p', 0, 10, { link_id: 'L' })]),
			track('v2', [clip('r', 0, 10)]),
			track('a1', [clip('q', 0, 10, { link_id: 'L' })], { kind: 'audio', locked: true })
		]
	};

	test('the preview promises nothing — the engine cuts no lane — and names the track', () => {
		expect(cutAllLanes(tl, 5, 30)).toEqual({ cut: [], locked: ['A1'], blocked: 'A1' });
	});

	test('with Alt (links off) the other clips are cut on their own and the locked one is left, as the engine does', () => {
		expect(cutAllLanes(tl, 5, 30, false)).toEqual({ cut: ['v1', 'v2'], locked: ['A1'], blocked: null });
	});

	test('a locked clip that is not linked to anything being cut is only left alone', () => {
		const other: Timeline = { tracks: [track('v1', [clip('p', 0, 10, { link_id: 'L' })]), track('a1', [clip('q', 0, 10, { link_id: 'M' })], { kind: 'audio', locked: true })] };
		expect(cutAllLanes(other, 5, 30)).toEqual({ cut: ['v1'], locked: ['A1'], blocked: null });
	});

	test('a partner the time is not inside does not block (it would not be cut)', () => {
		const away: Timeline = { tracks: [track('v1', [clip('p', 0, 10, { link_id: 'L' })]), track('a1', [clip('q', 6, 4, { link_id: 'L' })], { kind: 'audio', locked: true })] };
		expect(cutAllLanes(away, 5, 30).blocked).toBeNull();
		expect(cutAllLanes(away, 7, 30).blocked).toBe('A1');
	});

	test('the sentence says how to get past it, for the key and for the click', () => {
		const lanes = cutAllLanes(tl, 5, 30);
		// One sentence everywhere; only the razor click, which has Alt, says how Alt gets past it.
		expect(cutAllWhy(lanes)).toBe('A1 is locked and holds a clip linked to what would be cut — unlock it first');
		expect(cutAllWhy(lanes, 'pointer')).toBe('A1 is locked and holds a clip linked to what would be cut — unlock it, or hold Alt to cut the other clips on their own');
		expect(cutAllWhy(lanes)).not.toContain('Alt');
	});

	test('the frame it counts is the rate it is given: a half-frame sliver at 24 fps is a cut at 120 fps', () => {
		const t: Timeline = { tracks: [track('v1', [clip('p', 0, 10)])] };
		const near = 0.03;
		expect(cutAllLanes(t, near, 24).cut).toEqual([]);
		expect(cutAllLanes(t, near, 120).cut).toEqual(['v1']);
	});
});

describe('the preview agrees with the engine port it mirrors', () => {
	const cases: [string, Timeline, number, boolean][] = [
		['a locked linked partner', { tracks: [track('v1', [clip('p', 0, 10, { link_id: 'L' })]), track('a1', [clip('q', 0, 10, { link_id: 'L' })], { kind: 'audio', locked: true })] }, 5, true],
		['the same with links off', { tracks: [track('v1', [clip('p', 0, 10, { link_id: 'L' })]), track('a1', [clip('q', 0, 10, { link_id: 'L' })], { kind: 'audio', locked: true })] }, 5, false],
		['an unlinked locked clip', { tracks: [track('v1', [clip('p', 0, 10)]), track('a1', [clip('q', 0, 10)], { kind: 'audio', locked: true })] }, 5, true],
		['a partner the time is not inside', { tracks: [track('v1', [clip('p', 0, 10, { link_id: 'L' })]), track('a1', [clip('q', 6, 4, { link_id: 'L' })], { kind: 'audio', locked: true })] }, 5, true],
		['two unlocked lanes', { tracks: [track('v1', [clip('p', 0, 10)]), track('v2', [clip('r', 2, 6)])] }, 5, true]
	];
	for (const [name, tl, at, links] of cases) {
		test(name, () => {
			const preview = cutAllLanes(tl, at, 30, links);
			const scratch = structuredClone(tl);
			let outcome: 'cut' | 'refused' = 'cut';
			let result;
			try {
				result = splitAllTracks(scratch, at, undefined, { links, frame: 1 / 30 });
			} catch (e) {
				outcome = 'refused';
				expect(String((e as Error).message)).toContain('locked');
			}
			// Refused whole exactly when the preview says blocked; cut on the lanes it names otherwise.
			expect(outcome === 'refused').toBe(preview.blocked !== null);
			if (result) {
				const cutTracks = new Set(result.cuts.map((c) => c.track_id));
				expect([...cutTracks].sort()).toEqual([...preview.cut].sort());
			}
		});
	}
});

describe('why a cut through every track has nothing to cut', () => {
	const lanes = (cut: string[], locked: string[]) => ({ cut, locked, blocked: null });

	test('none when some lane has something', () => {
		expect(cutAllWhy(lanes(['v1'], ['A1']))).toBeNull();
	});

	test('the playhead is not on a clip, or only on clips of locked tracks', () => {
		expect(cutAllWhy(lanes([], []))).toBe('Move the playhead onto a clip to cut every track there');
		expect(cutAllWhy(lanes([], ['A1']))).toBe('A1 is locked — nothing else is under the playhead to cut');
		expect(cutAllWhy(lanes([], ['A1', 'V2']))).toBe('A1 and V2 are locked — nothing else is under the playhead to cut');
	});

	test('a click on empty lane space says the point matters, not the playhead', () => {
		expect(cutAllWhy(lanes([], []), 'pointer')).toBe('No clip is under that point on an unlocked track — nothing to cut');
		expect(cutAllWhy(lanes([], ['A1']), 'pointer')).toBe('A1 is locked — nothing else is under that point to cut');
		expect(cutAllWhy(lanes([], []), 'pointer')).not.toContain('playhead');
	});
});
