import { describe, expect, test } from 'bun:test';
import { DEFAULT_FRAME_PX, FRAME_STEPS, MAX_FRAME_PX, RESIZE_SETTLE_MS, requestWidth } from './preview-size';

describe('requestWidth', () => {
	test('is css width × pixel ratio, rounded up to a step', () => {
		expect(requestWidth(300, 1)).toBe(480);
		expect(requestWidth(480, 1)).toBe(480);
		expect(requestWidth(481, 1)).toBe(960);
		expect(requestWidth(913, 1)).toBe(960);
		expect(requestWidth(913, 2)).toBe(1920); // 1826 device px: not the 960 it used to be asked at
		expect(requestWidth(650, 2)).toBe(1440);
		expect(requestWidth(720, 1.5)).toBe(1440);
	});

	test('a frame the pane could not show at 720 css px is asked for at its real size', () => {
		// 1920×1080 screen: the pane allows ~913 css px of frame, which the 720 cap used to hold to 720.
		expect(requestWidth(913, 1)).toBeGreaterThan(720);
		// 2560×1440: the frame is ~1500 css px, the old cap's postage stamp.
		expect(requestWidth(1500, 1)).toBe(1920);
	});

	test('is bounded by the biggest step, however wide the box or sharp the screen', () => {
		for (const [w, dpr] of [[1920, 2], [4000, 1], [1500, 3], [1e9, 1]]) expect(requestWidth(w, dpr)).toBe(MAX_FRAME_PX);
		expect(MAX_FRAME_PX).toBe(1920);
	});

	test('moves in steps, so dragging a panel edge is a handful of requests, not one per pixel', () => {
		const asked = new Set<number>();
		for (let w = 200; w <= 1200; w++) asked.add(requestWidth(w, 1));
		expect([...asked].sort((a, b) => a - b)).toEqual([480, 960, 1440]);
	});

	test('never asks for less than the box shows, up to the cap', () => {
		for (const dpr of [1, 1.25, 1.5, 2]) {
			for (let w = 100; w <= 960; w += 7) {
				const asked = requestWidth(w, dpr);
				expect(asked).toBeGreaterThanOrEqual(Math.min(MAX_FRAME_PX, Math.ceil(w * dpr - 1e-6)));
				expect(FRAME_STEPS).toContain(asked);
			}
		}
	});

	test('a measurement that is not a size is the default, not NaN in a request', () => {
		for (const w of [0, -5, NaN, Infinity]) expect(requestWidth(w, 1)).toBe(DEFAULT_FRAME_PX);
		expect(requestWidth(500, NaN)).toBe(960);
		expect(requestWidth(500, 0)).toBe(960);
	});

	test('the steps ascend and the settle time is a debounce, not a stall', () => {
		for (let i = 1; i < FRAME_STEPS.length; i++) expect(FRAME_STEPS[i]).toBeGreaterThan(FRAME_STEPS[i - 1]);
		expect(RESIZE_SETTLE_MS).toBeGreaterThanOrEqual(100);
		expect(RESIZE_SETTLE_MS).toBeLessThanOrEqual(400);
	});
});
