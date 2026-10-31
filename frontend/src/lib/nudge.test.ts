import { describe, expect, test } from 'bun:test';
import { laneEdges, nudgedStart, nudgeFrames, nudgePlan, NUDGE_QUIET_MS, type NudgeRun } from './nudge';
import type { Clip, Timeline } from './types';
import { frameTime, onFrame } from './frames';

describe('nudgeFrames', () => {
	test('left and right are a frame, with Shift a second of the rate', () => {
		expect(nudgeFrames('ArrowRight', false, 30)).toBe(1);
		expect(nudgeFrames('ArrowLeft', false, 30)).toBe(-1);
		expect(nudgeFrames('ArrowRight', true, 30)).toBe(30);
		expect(nudgeFrames('ArrowLeft', true, 24)).toBe(-24);
	});

	test('a second at 29.97 is the 30 frames the timecode counts', () => {
		expect(nudgeFrames('ArrowRight', true, 30000 / 1001)).toBe(30);
	});

	test('a rate that is nonsense falls back to 30', () => {
		expect(nudgeFrames('ArrowRight', true, 0)).toBe(30);
		expect(nudgeFrames('ArrowRight', true, Number.NaN)).toBe(30);
	});

	test('other keys are not a nudge', () => {
		for (const k of ['ArrowUp', 'ArrowDown', 'a', 'Enter', ' ']) expect(nudgeFrames(k, false, 30)).toBeNull();
	});
});

describe('nudgedStart', () => {
	test('a run of frames moves the clip by that many frames of the cut, on the frame grid', () => {
		for (const fps of [24, 25, 30, 30000 / 1001]) {
			const start = frameTime(100, fps);
			for (const k of [1, -1, 5, -7, fps | 0]) {
				const to = nudgedStart(start, 2, k, [], fps);
				expect(onFrame(to, fps)).toBe(true);
				expect(to).toBe(frameTime(100 + k, fps));
			}
		}
	});

	test('a clip between frames goes to the neighbouring one, not to an unrelated place', () => {
		// 12.0123 s at 30 fps is frame 360.37: +1 is frame 361
		expect(nudgedStart(12.0123, 2, 1, [], 30)).toBe(frameTime(361, 30));
		expect(nudgedStart(12.0123, 2, -1, [], 30)).toBe(frameTime(359, 30));
	});

	test('never before 0', () => {
		expect(nudgedStart(0, 2, -1, [], 30)).toBe(0);
		expect(nudgedStart(frameTime(2, 30), 2, -30, [], 30)).toBe(0);
	});

	test('butting a neighbour lands exactly on its edge, not an ULP inside it', () => {
		// the neighbour ends where a hair of float noise would put the clip an ULP early
		const edge = 12;
		const start = edge + 1 / 30;
		const to = nudgedStart(start, 3, -1, [edge], 30);
		expect(to).toBe(edge);
	});

	test('its own tail butting the next clip is the latest start that does not pass it', () => {
		const dur = 1.1;
		const to = nudgedStart(frameTime(100, 30), dur, 1, [frameTime(101, 30) + dur], 30);
		expect(to + dur).toBeLessThanOrEqual(frameTime(101, 30) + dur);
	});

	test('the quiet period is a deliberate one', () => {
		expect(NUDGE_QUIET_MS).toBeGreaterThan(150);
		expect(NUDGE_QUIET_MS).toBeLessThan(1000);
	});
});

function clip(id: string, start: number, dur: number, extra: Partial<Clip> = {}): Clip {
	return { id, asset_id: 'a', source_in: 0, source_out: dur, timeline_start: start, volume: 1, fade_in: 0, fade_out: 0, ...extra };
}
const lanes = (...clips: Clip[][]): Timeline =>
	({
		tracks: clips.map((cs, i) => ({ id: `v${i + 1}`, kind: 'video', name: `V${i + 1}`, locked: false, clips: cs })),
		overlays: [],
		markers: []
	}) as unknown as Timeline;
const run = (over: Partial<NudgeRun> = {}): NudgeRun => ({
	clipId: 'c1',
	trackId: 'v1',
	members: new Set(['c1']),
	origStart: 2,
	dur: 2,
	frames: 1,
	alone: false,
	...over
});

describe('nudgePlan', () => {
	const fps = 30;

	test('the grabbed clip lands the run\u2019s frames on, as the move the run is written as', () => {
		const tl = lanes([clip('c1', 2, 2)]);
		const { at, plan } = nudgePlan(tl, run({ frames: 3 }), fps);
		expect(at).toBe(frameTime(60 + 3, fps));
		expect(plan.ok).toBe(true);
		expect(plan.moves).toEqual([{ clip_id: 'c1', timeline_start: at }]);
	});

	test('is a function of the run and the cut alone: a drag half-way elsewhere cannot change it', () => {
		// The bug: the run was written from whichever plan the timeline held, and a drag past its slop held another.
		const tl = lanes([clip('c1', 2, 2)]);
		const a = nudgePlan(tl, run(), fps);
		const b = nudgePlan(tl, run(), fps);
		expect(b).toEqual(a);
		expect(a.at).toBe(frameTime(61, fps));
		expect(a.plan.delta).toBeCloseTo(1 / fps, 9);
	});

	test('welds to a neighbour\u2019s edge and refuses a nudge onto one', () => {
		const tl = lanes([clip('c0', 0, 2), clip('c1', 2 + 1 / 30, 2), clip('c2', 6, 2)]);
		const back = nudgePlan(tl, run({ origStart: 2 + 1 / 30, frames: -1 }), fps);
		expect(back.at).toBe(2);
		expect(back.plan.ok).toBe(true);
		const into = nudgePlan(tl, run({ origStart: 2 + 1 / 30, frames: 100 }), fps);
		expect(into.plan.ok).toBe(false);
		expect(into.plan.reason).toBeTruthy();
	});

	test('a locked lane refuses and a nudge before 0 stops at 0', () => {
		const tl = lanes([clip('c1', 2, 2)]);
		(tl.tracks[0] as { locked: boolean }).locked = true;
		expect(nudgePlan(tl, run(), fps).plan.ok).toBe(false);
		(tl.tracks[0] as { locked: boolean }).locked = false;
		expect(nudgePlan(tl, run({ frames: -500 }), fps).at).toBe(0);
	});

	test('links: partners go along on their own lane unless Alt kept the clip alone', () => {
		const tl = lanes([clip('c1', 2, 2, { link_id: 'L' })], [clip('p1', 2, 2, { link_id: 'L' })]);
		const withLinks = nudgePlan(tl, run(), fps).plan;
		expect(withLinks.carried).toEqual(['p1']);
		expect(withLinks.link).toBe(true);
		const alone = nudgePlan(tl, run({ alone: true }), fps).plan;
		expect(alone.carried).toEqual([]);
		expect(alone.link).toBe(false);
	});

	test('a group moves together by the grabbed clip\u2019s distance', () => {
		const tl = lanes([clip('c1', 2, 2), clip('c2', 10, 2)]);
		const { plan } = nudgePlan(tl, run({ members: new Set(['c1', 'c2']), frames: 30 }), fps);
		expect(plan.ok).toBe(true);
		expect(plan.moves.map((m) => m.clip_id).sort()).toEqual(['c1', 'c2']);
		expect(plan.delta).toBeCloseTo(1, 9);
	});
});

describe('laneEdges', () => {
	test('every clip\u2019s start and end on the lane, except the ones that move', () => {
		const tl = lanes([clip('a', 0, 2), clip('b', 5, 3), clip('c', 10, 1)]);
		expect(laneEdges(tl, 'v1', new Set(['b']))).toEqual([0, 2, 10, 11]);
		expect(laneEdges(tl, 'nope', new Set())).toEqual([]);
	});
});
