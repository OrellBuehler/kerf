import { describe, expect, test } from 'bun:test';
import {
	boxPadding,
	dragPosition,
	packRows,
	sampleOverlay,
	scaledSize,
	snapSpanStart,
	snapTime,
	trimSpan,
	withKeyframeAt
} from './titles';
import type { TextOverlay } from './types';

function overlay(patch: Partial<TextOverlay> = {}): TextOverlay {
	return { id: 'o', text: 'Brasil 2025', start: 2, end: 6, pos_x: 0.5, pos_y: 0.5, size: 0.06, color: 'white', bold: false, ...patch };
}

describe('sampleOverlay', () => {
	test('a static title is where it says and fully opaque', () => {
		expect(sampleOverlay(overlay({ pos_x: 0.2, pos_y: 0.8 }), 3)).toEqual({ x: 0.2, y: 0.8, opacity: 1 });
	});

	test('interpolates keyframes over the overlay-local clock', () => {
		const o = overlay({
			keyframes: [
				{ time: 0, pos_x: 0.2, pos_y: 0.5, opacity: 0 },
				{ time: 2, pos_x: 0.6, pos_y: 0.5, opacity: 1 }
			]
		});
		const s = sampleOverlay(o, 3);
		expect(s.x).toBeCloseTo(0.4);
		expect(s.opacity).toBeCloseTo(0.5);
		expect(sampleOverlay(o, 99).x).toBeCloseTo(0.6);
	});
});

describe('dragPosition', () => {
	test('converts pixels to frame fractions', () => {
		expect(dragPosition({ x: 0.5, y: 0.5 }, 50, -100, 200, 400)).toEqual({ x: 0.75, y: 0.25 });
	});

	test('keeps the centre inside the frame', () => {
		expect(dragPosition({ x: 0.9, y: 0.1 }, 500, -500, 100, 100)).toEqual({ x: 1, y: 0 });
	});

	test('survives a frame with no size yet', () => {
		expect(dragPosition({ x: 0.3, y: 0.3 }, 10, 10, 0, 0)).toEqual({ x: 0.3, y: 0.3 });
	});
});

describe('scaledSize', () => {
	test('scales with the distance from the centre', () => {
		expect(scaledSize(0.06, 100, 150)).toBeCloseTo(0.09);
		expect(scaledSize(0.06, 100, 50)).toBeCloseTo(0.03);
	});

	test('clamps, but never below where the title already is', () => {
		expect(scaledSize(0.06, 100, 1)).toBe(0.02);
		expect(scaledSize(0.06, 100, 9000)).toBe(0.3);
		expect(scaledSize(0.4, 100, 100)).toBe(0.4);
	});

	test('a grab on the centre changes nothing', () => {
		expect(scaledSize(0.06, 0, 80)).toBe(0.06);
	});
});

describe('withKeyframeAt', () => {
	const animated = overlay({
		keyframes: [
			{ time: 0, pos_x: 0.2, pos_y: 0.5, opacity: 0 },
			{ time: 2, pos_x: 0.6, pos_y: 0.5, opacity: 1 }
		]
	});

	test('adds a keyframe at the playhead carrying the opacity in force', () => {
		const ks = withKeyframeAt(animated, 3, 0.9, 0.1);
		expect(ks.map((k) => k.time)).toEqual([0, 1, 2]);
		expect(ks[1]).toMatchObject({ pos_x: 0.9, pos_y: 0.1 });
		expect(ks[1].opacity).toBeCloseTo(0.5);
	});

	test('moves a keyframe the playhead is already on instead of stacking a second', () => {
		const ks = withKeyframeAt(animated, 4, 0.9, 0.9);
		expect(ks).toHaveLength(2);
		expect(ks[1]).toEqual({ time: 2, pos_x: 0.9, pos_y: 0.9, opacity: 1 });
	});
});

describe('packRows', () => {
	test('overlapping titles stack, sequential ones share a row', () => {
		const a = overlay({ id: 'a', start: 0, end: 2 });
		const b = overlay({ id: 'b', start: 2, end: 4 });
		const c = overlay({ id: 'c', start: 1, end: 3 });
		const rows = packRows([a, b, c]);
		expect(rows.get('a')).toBe(0);
		expect(rows.get('b')).toBe(0);
		expect(rows.get('c')).toBe(1);
	});
});

describe('snapping', () => {
	test('snapTime takes the nearest candidate within the threshold', () => {
		expect(snapTime(1.04, [0, 1, 2], 0.1)).toBe(1);
		expect(snapTime(1.5, [0, 1, 2], 0.1)).toBe(1.5);
	});

	test('snapSpanStart lands either edge of the span', () => {
		expect(snapSpanStart(0.95, 2, [1], 0.1)).toBe(1);
		expect(snapSpanStart(3.02, 2, [5], 0.1)).toBe(3);
		expect(snapSpanStart(-3, 2, [], 0.1)).toBe(0);
	});
});

describe('trimSpan', () => {
	test('keeps a minimum length and never goes before zero', () => {
		expect(trimSpan(2, 6, 'l', 9)).toEqual({ start: 5.9, end: 6 });
		expect(trimSpan(2, 6, 'l', -4)).toEqual({ start: 0, end: 6 });
		expect(trimSpan(2, 6, 'r', 1)).toEqual({ start: 2, end: 2.1 });
	});
});

describe('boxPadding', () => {
	test('only a title with a box colour is padded', () => {
		expect(boxPadding(overlay(), 1080)).toBe(0);
		expect(boxPadding(overlay({ bg: 'black@0.5' }), 1200)).toBeCloseTo(0.01);
	});
});
