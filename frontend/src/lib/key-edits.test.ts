import { describe, expect, test } from 'bun:test';
import { cutAtPlayhead, keyEditDone, planKeyEdit, planPlayheadSplit, splitDone, splitNotice } from './key-edits';
import type { Asset, Clip, Timeline, Track } from './types';

const FPS = 30;
const clip = (id: string, start: number, dur: number, extra: Partial<Clip> = {}): Clip => ({
	id,
	asset_id: 'a',
	source_in: 10,
	source_out: 10 + dur,
	timeline_start: start,
	volume: 1,
	fade_in: 0,
	fade_out: 0,
	...extra
});
const track = (id: string, clips: Clip[], extra: Partial<Track> = {}): Track => ({ id, kind: 'video', name: id.toUpperCase(), clips, ...extra });
const cut = (...tracks: Track[]): Timeline => ({ tracks });
const assets = [{ id: 'a', duration: 120, streams: [{ index: 0, kind: 'video', codec: 'h264', fps: 30 }] }] as unknown as Asset[];
const frame = 1 / FPS;

describe('Split at playhead — what it cuts', () => {
	const tl = cut(track('v1', [clip('c1', 0, 10), clip('c2', 10, 10)]), track('a1', [clip('c3', 0, 10)], { kind: 'audio' }));

	test('with nothing selected: every clip under the playhead, on the frame it is on', () => {
		const plan = planPlayheadSplit(tl, [], 3.004, FPS);
		expect(plan.selection).toBe(false);
		expect(plan.splits.map((s) => s.clipId)).toEqual(['c1', 'c3']);
		expect(plan.splits[0].at).toBeCloseTo(3, 6);
	});

	test('with a selection: only the selected clips under it', () => {
		const plan = planPlayheadSplit(tl, ['c3'], 3, FPS);
		expect(plan.selection).toBe(true);
		expect(plan.splits.map((s) => s.clipId)).toEqual(['c3']);
		expect(planPlayheadSplit(tl, ['c2'], 3, FPS).splits).toEqual([]);
	});

	test('a link group is one cut — the backend splits the partners itself', () => {
		const linked = cut(
			track('v1', [clip('c1', 0, 10, { link_id: 'L' })]),
			track('a1', [clip('c3', 0, 10, { link_id: 'L' })], { kind: 'audio' })
		);
		const plan = planPlayheadSplit(linked, ['c1', 'c3'], 3, FPS);
		expect(plan.splits.map((s) => s.clipId)).toEqual(['c1']);
		expect(plan.under).toBe(2);
	});

	test('the ends of a clip are not inside it', () => {
		expect(planPlayheadSplit(tl, [], 10, FPS).splits).toEqual([]);
		expect(planPlayheadSplit(tl, [], 0, FPS).splits).toEqual([]);
		expect(planPlayheadSplit(tl, [], 25, FPS).under).toBe(0);
	});

	test('a locked track is said for a selected clip and quietly skipped for "everything under it"', () => {
		const locked = cut(track('v1', [clip('c1', 0, 10)], { locked: true }), track('v2', [clip('c2', 0, 10)]));
		const all = planPlayheadSplit(locked, [], 3, FPS);
		expect(all.splits.map((s) => s.clipId)).toEqual(['c2']);
		expect(all.problems).toEqual([]);
		const picked = planPlayheadSplit(locked, ['c1'], 3, FPS);
		expect(picked.splits).toEqual([]);
		expect(picked.problems).toEqual(['V1 is locked']);
	});

	test('a clip too short to cut on a frame is a problem, not a cut', () => {
		const tiny = cut(track('v1', [clip('c1', 0, frame * 0.9)]));
		const plan = planPlayheadSplit(tiny, [], frame * 0.45, FPS);
		expect(plan.splits).toEqual([]);
		expect(plan.problems[0]).toContain('too short');
	});
});

describe('Split at playhead — what it says', () => {
	test('nothing under the playhead says where to put it', () => {
		const empty = { splits: [], problems: [], under: 0, selection: false };
		expect(splitNotice(empty, 0)).toBe('Move the playhead onto a clip to split it');
		expect(splitNotice({ ...empty, selection: true }, 1)).toBe('Move the playhead into the selected clip to split it');
		expect(splitNotice({ ...empty, selection: true }, 3)).toBe('Move the playhead into the selected clips to split them');
	});

	test('problems are the sentence', () => {
		expect(splitNotice({ splits: [], problems: ['V1 is locked'], under: 1, selection: true }, 1)).toBe('V1 is locked');
	});

	test('the toast counts', () => {
		expect(splitDone(1)).toBe('Split at the playhead');
		expect(splitDone(3)).toBe('Split 3 clips at the playhead');
	});
});

describe('Roll, slip and slide from the keyboard', () => {
	const tl = cut(track('v1', [clip('c1', 0, 5), clip('c2', 5, 5)]));
	const o = (over: Partial<Parameters<typeof planKeyEdit>[1]> = {}) => ({
		timeline: tl,
		assets,
		selected: ['c1'],
		primary: 'c1',
		time: 5.2,
		fps: FPS,
		...over
	});

	test('slip is in source seconds, a frame at the clip’s speed, and back is earlier in the footage', () => {
		const fwd = planKeyEdit('slip', o(), 1);
		expect(fwd.ok && fwd.plan.edit).toEqual({ tool: 'slip', clipId: 'c1' });
		expect(fwd.ok && fwd.plan.delta).toBeCloseTo(frame, 9);
		const back = planKeyEdit('slip', o(), -1);
		expect(back.ok && back.plan.delta).toBeCloseTo(-frame, 9);
		const fast = cut(track('v1', [clip('c1', 0, 5, { speed: 2 })]));
		const quick = planKeyEdit('slip', o({ timeline: fast }), 1);
		expect(quick.ok && quick.plan.delta).toBeCloseTo(frame * 2, 9);
	});

	test('a slip that cannot move — the footage starts at its first frame — says why instead of sending it', () => {
		const start = cut(track('v1', [clip('c1', 0, 5, { source_in: 0, source_out: 5 })]));
		const r = planKeyEdit('slip', o({ timeline: start }), -1);
		expect(r.ok).toBe(false);
		expect(!r.ok && r.reason.length).toBeGreaterThan(0);
	});

	test('slide moves the clip a frame along its track', () => {
		const r = planKeyEdit('slide', o({ selected: ['c2'], primary: 'c2' }), -1);
		expect(r.ok && r.plan.edit).toEqual({ tool: 'slide', clipId: 'c2' });
		expect(r.ok && r.plan.delta).toBeCloseTo(-frame, 9);
	});

	test('slip and slide need a clip', () => {
		expect(planKeyEdit('slip', o({ selected: [], primary: null }), 1)).toEqual({ ok: false, reason: 'Select a clip first' });
		expect(planKeyEdit('slide', o({ selected: [], primary: null }), 1)).toEqual({ ok: false, reason: 'Select a clip first' });
		expect(planKeyEdit('slip', o({ selected: ['gone'], primary: 'gone' }), 1).ok).toBe(false);
	});

	test('roll takes the cut nearest the playhead, and says where the cut stands afterwards', () => {
		const r = planKeyEdit('roll', o({ selected: [], primary: null, time: 5.3 }), 1);
		expect(r.ok && r.plan.edit).toEqual({ tool: 'roll', a: 'c1', b: 'c2' });
		expect(r.ok && r.plan.delta).toBeCloseTo(frame, 9);
		expect(r.ok && r.plan.cutAt).toBeCloseTo(5 + frame, 6);
		const back = planKeyEdit('roll', o({ selected: [], primary: null, time: 4.9 }), -1);
		expect(back.ok && back.plan.cutAt).toBeCloseTo(5 - frame, 6);
	});

	test('no cut near the playhead is a sentence', () => {
		const far = planKeyEdit('roll', o({ selected: [], primary: null, time: 9 }), 1);
		expect(far).toEqual({ ok: false, reason: 'Put the playhead on a cut between two touching clips to roll it' });
		const gap = cut(track('v1', [clip('c1', 0, 5), clip('c2', 6, 5)]));
		expect(planKeyEdit('roll', o({ timeline: gap, time: 5.5 }), 1).ok).toBe(false);
	});

	test('the cut that counts is on the selected clip’s track', () => {
		const two = cut(track('v1', [clip('c1', 0, 5), clip('c2', 5, 5)]), track('v2', [clip('d1', 0, 6), clip('d2', 6, 5)]));
		expect(cutAtPlayhead(two, [], 5.5)?.a).toBe('c1');
		expect(cutAtPlayhead(two, ['d1'], 5.5)?.a).toBe('d1');
		expect(cutAtPlayhead(two, [], 30)).toBeNull();
	});

	test('the toast names what was done', () => {
		expect(keyEditDone('roll', -1)).toBe('Rolled the cut back a frame');
		expect(keyEditDone('slip', 1)).toBe('Slipped the footage forward a frame');
		expect(keyEditDone('slide', 1)).toBe('Slid the clip forward a frame');
	});
});
