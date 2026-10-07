import { describe, expect, test } from 'bun:test';
import {
	LANE_MIN_PX,
	LANE_PX_CAP,
	LANE_TAIL_PX,
	ZOOM_DEFAULT,
	ZOOM_MAX,
	ZOOM_MIN,
	ZOOM_STEP,
	clampZoom,
	fitZoom,
	laneWidth,
	scrollFor,
	sliderToZoom,
	stepZoom,
	wheelZoomFactor,
	zoomAround,
	zoomCeiling,
	zoomLabel,
	zoomToSlider
} from './zoom';

describe('range', () => {
	test('is wide enough for an hour on screen and for frame-level work', () => {
		// An hour in a 1800 px window needs 0.5 px/s; the floor is well under that.
		expect(ZOOM_MIN).toBeLessThan(0.5);
		// 2000 px/s is 33 px a frame at 60 fps.
		expect(ZOOM_MAX / 60).toBeGreaterThan(30);
		expect(ZOOM_DEFAULT).toBeGreaterThan(ZOOM_MIN);
		expect(ZOOM_DEFAULT).toBeLessThan(ZOOM_MAX);
	});

	test('clampZoom holds a zoom inside it', () => {
		expect(clampZoom(0.001)).toBe(ZOOM_MIN);
		expect(clampZoom(1e9)).toBe(ZOOM_MAX);
		expect(clampZoom(36)).toBe(36);
	});

	test('clampZoom turns a value that is not a zoom into the default', () => {
		expect(clampZoom(NaN)).toBe(ZOOM_DEFAULT);
		expect(clampZoom(Infinity)).toBe(ZOOM_DEFAULT);
		expect(clampZoom(0)).toBe(ZOOM_DEFAULT);
		expect(clampZoom(-5)).toBe(ZOOM_DEFAULT);
	});

	test('a very long cut gets a lower ceiling so the lane stays inside what a browser lays out', () => {
		expect(zoomCeiling(60)).toBe(ZOOM_MAX);
		expect(zoomCeiling(3600)).toBe(ZOOM_MAX);
		const ten = zoomCeiling(36_000);
		expect(ten).toBeLessThan(ZOOM_MAX);
		expect(36_000 * ten).toBeLessThanOrEqual(LANE_PX_CAP);
		expect(clampZoom(ZOOM_MAX, 36_000)).toBe(ten);
	});

	test('the ceiling never drops below the floor, however long the cut', () => {
		expect(zoomCeiling(1e12)).toBe(ZOOM_MIN);
		expect(zoomCeiling(NaN)).toBe(ZOOM_MAX);
	});
});

describe('laneWidth', () => {
	test('is the cut plus a tail, with a floor, and at least 8 seconds of lane', () => {
		expect(laneWidth(100, 10)).toBe(1000 + LANE_TAIL_PX);
		expect(laneWidth(0, 10)).toBe(LANE_MIN_PX);
		expect(laneWidth(2, 100)).toBe(8 * 100 + LANE_TAIL_PX);
	});

	test('rounds a fractional width up to a whole pixel', () => {
		expect(laneWidth(10.3, 100)).toBe(Math.ceil(1030) + LANE_TAIL_PX);
		expect(laneWidth(10, 100.5)).toBe(Math.ceil(1005) + LANE_TAIL_PX);
	});
});

describe('stepZoom', () => {
	test('steps by a ratio, in and out', () => {
		expect(stepZoom(40, 1)).toBeCloseTo(40 * ZOOM_STEP, 9);
		expect(stepZoom(40, -1)).toBeCloseTo(40 / ZOOM_STEP, 9);
	});

	test('in then out comes back', () => {
		expect(stepZoom(stepZoom(36, 1), -1)).toBeCloseTo(36, 9);
	});

	test('stops at both ends of the range', () => {
		expect(stepZoom(ZOOM_MAX, 1)).toBe(ZOOM_MAX);
		expect(stepZoom(ZOOM_MIN, -1)).toBe(ZOOM_MIN);
	});

	test('takes as many steps from one end to the other as the decades demand', () => {
		let z = ZOOM_MIN;
		let n = 0;
		while (z < ZOOM_MAX && n < 200) {
			z = stepZoom(z, 1);
			n++;
		}
		expect(z).toBe(ZOOM_MAX);
		expect(n).toBe(Math.ceil(Math.log(ZOOM_MAX / ZOOM_MIN) / Math.log(ZOOM_STEP)));
	});
});

describe('wheelZoomFactor', () => {
	test('scrolling up zooms in, down zooms out, by the same amount', () => {
		const up = wheelZoomFactor(-100);
		const down = wheelZoomFactor(100);
		expect(up).toBeGreaterThan(1);
		expect(down).toBeLessThan(1);
		expect(up * down).toBeCloseTo(1, 9);
	});

	test('a 100 px notch is about the 15% the stepped zoom used to be', () => {
		expect(wheelZoomFactor(-100)).toBeGreaterThan(1.14);
		expect(wheelZoomFactor(-100)).toBeLessThan(1.18);
	});

	test('a trackpad pinch (small deltas) zooms by a little, and deltas add up', () => {
		const one = wheelZoomFactor(-4);
		expect(one).toBeGreaterThan(1);
		expect(one).toBeLessThan(1.01);
		expect(wheelZoomFactor(-4) * wheelZoomFactor(-4)).toBeCloseTo(wheelZoomFactor(-8), 9);
	});

	test('a browser that reports lines agrees with one that reports pixels on how far a notch goes', () => {
		const px = wheelZoomFactor(-100, 0);
		const lines = wheelZoomFactor(-3, 1);
		expect(Math.abs(lines - px)).toBeLessThan(0.02);
	});

	test('no single event zooms by more than 1.5x', () => {
		expect(wheelZoomFactor(-1e6)).toBe(1.5);
		expect(wheelZoomFactor(1e6)).toBeCloseTo(1 / 1.5, 9);
		expect(wheelZoomFactor(-3, 2)).toBeLessThanOrEqual(1.5);
	});

	test('a delta that is not a number does nothing', () => {
		expect(wheelZoomFactor(NaN)).toBe(1);
		expect(wheelZoomFactor(0)).toBe(1);
	});
});

describe('zoomAround', () => {
	const timeAt = (v: { zoom: number; scrollLeft: number }, offset: number) => (v.scrollLeft + offset) / v.zoom;

	test('keeps the time under the pointer fixed when zooming in', () => {
		const before = { zoom: 36, scrollLeft: 500 };
		const after = zoomAround(before, 300, 144);
		expect(timeAt(after, 300)).toBeCloseTo(timeAt(before, 300), 9);
		expect(after.zoom).toBe(144);
	});

	test('keeps the time under the pointer fixed when zooming out', () => {
		const before = { zoom: 400, scrollLeft: 12_000 };
		const after = zoomAround(before, 120, 25);
		expect(timeAt(after, 120)).toBeCloseTo(timeAt(before, 120), 9);
	});

	test('holds for every pointer position across the visible lane', () => {
		const before = { zoom: 50, scrollLeft: 4000 };
		for (const offset of [0, 1, 250, 777, 1200]) {
			const after = zoomAround(before, offset, 63.7);
			expect(timeAt(after, offset)).toBeCloseTo(timeAt(before, offset), 9);
		}
	});

	test('zooming by a factor and back returns the view', () => {
		const before = { zoom: 36, scrollLeft: 800 };
		const there = zoomAround(before, 410, 36 * 1.5);
		const back = zoomAround(there, 410, 36);
		expect(back.scrollLeft).toBeCloseTo(800, 6);
	});

	test('cannot scroll before the start: a point near the left edge stays on screen instead', () => {
		const after = zoomAround({ zoom: 100, scrollLeft: 0 }, 50, 10);
		expect(after.scrollLeft).toBe(0);
	});

	test('a pointer at the very left edge anchors the first visible time', () => {
		const after = zoomAround({ zoom: 20, scrollLeft: 200 }, 0, 80);
		expect(after.scrollLeft).toBeCloseTo((200 / 20) * 80, 9);
	});

	test('scrollFor is the scroll that puts a time at an offset', () => {
		expect(scrollFor(10, 100, 50)).toBe(400);
		expect(scrollFor(0, 0, 50)).toBe(0);
		expect(scrollFor(1, 500, 10)).toBe(0); // would be negative
	});
});

describe('fitZoom', () => {
	test('puts the whole cut, and the lane tail, in the visible width', () => {
		const z = fitZoom(120, 1248)!;
		expect(z).toBeCloseTo((1248 - LANE_TAIL_PX) / 120, 9);
		expect(laneWidth(120, z)).toBeLessThanOrEqual(1248 + 1);
	});

	test('a short cut zooms in to fill the width', () => {
		expect(fitZoom(2, 1048)!).toBeGreaterThan(ZOOM_DEFAULT);
	});

	test('a long cut zooms out', () => {
		expect(fitZoom(3600, 1248)!).toBeLessThan(1);
	});

	test('there is nothing to fit in an empty cut or before the width is known', () => {
		expect(fitZoom(0, 1000)).toBeNull();
		expect(fitZoom(-3, 1000)).toBeNull();
		expect(fitZoom(NaN, 1000)).toBeNull();
		expect(fitZoom(60, 0)).toBeNull();
	});

	test('stays inside the range: a cut too long for the floor shows as much as it can', () => {
		expect(fitZoom(1e9, 1000)).toBe(ZOOM_MIN);
		expect(fitZoom(0.001, 1000)).toBe(ZOOM_MAX);
	});

	test('a window narrower than the tail still gets a usable zoom', () => {
		expect(fitZoom(60, 30)).toBe(ZOOM_MIN);
	});
});

describe('the slider', () => {
	test('runs the whole range', () => {
		expect(zoomToSlider(ZOOM_MIN)).toBeCloseTo(0, 9);
		expect(zoomToSlider(ZOOM_MAX)).toBeCloseTo(1, 9);
		expect(sliderToZoom(0)).toBeCloseTo(ZOOM_MIN, 9);
		expect(sliderToZoom(1)).toBeCloseTo(ZOOM_MAX, 6);
	});

	test('is logarithmic: a decade is the same travel anywhere in the range', () => {
		const a = zoomToSlider(1) - zoomToSlider(0.1);
		const b = zoomToSlider(100) - zoomToSlider(10);
		expect(a).toBeCloseTo(b, 9);
	});

	test('round-trips', () => {
		for (const z of [0.05, 0.3, 1, 8, 36, 96, 500, 2000]) expect(sliderToZoom(zoomToSlider(z))).toBeCloseTo(z, 6);
	});

	test('is monotonic', () => {
		let last = -1;
		for (let p = 0; p <= 1; p += 0.05) {
			const z = sliderToZoom(p);
			expect(z).toBeGreaterThan(last);
			last = z;
		}
	});

	test('survives positions and zooms that are out of range', () => {
		expect(sliderToZoom(-1)).toBeCloseTo(ZOOM_MIN, 9);
		expect(sliderToZoom(7)).toBeCloseTo(ZOOM_MAX, 6);
		expect(sliderToZoom(NaN)).toBeCloseTo(ZOOM_MIN, 9);
		expect(zoomToSlider(1e9)).toBeCloseTo(1, 9);
		expect(Number.isFinite(zoomToSlider(NaN))).toBe(true);
	});
});

describe('zoomLabel', () => {
	test('reads in px/s with a precision that suits the size', () => {
		expect(zoomLabel(36)).toBe('36 px/s');
		expect(zoomLabel(0.5)).toBe('0.50 px/s');
		expect(zoomLabel(2.34)).toBe('2.3 px/s');
		expect(zoomLabel(1250)).toBe('1,250 px/s');
	});
});
