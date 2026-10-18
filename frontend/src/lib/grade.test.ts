import { describe, expect, test } from 'bun:test';
import {
	MAX_CURVE_POINTS,
	applyCurves,
	curveShape,
	evalShape,
	gradeIsIdentity,
	lutName,
	normalizeCurve,
	normalizeGrade,
	normalizeWheel,
	wheelNeutral,
	wheelRgb
} from './grade';
import type { CurvePoint } from './types';

// The Rust tests of `model/grade.rs`, replayed: the editor draws the curve the export renders.

const pts = (p: [number, number][]): CurvePoint[] => p.map(([x, y]) => ({ x, y }));

describe('curves', () => {
	test('identity curves are dropped', () => {
		expect(normalizeCurve(pts([[0, 0], [1, 1]]))).toEqual([]);
		expect(normalizeCurve(pts([[0, 0], [0.5, 0.5], [1, 1]]))).toEqual([]);
		expect(normalizeCurve(pts([[0.3, 0.7]]))).toEqual([]);
		expect(normalizeCurve(pts([[0.2, 0.2], [0.8, 0.8]])).length).toBe(2);
		const g = { curves: { master: pts([[0, 0], [1, 1]]) }, wheels: { shadows: { x: 0, y: 0, lightness: 0 }, midtones: { x: 0, y: 0, lightness: 0 }, highlights: { x: 0, y: 0, lightness: 0 } } };
		expect(gradeIsIdentity(g)).toBe(true);
		expect(normalizeGrade(g)).toEqual({});
	});

	test('points are clamped, sorted, deduplicated and capped', () => {
		expect(normalizeCurve(pts([[1.5, -1], [0.5, 0.2], [NaN, 0.3], [0.5, 0.9]]))).toEqual(pts([[0, 0.3], [0.5, 0.2], [1, 0]]));
		const many = Array.from({ length: 40 }, (_, i) => ({ x: i / 39, y: 0.5 }));
		expect(normalizeCurve(many).length).toBe(MAX_CURVE_POINTS);
	});

	test('the curve goes through its points and never overshoots', () => {
		const shape = curveShape(pts([[0, 0], [0.25, 0.1], [0.5, 0.9], [0.75, 0.92], [1, 1]]))!;
		for (const p of shape.points) expect(Math.abs(evalShape(shape, p.x) - p.y)).toBeLessThan(1e-12);
		let prev = -1;
		for (let i = 0; i <= 1000; i++) {
			const y = evalShape(shape, i / 1000);
			expect(y).toBeGreaterThanOrEqual(prev - 1e-12);
			prev = y;
		}
		const peak = curveShape(pts([[0, 0], [0.5, 0.8], [1, 0]]))!;
		for (let i = 0; i <= 1000; i++) expect(evalShape(peak, i / 1000)).toBeLessThanOrEqual(0.8 + 1e-12);
	});

	test('the values the Rust model is pinned to', () => {
		const shape = curveShape(pts([[0, 0], [0.25, 0.1], [0.5, 0.9], [0.75, 0.92], [1, 1]]))!;
		const want: [number, number][] = [
			[0.1, 0.016457208444468113],
			[0.3, 0.21877970302616673],
			[0.4, 0.6379377269512614],
			[0.6, 0.914919187180245],
			[0.9, 0.9610172732017916]
		];
		for (const [x, y] of want) expect(Math.abs(evalShape(shape, x) - y)).toBeLessThan(1e-15);
	});

	test('a straight line is exactly linear and the ends hold', () => {
		const invert = curveShape(pts([[0, 1], [1, 0]]))!;
		for (let i = 0; i <= 255; i++) expect(Math.abs(evalShape(invert, i / 255) - (1 - i / 255))).toBeLessThan(1e-12);
		const mid = curveShape(pts([[0.2, 0.3], [0.8, 0.6]]))!;
		expect(evalShape(mid, 0)).toBe(0.3);
		expect(evalShape(mid, 1)).toBe(0.6);
	});

	test('the master runs on the channel’s result', () => {
		const c = { master: pts([[0, 1], [1, 0]]), red: pts([[0, 0], [1, 0.5]]) };
		expect(Math.abs(applyCurves(c, 0, 1) - 0.5)).toBeLessThan(1e-12);
		expect(Math.abs(applyCurves(c, 1, 0.25) - 0.75)).toBeLessThan(1e-12);
	});
});

describe('wheels', () => {
	test('a wheel is a hue and a lightness', () => {
		expect(wheelRgb({ x: 1, y: 0, lightness: 0 })).toEqual([1, -0.5, -0.5]);
		expect(wheelRgb({ x: -0.5, y: Math.sqrt(3) / 2, lightness: 0 })).toEqual([-0.5, 1, -0.5]);
		expect(wheelRgb({ x: 0, y: 0, lightness: 0.2 })).toEqual([0.2, 0.2, 0.2]);
		expect(normalizeWheel({ x: 3, y: 4, lightness: 5 })).toEqual({ x: 0.6, y: 0.8, lightness: 1 });
		expect(wheelNeutral({ x: 0, y: 0, lightness: 0 })).toBe(true);
	});
});

describe('the LUT', () => {
	test('at zero it is neutral but kept', () => {
		const g = { lut: { path: '/a.cube', intensity: 0.001 } };
		expect(gradeIsIdentity(g)).toBe(true);
		expect(normalizeGrade(g)).toEqual({ lut: { path: '/a.cube', intensity: 0 } });
	});

	test('its name is the file name on either separator', () => {
		expect(lutName('/a/b/look.cube')).toBe('look.cube');
		expect(lutName('C:\\luts\\film.cube')).toBe('film.cube');
	});
});
