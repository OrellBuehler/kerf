import { describe, expect, test } from 'bun:test';
import { nudgedStart, nudgeFrames, NUDGE_QUIET_MS } from './nudge';
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
